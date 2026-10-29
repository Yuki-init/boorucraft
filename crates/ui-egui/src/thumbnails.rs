//! Loading thumbnails off the UI thread and keeping them as GPU textures.
//!
//! The grid asks for the thumbnails it is about to draw. Requests go on a stack that worker
//! threads take from the top, so the tiles the user is looking at now come first while
//! scrolling; old requests fall off the bottom.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

use boorucraft_thumbs::ThumbCache;

/// Most queued requests; older ones are dropped (they are asked for again when visible).
const MAX_QUEUE: usize = 512;
/// Most thumbnail textures kept; the least recently drawn go first.
const MAX_TEXTURES: usize = 3000;

struct Job {
    hash: String,
    path: PathBuf,
    format: String,
}

struct Done {
    hash: String,
    result: Result<image::RgbaImage, String>,
}

struct Shared {
    queue: Mutex<Vec<Job>>,
    wake: Condvar,
    stop: AtomicBool,
}

pub struct Thumbnails {
    shared: Arc<Shared>,
    results: Receiver<Done>,
    pending: HashSet<String>,
    failed: HashMap<String, String>,
    textures: HashMap<String, (egui::TextureHandle, u64)>,
    frame: u64,
    workers: Vec<std::thread::JoinHandle<()>>,
}

pub enum Thumb<'a> {
    Ready(&'a egui::TextureHandle),
    Loading,
    Failed(&'a str),
}

impl Thumbnails {
    pub fn new(cache: ThumbCache, ctx: egui::Context) -> Thumbnails {
        let shared = Arc::new(Shared { queue: Mutex::new(Vec::new()), wake: Condvar::new(), stop: AtomicBool::new(false) });
        let (tx, results) = channel();
        let n = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1)).clamp(1, 8);
        let workers = (0..n)
            .filter_map(|i| {
                let (shared, tx, cache, ctx) = (Arc::clone(&shared), tx.clone(), cache.clone(), ctx.clone());
                std::thread::Builder::new().name(format!("thumbs-{i}")).spawn(move || worker(&shared, &tx, &cache, &ctx)).ok()
            })
            .collect();
        Thumbnails { shared, results, pending: HashSet::new(), failed: HashMap::new(), textures: HashMap::new(), frame: 0, workers }
    }

    /// Call once per frame before drawing: turns finished thumbnails into textures.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        while let Ok(done) = self.results.try_recv() {
            self.pending.remove(&done.hash);
            match done.result {
                Ok(img) => {
                    let size = [img.width() as usize, img.height() as usize];
                    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    let tex = ctx.load_texture(format!("thumb-{}", done.hash), color, egui::TextureOptions::LINEAR);
                    self.textures.insert(done.hash, (tex, self.frame));
                }
                Err(e) => {
                    self.failed.insert(done.hash, e);
                }
            }
        }
        if self.textures.len() > MAX_TEXTURES {
            let mut by_age: Vec<(u64, String)> = self.textures.iter().map(|(k, (_, f))| (*f, k.clone())).collect();
            by_age.sort();
            for (_, k) in by_age.into_iter().take(self.textures.len() - MAX_TEXTURES) {
                self.textures.remove(&k);
            }
        }
    }

    /// The thumbnail for a file, asking for it if it isn't loaded yet.
    pub fn get(&mut self, hash: &str, path: Option<PathBuf>, format: &str) -> Thumb<'_> {
        if let Some(e) = self.failed.get(hash) {
            return Thumb::Failed(e);
        }
        let frame = self.frame;
        // `contains_key` first: returning a `get_mut` borrow conditionally upsets the borrow checker.
        if self.textures.contains_key(hash)
            && let Some((tex, used)) = self.textures.get_mut(hash)
        {
            *used = frame;
            return Thumb::Ready(tex);
        }
        if !self.pending.contains(hash)
            && let Some(path) = path
        {
            self.pending.insert(hash.to_string());
            if let Ok(mut q) = self.shared.queue.lock() {
                q.push(Job { hash: hash.to_string(), path, format: format.to_string() });
                if q.len() > MAX_QUEUE {
                    let dropped = q.remove(0);
                    self.pending.remove(&dropped.hash);
                }
            }
            self.shared.wake.notify_one();
        }
        Thumb::Loading
    }

    /// Forgets a file's thumbnail state (e.g. after its file changed).
    pub fn forget(&mut self, hash: &str) {
        self.failed.remove(hash);
        self.textures.remove(hash);
    }

    pub fn loading(&self) -> usize {
        self.pending.len()
    }
}

impl Drop for Thumbnails {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn worker(shared: &Shared, tx: &Sender<Done>, cache: &ThumbCache, ctx: &egui::Context) {
    loop {
        let job = {
            let Ok(mut q) = shared.queue.lock() else { return };
            loop {
                if shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(job) = q.pop() {
                    break job;
                }
                q = match shared.wake.wait(q) {
                    Ok(q) => q,
                    Err(_) => return,
                };
            }
        };
        let result = cache.get_or_make(&job.hash, &job.path, &job.format).map_err(|e| e.to_string());
        if tx.send(Done { hash: job.hash, result }).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

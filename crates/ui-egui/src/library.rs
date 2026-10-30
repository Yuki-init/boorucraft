//! Everything the window shows, without egui: the tag list, the open collections, background
//! scans, searches across collections, edits and debounced saves.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use boorucraft_core::{Query, Rating};
use boorucraft_dedup::Hashes;
use boorucraft_index::{ScanCache, ScanReport, SidecarReport, find_groups, needing_hashes, scan, set_hashes, sidecar_rel, write_sidecars};
use boorucraft_store::{Collection, FileEntry, Links, Row, SearchIndex, StoreError, TagList, compare_rows, paths, safefile};
use boorucraft_thumbs::ThumbCache;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// How long after the last edit collection.json and the sidecars are saved.
pub const SAVE_DELAY: Duration = Duration::from_secs(2);

/// Where the app keeps its own files: [`Dirs::system`] in the app, temporary folders in tests.
#[derive(Debug, Clone)]
pub struct Dirs {
    /// `config.json` lives here.
    pub config: PathBuf,
    /// Scan caches and thumbnails.
    pub cache: PathBuf,
    /// The central tag list.
    pub tags: PathBuf,
}

impl Dirs {
    pub fn system() -> Dirs {
        Dirs { config: paths::config_dir(), cache: paths::cache_dir(), tags: paths::tag_list_path() }
    }
}

/// The app's own settings, `~/.config/boorucraft/config.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub collections: Vec<PathBuf>,
    #[serde(default)]
    pub tile_size: Option<f32>,
    /// Most bits two perceptual hashes may differ by for near-duplicates (default 6).
    #[serde(default)]
    pub dupe_threshold: Option<u32>,
}

impl Config {
    pub fn path(dirs: &Dirs) -> PathBuf {
        dirs.config.join("config.json")
    }

    pub fn load(dirs: &Dirs) -> Config {
        safefile::read_capped(&Self::path(dirs), 16 * 1024 * 1024).ok().flatten().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, dirs: &Dirs) -> Result<(), String> {
        let path = Self::path(dirs);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        safefile::write_atomic(&path, &bytes, false).map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Moves a file to the system trash (replaced in tests).
pub type Trasher = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// One search result: a file in one of the open collections.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hit {
    pub coll: usize,
    pub hash: String,
}

pub enum CollState {
    Ready {
        c: Box<Collection>,
        cache: Box<ScanCache>,
    },
    /// The collection is on a scan thread.
    Scanning,
    Failed(String),
}

pub struct OpenCollection {
    pub root: PathBuf,
    pub name: String,
    pub state: CollState,
    pub index: SearchIndex,
    /// Set by edits; saved [`SAVE_DELAY`] later.
    pub dirty_since: Option<Instant>,
    pub sidecars_pending: BTreeSet<String>,
    /// The last save failed: the changes are still in memory and are saved again later.
    pub save_error: Option<String>,
    pub last_report: Option<String>,
}

impl OpenCollection {
    pub fn collection(&self) -> Option<&Collection> {
        match &self.state {
            CollState::Ready { c, .. } => Some(c),
            _ => None,
        }
    }

    pub fn read_only(&self) -> bool {
        self.collection().is_some_and(Collection::is_read_only)
    }
}

enum HashMsg {
    One { coll: usize, hash: String, result: Result<Hashes, String> },
    Finished,
}

/// The background job that computes perceptual hashes for pictures that have none.
struct HashJob {
    total: usize,
    done: usize,
    cancel: Arc<AtomicBool>,
}

struct ScanDone {
    coll: usize,
    c: Box<Collection>,
    cache: Box<ScanCache>,
    report: ScanReport,
}

pub struct Library {
    pub tags: TagList,
    pub colls: Vec<OpenCollection>,
    pub config: Config,
    pub dirs: Dirs,
    /// Collections came from the command line: don't write them into config.json.
    pub session_only: bool,
    pub hits: Vec<Hit>,
    pub query_error: Option<(String, std::ops::Range<usize>)>,
    /// Messages for the user (errors and notes), newest last.
    pub messages: Vec<(String, Instant)>,
    /// Tags of the current results with counts, most used first.
    pub result_tags: Vec<(String, u64)>,
    /// Ratings of the current results: safe, questionable, explicit, none.
    pub result_ratings: [u64; 4],
    /// Tag counts over every open collection (for autocomplete); `None` until next asked for.
    usage: Option<Vec<(String, u64)>>,
    /// Shuffles `order:random`; fixed for the session so results don't jump around.
    seed: u64,
    /// Near-duplicate groups over the open collections, best copy first.
    pub dupe_groups: Vec<Vec<Hit>>,
    /// The groups need working out again (after scans, hashing or decisions).
    dupes_stale: bool,
    links: Links,
    hash_job: Option<HashJob>,
    /// Pictures that couldn't be hashed this session (not retried until restart).
    hash_failed: HashSet<String>,
    hash_tx: Sender<HashMsg>,
    hash_rx: Receiver<HashMsg>,
    thumb_cache: ThumbCache,
    scan_tx: Sender<ScanDone>,
    scan_rx: Receiver<ScanDone>,
    /// Called when a background job finishes, to wake the UI.
    waker: Arc<dyn Fn() + Send + Sync>,
    /// Moves files to the trash: the system trash in the app.
    pub trasher: Trasher,
}

fn seed_now() -> u64 {
    let t = chrono::Utc::now();
    (t.timestamp() as u64).wrapping_mul(1_000_000_007) ^ u64::from(t.timestamp_subsec_nanos())
}

fn name_of(root: &Path) -> String {
    root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl Library {
    pub fn new(dirs: Dirs, config: Config, session_collections: Option<Vec<PathBuf>>, waker: Arc<dyn Fn() + Send + Sync>) -> Library {
        let (scan_tx, scan_rx) = channel();
        let (hash_tx, hash_rx) = channel();
        let mut messages = Vec::new();
        let path = dirs.tags.clone();
        let tags = match TagList::open(&path) {
            Ok(t) => t,
            Err(e @ StoreError::Locked { .. }) => {
                messages.push((format!("{e}. Tags can't be changed in this window."), Instant::now()));
                TagList::open_read_only(&path).unwrap_or_else(|_| TagList::in_memory())
            }
            Err(e) => {
                messages.push((format!("Couldn't open the tag list: {e}"), Instant::now()));
                TagList::in_memory()
            }
        };
        let session_only = session_collections.is_some();
        let roots = session_collections.unwrap_or_else(|| config.collections.clone());
        let mut lib = Library {
            tags,
            colls: Vec::new(),
            config,
            dirs: dirs.clone(),
            session_only,
            hits: Vec::new(),
            query_error: None,
            messages,
            result_tags: Vec::new(),
            result_ratings: [0; 4],
            usage: None,
            seed: seed_now(),
            dupe_groups: Vec::new(),
            dupes_stale: true,
            links: Links::default(),
            hash_job: None,
            hash_failed: HashSet::new(),
            hash_tx,
            hash_rx,
            thumb_cache: ThumbCache::new(&dirs.cache),
            scan_tx,
            scan_rx,
            waker,
            trasher: Arc::new(|p: &Path| trash::delete(p).map_err(|e| e.to_string())),
        };
        for root in roots {
            lib.open_collection(&root, false);
        }
        lib
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.messages.push((msg.into(), Instant::now()));
        if self.messages.len() > 20 {
            self.messages.remove(0);
        }
    }

    /// Opens a collection folder and starts a scan. `remember` adds it to config.json.
    pub fn open_collection(&mut self, root: &Path, remember: bool) {
        if self.colls.iter().any(|c| c.root == root || root.canonicalize().ok().is_some_and(|r| r == c.root)) {
            self.notify(format!("{} is already open", root.display()));
            return;
        }
        let opened = match Collection::open(root) {
            Err(StoreError::Locked { .. }) => {
                self.notify(format!("{} is open in another BooruCraft; showing it read-only", root.display()));
                Collection::open_read_only(root)
            }
            other => other,
        };
        let (state, root_path) = match opened {
            Ok(mut c) => {
                let changed = c.apply_tag_history(&self.tags);
                let root = c.root().to_path_buf();
                let cache = ScanCache::load(&self.dirs.cache, &root);
                let mut oc = OpenCollection {
                    root: root.clone(),
                    name: name_of(&root),
                    index: SearchIndex::build(&c),
                    state: CollState::Ready { c: Box::new(c), cache: Box::new(cache) },
                    dirty_since: None,
                    sidecars_pending: BTreeSet::new(),
                    save_error: None,
                    last_report: None,
                };
                if !changed.is_empty() {
                    oc.dirty_since = Some(Instant::now());
                    oc.sidecars_pending.extend(changed);
                }
                self.colls.push(oc);
                self.usage = None;
                self.dupes_stale = true;
                (None, root)
            }
            Err(e) => (Some(e.to_string()), root.to_path_buf()),
        };
        if let Some(err) = state {
            self.notify(format!("Couldn't open {}: {err}", root_path.display()));
            self.colls.push(OpenCollection {
                root: root_path.clone(),
                name: name_of(&root_path),
                state: CollState::Failed(err),
                index: SearchIndex::default(),
                dirty_since: None,
                sidecars_pending: BTreeSet::new(),
                save_error: None,
                last_report: None,
            });
        } else {
            let idx = self.colls.len() - 1;
            self.start_scan(idx);
        }
        if remember && !self.session_only && !self.config.collections.contains(&root_path) {
            self.config.collections.push(root_path);
            self.save_config();
        }
    }

    pub fn close_collection(&mut self, idx: usize) {
        // Background jobs report back by index, so indexes must not shift while any runs.
        if self.is_busy() {
            self.notify("Wait for scanning and duplicate checks to finish before closing a collection");
            return;
        }
        self.flush(idx);
        let oc = self.colls.remove(idx);
        self.usage = None;
        self.dupes_stale = true;
        self.config.collections.retain(|p| *p != oc.root);
        if !self.session_only {
            self.save_config();
        }
        self.hits.retain(|h| h.coll != idx);
        for h in &mut self.hits {
            if h.coll > idx {
                h.coll -= 1;
            }
        }
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save(&self.dirs) {
            self.notify(format!("Couldn't save settings: {e}"));
        }
    }

    pub fn set_tile_size(&mut self, size: f32) {
        self.config.tile_size = Some(size);
        if !self.session_only {
            self.save_config();
        }
    }

    /// Moves a collection to a scan thread.
    pub fn start_scan(&mut self, idx: usize) {
        self.flush(idx);
        let Some(oc) = self.colls.get_mut(idx) else { return };
        if oc.read_only() {
            return;
        }
        let CollState::Ready { c, cache } = std::mem::replace(&mut oc.state, CollState::Scanning) else { return };
        let tags = self.tags.snapshot();
        let tx = self.scan_tx.clone();
        let waker = Arc::clone(&self.waker);
        let spawned = std::thread::Builder::new().name("scan".into()).spawn(move || {
            let (mut c, mut cache) = (c, cache);
            let report = scan(&mut c, &tags, &mut cache);
            let _ = tx.send(ScanDone { coll: idx, c, cache, report });
            waker();
        });
        if let Err(e) = spawned {
            oc.state = CollState::Failed(format!("couldn't start a scan: {e}"));
        }
    }

    pub fn is_scanning(&self) -> bool {
        self.colls.iter().any(|c| matches!(c.state, CollState::Scanning))
    }

    /// A scan or the hashing job is running.
    pub fn is_busy(&self) -> bool {
        self.is_scanning() || self.hash_job.is_some()
    }

    /// Hashing progress: (done, total), while the job runs.
    pub fn hash_progress(&self) -> Option<(usize, usize)> {
        self.hash_job.as_ref().map(|j| (j.done, j.total))
    }

    /// Starts hashing pictures that have no perceptual hashes yet, if there are any and nothing
    /// else is running.
    fn start_hashing(&mut self) {
        if self.is_busy() {
            return;
        }
        let mut jobs: Vec<(usize, String, PathBuf, String)> = Vec::new();
        for (i, oc) in self.colls.iter().enumerate() {
            let Some(c) = oc.collection() else { continue };
            if c.is_read_only() {
                continue;
            }
            for (hash, path, format) in needing_hashes(c) {
                if boorucraft_thumbs::can_decode(&format) && !self.hash_failed.contains(&hash) {
                    jobs.push((i, hash, path, format));
                }
            }
        }
        if jobs.is_empty() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, cache, waker, stop) = (self.hash_tx.clone(), self.thumb_cache.clone(), Arc::clone(&self.waker), Arc::clone(&cancel));
        let total = jobs.len();
        let spawned = std::thread::Builder::new().name("hashing".into()).spawn(move || {
            jobs.par_iter().for_each(|(coll, hash, path, format)| {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let result = cache.thumbnail_and_hashes(hash, path, format).map(|(_, h)| h).map_err(|e| e.to_string());
                let _ = tx.send(HashMsg::One { coll: *coll, hash: hash.clone(), result });
            });
            let _ = tx.send(HashMsg::Finished);
            waker();
        });
        match spawned {
            Ok(_) => self.hash_job = Some(HashJob { total, done: 0, cancel }),
            Err(e) => self.notify(format!("Couldn't start looking for duplicates: {e}")),
        }
    }

    /// Takes in finished perceptual hashes. Returns true when the job finished.
    fn poll_hashes(&mut self) -> bool {
        let mut finished = false;
        let mut touched: BTreeSet<usize> = BTreeSet::new();
        while let Ok(msg) = self.hash_rx.try_recv() {
            match msg {
                HashMsg::One { coll, hash, result } => {
                    if let Some(job) = &mut self.hash_job {
                        job.done += 1;
                    }
                    match result {
                        Ok(h) => {
                            // The collection may be rescanning now; then the hash comes next time.
                            if let Some(oc) = self.colls.get_mut(coll)
                                && let CollState::Ready { c, .. } = &mut oc.state
                            {
                                set_hashes(c, &hash, h);
                                touched.insert(coll);
                            }
                        }
                        Err(_) => {
                            self.hash_failed.insert(hash);
                        }
                    }
                }
                HashMsg::Finished => {
                    self.hash_job = None;
                    self.dupes_stale = true;
                    finished = true;
                }
            }
        }
        for coll in touched {
            // Saved with the next debounced save; no sidecar change.
            if let Some(oc) = self.colls.get_mut(coll)
                && oc.dirty_since.is_none()
            {
                oc.dirty_since = Some(Instant::now());
            }
        }
        finished
    }

    pub fn dupe_threshold(&self) -> u32 {
        self.config.dupe_threshold.unwrap_or(boorucraft_dedup::DEFAULT_THRESHOLD).min(boorucraft_dedup::MAX_THRESHOLD)
    }

    /// Changes how alike pictures must be to count as duplicates (remembered in config.json).
    pub fn set_dupe_threshold(&mut self, bits: u32) {
        self.config.dupe_threshold = Some(bits.min(boorucraft_dedup::MAX_THRESHOLD));
        self.dupes_stale = true;
        if !self.session_only {
            self.save_config();
        }
    }

    /// Works the duplicate groups out again if anything changed since.
    pub fn refresh_dupes(&mut self) {
        if !self.dupes_stale {
            return;
        }
        self.dupes_stale = false;
        let colls: Vec<Option<&Collection>> = self.colls.iter().map(OpenCollection::collection).collect();
        let threshold = self.dupe_threshold();
        let groups = find_groups(&colls, threshold);
        self.links.dupes = groups.iter().flatten().map(|(_, h)| h.clone()).collect();
        self.dupe_groups = groups.into_iter().map(|g| g.into_iter().map(|(coll, hash)| Hit { coll, hash }).collect()).collect();
    }

    /// Takes finished scans and hashing back, and starts hashing new pictures. Returns true if
    /// the results may have changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.scan_rx.try_recv() {
            changed = true;
            let ScanDone { coll, mut c, cache, report } = done;
            let summary = format!("{} files: {} new, {} moved, {} edited, {} missing", report.files, report.added, report.moved, report.edited, report.missing);
            let problems = report.errors.len() + report.sidecars.errors.len();
            let save_err = c.save().err();
            let _ = cache.save();
            let name;
            {
                let Some(oc) = self.colls.get_mut(coll) else { continue };
                oc.index = SearchIndex::build(&c);
                self.usage = None;
                self.dupes_stale = true;
                oc.state = CollState::Ready { c, cache };
                oc.last_report = Some(summary);
                name = oc.name.clone();
            }
            if problems > 0 {
                self.notify(format!("{name}: {problems} file(s) had problems during the scan"));
            }
            if let Some(e) = save_err {
                self.notify(format!("{name}: couldn't save: {e}"));
            }
        }
        if self.poll_hashes() {
            changed = true;
        }
        if !self.is_busy() {
            self.start_hashing();
        }
        changed
    }

    /// Runs a query over every ready collection, sorted by its `order:` (default: newest
    /// imported first) across all of them.
    pub fn search(&mut self, text: &str) {
        let mut query = match Query::parse(text) {
            Ok(q) => {
                self.query_error = None;
                q
            }
            Err(e) => {
                self.query_error = Some((e.message.clone(), e.span.clone()));
                return;
            }
        };
        query.seed = self.seed;
        self.refresh_dupes();
        self.links.parents = self.colls.iter().flat_map(|oc| oc.index.parents()).map(str::to_string).collect();
        let now = chrono::Utc::now();
        let mut rows: Vec<(usize, &Row)> = Vec::new();
        for (i, oc) in self.colls.iter().enumerate() {
            if oc.collection().is_some() {
                rows.extend(oc.index.matching_rows(&query, now, &self.links).into_iter().map(|r| (i, r)));
            }
        }
        rows.sort_by(|a, b| compare_rows(query.order.as_ref(), query.seed, a.1, b.1).then(a.0.cmp(&b.0)));
        self.hits = rows.into_iter().map(|(coll, r)| Hit { coll, hash: r.hash.clone() }).collect();
        self.recount();
    }

    /// Recounts the tags and ratings of the current results (after a search or an edit).
    pub fn recount(&mut self) {
        let mut tags: HashMap<&str, u64> = HashMap::new();
        let mut ratings = [0u64; 4];
        for hit in &self.hits {
            let Some(e) = self.entry(hit) else { continue };
            for t in &e.tags {
                *tags.entry(t.as_str()).or_default() += 1;
            }
            let slot = e.rating.map_or(3, Rating::index);
            if let Some(n) = ratings.get_mut(slot) {
                *n += 1;
            }
        }
        let mut sorted: Vec<(String, u64)> = tags.into_iter().map(|(t, n)| (t.to_string(), n)).collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.result_tags = sorted;
        self.result_ratings = ratings;
    }

    /// How many files use each tag, over every open collection, most used first. Call
    /// [`Self::refresh_usage`] first in a frame.
    pub fn usage(&self) -> &[(String, u64)] {
        self.usage.as_deref().unwrap_or_default()
    }

    /// Recounts [`Self::usage`] if anything changed since.
    pub fn refresh_usage(&mut self) {
        if self.usage.is_none() {
            let mut counts: HashMap<&str, u64> = HashMap::new();
            for oc in &self.colls {
                for (t, n) in oc.index.tag_counts() {
                    *counts.entry(t).or_default() += n;
                }
            }
            let mut sorted: Vec<(String, u64)> = counts.into_iter().map(|(t, n)| (t.to_string(), n)).collect();
            sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            self.usage = Some(sorted);
        }
    }

    pub fn entry(&self, hit: &Hit) -> Option<&FileEntry> {
        self.colls.get(hit.coll)?.collection()?.get(&hit.hash)
    }

    /// The first existing path of a file.
    pub fn path_of(&self, hit: &Hit) -> Option<PathBuf> {
        let c = self.colls.get(hit.coll)?.collection()?;
        let e = c.get(&hit.hash)?;
        e.paths.iter().filter_map(|p| c.abs_path(p)).find(|p| p.exists()).or_else(|| e.paths.first().and_then(|p| c.abs_path(p)))
    }

    fn mark_dirty(&mut self, coll: usize, hashes: impl IntoIterator<Item = String>) {
        if let Some(oc) = self.colls.get_mut(coll) {
            oc.dirty_since = Some(Instant::now());
            oc.sidecars_pending.extend(hashes);
        }
    }

    /// Adds and removes tags on several files. Returns the number of files changed.
    pub fn edit_tags(&mut self, hits: &[Hit], add: &[String], remove: &[String]) -> usize {
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
        let mut changed = 0;
        let mut errors = Vec::new();
        let mut touched: Vec<(usize, String)> = Vec::new();
        for hit in hits {
            let Some(oc) = self.colls.get_mut(hit.coll) else { continue };
            let CollState::Ready { c, .. } = &mut oc.state else { continue };
            if c.is_read_only() {
                errors.push(format!("{} is read-only", oc.name));
                continue;
            }
            match c.edit_tags(&hit.hash, &add, &remove, &self.tags) {
                Ok(true) => {
                    changed += 1;
                    touched.push((hit.coll, hit.hash.clone()));
                }
                Ok(false) => {}
                Err(e) => errors.push(e.to_string()),
            }
        }
        self.after_edit(touched);
        if let Some(e) = errors.into_iter().next() {
            self.notify(e);
        }
        changed
    }

    /// Sets (or clears) the rating of several files.
    pub fn set_rating(&mut self, hits: &[Hit], rating: Option<Rating>) {
        self.edit_entries(hits, |e| {
            let changed = e.rating != rating;
            e.rating = rating;
            changed
        });
    }

    pub fn set_source(&mut self, hit: &Hit, source: Option<String>) {
        let source = source.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        self.edit_entries(std::slice::from_ref(hit), |e| {
            let changed = e.source != source;
            e.source = source.clone();
            changed
        });
    }

    fn edit_entries(&mut self, hits: &[Hit], mut f: impl FnMut(&mut FileEntry) -> bool) {
        let mut touched = Vec::new();
        for hit in hits {
            let Some(oc) = self.colls.get_mut(hit.coll) else { continue };
            let CollState::Ready { c, .. } = &mut oc.state else { continue };
            if c.is_read_only() {
                continue;
            }
            if let Some(e) = c.get_mut(&hit.hash)
                && f(e)
            {
                touched.push((hit.coll, hit.hash.clone()));
            }
        }
        self.after_edit(touched);
    }

    fn after_edit(&mut self, touched: Vec<(usize, String)>) {
        let colls: BTreeSet<usize> = touched.iter().map(|(c, _)| *c).collect();
        for (coll, hash) in touched {
            self.mark_dirty(coll, [hash]);
        }
        for coll in colls {
            if let Some(oc) = self.colls.get_mut(coll)
                && let CollState::Ready { c, .. } = &oc.state
            {
                oc.index = SearchIndex::build(c);
            }
        }
        self.usage = None;
        self.recount();
    }

    /// Marks dirty without any sidecar to rewrite (hashes, duplicate decisions).
    fn touch(&mut self, coll: usize) {
        if let Some(oc) = self.colls.get_mut(coll) {
            oc.dirty_since = Some(Instant::now());
        }
    }

    /// Records that the files of a group aren't duplicates of each other, in every collection
    /// involved, so the group doesn't come back.
    pub fn mark_not_duplicates(&mut self, group: &[Hit]) {
        let mut touched = BTreeSet::new();
        for (i, a) in group.iter().enumerate() {
            for b in group.iter().skip(i + 1) {
                for coll in [a.coll, b.coll] {
                    if let Some(oc) = self.colls.get_mut(coll)
                        && let CollState::Ready { c, .. } = &mut oc.state
                        && !c.is_read_only()
                    {
                        c.mark_not_duplicate(&a.hash, &b.hash);
                        touched.insert(coll);
                    }
                }
            }
        }
        for coll in touched {
            self.touch(coll);
        }
        self.dupes_stale = true;
    }

    /// A file by hash in any open collection.
    pub fn find(&self, hash: &str) -> Option<(usize, &FileEntry)> {
        self.colls.iter().enumerate().find_map(|(i, oc)| oc.collection()?.get(hash).map(|e| (i, e)))
    }

    /// Hashes of a file's parent, grandparent… (stops at 100 or at a loop).
    pub fn ancestors(&self, hash: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut at = self.find(hash).and_then(|(_, e)| e.parent.clone());
        while let Some(h) = at {
            if out.len() >= 100 || out.contains(&h) {
                break;
            }
            at = self.find(&h).and_then(|(_, e)| e.parent.clone());
            out.push(h);
        }
        out
    }

    /// Children of a file in the open collections.
    pub fn children(&self, hash: &str) -> Vec<Hit> {
        let mut out = Vec::new();
        for (coll, oc) in self.colls.iter().enumerate() {
            let Some(c) = oc.collection() else { continue };
            out.extend(c.files().filter(|(_, e)| e.parent.as_deref() == Some(hash)).map(|(h, _)| Hit { coll, hash: h.to_string() }));
        }
        out
    }

    /// Links `children` as versions of `parent` (`None` unlinks them). Refuses a link that would
    /// make a loop. Returns how many files changed.
    pub fn set_parent(&mut self, children: &[Hit], parent: Option<&Hit>) -> usize {
        let parent_hash = parent.map(|p| p.hash.clone());
        if let Some(p) = &parent_hash {
            let above = self.ancestors(p);
            if children.iter().any(|c| c.hash == *p || above.contains(&c.hash)) {
                self.notify("A file can't be the parent of itself or of its own parent");
                return 0;
            }
        }
        let mut changed = 0;
        self.edit_entries(children, |e| {
            let differs = e.parent != parent_hash;
            e.parent = parent_hash.clone();
            changed += usize::from(differs);
            differs
        });
        self.dupes_stale = true;
        changed
    }

    /// Copies the tags of `others` to `keep`; also their source and rating if `keep` has none
    /// (the strictest rating wins). Children of the others become children of `keep`. Returns
    /// whether anything changed.
    pub fn merge_into(&mut self, keep: &Hit, others: &[Hit]) -> bool {
        let mut moved = 0;
        for other in others.iter().filter(|h| h.hash != keep.hash) {
            let kids: Vec<Hit> = self.children(&other.hash).into_iter().filter(|k| k.hash != keep.hash).collect();
            moved += self.set_parent(&kids, Some(keep));
        }
        let mut tags = BTreeSet::new();
        let mut rating: Option<Rating> = None;
        let mut source = None;
        for e in others.iter().filter(|h| *h != keep).filter_map(|h| self.entry(h)) {
            tags.extend(e.tags.iter().cloned());
            rating = stricter(rating, e.rating);
            source = source.or_else(|| e.source.clone());
        }
        let mut changed = false;
        self.edit_entries(std::slice::from_ref(keep), |e| {
            let before = e.clone();
            e.tags.extend(tags.iter().cloned());
            if e.rating.is_none() {
                e.rating = rating;
            }
            if e.source.is_none() {
                e.source = source.clone();
            }
            changed = *e != before;
            changed
        });
        changed || moved > 0
    }

    /// Moves files (every copy, and each one's sidecar) to the trash and forgets them. A file
    /// whose copies can't all be trashed keeps the ones that are left. Returns how many files
    /// were removed.
    pub fn trash_files(&mut self, hits: &[Hit]) -> usize {
        let trasher = Arc::clone(&self.trasher);
        let mut removed = 0;
        let mut errors = Vec::new();
        let mut touched = BTreeSet::new();
        for hit in hits {
            let Some(oc) = self.colls.get_mut(hit.coll) else { continue };
            let name = oc.name.clone();
            let CollState::Ready { c, .. } = &mut oc.state else { continue };
            if c.is_read_only() {
                errors.push(format!("{name} is read-only"));
                continue;
            }
            let mode = c.settings().sidecars;
            let Some(paths) = c.get(&hit.hash).map(|e| e.paths.iter().cloned().collect::<Vec<_>>()) else { continue };
            let mut gone = Vec::new();
            for rel in &paths {
                let Some(abs) = c.abs_path(rel) else { continue };
                if abs.exists()
                    && let Err(e) = trasher(&abs)
                {
                    errors.push(format!("{}: {e}", abs.display()));
                    continue;
                }
                gone.push(rel.clone());
                // The sidecar goes too, unless another file shares it (`cat.xmp` for `cat.jpg`
                // and `cat.png`).
                let Some(side) = sidecar_rel(rel, mode) else { continue };
                let shared = c.files().any(|(h, e)| {
                    e.paths.iter().any(|p| p != rel && (h != hit.hash || !paths.contains(p)) && sidecar_rel(p, mode).as_deref() == Some(side.as_str()))
                });
                if let Some(side_abs) = c.abs_path(&side)
                    && side_abs.exists()
                    && !shared
                    && let Err(e) = trasher(&side_abs)
                {
                    errors.push(format!("{}: {e}", side_abs.display()));
                }
            }
            if gone.len() == paths.len() {
                c.remove(&hit.hash);
                removed += 1;
            } else if let Some(e) = c.get_mut(&hit.hash) {
                for g in &gone {
                    e.paths.remove(g);
                }
            }
            touched.insert(hit.coll);
        }
        // Children of removed files lose their parent link rather than point at nothing.
        let removed_hashes: HashSet<&str> = hits.iter().filter(|h| self.find(&h.hash).is_none()).map(|h| h.hash.as_str()).collect();
        let orphans: Vec<Hit> = removed_hashes.iter().flat_map(|h| self.children(h)).collect();
        if !orphans.is_empty() {
            self.set_parent(&orphans, None);
        }
        for coll in touched {
            if let Some(oc) = self.colls.get_mut(coll)
                && let CollState::Ready { c, .. } = &oc.state
            {
                oc.index = SearchIndex::build(c);
            }
            self.touch(coll);
        }
        let alive: Vec<bool> = self.hits.iter().map(|h| self.entry(h).is_some()).collect();
        let mut alive = alive.into_iter();
        self.hits.retain(|_| alive.next().unwrap_or(false));
        self.usage = None;
        self.dupes_stale = true;
        self.recount();
        if let Some(e) = errors.first() {
            self.notify(format!("Couldn't move to the trash: {e}"));
        }
        removed
    }

    /// Adds a tag to the central list. Returns its normalised name.
    pub fn create_tag(&mut self, name: &str) -> Option<String> {
        if self.tags.is_read_only() {
            self.notify("The tag list is open in another BooruCraft; new tags can't be created here");
            return None;
        }
        match self.tags.create(name, "") {
            Ok(t) => {
                if let Err(e) = self.tags.save() {
                    self.notify(format!("Couldn't save the tag list: {e}"));
                }
                Some(t)
            }
            Err(StoreError::TagExists(t)) => Some(t),
            Err(e) => {
                self.notify(e.to_string());
                None
            }
        }
    }

    /// Saves collections whose last edit is older than [`SAVE_DELAY`]. Returns how long until the
    /// next one is due.
    pub fn save_due(&mut self) -> Option<Duration> {
        let mut next: Option<Duration> = None;
        for i in 0..self.colls.len() {
            let Some(since) = self.colls.get(i).and_then(|c| c.dirty_since) else { continue };
            let age = since.elapsed();
            if age >= SAVE_DELAY {
                self.flush(i);
            } else {
                let left = SAVE_DELAY.saturating_sub(age);
                next = Some(next.map_or(left, |n| n.min(left)));
            }
        }
        next
    }

    /// Saves one collection and writes its pending sidecars now.
    pub fn flush(&mut self, idx: usize) {
        let Some(oc) = self.colls.get_mut(idx) else { return };
        if oc.dirty_since.is_none() && oc.save_error.is_none() {
            return;
        }
        let CollState::Ready { c, cache } = &mut oc.state else { return };
        let mut error = None;
        // Not dirty either way: a failed save is retried after the next edit or on exit,
        // not every frame.
        oc.dirty_since = None;
        match c.save() {
            Ok(()) => {
                oc.save_error = None;
                let mut report = SidecarReport::default();
                write_sidecars(c, &self.tags, oc.sidecars_pending.iter().map(String::as_str), cache, &mut report);
                let _ = cache.save();
                oc.sidecars_pending.clear();
                if let Some((p, e)) = report.errors.first() {
                    error = Some(format!("Sidecar {}: {e}", p.display()));
                }
            }
            Err(e) => {
                let msg = format!("{}: couldn't save: {e}", oc.name);
                oc.save_error = Some(msg.clone());
                error = Some(msg);
            }
        }
        if let Some(e) = error {
            self.notify(e);
        }
    }

    /// Saves everything now (on exit).
    pub fn flush_all(&mut self) {
        for i in 0..self.colls.len() {
            self.flush(i);
        }
        if self.tags.is_dirty() && !self.tags.is_read_only() {
            let _ = self.tags.save();
        }
    }

    /// True while edits wait for their save, or the last save failed.
    pub fn has_unsaved(&self) -> bool {
        self.colls.iter().any(|c| c.dirty_since.is_some() || c.save_error.is_some())
    }
}

/// The stricter of two ratings (explicit over questionable over safe).
fn stricter(a: Option<Rating>, b: Option<Rating>) -> Option<Rating> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y.index() > x.index() { y } else { x }),
        (x, y) => x.or(y),
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        if let Some(job) = &self.hash_job {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(path: &Path, shade: u8) {
        let img = image::RgbaImage::from_pixel(8, 6, image::Rgba([shade, 40, 90, 255]));
        img.save_with_format(path, image::ImageFormat::Png).unwrap();
    }

    /// A picture with some structure, `w` wide: shaded blocks, a gradient and a bright disc at
    /// `x` (0..1).
    fn detailed(path: &Path, w: u32, x: f32) {
        let h = w * 2 / 3;
        let shades = [30.0, 200.0, 90.0, 160.0, 60.0, 230.0, 120.0, 10.0, 180.0, 70.0, 140.0, 40.0];
        let img = image::RgbImage::from_fn(w, h, |px, py| {
            let (fx, fy) = (px as f32 / w as f32, py as f32 / h as f32);
            let block = shades[(fx * 4.0) as usize % 4 + (fy * 3.0) as usize % 3 * 4];
            let v: f32 = if (fx - x).powi(2) + (fy - 0.5).powi(2) < 0.03 { 250.0 } else { block * 0.8 + fx * 40.0 };
            image::Rgb([v as u8, (v * 0.7) as u8, 80])
        });
        img.save(path).unwrap();
    }

    #[test]
    fn near_duplicates_are_found_in_the_background() {
        let tmp = tempfile::tempdir().unwrap();
        let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        detailed(&one.join("big.png"), 600, 0.3);
        detailed(&two.join("small.jpg"), 200, 0.3);
        detailed(&one.join("other.png"), 600, 0.8);
        let mut lib = libraries(tmp.path(), &[&one, &two]);
        assert!(lib.hash_progress().is_none(), "the job finished");
        lib.search("dupes:yes");
        assert_eq!(lib.hits.len(), 2);
        assert_eq!(lib.dupe_groups.len(), 1);
        let best = &lib.dupe_groups[0][0];
        assert_eq!(lib.entry(best).unwrap().width, Some(600), "the bigger copy first");
        lib.search("dupes:no");
        assert_eq!(lib.hits.len(), 1);
        lib.flush_all();
        assert!(std::fs::read_to_string(two.join("collection.json")).unwrap().contains("\"phash\""), "hashes are saved");
    }

    #[test]
    fn keeping_one_copy_trashes_the_others_and_keeps_their_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let (pics, bin) = (tmp.path().join("pics"), tmp.path().join("bin"));
        std::fs::create_dir_all(&pics).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        detailed(&pics.join("big.png"), 600, 0.3);
        detailed(&pics.join("small.jpg"), 200, 0.3);
        detailed(&pics.join("copy.png"), 300, 0.3);
        detailed(&pics.join("x.png"), 600, 0.8);
        detailed(&pics.join("y.jpg"), 300, 0.8);
        let mut lib = library(tmp.path(), &pics);
        let moved = bin.clone();
        lib.trasher = Arc::new(move |p: &Path| std::fs::rename(p, moved.join(p.file_name().unwrap())).map_err(|e| e.to_string()));
        lib.refresh_dupes();
        assert_eq!(lib.dupe_groups.len(), 2);
        let first = |lib: &Library, name: &str| lib.dupe_groups.iter().find(|g| lib.entry(&g[0]).unwrap().paths.contains(name)).cloned().unwrap();
        let group = first(&lib, "big.png");
        assert_eq!(group.len(), 3);

        lib.create_tag("cat");
        lib.create_tag("sky");
        lib.edit_tags(&group[1..2], &["cat".into()], &[]);
        lib.set_rating(&group[1..2], Some(Rating::Questionable));
        lib.set_source(&group[2], Some("https://example.org/1".into()));
        lib.edit_tags(&group[2..3], &["sky".into()], &[]);
        lib.flush_all();
        let trashed_name = lib.entry(&group[1]).unwrap().paths.iter().next().unwrap().clone();
        assert!(pics.join(format!("{trashed_name}.xmp")).exists());

        assert!(lib.merge_into(&group[0], &group[1..]));
        assert_eq!(lib.trash_files(&group[1..]), 2);
        let kept = lib.entry(&group[0]).unwrap();
        assert_eq!(kept.tags, ["cat".to_string(), "sky".to_string()].into());
        assert_eq!(kept.rating, Some(Rating::Questionable));
        assert_eq!(kept.source.as_deref(), Some("https://example.org/1"));
        assert!(pics.join("big.png").exists());
        assert!(!pics.join(&trashed_name).exists() && bin.join(&trashed_name).exists(), "moved to the trash");
        assert!(bin.join(format!("{trashed_name}.xmp")).exists(), "with its sidecar");
        assert!(lib.entry(&group[1]).is_none());

        // Versions linked as parent and child aren't duplicates.
        let kept = group[0].clone();
        detailed(&pics.join("again.png"), 400, 0.3);
        lib.start_scan(0);
        let t0 = Instant::now();
        while lib.is_busy() {
            assert!(t0.elapsed() < Duration::from_secs(20), "rescan never finished");
            std::thread::sleep(Duration::from_millis(5));
            lib.poll();
        }
        lib.refresh_dupes();
        let again = first(&lib, "big.png");
        assert_eq!(again.len(), 2, "the new copy groups with the kept one");
        assert_eq!(lib.set_parent(&again[1..], Some(&kept)), 1);
        assert_eq!(lib.set_parent(std::slice::from_ref(&kept), Some(&again[1])), 0, "no loops");
        lib.refresh_dupes();
        assert_eq!(lib.dupe_groups.len(), 1, "only the other pair is left");
        lib.search(&format!("parent:{}", &kept.hash[..8]));
        assert_eq!(lib.hits.len(), 2);
        lib.search("child:yes");
        assert_eq!(lib.hits, vec![kept.clone()]);
        // Trashing the parent unlinks its child.
        assert_eq!(lib.trash_files(std::slice::from_ref(&kept)), 1);
        assert_eq!(lib.entry(&again[1]).unwrap().parent, None);

        // The other pair: not duplicates, and that is remembered.
        let other = first(&lib, "x.png");
        lib.mark_not_duplicates(&other);
        lib.refresh_dupes();
        assert!(lib.dupe_groups.is_empty());
        lib.flush_all();
        let json = std::fs::read_to_string(pics.join("collection.json")).unwrap();
        assert!(json.contains("not_duplicates") && !json.contains(&trashed_name), "{json}");
    }

    fn library(tmp: &Path, pics: &Path) -> Library {
        libraries(tmp, &[pics])
    }

    fn libraries(tmp: &Path, pics: &[&Path]) -> Library {
        let dirs = Dirs { config: tmp.join("cfg"), cache: tmp.join("cache"), tags: tmp.join("cfg/tags.json") };
        let mut lib = Library::new(dirs, Config::default(), Some(pics.iter().map(|p| p.to_path_buf()).collect()), Arc::new(|| {}));
        let start = Instant::now();
        lib.poll();
        while lib.is_busy() {
            assert!(start.elapsed() < Duration::from_secs(20), "scan never finished");
            lib.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        lib.poll();
        lib
    }

    #[test]
    fn edits_are_saved_to_the_collection_and_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let pics = tmp.path().join("pics");
        std::fs::create_dir_all(&pics).unwrap();
        picture(&pics.join("a.png"), 10);
        picture(&pics.join("b.png"), 200);

        let mut lib = library(tmp.path(), &pics);
        lib.search("");
        assert_eq!(lib.hits.len(), 2);
        let all = lib.hits.clone();

        assert_eq!(lib.create_tag("Blue Sky").as_deref(), Some("blue_sky"));
        assert_eq!(lib.edit_tags(&all, &["blue_sky".into()], &[]), 2);
        assert_eq!(lib.edit_tags(&all, &["not_listed".into()], &[]), 0, "only listed tags can be added");
        assert!(lib.messages.iter().any(|(m, _)| m.contains("not_listed")));
        lib.set_rating(&all[..1], Some(Rating::Safe));
        assert!(lib.has_unsaved());
        assert_eq!(lib.save_due().map(|d| d <= SAVE_DELAY), Some(true), "saves wait for the delay");

        lib.flush_all();
        assert!(!lib.has_unsaved());
        let json = std::fs::read_to_string(pics.join("collection.json")).unwrap();
        assert!(json.contains("blue_sky"));
        for name in ["a.png.xmp", "b.png.xmp"] {
            let xmp = std::fs::read_to_string(pics.join(name)).unwrap();
            assert!(xmp.contains("blue_sky"), "{name}");
        }
        assert!(!tmp.path().join("cfg/config.json").exists(), "session collections aren't remembered");

        lib.search("blue_sky rating:s");
        assert_eq!(lib.hits.len(), 1);
        lib.search("blue_sky -rating:s");
        assert_eq!(lib.hits.len(), 1);
    }

    #[test]
    fn results_from_several_collections_are_sorted_together() {
        let tmp = tempfile::tempdir().unwrap();
        let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        picture(&one.join("b.png"), 1);
        picture(&two.join("a.png"), 2);
        picture(&two.join("c.png"), 3);
        picture(&one.join("d.png"), 4);
        let mut lib = libraries(tmp.path(), &[&one, &two]);
        let names = |lib: &Library| -> Vec<String> { lib.hits.iter().map(|h| lib.entry(h).unwrap().paths.iter().next().unwrap().clone()).collect() };
        lib.search("order:name");
        assert_eq!(names(&lib), vec!["a.png", "b.png", "c.png", "d.png"]);
        lib.search("order:-name");
        assert_eq!(names(&lib), vec!["d.png", "c.png", "b.png", "a.png"]);
        lib.search("order:name width:>100");
        assert!(lib.hits.is_empty());
        lib.search("width:..");
        assert!(lib.query_error.is_some(), "errors are reported");

        // The sidebar counts follow the results, and edits.
        lib.search("");
        lib.create_tag("cat");
        let first = lib.hits[..1].to_vec();
        lib.edit_tags(&first, &["cat".into()], &[]);
        assert_eq!(lib.result_tags, vec![("cat".to_string(), 1)]);
        assert_eq!(lib.result_ratings, [0, 0, 0, 4]);
        lib.refresh_usage();
        assert_eq!(lib.usage(), &[("cat".to_string(), 1)]);
    }

    #[test]
    fn a_failed_save_keeps_the_changes_and_the_other_version() {
        let tmp = tempfile::tempdir().unwrap();
        let pics = tmp.path().join("pics");
        std::fs::create_dir_all(&pics).unwrap();
        picture(&pics.join("a.png"), 10);

        let mut lib = library(tmp.path(), &pics);
        lib.search("");
        let all = lib.hits.clone();
        lib.create_tag("cat");

        // Another program rewrites collection.json behind BooruCraft's back.
        let theirs = "{\"edited\": \"elsewhere\"}\n";
        std::fs::write(pics.join("collection.json"), theirs).unwrap();
        lib.edit_tags(&all, &["cat".into()], &[]);
        lib.flush_all();

        assert_eq!(std::fs::read_to_string(pics.join("collection.json")).unwrap(), theirs, "never saved over");
        assert!(lib.has_unsaved(), "the failure stays visible");
        assert!(lib.colls[0].save_error.is_some());
        assert!(lib.entry(&all[0]).unwrap().tags.contains("cat"), "the edit is kept in memory");
        assert!(lib.save_due().is_none(), "no retry every frame");
    }
}

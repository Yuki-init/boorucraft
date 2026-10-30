//! Everything the window shows, without egui: the tag list, the open collections, background
//! scans, searches across collections, edits and debounced saves.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use boorucraft_core::{Query, Rating};
use boorucraft_dedup::Hashes;
use boorucraft_index::{
    FolderWatcher, Relevance, ScanCache, ScanReport, SidecarReport, classify, find_groups, needing_hashes, scan, set_hashes, sidecar_rel, write_sidecars,
};
use boorucraft_store::{Collection, DiskChange, FileEntry, Links, Recovery, Row, SearchIndex, StoreError, TagList, compare_rows, paths, safefile};
use boorucraft_thumbs::ThumbCache;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// How long after the last edit collection.json and the sidecars are saved.
pub const SAVE_DELAY: Duration = Duration::from_secs(2);
/// How often collection.json and tags.json are checked for changes made by other programs.
pub const DISK_CHECK_EVERY: Duration = Duration::from_secs(2);
/// How long after the first change in a watched folder it is scanned again (changes that
/// arrive meanwhile join the same scan).
pub const WATCH_DELAY: Duration = Duration::from_secs(1);
/// Changed paths kept per collection until they are looked at; beyond this, just rescan.
const MAX_PENDING: usize = 10_000;

/// A file another program changed while BooruCraft had unsaved changes to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outside {
    TagList,
    Collection(usize),
}

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
    /// Show each pool and each family of versions as one tile (default on).
    #[serde(default)]
    pub stacks: Option<bool>,
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

/// What a stacked tile stands for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StackKey {
    Pool(String),
    /// A family of versions, by the hash of its top parent.
    Family(String),
}

impl StackKey {
    /// The search that shows the whole stack.
    pub fn query(&self) -> String {
        match self {
            StackKey::Pool(p) => format!("pool:{p}"),
            StackKey::Family(h) => format!("parent:{}", h.get(..16).unwrap_or(h)),
        }
    }
}

/// A tile standing for several results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    pub key: StackKey,
    /// Results in the stack, this one included.
    pub count: usize,
}

/// Where to move files within a pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolMove {
    Earlier,
    Later,
    First,
    Last,
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
    /// collection.json was changed by another program while this one had unsaved changes:
    /// nothing is saved until the user picks a version ([`Library::resolve_outside_change`]).
    pub outside_change: bool,
    /// Paths the folder watcher reported, looked at once the collection is ready.
    watch_pending: Vec<PathBuf>,
    /// A rescan is due then, because files changed.
    pub rescan_at: Option<Instant>,
    /// collection.json doesn't parse: `Some(true)` if its `.bak` can be loaded instead. See
    /// [`Library::recover_collection`].
    pub damaged: Option<bool>,
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
    /// The pool whose order the current results follow (a `pool:name` search).
    pub pool_view: Option<String>,
    /// Results that stand for a stack of others (hidden from `hits`).
    pub stacks: HashMap<Hit, Stack>,
    /// Tags of the current results with counts, most used first.
    pub result_tags: Vec<(String, u64)>,
    /// Ratings of the current results: safe, questionable, explicit, none.
    pub result_ratings: [u64; 4],
    /// Tag counts over every open collection (for autocomplete); `None` until next asked for.
    usage: Option<Vec<(String, u64)>>,
    /// Pool sizes, recounted with `usage`.
    pool_counts: Vec<(String, usize)>,
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
    /// tags.json was changed by another program while this one had unsaved changes.
    pub tags_outside_change: bool,
    last_disk_check: Option<Instant>,
    /// `None` if the system offers no file watching; changes are then found by Rescan.
    watcher: Option<FolderWatcher>,
    watch_rx: Receiver<Vec<PathBuf>>,
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
        let (watch_tx, watch_rx) = channel();
        let mut messages = Vec::new();
        let wake = Arc::clone(&waker);
        let watcher = match FolderWatcher::new(move |paths| {
            let _ = watch_tx.send(paths);
            wake();
        }) {
            Ok(mut w) => {
                // The tag list's folder, for outside changes to tags.json.
                if let Some(dir) = dirs.tags.parent().filter(|d| d.is_dir()) {
                    let _ = w.watch(dir, false);
                }
                Some(w)
            }
            Err(e) => {
                messages.push((format!("Folders can't be watched for changes ({e}); use Rescan after changing files"), Instant::now()));
                None
            }
        };
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
            pool_view: None,
            stacks: HashMap::new(),
            result_tags: Vec::new(),
            result_ratings: [0; 4],
            usage: None,
            pool_counts: Vec::new(),
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
            tags_outside_change: false,
            last_disk_check: None,
            watcher,
            watch_rx,
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
        let mut damaged = None;
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
                    outside_change: false,
                    watch_pending: Vec::new(),
                    rescan_at: None,
                    damaged: None,
                };
                if !changed.is_empty() {
                    oc.dirty_since = Some(Instant::now());
                    oc.sidecars_pending.extend(changed);
                }
                if let Some(w) = &mut self.watcher
                    && let Err(e) = w.watch(&root, true)
                {
                    self.messages.push((format!("{}: changes won't be noticed until Rescan ({e})", oc.name), Instant::now()));
                }
                self.colls.push(oc);
                self.usage = None;
                self.dupes_stale = true;
                (None, root)
            }
            Err(e) => {
                damaged = matches!(e, StoreError::Json { .. }).then(|| Collection::backup_is_readable(root));
                (Some(e.to_string()), root.to_path_buf())
            }
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
                outside_change: false,
                watch_pending: Vec::new(),
                rescan_at: None,
                damaged,
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

    /// Opens a collection whose collection.json is damaged, from its backup or from nothing (the
    /// scan that follows reads tags, ratings and sources back from the sidecars). A copy of the
    /// damaged file goes to `damaged/` in the config folder first.
    pub fn recover_collection(&mut self, idx: usize, from: Recovery) {
        let Some(oc) = self.colls.get(idx) else { return };
        let (root, name) = (oc.root.clone(), oc.name.clone());
        let copy = match Collection::copy_damaged(&root, &self.dirs.config.join("damaged")) {
            Ok(p) => p,
            Err(e) => {
                self.notify(format!("{name}: couldn't keep a copy of the damaged collection.json, so it wasn't touched: {e}"));
                return;
            }
        };
        let mut c = match Collection::open_recovering(&root, from) {
            Ok(c) => c,
            Err(e) => {
                self.notify(format!("{name}: couldn't recover: {e}"));
                return;
            }
        };
        c.apply_tag_history(&self.tags);
        let root = c.root().to_path_buf();
        let cache = ScanCache::load(&self.dirs.cache, &root);
        if let Some(w) = &mut self.watcher {
            let _ = w.watch(&root, true);
        }
        if let Some(oc) = self.colls.get_mut(idx) {
            oc.index = SearchIndex::build(&c);
            oc.state = CollState::Ready { c: Box::new(c), cache: Box::new(cache) };
            oc.root = root;
            oc.damaged = None;
            oc.dirty_since = Some(Instant::now());
        }
        self.usage = None;
        self.dupes_stale = true;
        let what = match from {
            Recovery::Backup => "loaded the backup",
            Recovery::Empty => "rebuilding from the sidecars",
        };
        self.notify(format!("{name}: {what}; the damaged collection.json was copied to {}", copy.display()));
        self.start_scan(idx);
    }

    pub fn close_collection(&mut self, idx: usize) {
        // Background jobs report back by index, so indexes must not shift while any runs.
        if self.is_busy() {
            self.notify("Wait for scanning and duplicate checks to finish before closing a collection");
            return;
        }
        self.flush(idx);
        let oc = self.colls.remove(idx);
        if let Some(w) = &mut self.watcher {
            w.unwatch(&oc.root);
        }
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
        if oc.read_only() || oc.outside_change {
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
            // The tag list may have changed while the scan ran.
            let caught_up = c.apply_tag_history(&self.tags);
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
                if !caught_up.is_empty() {
                    oc.sidecars_pending.extend(caught_up);
                    oc.dirty_since = Some(Instant::now());
                }
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
        self.take_watch_events();
        if self.last_disk_check.is_none_or(|t| t.elapsed() >= DISK_CHECK_EVERY) && self.check_outside_changes() {
            changed = true;
        }
        if !self.is_busy() {
            self.start_hashing();
        }
        changed
    }

    /// True if folders are watched for changes.
    pub fn is_watching(&self) -> bool {
        self.watcher.is_some()
    }

    /// Sorts the paths the watcher reported to their collections, and starts rescans that are
    /// due. Paths of a collection that is scanning wait until the scan is back.
    fn take_watch_events(&mut self) {
        let now = Instant::now();
        while let Ok(paths) = self.watch_rx.try_recv() {
            for p in paths {
                if p == self.dirs.tags {
                    self.last_disk_check = None;
                    continue;
                }
                // The innermost collection holding the path.
                let Some(oc) = self.colls.iter_mut().filter(|oc| p.starts_with(&oc.root)).max_by_key(|oc| oc.root.as_os_str().len()) else { continue };
                if oc.watch_pending.len() < MAX_PENDING {
                    oc.watch_pending.push(p);
                } else {
                    oc.rescan_at.get_or_insert(now + WATCH_DELAY);
                }
            }
        }
        let mut check_disk = false;
        let mut due = Vec::new();
        for (i, oc) in self.colls.iter_mut().enumerate() {
            let CollState::Ready { c, cache } = &oc.state else { continue };
            if oc.outside_change {
                continue;
            }
            for p in std::mem::take(&mut oc.watch_pending) {
                match classify(c, cache, &p) {
                    Relevance::Ignore => {}
                    Relevance::CollectionFile => check_disk = true,
                    // Read-only collections (open in another BooruCraft) are never scanned here.
                    Relevance::Rescan if c.is_read_only() => {}
                    Relevance::Rescan => {
                        oc.rescan_at.get_or_insert(now + WATCH_DELAY);
                    }
                }
            }
            if oc.rescan_at.is_some_and(|t| t <= now) {
                oc.rescan_at = None;
                due.push(i);
            }
        }
        if check_disk {
            self.last_disk_check = None;
        }
        for i in due {
            self.start_scan(i);
        }
    }

    /// How long until a rescan caused by file changes is due.
    pub fn rescan_due(&self) -> Option<Duration> {
        self.colls
            .iter()
            .filter(|oc| !oc.outside_change && matches!(oc.state, CollState::Ready { .. }))
            .filter_map(|oc| oc.rescan_at)
            .min()
            .map(|t| t.saturating_duration_since(Instant::now()))
    }

    /// Looks for changes other programs made to tags.json and each collection.json (a sync
    /// tool, a text editor, a restored backup). A changed file is reloaded unless BooruCraft
    /// has unsaved changes to it: then nothing is saved until the user picks a version. A
    /// deleted one is written again. Returns true if the results may have changed.
    pub fn check_outside_changes(&mut self) -> bool {
        self.last_disk_check = Some(Instant::now());
        let mut changed = false;
        if !self.tags_outside_change {
            match self.tags.disk_change() {
                Ok(DiskChange::Changed) if !self.tags.is_dirty() => match self.reload_tags() {
                    Ok(()) => {
                        self.notify("The tag list was changed outside BooruCraft; reloaded it");
                        changed = true;
                    }
                    Err(e) => {
                        self.tags_outside_change = true;
                        self.notify(format!("The tag list was changed outside BooruCraft, but couldn't be read: {e}"));
                    }
                },
                Ok(DiskChange::Changed) => {
                    self.tags_outside_change = true;
                    self.notify("The tag list was changed outside BooruCraft while this window had unsaved changes");
                }
                Ok(DiskChange::Deleted) if !self.tags.is_read_only() => {
                    let saved = self.tags.overwrite_disk_version().and_then(|()| self.tags.save());
                    self.notify(match saved {
                        Ok(()) => "The tag list was deleted outside BooruCraft; saved it again".to_string(),
                        Err(e) => format!("The tag list was deleted outside BooruCraft, and saving it again failed: {e}"),
                    });
                }
                _ => {}
            }
        }
        for i in 0..self.colls.len() {
            let (name, change, unsaved, read_only) = {
                let Some(oc) = self.colls.get_mut(i) else { continue };
                if oc.outside_change {
                    continue;
                }
                let CollState::Ready { c, .. } = &mut oc.state else { continue };
                (oc.name.clone(), c.disk_change(), c.is_dirty(), c.is_read_only())
            };
            match change {
                Ok(DiskChange::Changed) if !unsaved => match self.reload_collection(i) {
                    Ok(()) => {
                        self.notify(format!("{name}: collection.json was changed outside BooruCraft; reloaded it"));
                        changed = true;
                    }
                    Err(e) => {
                        if let Some(oc) = self.colls.get_mut(i) {
                            oc.outside_change = true;
                        }
                        self.notify(format!("{name}: collection.json was changed outside BooruCraft, but couldn't be read: {e}"));
                    }
                },
                Ok(DiskChange::Changed) => {
                    if let Some(oc) = self.colls.get_mut(i) {
                        oc.outside_change = true;
                    }
                    self.notify(format!("{name}: collection.json was changed outside BooruCraft while this window had unsaved changes"));
                }
                Ok(DiskChange::Deleted) if !read_only => {
                    let result = match self.colls.get_mut(i) {
                        Some(OpenCollection { state: CollState::Ready { c, .. }, dirty_since, .. }) => {
                            let r = c.overwrite_disk_version();
                            if r.is_ok() {
                                *dirty_since = Some(Instant::now());
                            }
                            r.map_err(|e| e.to_string())
                        }
                        _ => Err("the collection is busy".into()),
                    };
                    self.flush(i);
                    self.notify(match result {
                        Ok(()) => format!("{name}: collection.json was deleted outside BooruCraft; saved it again"),
                        Err(e) => format!("{name}: collection.json was deleted outside BooruCraft: {e}"),
                    });
                }
                _ => {}
            }
        }
        changed
    }

    /// Settles a file changed outside BooruCraft while it had unsaved changes: keep the
    /// version in memory (the other one stays in the `.bak`) or load the one on disk.
    pub fn resolve_outside_change(&mut self, which: Outside, keep_mine: bool) {
        let result = match which {
            Outside::TagList if keep_mine => self.tags.overwrite_disk_version().and_then(|()| self.tags.save()).map_err(|e| e.to_string()),
            Outside::TagList => self.reload_tags(),
            Outside::Collection(i) if keep_mine => {
                let Some(oc) = self.colls.get_mut(i) else { return };
                let CollState::Ready { c, .. } = &mut oc.state else { return };
                let r = c.overwrite_disk_version().map_err(|e| e.to_string());
                if r.is_ok() {
                    oc.dirty_since = Some(Instant::now());
                    oc.outside_change = false;
                    self.flush(i);
                }
                r
            }
            Outside::Collection(i) => self.reload_collection(i),
        };
        match (result, which) {
            (Ok(()), Outside::TagList) => self.tags_outside_change = false,
            (Ok(()), Outside::Collection(i)) => {
                if let Some(oc) = self.colls.get_mut(i) {
                    oc.outside_change = false;
                }
            }
            (Err(e), _) => self.notify(format!("Couldn't do that: {e}")),
        }
    }

    /// Loads tags.json from disk again and brings the open collections up to date with it.
    fn reload_tags(&mut self) -> Result<(), String> {
        self.tags.reload().map_err(|e| e.to_string())?;
        self.catch_up_collections();
        Ok(())
    }

    /// Applies tag-list changes recorded in its history (renames, merges, deletes, pool renames
    /// and deletes) to the open collections: their files, indexes and sidecars.
    fn catch_up_collections(&mut self) {
        for oc in &mut self.colls {
            let CollState::Ready { c, .. } = &mut oc.state else { continue };
            let revision = c.tags_revision();
            let changed = c.apply_tag_history(&self.tags);
            // Pool renames and deletes change no sidecar but still need indexing and saving.
            if c.tags_revision() != revision {
                oc.index = SearchIndex::build(c);
                oc.dirty_since = Some(Instant::now());
                oc.sidecars_pending.extend(changed);
            }
        }
        self.usage = None;
        self.dupes_stale = true;
    }

    /// Changes the central tag list with `f`, saves it, and brings the open collections up to
    /// date (closed ones catch up when next opened). Errors are reported and returned.
    fn change_tags<T>(&mut self, f: impl FnOnce(&mut TagList) -> Result<T, StoreError>) -> Option<T> {
        if self.tags.is_read_only() {
            self.notify("The tag list is open in another BooruCraft; it can't be changed here");
            return None;
        }
        if self.tags_outside_change {
            self.notify("The tag list was changed by another program: choose a version first");
            return None;
        }
        let out = match f(&mut self.tags) {
            Ok(v) => v,
            Err(e) => {
                self.notify(e.to_string());
                return None;
            }
        };
        if let Err(e) = self.tags.save() {
            self.notify(format!("Couldn't save the tag list: {e}"));
        }
        self.catch_up_collections();
        Some(out)
    }

    /// Tags used in the open collections that aren't in the tag list (copied from another
    /// computer, or added by another app in a sidecar), with file counts, most used first.
    /// Call [`Self::refresh_usage`] first in a frame.
    pub fn unknown_tags(&self) -> Vec<(String, u64)> {
        self.usage().iter().filter(|(t, _)| !self.tags.contains(t)).cloned().collect()
    }

    /// Settles an unknown tag in the open collections: files that have it get `with` (a tag
    /// from the list) instead, or just lose it when `with` is `None`. Returns how many files
    /// changed. (To keep it, add it to the list with [`Self::create_tag`].)
    pub fn replace_unknown_tag(&mut self, tag: &str, with: Option<&str>) -> usize {
        let with = match with.map(|w| self.tags.resolve(w)) {
            Some(Ok(w)) => Some(w),
            Some(Err(e)) => {
                self.notify(e.to_string());
                return 0;
            }
            None => None,
        };
        let hits: Vec<Hit> = self
            .colls
            .iter()
            .enumerate()
            .filter_map(|(i, oc)| oc.collection().map(|c| (i, c)))
            .flat_map(|(i, c)| c.files().filter(|(_, e)| e.tags.contains(tag)).map(move |(h, _)| Hit { coll: i, hash: h.to_string() }))
            .collect();
        let mut n = 0;
        self.edit_entries(&hits, |e| {
            let changed = e.tags.remove(tag);
            if let Some(w) = &with {
                e.tags.insert(w.clone());
            }
            n += usize::from(changed);
            changed
        });
        n
    }

    /// Renames a tag everywhere. Returns the new name.
    pub fn rename_tag(&mut self, from: &str, to: &str) -> Option<String> {
        self.change_tags(|t| t.rename(from, to))
    }

    /// Folds `from` into `into`: files with `from` get `into` instead.
    pub fn merge_tag(&mut self, from: &str, into: &str) -> Option<()> {
        let into = self.tags.resolve(into).map_err(|e| self.notify(e.to_string())).ok()?;
        self.change_tags(|t| t.merge(&[from], &into))
    }

    /// Removes a tag from the list and from every file.
    pub fn delete_tag(&mut self, tag: &str) -> Option<()> {
        self.change_tags(|t| t.delete(tag))
    }

    pub fn describe_tag(&mut self, tag: &str, text: &str) -> Option<()> {
        self.change_tags(|t| t.set_description(tag, text))
    }

    pub fn add_namespace(&mut self, name: &str, color: &str) -> Option<()> {
        self.change_tags(|t| t.add_namespace(name, color))
    }

    pub fn set_namespace_color(&mut self, name: &str, color: &str) -> Option<()> {
        self.change_tags(|t| t.set_namespace_color(name, color))
    }

    pub fn remove_namespace(&mut self, name: &str) -> Option<()> {
        self.change_tags(|t| t.remove_namespace(name))
    }

    pub fn rename_pool(&mut self, from: &str, to: &str) -> Option<String> {
        self.change_tags(|t| t.rename_pool(from, to))
    }

    pub fn delete_pool(&mut self, name: &str) -> Option<()> {
        self.change_tags(|t| t.delete_pool(name))
    }

    /// Loads a collection.json from disk again (dropping unsaved changes), then rewrites the
    /// sidecars of files whose tags, rating or source differ from before.
    fn reload_collection(&mut self, i: usize) -> Result<(), String> {
        let Some(oc) = self.colls.get_mut(i) else { return Err("no such collection".into()) };
        let CollState::Ready { c, .. } = &mut oc.state else { return Err("the collection is busy".into()) };
        let before: HashMap<String, FileEntry> = c.files().map(|(h, e)| (h.to_string(), e.clone())).collect();
        c.reload().map_err(|e| e.to_string())?;
        let mut touched: BTreeSet<String> = c.apply_tag_history(&self.tags);
        for (h, e) in c.files() {
            let old = before.get(h);
            if old.is_none_or(|o| o.tags != e.tags || o.rating != e.rating || o.source != e.source) {
                touched.insert(h.to_string());
            }
        }
        oc.index = SearchIndex::build(c);
        oc.save_error = None;
        oc.outside_change = false;
        if !touched.is_empty() {
            oc.sidecars_pending.extend(touched);
            oc.dirty_since = Some(Instant::now());
        }
        self.usage = None;
        self.dupes_stale = true;
        Ok(())
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
        self.pool_view = query.pool_order().map(str::to_string);
        self.refresh_dupes();
        self.links.parents = self.colls.iter().flat_map(|oc| oc.index.parents()).map(str::to_string).collect();
        let now = chrono::Utc::now();
        let mut rows: Vec<(usize, &Row)> = Vec::new();
        for (i, oc) in self.colls.iter().enumerate() {
            if oc.collection().is_some() {
                rows.extend(oc.index.matching_rows(&query, now, &self.links).into_iter().map(|r| (i, r)));
            }
        }
        rows.sort_by(|a, b| compare_rows(&query, a.1, b.1).then(a.0.cmp(&b.0)));
        self.hits = rows.into_iter().map(|(coll, r)| Hit { coll, hash: r.hash.clone() }).collect();
        self.stacks.clear();
        let targets_group = query.terms.iter().any(|t| matches!(t.kind, boorucraft_core::TermKind::Pool(_) | boorucraft_core::TermKind::Parent(_)));
        if self.stacking() && !targets_group {
            self.stack_hits();
        }
        self.recount();
    }

    pub fn stacking(&self) -> bool {
        self.config.stacks.unwrap_or(true)
    }

    pub fn set_stacking(&mut self, on: bool) {
        self.config.stacks = Some(on);
        if !self.session_only {
            self.save_config();
        }
    }

    /// The stack a file belongs to: its first pool (by name), else its family's top parent.
    fn stack_key(&self, hit: &Hit) -> Option<StackKey> {
        let e = self.entry(hit)?;
        if let Some(pool) = e.pools.keys().next() {
            return Some(StackKey::Pool(pool.clone()));
        }
        if e.parent.is_some() {
            let top = self.ancestors(&hit.hash).pop()?;
            return Some(StackKey::Family(top));
        }
        self.links.parents.contains(&hit.hash).then(|| StackKey::Family(hit.hash.clone()))
    }

    /// Folds each pool and family into one result: a pool's first page, a family's top parent
    /// (or the matching member nearest the top), at that file's place in the order.
    fn stack_hits(&mut self) {
        let keys: Vec<Option<StackKey>> = self.hits.iter().map(|h| self.stack_key(h)).collect();
        let mut best: HashMap<&StackKey, (usize, (u32, usize))> = HashMap::new();
        let mut counts: HashMap<&StackKey, usize> = HashMap::new();
        for (i, (hit, key)) in self.hits.iter().zip(&keys).enumerate() {
            let Some(key) = key else { continue };
            *counts.entry(key).or_default() += 1;
            // Lower ranks win: the page number in a pool, the depth in a family (the top parent
            // is 0); then the place in the order.
            let rank = match key {
                StackKey::Pool(p) => (self.entry(hit).and_then(|e| e.pools.get(p).copied()).unwrap_or(u32::MAX), i),
                StackKey::Family(_) => (u32::try_from(self.ancestors(&hit.hash).len()).unwrap_or(u32::MAX), i),
            };
            let slot = best.entry(key).or_insert((i, rank));
            if rank < slot.1 {
                *slot = (i, rank);
            }
        }
        let mut stacks = HashMap::new();
        let mut kept = Vec::with_capacity(self.hits.len());
        for (i, (hit, key)) in self.hits.iter().zip(&keys).enumerate() {
            match key {
                None => kept.push(hit.clone()),
                Some(k) => {
                    let count = counts.get(k).copied().unwrap_or(1);
                    if best.get(k).is_some_and(|(at, _)| *at == i) {
                        kept.push(hit.clone());
                        if count > 1 {
                            stacks.insert(hit.clone(), Stack { key: k.clone(), count });
                        }
                    }
                }
            }
        }
        self.hits = kept;
        self.stacks = stacks;
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
            self.pool_counts = self.count_pools();
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
        self.after_removal(hits, touched);
        if let Some(e) = errors.first() {
            self.notify(format!("Couldn't move to the trash: {e}"));
        }
        removed
    }

    /// Forgets the missing files among `hits` (files still on disk are left alone): their tags
    /// and links go; nothing on disk changes. Returns how many were forgotten.
    pub fn forget_missing(&mut self, hits: &[Hit]) -> usize {
        let mut touched = BTreeSet::new();
        let mut forgotten = 0;
        for (coll, oc) in self.colls.iter_mut().enumerate() {
            let CollState::Ready { c, .. } = &mut oc.state else { continue };
            if c.is_read_only() {
                continue;
            }
            let n = c.forget_missing(hits.iter().filter(|h| h.coll == coll).map(|h| h.hash.as_str())).len();
            if n > 0 {
                forgotten += n;
                touched.insert(coll);
            }
        }
        self.after_removal(hits, touched);
        forgotten
    }

    /// How many files of a collection weren't found in its last scan.
    pub fn missing_count(&self, coll: usize) -> usize {
        self.colls.get(coll).map_or(0, |oc| oc.index.missing_count())
    }

    /// After entries were removed: children elsewhere lose their link to them, the touched
    /// collections are reindexed and saved, and the results drop them.
    fn after_removal(&mut self, hits: &[Hit], touched: BTreeSet<usize>) {
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
    }

    /// Adds a pool to the central list. Returns its normalised name.
    pub fn create_pool(&mut self, name: &str) -> Option<String> {
        if self.tags.is_read_only() {
            self.notify("The tag list is open in another BooruCraft; new pools can't be created here");
            return None;
        }
        self.usage = None;
        match self.tags.create_pool(name, "") {
            Ok(p) => {
                if let Err(e) = self.tags.save() {
                    self.notify(format!("Couldn't save the tag list: {e}"));
                }
                Some(p)
            }
            Err(StoreError::PoolExists(p)) => Some(p),
            Err(e) => {
                self.notify(e.to_string());
                None
            }
        }
    }

    /// The files of a pool in the open collections, in order.
    pub fn pool_members(&self, pool: &str) -> Vec<Hit> {
        let mut out: Vec<(u32, Hit)> = Vec::new();
        for (coll, oc) in self.colls.iter().enumerate() {
            let Some(c) = oc.collection() else { continue };
            out.extend(c.files().filter_map(|(h, e)| e.pools.get(pool).map(|n| (*n, Hit { coll, hash: h.to_string() }))));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.hash.cmp(&b.1.hash)));
        out.into_iter().map(|(_, h)| h).collect()
    }

    /// Every pool in the list with how many open files are in it, by name. Call
    /// [`Self::refresh_usage`] first in a frame.
    pub fn pool_counts(&self) -> &[(String, usize)] {
        &self.pool_counts
    }

    fn count_pools(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = self.tags.pools().map(|(p, _)| (p.to_string(), 0)).collect();
        for oc in &self.colls {
            let Some(c) = oc.collection() else { continue };
            for (_, e) in c.files() {
                for p in e.pools.keys() {
                    *counts.entry(p.clone()).or_default() += 1;
                }
            }
        }
        counts.into_iter().collect()
    }

    /// Writes new positions (1, 2, 3…) for a pool's files in the given order.
    fn renumber_pool(&mut self, pool: &str, order: &[Hit]) {
        let positions: HashMap<&Hit, u32> = order.iter().zip(1u32..).collect();
        let mut touched = Vec::new();
        for hit in order {
            let Some(&n) = positions.get(hit) else { continue };
            let Some(oc) = self.colls.get_mut(hit.coll) else { continue };
            let CollState::Ready { c, .. } = &mut oc.state else { continue };
            if c.is_read_only() || c.get(&hit.hash).is_none_or(|e| e.pools.get(pool) == Some(&n)) {
                continue;
            }
            if let Some(e) = c.get_mut(&hit.hash) {
                e.pools.insert(pool.to_string(), n);
                touched.push(hit.coll);
            }
        }
        for coll in touched {
            self.touch(coll);
        }
        self.reindex();
    }

    /// Rebuilds every collection's index (after pool changes, which can touch many files).
    fn reindex(&mut self) {
        for oc in &mut self.colls {
            if let CollState::Ready { c, .. } = &oc.state {
                oc.index = SearchIndex::build(c);
            }
        }
        self.usage = None;
        self.recount();
    }

    /// Adds files to the end of a pool, in the given order. Returns how many were added.
    pub fn add_to_pool(&mut self, hits: &[Hit], pool: &str) -> usize {
        let pool = match self.tags.resolve_pool(pool) {
            Ok(p) => p,
            Err(e) => {
                self.notify(e.to_string());
                return 0;
            }
        };
        let mut order = self.pool_members(&pool);
        let before = order.len();
        for h in hits {
            if !order.contains(h) && self.entry(h).is_some() && !self.colls.get(h.coll).is_some_and(OpenCollection::read_only) {
                order.push(h.clone());
            }
        }
        self.renumber_pool(&pool, &order);
        order.len() - before
    }

    /// Takes files out of a pool (the files stay). Returns how many left it.
    pub fn remove_from_pool(&mut self, hits: &[Hit], pool: &str) -> usize {
        let mut removed = 0;
        self.edit_entries(hits, |e| {
            let hit = e.pools.remove(pool).is_some();
            removed += usize::from(hit);
            hit
        });
        let order = self.pool_members(pool);
        self.renumber_pool(pool, &order);
        removed
    }

    /// Moves files within a pool, keeping their order among themselves.
    pub fn move_in_pool(&mut self, pool: &str, hits: &[Hit], to: PoolMove) {
        let mut order = self.pool_members(pool);
        let picked = |h: &Hit| hits.contains(h);
        match to {
            PoolMove::First | PoolMove::Last => {
                let (mut sel, rest): (Vec<Hit>, Vec<Hit>) = order.into_iter().partition(|h| picked(h));
                order = if to == PoolMove::First {
                    sel.extend(rest);
                    sel
                } else {
                    let mut r = rest;
                    r.extend(sel);
                    r
                };
            }
            PoolMove::Earlier => {
                for i in 1..order.len() {
                    if picked(&order[i]) && !picked(&order[i - 1]) {
                        order.swap(i, i - 1);
                    }
                }
            }
            PoolMove::Later => {
                for i in (0..order.len().saturating_sub(1)).rev() {
                    if picked(&order[i]) && !picked(&order[i + 1]) {
                        order.swap(i, i + 1);
                    }
                }
            }
        }
        self.renumber_pool(pool, &order);
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
        if (oc.dirty_since.is_none() && oc.save_error.is_none()) || oc.outside_change {
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
    fn outside_changes_are_reloaded_or_settled_by_the_user() {
        let tmp = tempfile::tempdir().unwrap();
        let pics = tmp.path().join("pics");
        std::fs::create_dir_all(&pics).unwrap();
        picture(&pics.join("a.png"), 10);
        let mut lib = library(tmp.path(), &pics);
        lib.create_tag("cat");
        lib.create_tag("dog");
        lib.search("");
        let all = lib.hits.clone();
        lib.edit_tags(&all, &["cat".into()], &[]);
        lib.flush_all();
        assert!(!lib.check_outside_changes(), "our own saves aren't outside changes");
        let json = pics.join("collection.json");

        // No unsaved changes here: their version is loaded, and the sidecar follows it.
        let edited = std::fs::read_to_string(&json).unwrap().replace("\"cat\"", "\"dog\"");
        std::fs::write(&json, &edited).unwrap();
        assert!(lib.check_outside_changes());
        lib.search("dog");
        assert_eq!(lib.hits.len(), 1);
        lib.flush_all();
        assert!(std::fs::read_to_string(pics.join("a.png.xmp")).unwrap().contains("dog"));

        // Both sides changed: nothing is saved until the user chooses.
        lib.edit_tags(&all, &["cat".into()], &[]);
        std::fs::write(&json, edited.replace("\"dog\"", "\"dog\", \"theirs\"")).unwrap();
        lib.check_outside_changes();
        assert!(lib.colls[0].outside_change);
        lib.flush_all();
        assert!(std::fs::read_to_string(&json).unwrap().contains("theirs"), "not saved over");
        lib.resolve_outside_change(Outside::Collection(0), true);
        assert!(!lib.colls[0].outside_change);
        let saved = std::fs::read_to_string(&json).unwrap();
        assert!(saved.contains("\"cat\"") && !saved.contains("theirs"), "{saved}");
        assert!(std::fs::read_to_string(pics.join("collection.json.bak")).unwrap().contains("theirs"));

        // Again, but load theirs this time.
        lib.edit_tags(&all, &[], &["cat".into()]);
        std::fs::write(&json, saved.replace("\"dog\"", "\"dog\", \"theirs\"")).unwrap();
        lib.check_outside_changes();
        lib.resolve_outside_change(Outside::Collection(0), false);
        assert!(!lib.colls[0].outside_change);
        lib.flush_all();
        assert!(!lib.has_unsaved());
        assert!(std::fs::read_to_string(pics.join("a.png.xmp")).unwrap().contains("theirs"), "sidecars follow the loaded version");
        lib.search("cat theirs");
        assert_eq!(lib.hits.len(), 1, "their version, with cat still on it");

        // A damaged file isn't loaded; the user is asked instead.
        std::fs::write(&json, "{ broken").unwrap();
        lib.check_outside_changes();
        assert!(lib.colls[0].outside_change);
        lib.resolve_outside_change(Outside::Collection(0), false);
        assert!(lib.colls[0].outside_change, "can't load a damaged file");
        lib.resolve_outside_change(Outside::Collection(0), true);
        assert!(!lib.colls[0].outside_change);
        assert!(std::fs::read_to_string(&json).unwrap().contains("boorucraft-collection"));

        // Deleted: written again.
        std::fs::remove_file(&json).unwrap();
        lib.check_outside_changes();
        assert!(std::fs::read_to_string(&json).unwrap().contains("theirs"));

        // The tag list, edited by hand: a rename with its history entry reaches the files.
        let tags_path = tmp.path().join("cfg/tags.json");
        let mut list: serde_json::Value = serde_json::from_slice(&std::fs::read(&tags_path).unwrap()).unwrap();
        let rev = list["revision"].as_u64().unwrap() + 1;
        list["revision"] = rev.into();
        let tags = list["tags"].as_object_mut().unwrap();
        let info = tags.remove("dog").unwrap();
        tags.insert("hound".into(), info);
        list["history"].as_array_mut().unwrap().push(serde_json::json!({"revision": rev, "op": "rename", "from": "dog", "to": "hound"}));
        std::fs::write(&tags_path, serde_json::to_vec_pretty(&list).unwrap()).unwrap();
        assert!(lib.check_outside_changes());
        assert!(lib.tags.contains("hound") && !lib.tags.contains("dog"));
        lib.search("hound");
        assert_eq!(lib.hits.len(), 1);
        lib.flush_all();
        assert!(std::fs::read_to_string(pics.join("a.png.xmp")).unwrap().contains("hound"));

        // Unsaved tag list changes and an outside edit: the user chooses.
        let mut theirs: serde_json::Value = serde_json::from_slice(&std::fs::read(&tags_path).unwrap()).unwrap();
        theirs["tags"].as_object_mut().unwrap().insert("owl".into(), serde_json::json!({}));
        lib.tags.create("parrot", "").unwrap();
        std::fs::write(&tags_path, serde_json::to_vec_pretty(&theirs).unwrap()).unwrap();
        lib.check_outside_changes();
        assert!(lib.tags_outside_change);
        lib.resolve_outside_change(Outside::TagList, false);
        assert!(!lib.tags_outside_change && lib.tags.contains("owl") && !lib.tags.contains("parrot"));
    }

    /// Polls until `done` holds, failing after a few seconds.
    fn poll_until(lib: &mut Library, what: &str, done: impl Fn(&Library) -> bool) {
        let start = Instant::now();
        loop {
            lib.poll();
            if done(lib) {
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(15), "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn watched_folders_are_rescanned_but_not_for_our_own_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let pics = tmp.path().join("pics");
        std::fs::create_dir_all(&pics).unwrap();
        picture(&pics.join("a.png"), 10);
        let mut lib = library(tmp.path(), &pics);
        assert!(lib.is_watching());
        let files = |lib: &Library| lib.colls[0].collection().map_or(0, |c| c.files().filter(|(_, e)| !e.missing).count());

        // A picture copied in, and one in a new folder.
        picture(&pics.join("b.png"), 120);
        std::fs::create_dir(pics.join("sub")).unwrap();
        picture(&pics.join("sub/c.png"), 240);
        poll_until(&mut lib, "new pictures found", |l| !l.is_busy() && files(l) == 3);

        // Our own edits and saves (collection.json, sidecars) cause no rescan.
        lib.create_tag("cat");
        lib.search("");
        let all = lib.hits.clone();
        lib.edit_tags(&all, &["cat".into()], &[]);
        lib.flush_all();
        assert!(pics.join("a.png.xmp").exists());
        let quiet = Instant::now();
        while quiet.elapsed() < WATCH_DELAY * 2 {
            lib.poll();
            assert!(lib.colls[0].rescan_at.is_none() && !lib.is_scanning(), "rescanned after our own save");
            std::thread::sleep(Duration::from_millis(20));
        }

        // A deleted picture is marked missing.
        std::fs::remove_file(pics.join("sub/c.png")).unwrap();
        poll_until(&mut lib, "deletion noticed", |l| !l.is_busy() && files(l) == 2);

        // Another app adds a keyword to a sidecar: read back.
        lib.create_tag("dog");
        let xmp = std::fs::read_to_string(pics.join("a.png.xmp")).unwrap();
        std::fs::write(pics.join("a.png.xmp"), xmp.replacen("<rdf:li>cat</rdf:li>", "<rdf:li>cat</rdf:li><rdf:li>dog</rdf:li>", 1)).unwrap();
        poll_until(&mut lib, "sidecar read back", |l| l.colls[0].collection().is_some_and(|c| c.files().any(|(_, e)| e.tags.contains("dog"))));
    }

    #[test]
    fn missing_files_can_be_forgotten() {
        let tmp = tempfile::tempdir().unwrap();
        let pics = tmp.path().join("pics");
        std::fs::create_dir_all(&pics).unwrap();
        picture(&pics.join("a.png"), 10);
        picture(&pics.join("b.png"), 120);
        picture(&pics.join("c.png"), 240);
        let mut lib = library(tmp.path(), &pics);
        lib.search("");
        let all = lib.hits.clone();
        let hit = |lib: &Library, name: &str| all.iter().find(|h| lib.entry(h).is_some_and(|e| e.paths.contains(name))).cloned().unwrap();
        let (a, b, c) = (hit(&lib, "a.png"), hit(&lib, "b.png"), hit(&lib, "c.png"));
        lib.set_parent(std::slice::from_ref(&c), Some(&a));
        std::fs::remove_file(pics.join("a.png")).unwrap();
        std::fs::remove_file(pics.join("b.png")).unwrap();
        lib.start_scan(0);
        poll_until(&mut lib, "scan", |l| !l.is_busy());
        assert_eq!(lib.missing_count(0), 2);
        lib.search("missing:yes");
        assert_eq!(lib.hits.len(), 2);

        assert_eq!(lib.forget_missing(&[a.clone(), c.clone()]), 1, "c is on disk and stays");
        assert!(lib.entry(&a).is_none() && lib.entry(&c).is_some());
        assert_eq!(lib.entry(&c).unwrap().parent, None, "its child lost the link");
        assert_eq!(lib.missing_count(0), 1);
        assert_eq!(lib.hits, vec![b.clone()], "forgotten files leave the results");
        lib.flush_all();
        assert!(!std::fs::read_to_string(pics.join("collection.json")).unwrap().contains(&a.hash));
    }

    #[test]
    fn tag_list_changes_reach_open_and_closed_collections() {
        let tmp = tempfile::tempdir().unwrap();
        let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
        for d in [&one, &two] {
            std::fs::create_dir_all(d).unwrap();
            picture(&d.join("a.png"), 10);
        }
        let mut lib = libraries(tmp.path(), &[&one, &two]);
        for t in ["cat", "kitty", "dog", "temp"] {
            lib.create_tag(t);
        }
        lib.search("");
        let all = lib.hits.clone();
        lib.edit_tags(&all, &["kitty".into(), "dog".into(), "temp".into()], &[]);
        lib.flush_all();
        lib.close_collection(1);

        assert_eq!(lib.merge_tag("kitty", "cat"), Some(()));
        assert_eq!(lib.rename_tag("dog", "hound").as_deref(), Some("hound"));
        assert_eq!(lib.delete_tag("temp"), Some(()));
        assert_eq!(lib.rename_tag("hound", "cat"), None, "can't rename onto an existing tag");
        assert_eq!(lib.merge_tag("cat", "cat"), None);
        assert!(lib.describe_tag("cat", "  felines ").is_some());
        assert_eq!(lib.tags.tags().find(|(t, _)| *t == "cat").unwrap().1.description, "felines");
        lib.search("cat hound -temp -kitty");
        assert_eq!(lib.hits.len(), 1);
        lib.flush_all();
        let xmp = std::fs::read_to_string(one.join("a.png.xmp")).unwrap();
        assert!(xmp.contains("hound") && xmp.contains(">cat<") && !xmp.contains("kitty") && !xmp.contains("temp"), "{xmp}");

        // The closed collection catches up when opened again.
        assert!(std::fs::read_to_string(two.join("collection.json")).unwrap().contains("kitty"));
        lib.open_collection(&two, false);
        poll_until(&mut lib, "scan", |l| !l.is_busy());
        lib.search("cat hound");
        assert_eq!(lib.hits.len(), 2);
        lib.flush_all();
        assert!(!std::fs::read_to_string(two.join("a.png.xmp")).unwrap().contains("kitty"));

        // Namespaces and pools.
        assert!(lib.add_namespace("place", "#336699").is_some());
        assert!(lib.add_namespace("place", "#000000").is_none(), "already exists");
        assert!(lib.set_namespace_color("place", "#112233").is_some());
        assert_eq!(lib.tags.namespaces().find(|(n, _)| *n == "place").unwrap().1.color, "#112233");
        assert!(lib.remove_namespace("place").is_some());
        assert!(!lib.tags.is_namespace("place"));
        lib.create_pool("trip");
        assert_eq!(lib.add_to_pool(&lib.hits.clone(), "trip"), 2);
        lib.search("pool:trip");
        assert_eq!(lib.hits.len(), 2, "in the pool before the rename");
        assert_eq!(lib.rename_pool("trip", "holiday").as_deref(), Some("holiday"));
        lib.search("pool:holiday");
        assert_eq!(lib.hits.len(), 2);
        assert!(lib.delete_pool("holiday").is_some());
        lib.search("pool:any");
        assert!(lib.hits.is_empty());
    }

    #[test]
    fn unknown_tags_are_added_mapped_or_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let pics = tmp.path().join("pics");
        std::fs::create_dir_all(&pics).unwrap();
        picture(&pics.join("a.png"), 10);
        picture(&pics.join("b.png"), 120);
        // Sidecars from another app, with keywords the tag list doesn't have.
        let xmp = |kw: &str| {
            format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:subject><rdf:Bag>{kw}</rdf:Bag></dc:subject></rdf:Description></rdf:RDF></x:xmpmeta>"#
            )
        };
        std::fs::write(pics.join("a.png.xmp"), xmp("<rdf:li>Kitty</rdf:li><rdf:li>Beach</rdf:li><rdf:li>junk</rdf:li>")).unwrap();
        std::fs::write(pics.join("b.png.xmp"), xmp("<rdf:li>Kitty</rdf:li>")).unwrap();
        let mut lib = library(tmp.path(), &pics);
        lib.create_tag("cat");
        lib.refresh_usage();
        assert_eq!(lib.unknown_tags(), vec![("kitty".to_string(), 2), ("beach".to_string(), 1), ("junk".to_string(), 1)]);

        assert_eq!(lib.replace_unknown_tag("kitty", Some("cat")), 2);
        assert_eq!(lib.replace_unknown_tag("junk", None), 1);
        assert_eq!(lib.replace_unknown_tag("beach", Some("not_a_tag")), 0, "only listed tags can replace one");
        lib.create_tag("beach");
        lib.refresh_usage();
        assert!(lib.unknown_tags().is_empty());
        lib.search("cat");
        assert_eq!(lib.hits.len(), 2);
        lib.flush_all();
        let a = std::fs::read_to_string(pics.join("a.png.xmp")).unwrap();
        assert!(a.contains(">cat<") && !a.contains("junk") && a.contains("beach"), "{a}");
    }

    #[test]
    fn a_damaged_collection_is_recovered_from_its_backup_or_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let pics = tmp.path().join("pics");
        std::fs::create_dir_all(&pics).unwrap();
        picture(&pics.join("a.png"), 10);
        let mut lib = library(tmp.path(), &pics);
        lib.create_tag("cat");
        lib.search("");
        let all = lib.hits.clone();
        lib.edit_tags(&all, &["cat".into()], &[]);
        lib.set_rating(&all, Some(Rating::Explicit));
        lib.flush_all();
        drop(lib);
        let json = pics.join("collection.json");

        // Damaged, with a good backup.
        std::fs::write(&json, "{ damaged").unwrap();
        let mut lib = library(tmp.path(), &pics);
        assert_eq!(lib.colls[0].damaged, Some(true));
        assert!(matches!(lib.colls[0].state, CollState::Failed(_)));
        lib.recover_collection(0, Recovery::Backup);
        poll_until(&mut lib, "scan", |l| !l.is_busy());
        assert_eq!(lib.colls[0].damaged, None);
        lib.flush_all();
        let kept: Vec<_> = std::fs::read_dir(tmp.path().join("cfg/damaged")).unwrap().flatten().collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read_to_string(kept[0].path()).unwrap(), "{ damaged", "a copy of the damaged file is kept");
        assert!(kept[0].file_name().to_string_lossy().starts_with("pics-"));
        drop(lib);

        // Damaged with no usable backup: rebuilt from the sidecars.
        std::fs::write(&json, "{ damaged").unwrap();
        std::fs::remove_file(pics.join("collection.json.bak")).unwrap();
        let mut lib = library(tmp.path(), &pics);
        assert_eq!(lib.colls[0].damaged, Some(false));
        lib.recover_collection(0, Recovery::Empty);
        poll_until(&mut lib, "scan", |l| !l.is_busy());
        lib.search("cat rating:e");
        assert_eq!(lib.hits.len(), 1, "tag and rating came back from the sidecar");
        lib.flush_all();
        assert!(std::fs::read_to_string(&json).unwrap().contains("\"cat\""));
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
    fn pools_keep_their_order_across_collections() {
        let tmp = tempfile::tempdir().unwrap();
        let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        for (dir, name, shade) in [(&one, "p1.png", 1), (&two, "p2.png", 2), (&one, "p3.png", 3), (&two, "p4.png", 4)] {
            picture(&dir.join(name), shade);
        }
        let mut lib = libraries(tmp.path(), &[&one, &two]);
        let names = |lib: &Library| -> Vec<String> { lib.hits.iter().map(|h| lib.entry(h).unwrap().paths.iter().next().unwrap().clone()).collect() };
        lib.search("order:name");
        let files = lib.hits.clone();
        assert_eq!(lib.add_to_pool(&files, "comic"), 0, "pools must exist first");
        assert_eq!(lib.create_pool("Comic").as_deref(), Some("comic"));
        assert_eq!(lib.add_to_pool(&[files[2].clone(), files[0].clone()], "comic"), 2);
        assert_eq!(lib.add_to_pool(&files, "comic"), 2, "only new files are added, at the end");
        lib.search("pool:comic");
        assert_eq!(names(&lib), vec!["p3.png", "p1.png", "p2.png", "p4.png"]);
        assert_eq!(lib.pool_view.as_deref(), Some("comic"));

        lib.move_in_pool("comic", &[files[3].clone()], PoolMove::First);
        lib.move_in_pool("comic", &[files[0].clone()], PoolMove::Earlier);
        lib.search("pool:comic");
        assert_eq!(names(&lib), vec!["p4.png", "p1.png", "p3.png", "p2.png"]);
        lib.move_in_pool("comic", &[files[3].clone(), files[0].clone()], PoolMove::Last);
        lib.search("pool:comic");
        assert_eq!(names(&lib), vec!["p3.png", "p2.png", "p4.png", "p1.png"], "moved together, in their order");

        assert_eq!(lib.remove_from_pool(&files[1..2], "comic"), 1);
        lib.search("pool:comic");
        assert_eq!(names(&lib), vec!["p3.png", "p4.png", "p1.png"]);
        let positions: Vec<u32> = lib.hits.iter().map(|h| lib.entry(h).unwrap().pools["comic"]).collect();
        assert_eq!(positions, vec![1, 2, 3], "renumbered without gaps");
        lib.refresh_usage();
        assert_eq!(lib.pool_counts(), &[("comic".to_string(), 3)]);
        lib.search("pool:none");
        assert_eq!(names(&lib), vec!["p2.png"]);
        lib.flush_all();
        assert!(std::fs::read_to_string(two.join("collection.json")).unwrap().contains("\"comic\": 2"));
    }

    #[test]
    fn pools_and_families_stack() {
        let tmp = tempfile::tempdir().unwrap();
        let pics = tmp.path().join("pics");
        std::fs::create_dir_all(&pics).unwrap();
        for (i, name) in ["page1.png", "page2.png", "page3.png", "base.png", "edit.png", "crop.png", "alone.png"].iter().enumerate() {
            picture(&pics.join(name), i as u8 * 30);
        }
        let mut lib = library(tmp.path(), &pics);
        let by_name = |lib: &Library, n: &str| lib.hits.iter().find(|h| lib.entry(h).unwrap().paths.contains(n)).cloned().unwrap();
        lib.set_stacking(false);
        lib.search("order:name");
        let (p1, p2, p3) = (by_name(&lib, "page1.png"), by_name(&lib, "page2.png"), by_name(&lib, "page3.png"));
        let (base, edit, crop) = (by_name(&lib, "base.png"), by_name(&lib, "edit.png"), by_name(&lib, "crop.png"));
        lib.create_pool("comic");
        lib.add_to_pool(&[p2.clone(), p1.clone(), p3.clone()], "comic");
        lib.set_parent(std::slice::from_ref(&edit), Some(&base));
        lib.set_parent(std::slice::from_ref(&crop), Some(&edit));

        lib.set_stacking(true);
        lib.search("order:name");
        let names: Vec<String> = lib.hits.iter().map(|h| lib.entry(h).unwrap().paths.iter().next().unwrap().clone()).collect();
        assert_eq!(names, vec!["alone.png", "base.png", "page2.png"], "one tile per pool (its first page) and per family (its top parent)");
        assert_eq!(lib.stacks.get(&p2).map(|s| (s.key.query(), s.count)), Some(("pool:comic".into(), 3)));
        assert_eq!(lib.stacks.get(&base).map(|s| s.count), Some(3), "grandchildren too");
        lib.search(&format!("-hash:{}", &base.hash[..16]));
        assert!(lib.hits.contains(&edit) && !lib.hits.contains(&crop), "without the parent, the member nearest the top stands for the family");
        lib.search("pool:comic");
        assert_eq!(lib.hits.len(), 3, "a pool search isn't stacked");
        assert!(lib.stacks.is_empty());
        lib.search(&lib.stacks.get(&base).map_or_else(|| StackKey::Family(base.hash.clone()).query(), |s| s.key.query()));
        assert_eq!(lib.hits.len(), 2, "parent: shows a file and its direct children");
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

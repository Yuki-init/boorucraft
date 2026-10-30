//! Scanning a collection folder.
//!
//! 1. Walk the folder (no hidden folders, no symlinks, no nested collections).
//! 2. Reuse the cached hash of files whose size and modification time haven't changed; hash
//!    the rest in parallel and read their metadata.
//! 3. Rebuild each file's path list from what was found. A known hash at a new path keeps its
//!    tags (moved or copied). A file whose paths all vanished is marked missing, not removed.
//! 4. A path whose content changed (an edited picture) passes its tags to the new content, if
//!    the old content is now gone.
//! 5. Read back sidecars other apps changed, and write sidecars that are missing.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use boorucraft_store::{Collection, FileEntry, TagList};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::cache::{ScanCache, Seen, mtime_ns};
use crate::probe::{self, Hashes, IMAGE_EXTENSIONS, Meta, VIDEO_EXTENSIONS};
use crate::sidecar::{self, SidecarReport};

#[derive(Debug, Default)]
pub struct ScanReport {
    /// Files found on disk.
    pub files: usize,
    /// Files that had to be hashed (new or changed since the last scan).
    pub hashed: usize,
    /// New pictures added to the collection.
    pub added: usize,
    /// Known pictures found at a new path (moved or copied).
    pub moved: usize,
    /// Pictures whose content changed in place; tags carried over.
    pub edited: usize,
    /// Pictures no longer found anywhere in the folder.
    pub missing: usize,
    pub sidecars: SidecarReport,
    pub errors: Vec<(PathBuf, String)>,
}

/// How far a running scan has got, readable from another thread while it runs.
#[derive(Debug, Default)]
pub struct ScanProgress {
    /// Files found so far while walking the folder.
    found: AtomicUsize,
    /// Set once the walk is done and hashing starts.
    hashing: AtomicBool,
    /// Files that need hashing, and how many of them are done.
    to_hash: AtomicUsize,
    hashed: AtomicUsize,
}

/// What [`ScanProgress::get`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStage {
    /// Walking the folder: files found so far.
    Walking(usize),
    /// Hashing new or changed files: done of total.
    Hashing(usize, usize),
}

impl ScanProgress {
    pub fn get(&self) -> ScanStage {
        if self.hashing.load(Ordering::Relaxed) {
            ScanStage::Hashing(self.hashed.load(Ordering::Relaxed), self.to_hash.load(Ordering::Relaxed))
        } else {
            ScanStage::Walking(self.found.load(Ordering::Relaxed))
        }
    }
}

/// Adds one to the counter when dropped.
struct Counted<'a>(&'a AtomicUsize);

impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

struct Found {
    rel: String,
    abs: PathBuf,
    seen: Seen,
    ext: String,
}

pub(crate) fn extensions(c: &Collection) -> BTreeSet<String> {
    let custom = &c.settings().extensions;
    if custom.is_empty() {
        IMAGE_EXTENSIONS.iter().chain(VIDEO_EXTENSIONS).map(|e| e.to_string()).collect()
    } else {
        custom.iter().map(|e| e.trim_start_matches('.').to_lowercase()).collect()
    }
}

fn walk(c: &Collection, report: &mut ScanReport, progress: &ScanProgress) -> Vec<Found> {
    let exts = extensions(c);
    let root = c.root().to_path_buf();
    let walker = WalkDir::new(&root).follow_links(false).into_iter().filter_entry(|e| {
        if e.depth() == 0 {
            return true;
        }
        let hidden = e.file_name().to_string_lossy().starts_with('.');
        let nested = e.file_type().is_dir() && e.path().join(boorucraft_store::collection::COLLECTION_FILE).exists();
        !hidden && !nested
    });
    let mut out = Vec::new();
    for item in walker {
        let entry = match item {
            Ok(e) => e,
            Err(e) => {
                report.errors.push((e.path().map(PathBuf::from).unwrap_or_else(|| root.clone()), e.to_string()));
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry.path().extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !exts.contains(&ext) {
            continue;
        }
        let Some(rel) = c.rel_path(entry.path()) else {
            report.errors.push((entry.path().to_path_buf(), "file name isn't valid Unicode; skipped".into()));
            continue;
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                report.errors.push((entry.path().to_path_buf(), e.to_string()));
                continue;
            }
        };
        out.push(Found { rel, abs: entry.into_path(), seen: Seen { size: meta.len(), mtime: mtime_ns(&meta) }, ext });
        progress.found.store(out.len(), Ordering::Relaxed);
    }
    out
}

/// Scans the collection folder and updates the collection. Doesn't save anything; the caller
/// saves the collection and the cache.
pub fn scan(c: &mut Collection, tags: &TagList, cache: &mut ScanCache) -> ScanReport {
    scan_with_progress(c, tags, cache, &ScanProgress::default())
}

/// [`scan`], reporting how far it has got through `progress`.
pub fn scan_with_progress(c: &mut Collection, tags: &TagList, cache: &mut ScanCache, progress: &ScanProgress) -> ScanReport {
    let mut report = ScanReport::default();
    let found = walk(c, &mut report, progress);
    report.files = found.len();

    // Hashes: cached when unchanged, computed otherwise.
    let mut hash_of: BTreeMap<String, String> = BTreeMap::new();
    let mut to_hash: Vec<&Found> = Vec::new();
    for f in &found {
        match cache.hash_if_unchanged(&f.rel, f.seen) {
            Some(h) if c.get(h).is_some() => {
                hash_of.insert(f.rel.clone(), h.to_string());
            }
            _ => to_hash.push(f),
        }
    }
    report.hashed = to_hash.len();
    progress.to_hash.store(to_hash.len(), Ordering::Relaxed);
    progress.hashing.store(true, Ordering::Relaxed);
    let results: Vec<Result<(&Found, Hashes, Meta), (PathBuf, String)>> = to_hash
        .par_iter()
        .map(|f| {
            // Counted even when it fails, so the count still reaches the total.
            let _done = Counted(&progress.hashed);
            let hashes = probe::hash_file(&f.abs).map_err(|e| (f.abs.clone(), e.to_string()))?;
            let meta = probe::probe(&f.abs, &hashes.header, &f.ext);
            Ok((*f, hashes, meta))
        })
        .collect();
    let mut new_entries: HashMap<String, FileEntry> = HashMap::new();
    for r in results {
        match r {
            Ok((f, hashes, meta)) => {
                cache.set_file(&f.rel, f.seen, &hashes.sha256);
                hash_of.insert(f.rel.clone(), hashes.sha256.clone());
                if c.get(&hashes.sha256).is_none() && !new_entries.contains_key(&hashes.sha256) {
                    let mut e = FileEntry::new(hashes.md5, meta.kind, meta.format, f.seen.size);
                    e.width = meta.width;
                    e.height = meta.height;
                    e.animated = meta.animated;
                    e.taken = meta.taken.or_else(|| mtime_as_date(f.seen.mtime));
                    new_entries.insert(hashes.sha256, e);
                }
            }
            Err(e) => report.errors.push(e),
        }
    }

    // Where each hash lives now; content that changed under an existing path.
    let old_paths = c.path_map();
    let mut paths_now: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut edits: Vec<(String, String)> = Vec::new();
    for (rel, hash) in &hash_of {
        paths_now.entry(hash.as_str()).or_default().insert(rel.clone());
        match old_paths.get(rel) {
            Some(old) if old != hash => edits.push((old.clone(), hash.clone())),
            Some(_) => {}
            None if !new_entries.contains_key(hash) => report.moved += 1,
            None => {}
        }
    }

    report.added = new_entries.len();
    let fresh: BTreeSet<String> = new_entries.keys().cloned().collect();
    for (hash, entry) in new_entries {
        c.insert(hash, entry);
    }
    let hashes: Vec<String> = c.files().map(|(h, _)| h.to_string()).collect();
    for hash in &hashes {
        let now = paths_now.get(hash.as_str());
        let Some(entry) = c.get(hash) else { continue };
        let (paths, missing) = match now {
            Some(p) => (p.clone(), false),
            None => (entry.paths.clone(), true),
        };
        if (entry.paths != paths || entry.missing != missing)
            && let Some(e) = c.get_mut(hash)
        {
            e.paths = paths;
            e.missing = missing;
        }
    }

    // Edited in place: the old content's tags move to the new content.
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for (old, new) in edits {
        let Some(old_entry) = c.get(&old).filter(|e| e.missing).cloned() else { continue };
        let Some(e) = c.get_mut(&new) else { continue };
        e.tags.extend(old_entry.tags);
        e.rating = e.rating.or(old_entry.rating);
        e.source = e.source.take().or(old_entry.source);
        e.imported = e.imported.min(old_entry.imported);
        c.remove(&old);
        touched.insert(new);
        report.edited += 1;
    }
    report.missing = c.files().filter(|(_, e)| e.missing).count();
    cache.retain_files(|rel| hash_of.contains_key(rel));

    // Sidecars.
    sidecar::read_back(c, tags, cache, &fresh, &mut report.sidecars);
    touched.extend(sidecar::missing_sidecars(c));
    sidecar::write_sidecars(c, tags, touched.iter().map(String::as_str), cache, &mut report.sidecars);
    report
}

fn mtime_as_date(ns: u64) -> Option<chrono::DateTime<chrono::Utc>> {
    if ns == 0 {
        return None;
    }
    let secs = i64::try_from(ns / 1_000_000_000).ok()?;
    chrono::DateTime::from_timestamp(secs, u32::try_from(ns % 1_000_000_000).ok()?)
}

//! Watching collection folders while the app runs.
//!
//! [`FolderWatcher`] reports changed paths (through `notify`: inotify on Linux) and
//! [`classify`] decides what each means for a collection: nothing, a rescan, or a look at
//! collection.json. BooruCraft's own writes are recognised and ignored: its temp files, and
//! sidecars whose modification time is the one recorded right after writing them.

use std::path::{Path, PathBuf};

use boorucraft_store::Collection;
use boorucraft_store::collection::COLLECTION_FILE;
use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::cache::{ScanCache, Seen, mtime_ns};
use crate::scan::extensions;

/// What a changed path means for the collection it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relevance {
    /// Not ours to care about, or BooruCraft's own write.
    Ignore,
    /// collection.json itself: compare it with what was loaded.
    CollectionFile,
    /// A picture, sidecar or folder changed: scan again.
    Rescan,
}

/// Watches folders and calls back with batches of changed paths (on notify's thread).
pub struct FolderWatcher {
    inner: RecommendedWatcher,
}

impl std::fmt::Debug for FolderWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FolderWatcher")
    }
}

impl FolderWatcher {
    pub fn new(on_change: impl Fn(Vec<PathBuf>) + Send + 'static) -> Result<FolderWatcher, String> {
        let inner = notify::recommended_watcher(move |res: notify::Result<Event>| {
            // Errors (e.g. the kernel's event queue overflowed) are reported with no paths or
            // the watched root; a rescan of that root catches up either way.
            match res {
                Ok(event) if matters(&event.kind) => on_change(event.paths),
                Ok(_) => {}
                Err(e) => on_change(e.paths),
            }
        })
        .map_err(|e| e.to_string())?;
        Ok(FolderWatcher { inner })
    }

    /// Watches `dir` and everything below it (`recursive`), or only the files directly in it.
    pub fn watch(&mut self, dir: &Path, recursive: bool) -> Result<(), String> {
        let mode = if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
        self.inner.watch(dir, mode).map_err(|e| e.to_string())
    }

    pub fn unwatch(&mut self, dir: &Path) {
        let _ = self.inner.unwatch(dir);
    }
}

/// Opening and reading files are not changes; a file closed after writing is.
fn matters(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// What a change at `path` (absolute) means for collection `c`, whose scan cache is `cache`.
pub fn classify(c: &Collection, cache: &ScanCache, path: &Path) -> Relevance {
    if path == c.root() {
        return Relevance::Rescan;
    }
    let Some(rel) = c.rel_path(path) else { return Relevance::Ignore };
    if rel == COLLECTION_FILE {
        return Relevance::CollectionFile;
    }
    // collection.json.bak/.tmp/.lock, sidecar temp files, hidden files and folders.
    let name = rel.rsplit('/').next().unwrap_or(&rel);
    if rel.starts_with(&format!("{COLLECTION_FILE}.")) || name.ends_with(".tmp") || rel.split('/').any(|part| part.starts_with('.')) {
        return Relevance::Ignore;
    }
    let meta = std::fs::symlink_metadata(path).ok();
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    if ext == "xmp" {
        return match meta {
            // Written or read by BooruCraft since: nothing new.
            Some(m) if cache.sidecar_mtime(&rel) == Some(mtime_ns(&m)) => Relevance::Ignore,
            _ => Relevance::Rescan,
        };
    }
    if extensions(c).contains(&ext) {
        return match meta {
            Some(m) if m.is_file() => {
                let seen = Seen { size: m.len(), mtime: mtime_ns(&m) };
                if cache.hash_if_unchanged(&rel, seen).is_some() { Relevance::Ignore } else { Relevance::Rescan }
            }
            Some(_) => Relevance::Ignore,
            None if c.hash_of_path(&rel).is_some() => Relevance::Rescan,
            None => Relevance::Ignore,
        };
    }
    match meta {
        // A folder appeared, or was renamed or moved in.
        Some(m) if m.is_dir() => Relevance::Rescan,
        Some(_) => Relevance::Ignore,
        // Gone: matters if it was a folder holding known files.
        None => {
            let prefix = format!("{rel}/");
            if c.files().any(|(_, e)| e.paths.iter().any(|p| p.starts_with(&prefix))) { Relevance::Rescan } else { Relevance::Ignore }
        }
    }
}

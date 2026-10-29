//! The scan cache: what each path looked like when last hashed, and when BooruCraft last wrote
//! each sidecar. Lives in `~/.cache/boorucraft/scan/`; losing it only means rehashing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use boorucraft_store::{hex_sha256, safefile};
use serde::{Deserialize, Serialize};

const MAX_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub size: u64,
    pub mtime: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFile {
    pub size: u64,
    pub mtime: u64,
    pub hash: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    files: BTreeMap<String, CachedFile>,
    /// Sidecar relative path → its modification time right after BooruCraft wrote or read it.
    #[serde(default)]
    sidecars: BTreeMap<String, u64>,
}

#[derive(Debug)]
pub struct ScanCache {
    path: PathBuf,
    data: CacheFile,
}

impl ScanCache {
    /// The cache for the collection at `root` (an unreadable cache starts empty).
    pub fn load(cache_dir: &Path, root: &Path) -> ScanCache {
        let key = hex_sha256(root.to_string_lossy().as_bytes());
        let path = cache_dir.join("scan").join(format!("{}.json", key.get(..32).unwrap_or(&key)));
        let data = safefile::read_capped(&path, MAX_BYTES).ok().flatten().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        ScanCache { path, data }
    }

    /// A cache that is never written, for tests.
    pub fn in_memory() -> ScanCache {
        ScanCache { path: PathBuf::new(), data: CacheFile::default() }
    }

    pub fn save(&self) -> std::io::Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec(&self.data).map_err(std::io::Error::other)?;
        safefile::write_atomic(&self.path, &bytes, false).map(|_| ()).map_err(std::io::Error::other)
    }

    /// The cached hash, if the file still has the size and mtime it had when hashed.
    pub fn hash_if_unchanged(&self, rel: &str, seen: Seen) -> Option<&str> {
        self.data.files.get(rel).filter(|c| c.size == seen.size && c.mtime == seen.mtime).map(|c| c.hash.as_str())
    }

    pub fn set_file(&mut self, rel: &str, seen: Seen, hash: &str) {
        self.data.files.insert(rel.to_string(), CachedFile { size: seen.size, mtime: seen.mtime, hash: hash.to_string() });
    }

    /// Forgets paths that weren't found in the latest scan.
    pub fn retain_files(&mut self, keep: impl Fn(&str) -> bool) {
        self.data.files.retain(|k, _| keep(k));
    }

    pub fn sidecar_mtime(&self, rel: &str) -> Option<u64> {
        self.data.sidecars.get(rel).copied()
    }

    pub fn set_sidecar_mtime(&mut self, rel: &str, mtime: u64) {
        self.data.sidecars.insert(rel.to_string(), mtime);
    }
}

/// Modification time in nanoseconds since 1970 (0 if unknown or earlier).
pub fn mtime_ns(meta: &std::fs::Metadata) -> u64 {
    meta.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

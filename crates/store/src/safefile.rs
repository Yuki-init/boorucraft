//! Reading and writing our JSON files safely.
//!
//! - [`write_atomic`]: write `X.tmp`, flush it to disk, copy the old `X` to `X.bak`, rename
//!   `X.tmp` over `X`. A crash leaves either the old or the new file, never half of one.
//! - [`Stamp`]: a hash of the bytes last loaded or saved, to notice when another program changed
//!   the file in between.
//! - [`disk_change`]: has the file changed since then? Size and modification time first, so the
//!   usual "no" costs one `stat`.
//! - [`Lock`]: `X.lock`, holding the process ID and host name, so two BooruCraft instances don't
//!   overwrite each other. Removed when dropped; a lock left by a crashed process on this machine
//!   is detected and replaced.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use crate::{Result, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp([u8; 32]);

impl Stamp {
    pub fn of(bytes: &[u8]) -> Stamp {
        Stamp(Sha256::digest(bytes).into())
    }
}

/// `path` with `suffix` added to its file name: `collection.json` → `collection.json.bak`.
pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// Reads a whole file of at most `max` bytes. `Ok(None)` if it doesn't exist.
pub fn read_capped(path: &Path, max: u64) -> Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StoreError::io(path, e)),
    };
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| StoreError::io(path, e))?;
    if bytes.len() as u64 > max {
        return Err(StoreError::TooLarge { path: path.to_path_buf(), max });
    }
    Ok(Some(bytes))
}

/// Fails with [`StoreError::ChangedOnDisk`] if the file no longer matches `expected` (`None`: it
/// didn't exist when loaded).
pub fn check_unchanged(path: &Path, expected: Option<Stamp>, max: u64) -> Result<()> {
    let now = read_capped(path, max).map(|b| b.map(|b| Stamp::of(&b)));
    match now {
        Ok(now) if now == expected => Ok(()),
        Ok(_) | Err(StoreError::TooLarge { .. }) => Err(StoreError::ChangedOnDisk { path: path.to_path_buf() }),
        Err(e) => Err(e),
    }
}

/// Size and modification time of a file: a cheap first test for "has it changed?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    len: u64,
    modified: Option<SystemTime>,
}

impl Meta {
    /// `None` if the file doesn't exist (or can't be read).
    pub fn of(path: &Path) -> Option<Meta> {
        let m = fs::metadata(path).ok()?;
        Some(Meta { len: m.len(), modified: m.modified().ok() })
    }
}

/// How a file on disk compares with the version last loaded or saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskChange {
    Unchanged,
    /// Different content (another program wrote it).
    Changed,
    /// It existed and is gone now.
    Deleted,
}

/// Compares `path` with the version last loaded or saved: `stamp` is its content (`None`: there
/// was no file) and `meta` its size and time. When only the time changed (touched, or rewritten
/// with the same bytes), `meta` is updated so the next check is a single `stat` again.
pub fn disk_change(path: &Path, stamp: Option<Stamp>, meta: &mut Option<Meta>, max: u64) -> Result<DiskChange> {
    let now = Meta::of(path);
    if now == *meta {
        return Ok(DiskChange::Unchanged);
    }
    let bytes = match read_capped(path, max) {
        Ok(b) => b,
        Err(StoreError::TooLarge { .. }) => return Ok(DiskChange::Changed),
        Err(e) => return Err(e),
    };
    match (bytes, stamp) {
        (None, None) => Ok(DiskChange::Unchanged),
        (None, Some(_)) => Ok(DiskChange::Deleted),
        (Some(b), Some(s)) if Stamp::of(&b) == s => {
            *meta = now;
            Ok(DiskChange::Unchanged)
        }
        (Some(_), _) => Ok(DiskChange::Changed),
    }
}

/// Replaces `path` with `bytes` atomically, keeping the previous version as `path.bak` if
/// `backup` is set.
pub fn write_atomic(path: &Path, bytes: &[u8], backup: bool) -> Result<Stamp> {
    let tmp = sibling(path, ".tmp");
    let result = (|| -> io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if backup && path.exists() {
            fs::copy(path, sibling(path, ".bak"))?;
        }
        fs::rename(&tmp, path)?;
        // Make the rename itself durable. Directories can't be opened for this everywhere;
        // failing here doesn't lose data, so it is ignored.
        if let Some(dir) = path.parent()
            && let Ok(d) = File::open(dir)
        {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(StoreError::io(path, e));
    }
    Ok(Stamp::of(bytes))
}

/// An exclusive claim on a file, held as `X.lock` next to it.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    /// Claims `target` (the file being protected, not the lock file).
    pub fn acquire(target: &Path) -> Result<Lock> {
        let path = sibling(target, ".lock");
        let me = format!("{} {}", std::process::id(), hostname());
        for attempt in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    f.write_all(me.as_bytes()).map_err(|e| StoreError::io(&path, e))?;
                    return Ok(Lock { path });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let holder = fs::read_to_string(&path).unwrap_or_default();
                    if attempt == 0 && is_stale(&holder) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return Err(StoreError::Locked { path: target.to_path_buf(), holder: describe_holder(&holder, &path) });
                }
                Err(e) => return Err(StoreError::io(&path, e)),
            }
        }
        Err(StoreError::Locked { path: target.to_path_buf(), holder: describe_holder("", &path) })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn describe_holder(holder: &str, lock: &Path) -> String {
    match holder.trim().split_once(' ') {
        Some((pid, host)) => format!("BooruCraft process {pid} on {host} (delete {} if that process is gone)", lock.display()),
        None => format!("another program (delete {} if nothing is using it)", lock.display()),
    }
}

/// A lock is stale if it was taken on this machine by a process that no longer runs.
fn is_stale(holder: &str) -> bool {
    let Some((pid, host)) = holder.trim().split_once(' ') else { return false };
    let Ok(pid) = pid.parse::<u32>() else { return false };
    host == hostname() && pid != std::process::id() && !process_alive(pid)
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Without a cheap check, assume the holder is alive; the user can delete the lock file.
#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    true
}

fn hostname() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|h| h.trim().replace(' ', "_"))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown-host".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_keeps_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        write_atomic(&p, b"one", true).unwrap();
        assert!(!sibling(&p, ".bak").exists());
        let stamp = write_atomic(&p, b"two", true).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read(sibling(&p, ".bak")).unwrap(), b"one");
        assert!(!sibling(&p, ".tmp").exists());
        check_unchanged(&p, Some(stamp), 100).unwrap();
        let q = dir.path().join("plain");
        write_atomic(&q, b"1", false).unwrap();
        write_atomic(&q, b"2", false).unwrap();
        assert!(!sibling(&q, ".bak").exists());
        fs::write(&p, b"changed").unwrap();
        assert!(matches!(check_unchanged(&p, Some(stamp), 100), Err(StoreError::ChangedOnDisk { .. })));
        assert!(matches!(check_unchanged(&dir.path().join("none"), None, 100), Ok(())));
    }

    #[test]
    fn notices_changes_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        let mut meta = None;
        assert_eq!(disk_change(&p, None, &mut meta, 100).unwrap(), DiskChange::Unchanged, "still no file");
        let stamp = write_atomic(&p, b"one", false).unwrap();
        let mut meta = Meta::of(&p);
        assert_eq!(disk_change(&p, Some(stamp), &mut meta, 100).unwrap(), DiskChange::Unchanged);
        // Same bytes written again: only the time differs, and the check learns the new time.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&p, b"one").unwrap();
        let mut stale = Some(Meta { len: 3, modified: None });
        assert_eq!(disk_change(&p, Some(stamp), &mut stale, 100).unwrap(), DiskChange::Unchanged);
        assert_eq!(stale, Meta::of(&p));
        fs::write(&p, b"two!").unwrap();
        assert_eq!(disk_change(&p, Some(stamp), &mut meta, 100).unwrap(), DiskChange::Changed);
        fs::write(&p, [b'x'; 200]).unwrap();
        assert_eq!(disk_change(&p, Some(stamp), &mut meta, 100).unwrap(), DiskChange::Changed, "too large counts as changed");
        fs::remove_file(&p).unwrap();
        assert_eq!(disk_change(&p, Some(stamp), &mut meta, 100).unwrap(), DiskChange::Deleted);
        fs::write(&p, b"new").unwrap();
        assert_eq!(disk_change(&p, None, &mut None, 100).unwrap(), DiskChange::Changed, "appeared");
    }

    #[test]
    fn read_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big");
        fs::write(&p, [0u8; 11]).unwrap();
        assert!(matches!(read_capped(&p, 10), Err(StoreError::TooLarge { .. })));
        assert_eq!(read_capped(&p, 11).unwrap().unwrap().len(), 11);
        assert!(read_capped(&dir.path().join("missing"), 10).unwrap().is_none());
    }

    #[test]
    fn lock_is_exclusive_and_released() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        let lock = Lock::acquire(&p).unwrap();
        assert!(matches!(Lock::acquire(&p), Err(StoreError::Locked { .. })));
        drop(lock);
        let _again = Lock::acquire(&p).unwrap();
    }

    #[test]
    fn stale_lock_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        // PIDs this high don't exist on Linux (pid_max is at most 2^22).
        fs::write(sibling(&p, ".lock"), format!("4294967 {}", hostname())).unwrap();
        if cfg!(target_os = "linux") {
            let _lock = Lock::acquire(&p).unwrap();
        }
        // A lock from another machine is never treated as stale.
        fs::write(sibling(&p, ".lock"), "1 some-other-host").unwrap();
        assert!(matches!(Lock::acquire(&p), Err(StoreError::Locked { .. })));
    }
}

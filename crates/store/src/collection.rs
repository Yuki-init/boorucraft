//! A collection: one folder of pictures and its `collection.json`.
//!
//! Files are keyed by their SHA-256 content hash, so tags follow a picture when it is moved or
//! renamed. Paths are relative to the folder and always use `/`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use boorucraft_core::{Rating, normalize_tag};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::safefile::{self, Lock, Stamp};
use crate::taglist::{HistoryOp, TagList};
use crate::{Result, StoreError};

pub const COLLECTION_FILE: &str = "collection.json";
pub const FORMAT: &str = "boorucraft-collection";
pub const VERSION: u32 = 1;
/// Biggest collection.json we read (roughly three million files).
pub const MAX_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Video,
    #[default]
    Other,
}

/// How XMP sidecars are named, or whether they are written at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidecarMode {
    /// `cat.png.xmp` (digiKam, darktable).
    #[default]
    FileExt,
    /// `cat.xmp` (Lightroom).
    Stem,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSettings {
    #[serde(default)]
    pub sidecars: SidecarMode,
    /// Read back sidecars that another app changed.
    #[serde(default = "yes")]
    pub read_back_sidecars: bool,
    /// File extensions to include (lowercase, no dot). Empty: the default list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Default for CollectionSettings {
    fn default() -> Self {
        CollectionSettings { sidecars: SidecarMode::default(), read_back_sidecars: true, extensions: Vec::new() }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub md5: String,
    /// Relative paths of every copy, `/`-separated.
    pub paths: BTreeSet<String>,
    #[serde(default)]
    pub kind: Kind,
    /// Lowercase format name, e.g. `png`, `mp4`.
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub animated: bool,
    /// When the picture was taken (EXIF), else the file's modification time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken: Option<DateTime<Utc>>,
    pub imported: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<Rating>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub tags: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhash: Option<String>,
    /// No copy was found on disk in the last scan. Kept so tags return if the file does.
    #[serde(default, skip_serializing_if = "is_false")]
    pub missing: bool,
    /// SHA-256 of the parent file (this one is an edit, crop or alternate version of it). The
    /// parent may be in another collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Pools this file is in, with its position in each (1, 2, 3…; gaps are fine).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pools: BTreeMap<String, u32>,
}

impl FileEntry {
    /// A fresh entry with no tags, imported now.
    pub fn new(md5: String, kind: Kind, format: String, size: u64) -> FileEntry {
        FileEntry {
            md5,
            paths: BTreeSet::new(),
            kind,
            format,
            size,
            width: None,
            height: None,
            animated: false,
            taken: None,
            imported: Utc::now(),
            rating: None,
            source: None,
            tags: BTreeSet::new(),
            phash: None,
            dhash: None,
            missing: false,
            parent: None,
            pools: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CollectionFile {
    format: String,
    version: u32,
    #[serde(default)]
    settings: CollectionSettings,
    /// The tag list revision this collection has caught up with.
    #[serde(default)]
    tags_revision: u64,
    #[serde(default)]
    files: BTreeMap<String, FileEntry>,
    #[serde(default)]
    not_duplicates: BTreeSet<[String; 2]>,
}

impl Default for CollectionFile {
    fn default() -> Self {
        CollectionFile {
            format: FORMAT.into(),
            version: VERSION,
            settings: CollectionSettings::default(),
            tags_revision: 0,
            files: BTreeMap::new(),
            not_duplicates: BTreeSet::new(),
        }
    }
}

#[derive(Debug)]
pub struct Collection {
    root: PathBuf,
    json: PathBuf,
    data: CollectionFile,
    stamp: Option<Stamp>,
    lock: Option<Lock>,
    dirty: bool,
}

impl Collection {
    /// Opens the collection in `root` for editing. A folder without `collection.json` opens as a
    /// new, empty collection; the file is written on the first save.
    pub fn open(root: &Path) -> Result<Collection> {
        let root = root.canonicalize().map_err(|e| StoreError::io(root, e))?;
        let json = root.join(COLLECTION_FILE);
        let lock = Lock::acquire(&json)?;
        let mut c = Self::load(root, json)?;
        c.lock = Some(lock);
        Ok(c)
    }

    /// Opens without locking; [`Collection::save`] then fails.
    pub fn open_read_only(root: &Path) -> Result<Collection> {
        let root = root.canonicalize().map_err(|e| StoreError::io(root, e))?;
        let json = root.join(COLLECTION_FILE);
        Self::load(root, json)
    }

    fn load(root: PathBuf, json: PathBuf) -> Result<Collection> {
        if !root.is_dir() {
            return Err(StoreError::Invalid(format!("{} is not a folder", root.display())));
        }
        let Some(bytes) = safefile::read_capped(&json, MAX_BYTES)? else {
            return Ok(Collection { root, json, data: CollectionFile::default(), stamp: None, lock: None, dirty: true });
        };
        let data: CollectionFile = serde_json::from_slice(&bytes).map_err(|source| StoreError::Json { path: json.clone(), source })?;
        if data.format != FORMAT {
            return Err(StoreError::WrongFormat { path: json, expected: "BooruCraft collection" });
        }
        if data.version > VERSION {
            return Err(StoreError::NewerVersion { path: json, version: data.version });
        }
        Ok(Collection { root, json, data, stamp: Some(Stamp::of(&bytes)), lock: None, dirty: false })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn json_path(&self) -> &Path {
        &self.json
    }

    /// No collection.json existed when it was opened.
    pub fn is_new(&self) -> bool {
        self.stamp.is_none()
    }

    pub fn is_read_only(&self) -> bool {
        self.lock.is_none()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn settings(&self) -> &CollectionSettings {
        &self.data.settings
    }

    pub fn settings_mut(&mut self) -> &mut CollectionSettings {
        self.dirty = true;
        &mut self.data.settings
    }

    pub fn len(&self) -> usize {
        self.data.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.files.is_empty()
    }

    /// Files in hash order.
    pub fn files(&self) -> impl Iterator<Item = (&str, &FileEntry)> {
        self.data.files.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn get(&self, hash: &str) -> Option<&FileEntry> {
        self.data.files.get(hash)
    }

    pub fn get_mut(&mut self, hash: &str) -> Option<&mut FileEntry> {
        let entry = self.data.files.get_mut(hash);
        if entry.is_some() {
            self.dirty = true;
        }
        entry
    }

    pub fn insert(&mut self, hash: String, entry: FileEntry) {
        self.data.files.insert(hash, entry);
        self.dirty = true;
    }

    pub fn remove(&mut self, hash: &str) -> Option<FileEntry> {
        let removed = self.data.files.remove(hash);
        if removed.is_some() {
            self.dirty = true;
        }
        removed
    }

    /// The hash of the file at a relative path.
    pub fn hash_of_path(&self, rel: &str) -> Option<&str> {
        self.data.files.iter().find(|(_, e)| e.paths.contains(rel)).map(|(k, _)| k.as_str())
    }

    /// A map from every relative path to its file's hash.
    pub fn path_map(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        for (hash, entry) in &self.data.files {
            for p in &entry.paths {
                map.insert(p.clone(), hash.clone());
            }
        }
        map
    }

    pub fn tags_revision(&self) -> u64 {
        self.data.tags_revision
    }

    pub fn is_not_duplicate(&self, a: &str, b: &str) -> bool {
        let pair = if a <= b { [a.to_string(), b.to_string()] } else { [b.to_string(), a.to_string()] };
        self.data.not_duplicates.contains(&pair)
    }

    /// Records that two files aren't duplicates, so they are never grouped again.
    pub fn mark_not_duplicate(&mut self, a: &str, b: &str) {
        if a == b {
            return;
        }
        let pair = if a <= b { [a.to_string(), b.to_string()] } else { [b.to_string(), a.to_string()] };
        self.dirty |= self.data.not_duplicates.insert(pair);
    }

    /// `path` relative to the root with `/` separators, if it is inside the collection.
    pub fn rel_path(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let parts: Option<Vec<&str>> = rel
            .components()
            .map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();
        let parts = parts?;
        if parts.is_empty() { None } else { Some(parts.join("/")) }
    }

    /// The absolute path for a stored relative path. `None` for paths that would leave the
    /// folder (`..`, absolute paths): collection.json may have been edited by hand.
    pub fn abs_path(&self, rel: &str) -> Option<PathBuf> {
        let mut out = self.root.clone();
        for part in rel.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains('\\') || Path::new(part).is_absolute() || part.contains(':') && cfg!(windows) {
                return None;
            }
            out.push(part);
        }
        Some(out)
    }

    /// Catches up with renames, merges and deletes made in the tag list since this collection
    /// was last synced. Returns the hashes of files whose tags changed.
    pub fn apply_tag_history(&mut self, tags: &TagList) -> BTreeSet<String> {
        let mut changed = BTreeSet::new();
        if tags.revision() <= self.data.tags_revision {
            return changed;
        }
        let ops: Vec<HistoryOp> = tags.history_since(self.data.tags_revision).map(|h| h.op.clone()).collect();
        for (hash, entry) in &mut self.data.files {
            let before = entry.tags.clone();
            for op in &ops {
                match op {
                    HistoryOp::Rename { from, to } => {
                        if entry.tags.remove(from) {
                            entry.tags.insert(to.clone());
                        }
                    }
                    HistoryOp::Merge { from, into } => {
                        let mut hit = false;
                        for f in from {
                            hit |= entry.tags.remove(f);
                        }
                        if hit {
                            entry.tags.insert(into.clone());
                        }
                    }
                    HistoryOp::Delete { tag } => {
                        entry.tags.remove(tag);
                    }
                    HistoryOp::RenamePool { from, to } => {
                        if let Some(pos) = entry.pools.remove(from) {
                            entry.pools.insert(to.clone(), pos);
                        }
                    }
                    HistoryOp::DeletePool { pool } => {
                        entry.pools.remove(pool);
                    }
                }
            }
            // Only tag changes touch sidecars; pools live in collection.json alone.
            if entry.tags != before {
                changed.insert(hash.clone());
            }
        }
        self.data.tags_revision = tags.revision();
        self.dirty = true;
        changed
    }

    /// Adds and removes tags on one file. Added tags must be in the tag list; removed ones
    /// needn't be (so unknown tags can be cleaned up). Returns whether anything changed.
    pub fn edit_tags(&mut self, hash: &str, add: &[&str], remove: &[&str], tags: &TagList) -> Result<bool> {
        let add: Vec<String> = add.iter().map(|t| tags.resolve(t)).collect::<Result<_>>()?;
        let remove: Vec<String> = remove.iter().map(|t| normalize_tag(t)).collect::<Result<_, _>>()?;
        let entry = self.data.files.get_mut(hash).ok_or_else(|| StoreError::Invalid(format!("no file with hash {hash} in this collection")))?;
        let before = entry.tags.clone();
        for t in remove {
            entry.tags.remove(&t);
        }
        entry.tags.extend(add);
        let changed = entry.tags != before;
        self.dirty |= changed;
        Ok(changed)
    }

    /// Tags used here that aren't in the tag list, with how many files have each.
    pub fn unknown_tags(&self, tags: &TagList) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for entry in self.data.files.values() {
            for t in &entry.tags {
                if !tags.contains(t) {
                    *out.entry(t.clone()).or_insert(0) += 1;
                }
            }
        }
        out
    }

    /// Saves if anything changed. Fails if read-only or if collection.json was changed by
    /// another program since it was loaded.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.lock.is_none() {
            return Err(StoreError::ReadOnly(self.json.clone()));
        }
        safefile::check_unchanged(&self.json, self.stamp, MAX_BYTES)?;
        let mut bytes = serde_json::to_vec_pretty(&self.data).map_err(|source| StoreError::Json { path: self.json.clone(), source })?;
        bytes.push(b'\n');
        self.stamp = Some(safefile::write_atomic(&self.json, &bytes, true)?);
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, tags: &[&str]) -> FileEntry {
        let mut e = FileEntry::new("md5".into(), Kind::Image, "png".into(), 10);
        e.paths.insert(path.into());
        e.tags = tags.iter().map(|t| t.to_string()).collect();
        e
    }

    #[test]
    fn saves_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Collection::open(dir.path()).unwrap();
        assert!(c.is_new());
        c.insert("aa".into(), entry("2024/cat.png", &["cat"]));
        c.save().unwrap();
        assert!(matches!(Collection::open(dir.path()), Err(StoreError::Locked { .. })));
        drop(c);
        let c = Collection::open_read_only(dir.path()).unwrap();
        assert_eq!(c.hash_of_path("2024/cat.png"), Some("aa"));
        assert_eq!(c.get("aa").unwrap().tags.len(), 1);
        let text = std::fs::read_to_string(dir.path().join(COLLECTION_FILE)).unwrap();
        assert!(text.contains("\"format\": \"boorucraft-collection\""), "{text}");
        assert!(!text.contains("missing"), "false flags are left out");
    }

    #[test]
    fn edits_tags_from_the_list_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut tags = TagList::in_memory();
        tags.create("cat", "").unwrap();
        let mut c = Collection::open(dir.path()).unwrap();
        c.insert("aa".into(), entry("a.png", &["old"]));
        assert!(c.edit_tags("aa", &["Cat"], &["old"], &tags).unwrap());
        assert!(!c.edit_tags("aa", &["cat"], &[], &tags).unwrap());
        assert!(matches!(c.edit_tags("aa", &["dog"], &[], &tags), Err(StoreError::UnknownTag(_))));
        assert_eq!(c.get("aa").unwrap().tags, ["cat".to_string()].into());
    }

    #[test]
    fn catches_up_with_the_tag_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut tags = TagList::in_memory();
        for t in ["kitty", "feline", "cat", "junk", "sky"] {
            tags.create(t, "").unwrap();
        }
        let mut c = Collection::open(dir.path()).unwrap();
        c.insert("a".into(), entry("a.png", &["kitty", "sky"]));
        c.insert("b".into(), entry("b.png", &["feline", "junk"]));
        c.insert("c".into(), entry("c.png", &["sky"]));
        c.apply_tag_history(&tags);
        tags.rename("kitty", "puss").unwrap();
        tags.merge(&["puss", "feline"], "cat").unwrap();
        tags.delete("junk").unwrap();
        let changed = c.apply_tag_history(&tags);
        assert_eq!(changed, ["a".to_string(), "b".to_string()].into());
        assert_eq!(c.get("a").unwrap().tags, ["cat".to_string(), "sky".to_string()].into());
        assert_eq!(c.get("b").unwrap().tags, ["cat".to_string()].into());
        assert_eq!(c.tags_revision(), 3);
        assert!(c.apply_tag_history(&tags).is_empty());
        assert!(c.unknown_tags(&tags).is_empty());
    }

    #[test]
    fn paths_stay_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let c = Collection::open_read_only(dir.path()).unwrap();
        let root = c.root().to_path_buf();
        assert_eq!(c.abs_path("a/b.png"), Some(root.join("a").join("b.png")));
        assert_eq!(c.abs_path("../etc/passwd"), None);
        assert_eq!(c.abs_path("/etc/passwd"), None);
        assert_eq!(c.abs_path("a//b"), None);
        assert_eq!(c.rel_path(&root.join("x").join("y.png")).as_deref(), Some("x/y.png"));
        assert_eq!(c.rel_path(Path::new("/elsewhere/y.png")), None);
    }

    #[test]
    fn read_only_cannot_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Collection::open_read_only(dir.path()).unwrap();
        c.insert("a".into(), entry("a.png", &[]));
        assert!(matches!(c.save(), Err(StoreError::ReadOnly(_))));
    }
}

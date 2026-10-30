//! The central tag list: the one set of valid tags and namespaces shared by every collection.
//!
//! Renames, merges and deletes bump `revision` and are recorded in `history`, so a collection
//! that wasn't open at the time catches up when it next opens
//! ([`crate::Collection::apply_tag_history`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use boorucraft_core::{is_valid_namespace, normalize_tag, split_namespace};
use serde::{Deserialize, Serialize};

use crate::safefile::{self, DiskChange, Lock, Meta, Stamp};
use crate::{Result, StoreError};

pub const FORMAT: &str = "boorucraft-tags";
pub const VERSION: u32 = 1;
/// Biggest tags.json we read.
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespaceInfo {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagInfo {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolInfo {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum HistoryOp {
    Rename { from: String, to: String },
    Merge { from: Vec<String>, into: String },
    Delete { tag: String },
    RenamePool { from: String, to: String },
    DeletePool { pool: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub revision: u64,
    #[serde(flatten)]
    pub op: HistoryOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TagListFile {
    format: String,
    version: u32,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    namespaces: BTreeMap<String, NamespaceInfo>,
    #[serde(default)]
    tags: BTreeMap<String, TagInfo>,
    /// Pools: named, ordered groups of files (positions live in each collection).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pools: BTreeMap<String, PoolInfo>,
    #[serde(default)]
    history: Vec<HistoryEntry>,
}

impl Default for TagListFile {
    fn default() -> Self {
        let ns = |c: &str| NamespaceInfo { color: c.to_string() };
        TagListFile {
            format: FORMAT.into(),
            version: VERSION,
            revision: 0,
            namespaces: [("artist", ns("#d04040")), ("character", ns("#40a040")), ("series", ns("#a040c0")), ("meta", ns("#e09030"))]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            tags: BTreeMap::new(),
            pools: BTreeMap::new(),
            history: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct TagList {
    path: PathBuf,
    data: TagListFile,
    /// What was on disk at load or last save (`None`: no file yet).
    stamp: Option<Stamp>,
    /// Size and time of the file when loaded or saved, see [`TagList::disk_change`].
    meta: Option<Meta>,
    /// `None` when opened read-only.
    lock: Option<Lock>,
    dirty: bool,
}

impl TagList {
    /// Opens the tag list for editing, creating a new one (with the default namespaces) if the
    /// file doesn't exist yet. Fails if another BooruCraft has it open.
    pub fn open(path: &Path) -> Result<TagList> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        }
        let lock = Lock::acquire(path)?;
        let mut list = Self::load(path)?;
        list.lock = Some(lock);
        Ok(list)
    }

    /// Opens the tag list without locking it; [`TagList::save`] then fails.
    pub fn open_read_only(path: &Path) -> Result<TagList> {
        Self::load(path)
    }

    fn load(path: &Path) -> Result<TagList> {
        let Some(bytes) = safefile::read_capped(path, MAX_BYTES)? else {
            return Ok(TagList { path: path.to_path_buf(), data: TagListFile::default(), stamp: None, meta: None, lock: None, dirty: true });
        };
        let data: TagListFile = serde_json::from_slice(&bytes).map_err(|source| StoreError::Json { path: path.to_path_buf(), source })?;
        if data.format != FORMAT {
            return Err(StoreError::WrongFormat { path: path.to_path_buf(), expected: "BooruCraft tag list" });
        }
        if data.version > VERSION {
            return Err(StoreError::NewerVersion { path: path.to_path_buf(), version: data.version });
        }
        Ok(TagList { path: path.to_path_buf(), data, stamp: Some(Stamp::of(&bytes)), meta: Meta::of(path), lock: None, dirty: false })
    }

    /// An in-memory list that is never saved, for tests and previews.
    pub fn in_memory() -> TagList {
        TagList { path: PathBuf::new(), data: TagListFile::default(), stamp: None, meta: None, lock: None, dirty: false }
    }

    /// A read-only copy, e.g. for a background scan. Saving it fails.
    pub fn snapshot(&self) -> TagList {
        TagList { path: self.path.clone(), data: self.data.clone(), stamp: self.stamp, meta: self.meta, lock: None, dirty: false }
    }

    /// Opened without the lock (another BooruCraft had it): changes can't be saved.
    pub fn is_read_only(&self) -> bool {
        self.lock.is_none()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn revision(&self) -> u64 {
        self.data.revision
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn len(&self) -> usize {
        self.data.tags.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.tags.is_empty()
    }

    pub fn contains(&self, tag: &str) -> bool {
        self.data.tags.contains_key(tag)
    }

    pub fn is_namespace(&self, name: &str) -> bool {
        self.data.namespaces.contains_key(name)
    }

    pub fn tags(&self) -> impl Iterator<Item = (&str, &TagInfo)> {
        self.data.tags.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn namespaces(&self) -> impl Iterator<Item = (&str, &NamespaceInfo)> {
        self.data.namespaces.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// The namespace of a tag, if its prefix is a known namespace.
    pub fn namespace_of<'a>(&self, tag: &'a str) -> Option<&'a str> {
        split_namespace(tag, |n| self.is_namespace(n)).0
    }

    /// Normalises `input` and checks that it is in the list.
    pub fn resolve(&self, input: &str) -> Result<String> {
        let tag = normalize_tag(input)?;
        if self.contains(&tag) { Ok(tag) } else { Err(StoreError::UnknownTag(tag)) }
    }

    /// Existing tags that start with, then contain, `input`; for "did you mean" and autocomplete.
    pub fn suggest(&self, input: &str, limit: usize) -> Vec<&str> {
        let needle = input.trim().to_lowercase().replace(char::is_whitespace, "_");
        if needle.is_empty() {
            return Vec::new();
        }
        let starts = self.data.tags.range(needle.clone()..).map(|(k, _)| k.as_str()).take_while(|k| k.starts_with(&needle));
        let mut out: Vec<&str> = starts.take(limit).collect();
        if out.len() < limit {
            for k in self.data.tags.keys() {
                if out.len() >= limit {
                    break;
                }
                let name = split_namespace(k, |n| self.is_namespace(n)).1;
                if (k.contains(&needle) || name.starts_with(&needle)) && !out.contains(&k.as_str()) {
                    out.push(k);
                }
            }
        }
        if out.len() < limit {
            // Typos: tags within a small edit distance, closest first.
            let max = if needle.chars().count() <= 4 { 1 } else { 2 };
            let mut close: Vec<(usize, &str)> = self
                .data
                .tags
                .keys()
                .filter_map(|k| {
                    let name = split_namespace(k, |n| self.is_namespace(n)).1;
                    let d = edit_distance(&needle, k, max).min(edit_distance(&needle, name, max));
                    (d <= max).then_some((d, k.as_str()))
                })
                .filter(|(_, k)| !out.contains(k))
                .collect();
            close.sort();
            out.extend(close.into_iter().map(|(_, k)| k).take(limit - out.len()));
        }
        out
    }

    /// Adds a tag; returns its normalised name.
    pub fn create(&mut self, input: &str, description: &str) -> Result<String> {
        let tag = normalize_tag(input)?;
        if self.contains(&tag) {
            return Err(StoreError::TagExists(tag));
        }
        self.data.tags.insert(tag.clone(), TagInfo { description: description.trim().to_string() });
        self.dirty = true;
        Ok(tag)
    }

    pub fn set_description(&mut self, tag: &str, description: &str) -> Result<()> {
        let info = self.data.tags.get_mut(tag).ok_or_else(|| StoreError::UnknownTag(tag.to_string()))?;
        info.description = description.trim().to_string();
        self.dirty = true;
        Ok(())
    }

    pub fn add_namespace(&mut self, name: &str, color: &str) -> Result<()> {
        let name = name.trim().to_lowercase();
        if !is_valid_namespace(&name) {
            return Err(StoreError::BadNamespace(name));
        }
        if self.is_namespace(&name) {
            return Err(StoreError::NamespaceExists(name));
        }
        self.data.namespaces.insert(name, NamespaceInfo { color: color.trim().to_string() });
        self.dirty = true;
        Ok(())
    }

    pub fn set_namespace_color(&mut self, name: &str, color: &str) -> Result<()> {
        let info = self.data.namespaces.get_mut(name).ok_or_else(|| StoreError::Invalid(format!("no namespace `{name}`")))?;
        info.color = color.trim().to_string();
        self.dirty = true;
        Ok(())
    }

    /// Stops treating `name:` as a namespace. Tags keep their names (`artist:foo` stays) and just
    /// lose the grouping and colour, so nothing in the collections changes.
    pub fn remove_namespace(&mut self, name: &str) -> Result<()> {
        self.data.namespaces.remove(name).ok_or_else(|| StoreError::Invalid(format!("no namespace `{name}`")))?;
        self.dirty = true;
        Ok(())
    }

    fn record(&mut self, op: HistoryOp) {
        self.data.revision = self.data.revision.saturating_add(1);
        self.data.history.push(HistoryEntry { revision: self.data.revision, op });
        self.dirty = true;
    }

    /// Renames `from` to `to` (which must not exist yet) everywhere.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<String> {
        let from = normalize_tag(from)?;
        let to = normalize_tag(to)?;
        if self.contains(&to) {
            return Err(StoreError::TagExists(to));
        }
        let info = self.data.tags.remove(&from).ok_or_else(|| StoreError::UnknownTag(from.clone()))?;
        self.data.tags.insert(to.clone(), info);
        self.record(HistoryOp::Rename { from, to: to.clone() });
        Ok(to)
    }

    /// Folds `from` tags into `into`: files with any of them get `into` instead.
    pub fn merge(&mut self, from: &[&str], into: &str) -> Result<()> {
        let into = normalize_tag(into)?;
        if !self.contains(&into) {
            return Err(StoreError::UnknownTag(into));
        }
        let mut froms = Vec::new();
        for f in from {
            let f = normalize_tag(f)?;
            if f == into {
                return Err(StoreError::Invalid(format!("can't merge `{f}` into itself")));
            }
            if !self.contains(&f) {
                return Err(StoreError::UnknownTag(f));
            }
            if !froms.contains(&f) {
                froms.push(f);
            }
        }
        if froms.is_empty() {
            return Err(StoreError::Invalid("nothing to merge".into()));
        }
        for f in &froms {
            self.data.tags.remove(f);
        }
        self.record(HistoryOp::Merge { from: froms, into });
        Ok(())
    }

    /// Removes a tag from the list and, as collections catch up, from every file.
    pub fn delete(&mut self, tag: &str) -> Result<()> {
        let tag = normalize_tag(tag)?;
        if self.data.tags.remove(&tag).is_none() {
            return Err(StoreError::UnknownTag(tag));
        }
        self.record(HistoryOp::Delete { tag });
        Ok(())
    }

    pub fn pools(&self) -> impl Iterator<Item = (&str, &PoolInfo)> {
        self.data.pools.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn has_pool(&self, name: &str) -> bool {
        self.data.pools.contains_key(name)
    }

    /// A pool name in its stored form (like a tag: lowercase, `_` for spaces), refusing the
    /// words `pool:` searches use.
    pub fn normalize_pool(input: &str) -> Result<String> {
        let name = normalize_tag(input)?;
        if matches!(name.as_str(), "any" | "none") || name.contains('*') {
            return Err(StoreError::Invalid(format!("`{name}` can't be a pool name")));
        }
        Ok(name)
    }

    /// Normalises `input` and checks that the pool exists.
    pub fn resolve_pool(&self, input: &str) -> Result<String> {
        let name = Self::normalize_pool(input)?;
        if self.has_pool(&name) { Ok(name) } else { Err(StoreError::UnknownPool(name)) }
    }

    /// Pools starting with, then containing, `input`.
    pub fn suggest_pools(&self, input: &str, limit: usize) -> Vec<&str> {
        let needle = input.trim().to_lowercase().replace(char::is_whitespace, "_");
        let mut out: Vec<&str> = self.data.pools.keys().map(String::as_str).filter(|p| p.starts_with(&needle)).take(limit).collect();
        for p in self.data.pools.keys() {
            if out.len() >= limit {
                break;
            }
            if p.contains(&needle) && !out.contains(&p.as_str()) {
                out.push(p);
            }
        }
        out
    }

    pub fn create_pool(&mut self, input: &str, description: &str) -> Result<String> {
        let name = Self::normalize_pool(input)?;
        if self.has_pool(&name) {
            return Err(StoreError::PoolExists(name));
        }
        self.data.pools.insert(name.clone(), PoolInfo { description: description.trim().to_string() });
        self.dirty = true;
        Ok(name)
    }

    pub fn rename_pool(&mut self, from: &str, to: &str) -> Result<String> {
        let from = Self::normalize_pool(from)?;
        let to = Self::normalize_pool(to)?;
        if self.has_pool(&to) {
            return Err(StoreError::PoolExists(to));
        }
        let info = self.data.pools.remove(&from).ok_or_else(|| StoreError::UnknownPool(from.clone()))?;
        self.data.pools.insert(to.clone(), info);
        self.record(HistoryOp::RenamePool { from, to: to.clone() });
        Ok(to)
    }

    /// Removes a pool (the files stay; they just leave the pool).
    pub fn delete_pool(&mut self, name: &str) -> Result<()> {
        let pool = Self::normalize_pool(name)?;
        if self.data.pools.remove(&pool).is_none() {
            return Err(StoreError::UnknownPool(pool));
        }
        self.record(HistoryOp::DeletePool { pool });
        Ok(())
    }

    /// Changes made after `revision`, oldest first.
    pub fn history_since(&self, revision: u64) -> impl Iterator<Item = &HistoryEntry> {
        self.data.history.iter().filter(move |h| h.revision > revision)
    }

    /// Saves if anything changed. Fails if the list is read-only or was changed on disk by
    /// someone else since it was loaded.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.lock.is_none() {
            return Err(StoreError::ReadOnly(self.path.clone()));
        }
        safefile::check_unchanged(&self.path, self.stamp, MAX_BYTES)?;
        let mut bytes = serde_json::to_vec_pretty(&self.data).map_err(|source| StoreError::Json { path: self.path.clone(), source })?;
        bytes.push(b'\n');
        self.stamp = Some(safefile::write_atomic(&self.path, &bytes, true)?);
        self.meta = Meta::of(&self.path);
        self.dirty = false;
        Ok(())
    }

    /// Has another program changed or deleted the file since it was loaded or saved?
    pub fn disk_change(&mut self) -> Result<DiskChange> {
        if self.path.as_os_str().is_empty() {
            return Ok(DiskChange::Unchanged);
        }
        safefile::disk_change(&self.path, self.stamp, &mut self.meta, MAX_BYTES)
    }

    /// Replaces the list in memory with the file as it is on disk now (unsaved changes are
    /// dropped); the lock is kept. Fails, changing nothing, if it can't be read or parsed.
    pub fn reload(&mut self) -> Result<()> {
        let fresh = Self::load(&self.path)?;
        if fresh.stamp.is_none() {
            return Err(StoreError::Invalid(format!("{} no longer exists", self.path.display())));
        }
        self.data = fresh.data;
        self.stamp = fresh.stamp;
        self.meta = fresh.meta;
        self.dirty = false;
        Ok(())
    }

    /// Keeps the list in memory: the next save replaces the file even though another program
    /// changed or deleted it (its version stays in the `.bak`).
    pub fn overwrite_disk_version(&mut self) -> Result<()> {
        self.stamp = safefile::read_capped(&self.path, MAX_BYTES)?.map(|b| Stamp::of(&b));
        self.meta = Meta::of(&self.path);
        self.dirty = true;
        Ok(())
    }
}

/// Levenshtein distance, giving up (returning `max + 1`) once it must exceed `max`.
fn edit_distance(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        let mut row_min = cur[0];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
            row_min = row_min.min(cur[j + 1]);
        }
        if row_min > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_resolve_and_suggest() {
        let mut t = TagList::in_memory();
        assert_eq!(t.create(" Blue Sky ", "").unwrap(), "blue_sky");
        t.create("artist:Foo", "").unwrap();
        t.create("sky", "").unwrap();
        assert!(matches!(t.create("SKY", ""), Err(StoreError::TagExists(_))));
        assert_eq!(t.resolve("Blue sky").unwrap(), "blue_sky");
        assert!(matches!(t.resolve("cloud"), Err(StoreError::UnknownTag(_))));
        assert_eq!(t.suggest("sk", 5), vec!["sky", "blue_sky"]);
        assert_eq!(t.suggest("fo", 5), vec!["artist:foo"]);
        assert_eq!(t.suggest("skt", 5), vec!["sky"]);
        assert_eq!(t.suggest("blue_skyy", 5), vec!["blue_sky"]);
        assert_eq!(t.suggest("fooo", 5), vec!["artist:foo"]);
        assert!(t.suggest("zzzzzz", 5).is_empty());
        assert_eq!(t.namespace_of("artist:foo"), Some("artist"));
        assert_eq!(t.namespace_of("re:zero"), None);
    }

    #[test]
    fn edit_distances() {
        assert_eq!(edit_distance("cat", "cat", 2), 0);
        assert_eq!(edit_distance("catt", "cat", 2), 1);
        assert_eq!(edit_distance("kitten", "sitting", 3), 3);
        assert_eq!(edit_distance("kitten", "sitting", 2), 3);
        assert_eq!(edit_distance("", "abc", 5), 3);
        assert_eq!(edit_distance("école", "ecole", 2), 1);
    }

    #[test]
    fn rename_merge_delete_are_recorded() {
        let mut t = TagList::in_memory();
        for n in ["kitty", "cat", "feline", "kitten", "junk"] {
            t.create(n, "").unwrap();
        }
        t.rename("kitty", "puss").unwrap();
        assert!(matches!(t.rename("puss", "cat"), Err(StoreError::TagExists(_))));
        t.merge(&["feline", "puss", "feline"], "cat").unwrap();
        assert!(t.merge(&["cat"], "cat").is_err());
        t.delete("junk").unwrap();
        assert!(t.delete("junk").is_err());
        assert_eq!(t.revision(), 3);
        let ops: Vec<_> = t.history_since(1).map(|h| h.op.clone()).collect();
        assert_eq!(ops, vec![HistoryOp::Merge { from: vec!["feline".into(), "puss".into()], into: "cat".into() }, HistoryOp::Delete { tag: "junk".into() }]);
        let names: Vec<_> = t.tags().map(|(k, _)| k).collect();
        assert_eq!(names, vec!["cat", "kitten"]);
    }

    #[test]
    fn pools() {
        let mut t = TagList::in_memory();
        assert_eq!(t.create_pool("My Comic", "").unwrap(), "my_comic");
        assert!(matches!(t.create_pool("my comic", ""), Err(StoreError::PoolExists(_))));
        assert!(t.create_pool("none", "").is_err() && t.create_pool("a*", "").is_err());
        assert_eq!(t.suggest_pools("com", 5), vec!["my_comic"]);
        assert_eq!(t.rename_pool("my_comic", "trip").unwrap(), "trip");
        t.delete_pool("trip").unwrap();
        let ops: Vec<_> = t.history_since(0).map(|h| h.op.clone()).collect();
        assert_eq!(ops, vec![HistoryOp::RenamePool { from: "my_comic".into(), to: "trip".into() }, HistoryOp::DeletePool { pool: "trip".into() }]);
        assert!(matches!(t.resolve_pool("trip"), Err(StoreError::UnknownPool(_))));
        let json = serde_json::to_string(&t.history_since(0).next().unwrap()).unwrap();
        assert!(json.contains("\"op\":\"rename-pool\""), "{json}");
    }

    #[test]
    fn namespaces() {
        let mut t = TagList::in_memory();
        assert!(t.is_namespace("artist"));
        t.add_namespace("Place", "#123456").unwrap();
        assert!(t.is_namespace("place"));
        assert!(matches!(t.add_namespace("rating", ""), Err(StoreError::BadNamespace(_))));
        assert!(matches!(t.add_namespace("place", ""), Err(StoreError::NamespaceExists(_))));
    }

    #[test]
    fn saves_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cfg/tags.json");
        let mut t = TagList::open(&p).unwrap();
        t.create("cat", "Cats").unwrap();
        t.rename("cat", "neko").unwrap();
        t.save().unwrap();
        assert!(matches!(TagList::open(&p), Err(StoreError::Locked { .. })));
        drop(t);
        let t = TagList::open_read_only(&p).unwrap();
        assert!(t.contains("neko"));
        assert_eq!(t.revision(), 1);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\"op\": \"rename\""), "{text}");
    }

    #[test]
    fn refuses_to_overwrite_outside_changes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tags.json");
        let mut t = TagList::open(&p).unwrap();
        t.save().unwrap();
        std::fs::write(&p, "{\"format\":\"boorucraft-tags\",\"version\":1}").unwrap();
        t.create("cat", "").unwrap();
        assert!(matches!(t.save(), Err(StoreError::ChangedOnDisk { .. })));
        assert_eq!(t.disk_change().unwrap(), DiskChange::Changed);

        // Keep mine: the next save replaces their version, which stays in the .bak.
        t.overwrite_disk_version().unwrap();
        t.save().unwrap();
        assert!(t.contains("cat"));
        assert_eq!(t.disk_change().unwrap(), DiskChange::Unchanged);
        assert!(std::fs::read_to_string(dir.path().join("tags.json.bak")).unwrap().contains("\"version\":1}"));

        // Load theirs: unsaved changes are dropped.
        std::fs::write(&p, "{\"format\":\"boorucraft-tags\",\"version\":1,\"tags\":{\"dog\":{}}}").unwrap();
        t.create("bird", "").unwrap();
        t.reload().unwrap();
        assert!(t.contains("dog") && !t.contains("cat") && !t.contains("bird") && !t.is_dirty());
        assert_eq!(t.disk_change().unwrap(), DiskChange::Unchanged);
        // A damaged file is not loaded; the list in memory stays.
        std::fs::write(&p, "{ not json").unwrap();
        assert!(t.reload().is_err());
        assert!(t.contains("dog"));
        std::fs::remove_file(&p).unwrap();
        assert_eq!(t.disk_change().unwrap(), DiskChange::Deleted);
        assert!(t.reload().is_err());
    }

    #[test]
    fn rejects_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tags.json");
        std::fs::write(&p, "{\"format\":\"something\",\"version\":1}").unwrap();
        assert!(matches!(TagList::open_read_only(&p), Err(StoreError::WrongFormat { .. })));
        std::fs::write(&p, "{\"format\":\"boorucraft-tags\",\"version\":99}").unwrap();
        assert!(matches!(TagList::open_read_only(&p), Err(StoreError::NewerVersion { .. })));
        std::fs::write(&p, "not json").unwrap();
        assert!(matches!(TagList::open_read_only(&p), Err(StoreError::Json { .. })));
    }
}

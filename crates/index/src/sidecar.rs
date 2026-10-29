//! Keeping XMP sidecars in step with collection.json.
//!
//! collection.json is the source of truth. Sidecars are written after every change, and read
//! back during scans when their modification time shows another app touched them:
//!
//! - a sidecar BooruCraft wrote before (it has `boorucraft:Hash`) and someone else changed:
//!   its tags replace the file's tags;
//! - a sidecar BooruCraft never wrote (from digiKam, Lightroom…): its tags are added, nothing is
//!   removed.

use std::collections::BTreeSet;
use std::path::PathBuf;

use boorucraft_core::normalize_tag;
use boorucraft_store::{Collection, FileEntry, SidecarMode, TagList, safefile};
use boorucraft_xmp::{MAX_SIDECAR_BYTES, SidecarData};

use crate::cache::{ScanCache, mtime_ns};

#[derive(Debug, Default)]
pub struct SidecarReport {
    pub read: usize,
    pub written: usize,
    pub errors: Vec<(PathBuf, String)>,
}

/// The sidecar's relative path for a file's relative path, or `None` if sidecars are off.
pub fn sidecar_rel(rel: &str, mode: SidecarMode) -> Option<String> {
    match mode {
        SidecarMode::Off => None,
        SidecarMode::FileExt => Some(format!("{rel}.xmp")),
        SidecarMode::Stem => {
            let (dir, name) = rel.rsplit_once('/').map_or(("", rel), |(d, n)| (d, n));
            let stem = match name.rsplit_once('.') {
                Some((s, _)) if !s.is_empty() => s,
                _ => name,
            };
            Some(if dir.is_empty() { format!("{stem}.xmp") } else { format!("{dir}/{stem}.xmp") })
        }
    }
}

fn data_for(hash: &str, entry: &FileEntry) -> SidecarData {
    SidecarData { tags: entry.tags.clone(), rating: entry.rating, source: entry.source.clone(), hash: Some(hash.to_string()) }
}

fn has_content(entry: &FileEntry) -> bool {
    !entry.tags.is_empty() || entry.rating.is_some() || entry.source.is_some()
}

/// Writes the sidecars of the given files (every copy of each). A sidecar that can't be parsed
/// is left untouched and reported.
pub fn write_sidecars<'a>(c: &Collection, tags: &TagList, hashes: impl IntoIterator<Item = &'a str>, cache: &mut ScanCache, report: &mut SidecarReport) {
    let mode = c.settings().sidecars;
    let is_ns = |n: &str| tags.is_namespace(n);
    for hash in hashes {
        let Some(entry) = c.get(hash) else { continue };
        if entry.missing {
            continue;
        }
        let data = data_for(hash, entry);
        for rel in &entry.paths {
            let Some(srel) = sidecar_rel(rel, mode) else { continue };
            let Some(abs) = c.abs_path(&srel) else { continue };
            let existing = match safefile::read_capped(&abs, MAX_SIDECAR_BYTES as u64) {
                Ok(e) => e,
                Err(e) => {
                    report.errors.push((abs, e.to_string()));
                    continue;
                }
            };
            if existing.is_none() && !has_content(entry) {
                continue;
            }
            let new = match boorucraft_xmp::update(existing.as_deref(), &data, &is_ns) {
                Ok(n) => n,
                Err(e) => {
                    report.errors.push((abs, format!("{e}; left unchanged")));
                    continue;
                }
            };
            if existing.as_deref() != Some(new.as_slice()) {
                if let Err(e) = safefile::write_atomic(&abs, &new, false) {
                    report.errors.push((abs, e.to_string()));
                    continue;
                }
                report.written += 1;
            }
            if let Ok(meta) = std::fs::metadata(&abs) {
                cache.set_sidecar_mtime(&srel, mtime_ns(&meta));
            }
        }
    }
}

/// Files that have tags, a rating or a source but no sidecar yet.
pub fn missing_sidecars(c: &Collection) -> Vec<String> {
    let mode = c.settings().sidecars;
    c.files()
        .filter(|(_, e)| !e.missing && has_content(e))
        .filter(|(_, e)| e.paths.iter().any(|rel| sidecar_rel(rel, mode).and_then(|s| c.abs_path(&s)).is_some_and(|p| !p.exists())))
        .map(|(h, _)| h.to_string())
        .collect()
}

/// Reads sidecars changed by other apps since BooruCraft last wrote or read them. Returns the
/// files whose data changed.
pub fn read_back(c: &mut Collection, tags: &TagList, cache: &mut ScanCache, report: &mut SidecarReport) -> BTreeSet<String> {
    let mut changed = BTreeSet::new();
    let mode = c.settings().sidecars;
    if !c.settings().read_back_sidecars || mode == SidecarMode::Off {
        return changed;
    }
    let candidates: Vec<(String, String, PathBuf)> = c
        .files()
        .filter(|(_, e)| !e.missing)
        .flat_map(|(h, e)| e.paths.iter().filter_map(move |rel| sidecar_rel(rel, mode).map(|s| (h.to_string(), s))))
        .filter_map(|(h, s)| c.abs_path(&s).map(|abs| (h, s, abs)))
        .collect();
    for (hash, srel, abs) in candidates {
        let Ok(meta) = std::fs::metadata(&abs) else { continue };
        let mtime = mtime_ns(&meta);
        if cache.sidecar_mtime(&srel) == Some(mtime) {
            continue;
        }
        let bytes = match safefile::read_capped(&abs, MAX_SIDECAR_BYTES as u64) {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(e) => {
                report.errors.push((abs, e.to_string()));
                continue;
            }
        };
        let sd = match boorucraft_xmp::read(&bytes, |n| tags.is_namespace(n)) {
            Ok(sd) => sd,
            Err(e) => {
                report.errors.push((abs, e.to_string()));
                continue;
            }
        };
        let mut found = BTreeSet::new();
        for t in &sd.tags {
            match normalize_tag(t) {
                Ok(t) => {
                    found.insert(t);
                }
                Err(e) => report.errors.push((abs.clone(), format!("skipped keyword: {e}"))),
            }
        }
        let Some(entry) = c.get(&hash) else { continue };
        let ours = sd.hash.is_some();
        let new_tags: BTreeSet<String> = if ours { found } else { entry.tags.union(&found).cloned().collect() };
        let new_rating = sd.rating.or(entry.rating);
        let new_source = sd.source.clone().or_else(|| entry.source.clone());
        if new_tags != entry.tags || new_rating != entry.rating || new_source != entry.source {
            if let Some(e) = c.get_mut(&hash) {
                e.tags = new_tags;
                e.rating = new_rating;
                e.source = new_source;
            }
            changed.insert(hash);
            report.read += 1;
        }
        cache.set_sidecar_mtime(&srel, mtime);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_names() {
        assert_eq!(sidecar_rel("a/cat.png", SidecarMode::FileExt).as_deref(), Some("a/cat.png.xmp"));
        assert_eq!(sidecar_rel("a/cat.png", SidecarMode::Stem).as_deref(), Some("a/cat.xmp"));
        assert_eq!(sidecar_rel("cat.tar.gz", SidecarMode::Stem).as_deref(), Some("cat.tar.xmp"));
        assert_eq!(sidecar_rel(".hidden", SidecarMode::Stem).as_deref(), Some(".hidden.xmp"));
        assert_eq!(sidecar_rel("noext", SidecarMode::Stem).as_deref(), Some("noext.xmp"));
        assert_eq!(sidecar_rel("cat.png", SidecarMode::Off), None);
    }
}

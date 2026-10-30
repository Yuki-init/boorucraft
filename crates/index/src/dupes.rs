//! Near-duplicate groups across open collections, from the perceptual hashes stored in each
//! `collection.json` (computed from thumbnails by the caller, see `boorucraft-thumbs`).

use std::path::PathBuf;

use boorucraft_dedup::{Hashes, from_hex, groups};
use boorucraft_store::{Collection, FileEntry, Kind};

/// One file of a duplicate group: the collection's position in the list given, and the hash.
pub type Member = (usize, String);

/// The stored perceptual hashes of a file, if it has both.
pub fn stored_hashes(e: &FileEntry) -> Option<Hashes> {
    Some(Hashes { dhash: from_hex(e.dhash.as_deref()?)?, phash: from_hex(e.phash.as_deref()?)? })
}

pub fn set_hashes(c: &mut Collection, hash: &str, h: Hashes) {
    if let Some(e) = c.get_mut(hash) {
        e.dhash = Some(boorucraft_dedup::to_hex(h.dhash));
        e.phash = Some(boorucraft_dedup::to_hex(h.phash));
    }
}

/// Pictures that still need perceptual hashes: (hash, path on disk, format). The caller skips
/// formats it can't decode.
pub fn needing_hashes(c: &Collection) -> Vec<(String, PathBuf, String)> {
    c.files()
        .filter(|(_, e)| e.kind == Kind::Image && !e.missing && stored_hashes(e).is_none())
        .filter_map(|(h, e)| {
            let path = e.paths.iter().filter_map(|p| c.abs_path(p)).find(|p| p.exists())?;
            Some((h.to_string(), path, e.format.clone()))
        })
        .collect()
}

/// "Better" copies first: more pixels, then bigger files, then the one imported first.
fn better(a: &FileEntry, b: &FileEntry) -> std::cmp::Ordering {
    let px = |e: &FileEntry| u64::from(e.width.unwrap_or(0)) * u64::from(e.height.unwrap_or(0));
    px(b).cmp(&px(a)).then(b.size.cmp(&a.size)).then(a.imported.cmp(&b.imported))
}

/// Groups of near-duplicates over the given collections (`None` entries are skipped but keep
/// their position). Missing files are left out. Pairs marked "not duplicates" in either
/// collection stay apart, and so do pairs `related(a, b)` says are linked some other way.
/// Each group lists the best copy first.
pub fn find_groups(colls: &[Option<&Collection>], threshold: u32, related: impl Fn(&Member, &Member) -> bool) -> Vec<Vec<Member>> {
    let mut members: Vec<Member> = Vec::new();
    let mut items: Vec<Hashes> = Vec::new();
    for (ci, c) in colls.iter().enumerate() {
        let Some(c) = c else { continue };
        for (hash, e) in c.files() {
            if e.missing {
                continue;
            }
            if let Some(h) = stored_hashes(e) {
                members.push((ci, hash.to_string()));
                items.push(h);
            }
        }
    }
    let marked = |a: &Member, b: &Member| [a.0, b.0].iter().any(|ci| colls.get(*ci).copied().flatten().is_some_and(|c| c.is_not_duplicate(&a.1, &b.1)));
    let entry = |m: &Member| colls.get(m.0).copied().flatten().and_then(|c| c.get(&m.1));
    let mut out: Vec<Vec<Member>> = groups(&items, threshold, |i, j| match (members.get(i), members.get(j)) {
        (Some(a), Some(b)) => marked(a, b) || related(a, b),
        _ => true,
    })
    .into_iter()
    .map(|g| g.into_iter().filter_map(|i| members.get(i).cloned()).collect::<Vec<_>>())
    .collect();
    for g in &mut out {
        g.sort_by(|a, b| match (entry(a), entry(b)) {
            (Some(x), Some(y)) => better(x, y).then_with(|| a.cmp(b)),
            _ => a.cmp(b),
        });
    }
    out
}

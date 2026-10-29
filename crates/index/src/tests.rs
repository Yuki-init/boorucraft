use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use boorucraft_store::{Collection, Kind, TagList};

use crate::{ScanCache, scan, write_sidecars};

/// A PNG header with an IHDR chunk; enough for format and size detection. `w` makes the content
/// unique.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(&13u32.to_be_bytes());
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b.extend_from_slice(&[0, 0, 0, 0]);
    b.extend_from_slice(b"IDAT");
    b
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn tag_list() -> TagList {
    let mut t = TagList::in_memory();
    for n in ["cat", "sky", "dog", "artist:foo"] {
        t.create(n, "").unwrap();
    }
    t
}

fn hash_at(c: &Collection, rel: &str) -> String {
    c.hash_of_path(rel).unwrap_or_else(|| panic!("{rel} not in collection")).to_string()
}

/// Sets a file's mtime well away from "now" so a change is always visible.
fn bump_mtime(p: &Path, secs: u64) {
    let f = fs::File::options().write(true).open(p).unwrap();
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)).unwrap();
}

#[test]
fn adds_files_and_reuses_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.png", &png(10, 20));
    write(root, "sub/b.PNG", &png(11, 20));
    write(root, "notes.txt", b"ignored");
    write(root, ".hidden/c.png", &png(12, 20));
    write(root, "nested/collection.json", b"{}");
    write(root, "nested/d.png", &png(13, 20));
    write(root, "a.png.xmp", b"<x/>");
    let tags = tag_list();
    let mut cache = ScanCache::in_memory();
    let mut c = Collection::open(root).unwrap();
    let r = scan(&mut c, &tags, &mut cache);
    assert_eq!((r.files, r.hashed, r.added), (2, 2, 2), "{:?}", r.errors);
    let a = c.get(&hash_at(&c, "a.png")).unwrap();
    assert_eq!((a.kind, a.format.as_str(), a.width, a.height), (Kind::Image, "png", Some(10), Some(20)));
    assert_eq!(a.md5.len(), 32);
    assert!(c.hash_of_path("sub/b.PNG").is_some());

    let r = scan(&mut c, &tags, &mut cache);
    assert_eq!((r.files, r.hashed, r.added), (2, 0, 0));
}

#[test]
fn moved_and_missing_files_keep_their_tags() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.png", &png(10, 20));
    let tags = tag_list();
    let mut cache = ScanCache::in_memory();
    let mut c = Collection::open(root).unwrap();
    scan(&mut c, &tags, &mut cache);
    let h = hash_at(&c, "a.png");
    c.edit_tags(&h, &["cat"], &[], &tags).unwrap();

    fs::create_dir(root.join("moved")).unwrap();
    fs::rename(root.join("a.png"), root.join("moved/a2.png")).unwrap();
    let r = scan(&mut c, &tags, &mut cache);
    assert_eq!((r.moved, r.added, r.missing), (1, 0, 0));
    assert_eq!(hash_at(&c, "moved/a2.png"), h);
    assert!(c.hash_of_path("a.png").is_none());

    fs::remove_file(root.join("moved/a2.png")).unwrap();
    let r = scan(&mut c, &tags, &mut cache);
    assert_eq!(r.missing, 1);
    let e = c.get(&h).unwrap();
    assert!(e.missing);
    assert!(e.tags.contains("cat"));
    assert!(e.paths.contains("moved/a2.png"), "last known path is kept");

    write(root, "back.png", &png(10, 20));
    let r = scan(&mut c, &tags, &mut cache);
    assert_eq!(r.missing, 0);
    let e = c.get(&h).unwrap();
    assert!(!e.missing);
    assert_eq!(e.paths, ["back.png".to_string()].into());
}

#[test]
fn edited_pictures_carry_their_tags_over() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.png", &png(10, 20));
    let tags = tag_list();
    let mut cache = ScanCache::in_memory();
    let mut c = Collection::open(root).unwrap();
    scan(&mut c, &tags, &mut cache);
    let old = hash_at(&c, "a.png");
    c.edit_tags(&old, &["sky"], &[], &tags).unwrap();

    write(root, "a.png", &png(99, 20));
    bump_mtime(&root.join("a.png"), 1);
    let r = scan(&mut c, &tags, &mut cache);
    assert_eq!((r.edited, r.added, r.missing), (1, 1, 0), "{r:?}");
    let new = hash_at(&c, "a.png");
    assert_ne!(new, old);
    assert!(c.get(&old).is_none());
    assert!(c.get(&new).unwrap().tags.contains("sky"));
    assert_eq!(c.get(&new).unwrap().width, Some(99));
}

#[test]
fn sidecars_are_written_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.png", &png(10, 20));
    let tags = tag_list();
    let mut cache = ScanCache::in_memory();
    let mut c = Collection::open(root).unwrap();
    scan(&mut c, &tags, &mut cache);
    let h = hash_at(&c, "a.png");
    c.edit_tags(&h, &["cat", "artist:foo"], &[], &tags).unwrap();
    let mut report = Default::default();
    write_sidecars(&c, &tags, [h.as_str()], &mut cache, &mut report);
    assert_eq!(report.written, 1);
    let xmp_path = root.join("a.png.xmp");
    let text = fs::read_to_string(&xmp_path).unwrap();
    assert!(text.contains("<rdf:li>artist|foo</rdf:li>"), "{text}");

    // Nothing changed: a scan reads nothing back and writes nothing.
    let r = scan(&mut c, &tags, &mut cache);
    assert_eq!((r.sidecars.read, r.sidecars.written), (0, 0));

    // Another app replaces `cat` with `dog` in our sidecar.
    fs::write(&xmp_path, text.replace("<rdf:li>cat</rdf:li>", "<rdf:li>Dog</rdf:li>")).unwrap();
    bump_mtime(&xmp_path, 2);
    let r = scan(&mut c, &tags, &mut cache);
    assert_eq!(r.sidecars.read, 1, "{:?}", r.sidecars.errors);
    assert_eq!(c.get(&h).unwrap().tags, ["artist:foo".to_string(), "dog".to_string()].into());
}

#[test]
fn foreign_sidecars_add_tags_and_damaged_ones_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.png", &png(10, 20));
    write(root, "b.png", &png(11, 20));
    let foreign = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:subject><rdf:Bag><rdf:li>Blue Sky</rdf:li></rdf:Bag></dc:subject></rdf:Description>
</rdf:RDF></x:xmpmeta>"#;
    write(root, "a.png.xmp", foreign.as_bytes());
    write(root, "b.png.xmp", b"<broken");
    let tags = tag_list();
    let mut cache = ScanCache::in_memory();
    let mut c = Collection::open(root).unwrap();
    let r = scan(&mut c, &tags, &mut cache);
    assert_eq!(r.sidecars.read, 1);
    let a = hash_at(&c, "a.png");
    assert_eq!(c.get(&a).unwrap().tags, ["blue_sky".to_string()].into());
    assert_eq!(c.unknown_tags(&tags).into_keys().collect::<Vec<_>>(), vec!["blue_sky".to_string()]);
    assert_eq!(r.sidecars.errors.len(), 1, "the broken sidecar is reported");

    let b = hash_at(&c, "b.png");
    c.edit_tags(&b, &["cat"], &[], &tags).unwrap();
    let mut report = Default::default();
    write_sidecars(&c, &tags, [b.as_str()], &mut cache, &mut report);
    assert_eq!(report.written, 0);
    assert_eq!(fs::read(root.join("b.png.xmp")).unwrap(), b"<broken");
}

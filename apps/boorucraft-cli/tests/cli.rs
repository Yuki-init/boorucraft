//! Runs the real binary on a temporary folder, with its config and cache in another.

use std::path::Path;
use std::process::{Command, Output};

fn png(w: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(&13u32.to_be_bytes());
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&7u32.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b
}

struct Env {
    home: tempfile::TempDir,
    pics: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let env = Env { home: tempfile::tempdir().unwrap(), pics: tempfile::tempdir().unwrap() };
        std::fs::write(env.pics.path().join("a.png"), png(1)).unwrap();
        std::fs::write(env.pics.path().join("b.png"), png(2)).unwrap();
        env
    }

    fn dir(&self) -> &str {
        self.pics.path().to_str().unwrap()
    }

    fn file(&self, name: &str) -> String {
        self.pics.path().join(name).to_str().unwrap().to_string()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_boorucraft-cli"))
            .args(args)
            .env("BOORUCRAFT_CONFIG_DIR", self.home.path().join("config"))
            .env("BOORUCRAFT_CACHE_DIR", self.home.path().join("cache"))
            .env_remove("BOORUCRAFT_TAGS")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(!out.status.success(), "{args:?} should fail");
        String::from_utf8(out.stderr).unwrap()
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.pics.path().join(name)).unwrap()
    }
}

fn names(out: &str) -> Vec<String> {
    out.lines().map(|l| Path::new(l.trim_end_matches("  [missing]")).file_name().unwrap().to_string_lossy().into_owned()).collect()
}

#[test]
fn scan_tag_search() {
    let e = Env::new();
    let (dir, a) = (e.dir(), e.file("a.png"));
    assert!(e.ok(&["scan", dir]).contains("2 files (2 hashed): 2 new"));
    assert!(e.read("collection.json").contains("boorucraft-collection"));

    assert!(e.fails(&["tag", dir, &a, "cat"]).contains("isn't in the tag list"));
    e.ok(&["tag", dir, &a, "cat", "artist:foo", "--create", "--rating", "s"]);
    assert!(e.read("a.png.xmp").contains("<rdf:li>artist|foo</rdf:li>"));
    assert!(!e.pics.path().join("b.png.xmp").exists(), "untagged files get no sidecar");

    assert_eq!(names(&e.ok(&["search", dir, "cat", "rating:s"])), vec!["a.png"]);
    assert_eq!(names(&e.ok(&["search", dir, "-cat"])), vec!["b.png"]);
    assert_eq!(names(&e.ok(&["search", dir])).len(), 2);
    let bad = e.fails(&["search", dir, "cat", "rating:x"]);
    assert!(bad.contains("unknown rating") && bad.contains("    ^^^^^^^^"), "{bad}");
    assert!(e.fails(&["tag", dir, &a, "catt"]).contains("Did you mean: cat"));
}

#[test]
fn tag_list_changes_reach_collections() {
    let e = Env::new();
    let (dir, a) = (e.dir(), e.file("a.png"));
    e.ok(&["scan", dir]);
    e.ok(&["tag", dir, &a, "cat", "--create"]);
    e.ok(&["tags", "rename", "cat", "neko"]);
    // Read-only commands see the rename without saving it.
    assert_eq!(names(&e.ok(&["search", dir, "neko"])), vec!["a.png"]);
    assert!(e.read("collection.json").contains("\"cat\""));
    // The next writing command catches the collection and its sidecars up.
    e.ok(&["scan", dir]);
    let json = e.read("collection.json");
    assert!(json.contains("\"neko\"") && !json.contains("\"cat\""), "{json}");
    assert!(e.read("a.png.xmp").contains("<rdf:li>neko</rdf:li>"));

    e.ok(&["tag", dir, &a, "-neko"]);
    assert!(!e.read("a.png.xmp").contains("neko"));
    assert!(e.ok(&["tags"]).contains("neko"));
    e.ok(&["tags", "delete", "neko"]);
    assert!(!e.ok(&["tags"]).contains("neko"));
}

#[test]
fn info_and_unknown_tags() {
    let e = Env::new();
    let (dir, a) = (e.dir(), e.file("a.png"));
    e.ok(&["scan", dir]);
    let info = e.ok(&["info", dir, &a]);
    assert!(info.contains("size:     1x7"), "{info}");
    std::fs::write(e.pics.path().join("b.png.xmp"), r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:subject><rdf:Bag><rdf:li>Imported Tag</rdf:li></rdf:Bag></dc:subject></rdf:Description></rdf:RDF></x:xmpmeta>"#).unwrap();
    e.ok(&["scan", dir]);
    assert!(e.ok(&["unknown", dir]).contains("imported_tag"));
    assert!(e.ok(&["info", dir]).contains("files: 2"));
    assert!(e.fails(&["bogus"]).contains("usage"));
}

#[test]
fn forget_missing() {
    let e = Env::new();
    let (dir, a) = (e.dir(), e.file("a.png"));
    e.ok(&["scan", dir]);
    e.ok(&["tag", dir, &a, "cat", "--create"]);
    std::fs::remove_file(&a).unwrap();
    e.ok(&["scan", dir]);
    let listed = e.ok(&["forget-missing", dir]);
    assert!(listed.contains("a.png  (1 tag)"), "{listed}");
    assert!(e.read("collection.json").contains("\"cat\""), "listing changes nothing");
    e.ok(&["forget-missing", dir, "--yes"]);
    assert!(!e.read("collection.json").contains("\"cat\""));
    assert_eq!(names(&e.ok(&["search", dir])), vec!["b.png"]);
    assert_eq!(e.ok(&["forget-missing", dir]), "");
}

#[test]
fn booru_face_tags() {
    let e = Env::new();
    let (dir, a) = (e.dir(), e.file("a.png"));
    e.ok(&["scan", dir]);
    e.ok(&["tag", dir, &a, ":d", ":3", "smile", "--create"]);
    assert!(e.read("a.png.xmp").contains("<rdf:li>:d</rdf:li>"));
    assert_eq!(names(&e.ok(&["search", dir, ":d"])), vec!["a.png"]);
    assert_eq!(names(&e.ok(&["search", dir, "-:3"])), vec!["b.png"]);
    assert!(e.fails(&["tag", dir, &a, "cat:", "--create"]).contains("can't end with `:`"));
}

/// Regression: `boorucraft-cli info DIR | head` panicked once `head` closed the pipe. The command
/// must finish (and save) quietly instead.
#[test]
fn closed_output_pipes_are_not_a_crash() {
    use std::process::Stdio;
    let e = Env::new();
    for args in [vec!["scan", e.dir()], vec!["info", e.dir()], vec!["tags"]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_boorucraft-cli"))
            .args(&args)
            .env("BOORUCRAFT_CONFIG_DIR", e.home.path().join("config"))
            .env("BOORUCRAFT_CACHE_DIR", e.home.path().join("cache"))
            .env_remove("BOORUCRAFT_TAGS")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Close the reading ends before the program writes anything.
        drop(child.stdout.take());
        drop(child.stderr.take());
        let status = child.wait().unwrap();
        assert!(status.success(), "{args:?} exited with {status}");
    }
    assert!(e.read("collection.json").contains("boorucraft-collection"), "scan still saved");
}

/// A real picture, `w` pixels wide: blocks of different shades and a bright disc at `x` (0..1).
fn picture(path: &Path, w: u32, x: f32) {
    let h = w * 2 / 3;
    let shades = [30.0, 200.0, 90.0, 160.0, 60.0, 230.0, 120.0, 10.0, 180.0, 70.0, 140.0, 40.0];
    let img = image::RgbImage::from_fn(w, h, |px, py| {
        let (fx, fy) = (px as f32 / w as f32, py as f32 / h as f32);
        let block = shades[(fx * 4.0) as usize % 4 + (fy * 3.0) as usize % 3 * 4];
        let v = if (fx - x).powi(2) + (fy - 0.5).powi(2) < 0.03 { 250.0 } else { block * 0.8 + fx * 40.0 };
        image::Rgb([v as u8, (v * 0.7) as u8, 80])
    });
    img.save(path).unwrap();
}

#[test]
fn near_duplicates() {
    let e = Env::new();
    let dir = e.dir();
    picture(&e.pics.path().join("big.png"), 600, 0.3);
    picture(&e.pics.path().join("small.jpg"), 200, 0.3);
    picture(&e.pics.path().join("other.png"), 600, 0.8);
    e.ok(&["scan", dir]);
    let out = e.ok(&["dupes", dir]);
    assert!(out.contains("group 1 (2 files)") && !out.contains("group 2"), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[1].contains("big.png") && lines[2].contains("small.jpg"), "the bigger copy first: {out}");
    assert!(e.read("collection.json").contains("\"phash\""), "hashes are saved");
    let mut found = names(&e.ok(&["search", dir, "dupes:yes"]));
    found.sort();
    assert_eq!(found, vec!["big.png", "small.jpg"]);
    assert!(e.fails(&["dupes", dir, "--threshold", "99"]).contains("--threshold"));

    // Linked as versions: no longer duplicates.
    let (big, small) = (e.file("big.png"), e.file("small.jpg"));
    e.ok(&["tag", dir, &small, "--parent", &big]);
    assert!(e.ok(&["info", dir, &small]).contains("parent:   big.png"));
    assert!(e.ok(&["info", dir, &big]).contains("children: small.jpg"));
    assert!(e.fails(&["tag", dir, &big, "--parent", &small]).contains("own parent"));
    assert!(e.ok(&["dupes", dir]).is_empty());
    assert_eq!(names(&e.ok(&["search", dir, "child:yes"])), vec!["big.png"]);
    e.ok(&["tag", dir, &small, "--parent", "none"]);
    assert!(e.ok(&["search", dir, "parent:any"]).is_empty());
}

#[test]
fn pools() {
    let e = Env::new();
    let (dir, a, b) = (e.dir(), e.file("a.png"), e.file("b.png"));
    e.ok(&["scan", dir]);
    assert!(e.fails(&["pools", "add", dir, "comic", &a]).contains("create it first"));
    e.ok(&["pools", "create", "Comic"]);
    e.ok(&["pools", "add", dir, "comic", &b, &a]);
    assert_eq!(names(&e.ok(&["search", dir, "pool:comic"])), vec!["b.png", "a.png"], "pool order");
    e.ok(&["pools", "rename", "comic", "manga"]);
    assert_eq!(names(&e.ok(&["search", dir, "pool:manga", "order:-pool"])), vec!["a.png", "b.png"]);
    e.ok(&["pools", "remove", dir, "manga", &b]);
    assert_eq!(names(&e.ok(&["search", dir, "pool:any"])), vec!["a.png"]);
    assert!(e.read("collection.json").contains("\"manga\": 1"));
    e.ok(&["pools", "delete", "manga"]);
    e.ok(&["scan", dir]);
    assert!(!e.read("collection.json").contains("manga"), "deleted pools leave the files");
    assert!(e.ok(&["pools"]).is_empty());
}

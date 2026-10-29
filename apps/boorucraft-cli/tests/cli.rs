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

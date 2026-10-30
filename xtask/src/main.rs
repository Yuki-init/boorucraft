//! `cargo xtask <task>`: BooruCraft's quality gates. Run them through `scripts/check.sh` (Podman).
//!
//! - `ci`: fmt check, clippy `-D warnings`, tests, `layers`, `rust-only`.
//! - `layers`: each crate depends only on lower layers, and nothing below the UI layer pulls in a
//!   windowing or GPU toolkit (so the UI stays swappable).
//! - `rust-only`: no crate in Cargo.lock builds or links C/C++ code (AGENTS.md §2).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Layer of every workspace crate. A crate may depend only on crates in lower layers.
/// L0 core → L1 store, xmp, dedup → L2 index, thumbs → L3 UI → L4 apps.
#[rustfmt::skip]
const LAYERS: &[(&str, u8)] = &[
    ("boorucraft-core", 0),
    ("boorucraft-store", 1),
    ("boorucraft-xmp", 1),
    ("boorucraft-dedup", 1),
    ("boorucraft-index", 2),
    ("boorucraft-thumbs", 2),
    ("boorucraft-ui-egui", 3),
    ("boorucraft", 4),
    ("boorucraft-cli", 4),
];

/// Dev-only helpers usable from any layer.
const ANY_LAYER: &[&str] = &["xtask"];

/// Toolkits that only the UI layer (L3) and apps may use.
const UI_ONLY: &[&str] = &["egui", "eframe", "egui-wgpu", "egui-winit", "winit", "wgpu", "rfd"];
const UI_LAYER: u8 = 3;

/// Crates whose only job is building or binding C/C++. None may appear in Cargo.lock.
/// (`cc` and `pkg-config` may: several crates list them for optional features we don't use.
/// The build check below catches them if they ever compile or link anything.)
const C_ONLY_CRATES: &[&str] = &["cmake", "bindgen", "vcpkg", "system-deps", "cxx", "cxx-build", "autotools", "nasm-rs"];

/// Native libraries a build script may link: `dl` is the C library's dynamic loader, used to
/// load the window system and GPU drivers at run time.
const ALLOWED_LINKS: &[&str] = &["dl"];

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let result = match task.as_str() {
        "ci" => ci(),
        "layers" => layers(),
        "rust-only" => rust_only(),
        _ => Err("usage: cargo xtask <ci|layers|rust-only>".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask {task}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn cargo(args: &[&str]) -> Result<(), String> {
    eprintln!("==> cargo {}", args.join(" "));
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())).args(args).current_dir(root()).status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("cargo {} failed", args.join(" "))) }
}

fn ci() -> Result<(), String> {
    cargo(&["fmt", "--all", "--check"])?;
    cargo(&["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"])?;
    cargo(&["test", "--workspace", "--locked"])?;
    layers()?;
    rust_only()?;
    eprintln!("==> ci passed");
    Ok(())
}

/// Every package name in Cargo.lock.
fn locked_packages() -> Result<Vec<String>, String> {
    let lock = std::fs::read_to_string(root().join("Cargo.lock")).map_err(|e| format!("Cargo.lock: {e}"))?;
    Ok(lock.lines().filter_map(|l| l.strip_prefix("name = ")).map(|n| n.trim_matches('"').to_string()).collect())
}

/// No C-only crate in Cargo.lock, and no build script that compiles C or links a native
/// library (Cargo reports both as `linked_libs` of the build script).
fn rust_only() -> Result<(), String> {
    let mut problems = Vec::new();
    for name in locked_packages()? {
        if C_ONLY_CRATES.contains(&name.as_str()) {
            problems.push(format!("{name} exists to build or bind C code; find a pure-Rust alternative (`cargo tree -i {name}`)"));
        }
    }
    eprintln!("==> cargo build --workspace --all-targets --locked --message-format=json");
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--workspace", "--all-targets", "--locked", "--message-format=json"])
        .current_dir(root())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("cargo build failed".into());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if msg["reason"] != "build-script-executed" {
            continue;
        }
        let package = msg["package_id"].as_str().unwrap_or("?");
        let package = package.rsplit(['/', '#']).next().unwrap_or(package);
        for lib in msg["linked_libs"].as_array().into_iter().flatten().filter_map(|l| l.as_str()) {
            let name = lib.rsplit('=').next().unwrap_or(lib);
            if lib.starts_with("static=") || !ALLOWED_LINKS.contains(&name) {
                problems.push(format!("{package}: its build script links native library `{lib}`"));
            }
        }
    }
    report("rust-only", problems)
}

/// Manifest paths of every workspace member.
fn manifests() -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for dir in ["crates", "apps"] {
        let Ok(entries) = std::fs::read_dir(root().join(dir)) else { continue };
        for e in entries.flatten() {
            let m = e.path().join("Cargo.toml");
            if m.is_file() {
                out.push(m);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The package name and the names in `[dependencies]` (not dev-/build-dependencies) of a manifest.
/// A tiny line parser: enough for our own manifests, which keep one dependency per line.
fn parse_manifest(text: &str) -> (String, Vec<String>) {
    let mut name = String::new();
    let mut deps = Vec::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if section == "package" && key == "name" {
            name = value.trim().trim_matches('"').to_string();
        } else if section == "dependencies" || section.starts_with("target.") && section.ends_with(".dependencies") {
            deps.push(key.to_string());
        }
    }
    (name, deps)
}

fn layers() -> Result<(), String> {
    let mut problems = Vec::new();
    for m in manifests()? {
        let text = std::fs::read_to_string(&m).map_err(|e| format!("{}: {e}", m.display()))?;
        let (name, deps) = parse_manifest(&text);
        if ANY_LAYER.contains(&name.as_str()) {
            continue;
        }
        let Some(&(_, layer)) = LAYERS.iter().find(|(n, _)| *n == name) else {
            problems.push(format!("{name}: not in xtask's LAYERS table; give it a layer"));
            continue;
        };
        for d in deps {
            if UI_ONLY.contains(&d.as_str()) && layer < UI_LAYER {
                problems.push(format!("{name} (L{layer}) depends on {d}, which only the UI layer (L{UI_LAYER}) and apps may use"));
            }
            if ANY_LAYER.contains(&d.as_str()) {
                problems.push(format!("{name} uses {d} as a normal dependency; it is for dev-dependencies only"));
            }
            if let Some(&(_, dl)) = LAYERS.iter().find(|(n, _)| *n == d)
                && dl >= layer
            {
                problems.push(format!("{name} (L{layer}) depends on {d} (L{dl}); depend only on lower layers"));
            }
        }
    }
    report("layers", problems)
}

fn report(what: &str, problems: Vec<String>) -> Result<(), String> {
    if problems.is_empty() {
        eprintln!("==> {what}: ok");
        Ok(())
    } else {
        for p in &problems {
            eprintln!("  {p}");
        }
        Err(format!("{} problem(s)", problems.len()))
    }
}

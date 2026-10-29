//! Where BooruCraft keeps its own files.
//!
//! | | config | cache |
//! |---|---|---|
//! | Linux, BSD | `$XDG_CONFIG_HOME` or `~/.config` | `$XDG_CACHE_HOME` or `~/.cache` |
//! | macOS | `~/Library/Application Support` | `~/Library/Caches` |
//! | Windows | `%APPDATA%` | `%LOCALAPPDATA%` |
//!
//! each with a `boorucraft` folder inside, overridable with `BOORUCRAFT_CONFIG_DIR` and
//! `BOORUCRAFT_CACHE_DIR`.

use std::path::PathBuf;

/// The config folder (`~/.config/boorucraft` on Linux).
pub fn config_dir() -> PathBuf {
    env_path("BOORUCRAFT_CONFIG_DIR").unwrap_or_else(|| platform_dir(Kind::Config).join("boorucraft"))
}

/// The cache folder (`~/.cache/boorucraft` on Linux). Everything in it can be rebuilt.
pub fn cache_dir() -> PathBuf {
    env_path("BOORUCRAFT_CACHE_DIR").unwrap_or_else(|| platform_dir(Kind::Cache).join("boorucraft"))
}

/// `$BOORUCRAFT_TAGS`, else `tags.json` in [`config_dir`].
pub fn tag_list_path() -> PathBuf {
    env_path("BOORUCRAFT_TAGS").unwrap_or_else(|| config_dir().join("tags.json"))
}

#[derive(Clone, Copy)]
enum Kind {
    Config,
    Cache,
}

/// An absolute path from an environment variable; relative or empty values are ignored, as the
/// XDG spec asks.
fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute())
}

fn home() -> PathBuf {
    env_path("HOME").or_else(|| env_path("USERPROFILE")).unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(windows)]
fn platform_dir(kind: Kind) -> PathBuf {
    let var = match kind {
        Kind::Config => "APPDATA",
        Kind::Cache => "LOCALAPPDATA",
    };
    env_path(var).unwrap_or_else(|| home().join("AppData").join(if matches!(kind, Kind::Config) { "Roaming" } else { "Local" }))
}

#[cfg(target_os = "macos")]
fn platform_dir(kind: Kind) -> PathBuf {
    match kind {
        Kind::Config => home().join("Library").join("Application Support"),
        Kind::Cache => home().join("Library").join("Caches"),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn platform_dir(kind: Kind) -> PathBuf {
    match kind {
        Kind::Config => env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config")),
        Kind::Cache => env_path("XDG_CACHE_HOME").unwrap_or_else(|| home().join(".cache")),
    }
}

//! Scanning collection folders: hashing, metadata, path tracking and XMP sidecar sync; duplicate
//! groups across collections.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod cache;
pub mod dupes;
pub mod probe;
pub mod scan;
pub mod sidecar;
pub mod watch;

pub use cache::ScanCache;
pub use dupes::{family, find_groups, needing_hashes, set_hashes, stored_hashes};
pub use scan::{ScanReport, scan};
pub use sidecar::{SidecarReport, read_back, sidecar_rel, write_sidecars};
pub use watch::{FolderWatcher, Relevance, classify};

#[cfg(test)]
mod tests;

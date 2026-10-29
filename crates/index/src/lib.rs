//! Scanning collection folders: hashing, metadata, path tracking and XMP sidecar sync.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod cache;
pub mod probe;
pub mod scan;
pub mod sidecar;

pub use cache::ScanCache;
pub use scan::{ScanReport, scan};
pub use sidecar::{SidecarReport, read_back, sidecar_rel, write_sidecars};

#[cfg(test)]
mod tests;

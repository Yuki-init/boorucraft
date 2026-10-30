//! BooruCraft's data on disk and in memory.
//!
//! - [`TagList`]: the central list of valid tags and namespaces (`~/.config/boorucraft/tags.json`).
//! - [`Collection`]: one folder's `collection.json`, keyed by content hash.
//! - [`SearchIndex`]: roaring bitmaps over a collection, for instant searches.
//!
//! Both files are saved atomically with a `.bak` copy, guarded by a lock file, and never saved
//! over a version another program wrote in the meantime ([`safefile`]).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod collection;
pub mod error;
pub mod index;
pub mod paths;
pub mod safefile;
pub mod taglist;

pub use collection::{Collection, CollectionSettings, FileEntry, Kind, Recovery, SidecarMode};
pub use error::StoreError;
pub use index::{Links, Row, SearchIndex, compare_rows};
pub use safefile::DiskChange;
pub use taglist::{HistoryEntry, HistoryOp, PoolInfo, TagList};

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Hex SHA-256 of `bytes`: the content hash used to identify files.
pub fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    to_hex(&Sha256::digest(bytes))
}

pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

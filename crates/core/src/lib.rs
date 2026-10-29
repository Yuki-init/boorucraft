//! BooruCraft's data types: tag names, ratings and the search query language.
//!
//! No IO here; everything is plain data and pure functions.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod query;
pub mod rating;
pub mod tag;

pub use query::{Mode, Query, QueryError, TagPattern, Term, TermKind};
pub use rating::Rating;
pub use tag::{TagError, glob_match, is_valid_namespace, normalize_tag, split_namespace};

/// Metatag names used by the search language (`rating:s`, `width:>1920`…). They can't be used as
/// tag namespaces, so a query term's meaning never depends on the tag list.
pub const METATAGS: &[&str] = &[
    "rating", "width", "height", "ratio", "mpixels", "filesize", "type", "kind", "animated", "date", "imported", "tagcount", "path", "dupes", "missing",
    "order", "md5", "hash", "source",
];

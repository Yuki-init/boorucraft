use std::io;
use std::path::PathBuf;

use boorucraft_core::TagError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{}: not valid JSON: {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{} is larger than {max} bytes", path.display())]
    TooLarge { path: PathBuf, max: u64 },
    #[error("{} is not a {expected} file", path.display())]
    WrongFormat { path: PathBuf, expected: &'static str },
    #[error("{} was written by a newer BooruCraft (format version {version}); update BooruCraft to open it", path.display())]
    NewerVersion { path: PathBuf, version: u32 },
    #[error("{} is in use by {holder}", path.display())]
    Locked { path: PathBuf, holder: String },
    #[error("{} was changed by another program since it was loaded; reload it before saving", path.display())]
    ChangedOnDisk { path: PathBuf },
    #[error("{} is open read-only", .0.display())]
    ReadOnly(PathBuf),
    #[error(transparent)]
    Tag(#[from] TagError),
    #[error("tag `{0}` is not in the tag list; create it first")]
    UnknownTag(String),
    #[error("tag `{0}` already exists")]
    TagExists(String),
    #[error("`{0}` is not a valid namespace name (a-z, 0-9 and _, at most 32 characters, not a search keyword)")]
    BadNamespace(String),
    #[error("namespace `{0}` already exists")]
    NamespaceExists(String),
    #[error("pool `{0}` is not in the pool list; create it first")]
    UnknownPool(String),
    #[error("pool `{0}` already exists")]
    PoolExists(String),
    #[error("{0}")]
    Invalid(String),
}

impl StoreError {
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> StoreError {
        StoreError::Io { path: path.into(), source }
    }
}

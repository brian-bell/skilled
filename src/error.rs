use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("application metadata operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("application metadata schema {found} is newer than supported schema {supported}")]
    UnsupportedSchema { found: i64, supported: i64 },
    #[error("the current user's home directory could not be determined")]
    HomeDirectoryUnavailable,
    #[error("the platform application-data directory could not be determined")]
    DataDirectoryUnavailable,
}

pub type Result<T> = std::result::Result<T, Error>;

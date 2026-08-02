use std::{io, path::PathBuf};

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
    #[error("source path is not a directory: {0}")]
    SourcePathNotDirectory(PathBuf),
    #[error("source path is not representable as a portable catalog path: {0:?}")]
    InvalidSourcePath(PathBuf),
    #[error("source scan exceeded its bounded directory limit")]
    SourceScanLimitExceeded,
    #[error("stored catalog classification is invalid: {0}")]
    InvalidCatalogClassification(String),
    #[error("source path {path} is outside its resolved Git top level {top_level}")]
    SourceOutsideGitTopLevel { path: PathBuf, top_level: PathBuf },
    #[error("the installed git executable could not be invoked: {0}")]
    GitUnavailable(io::Error),
    #[error("git output was not valid UTF-8")]
    InvalidGitOutput,
    #[error("git command failed in {repository:?}: git {arguments:?}: {stderr}")]
    GitCommand {
        repository: PathBuf,
        arguments: Vec<String>,
        stderr: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

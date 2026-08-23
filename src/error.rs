use std::{io, path::PathBuf};

use thiserror::Error;

/// The private metadata store could not be used for this session.
///
/// The path and operation cause remain separate until presentation so both can
/// be escaped as terminal input independently.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("application metadata unavailable at {}: {cause}", database_path.display())]
pub struct MetadataFailure {
    database_path: PathBuf,
    cause: String,
}

impl MetadataFailure {
    pub(crate) fn new(database_path: PathBuf, cause: impl Into<String>) -> Self {
        Self {
            database_path,
            cause: cause.into(),
        }
    }

    pub fn database_path(&self) -> &std::path::Path {
        &self.database_path
    }

    pub fn cause(&self) -> &str {
        &self.cause
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    MetadataUnavailable(MetadataFailure),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("application metadata operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("application metadata schema {found} is newer than supported schema {supported}")]
    UnsupportedSchema { found: i64, supported: i64 },
    #[error(
        "the application metadata database opened read-only and cannot be written this session"
    )]
    ReadOnlyMetadata,
    #[error("stored setup metadata is invalid: {0}")]
    InvalidSetupMetadata(String),
    #[error("stored metadata field {field} holds {value} rather than 0 or 1")]
    InvalidStoredBoolean { field: &'static str, value: i64 },
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
    #[error("path is not representable in the application metadata: {0:?}")]
    UnrepresentablePath(PathBuf),
    #[error("stored ownership receipt names an unknown agent: {0}")]
    InvalidStoredAgent(String),
    #[error("stored ownership receipt names an unknown operation: {0}")]
    InvalidStoredReceiptOperation(String),
    #[error("stored catalog path must be repository-relative: {0:?}")]
    UnsafeCatalogPath(PathBuf),
    #[error("stored catalog root must not be a symbolic link: {0:?}")]
    CatalogRootSymlink(PathBuf),
    #[error("stored catalog root resolves outside its source repository: {0:?}")]
    CatalogOutsideSource(PathBuf),
    #[error("source changed after it was previewed; inspect it again before registering")]
    SourceChangedAfterPreview,
    #[error("select at least one catalog root before registering the source")]
    NoCatalogsSelected,
    #[error("source path {path} is outside its resolved Git top level {top_level}")]
    SourceOutsideGitTopLevel { path: PathBuf, top_level: PathBuf },
    #[error("the installed git executable could not be invoked: {0}")]
    GitUnavailable(io::Error),
    #[error("git output was not valid UTF-8")]
    InvalidGitOutput,
    /// A fetch was refused because its destination would not be the destination.
    ///
    /// Git resolves a symbolic ref before updating it, so fetching into one
    /// would move whatever it points at — a local branch, in the case this
    /// guards — during a check that is only ever allowed to advance the
    /// configured remote-tracking ref.
    #[error(
        "remote-tracking ref {reference} is a symbolic ref to {target}; fetching it would move that ref instead"
    )]
    SymbolicTrackingRef { reference: String, target: String },
    #[error("git command failed in {repository:?}: git {arguments:?}: {stderr}")]
    GitCommand {
        repository: PathBuf,
        arguments: Vec<String>,
        stderr: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

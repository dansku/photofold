//! Error types.
//!
//! Two kinds of failure exist in this program, and they are deliberately
//! different types:
//!
//! * [`Error`] is fatal. It aborts the run (bad arguments, unusable output
//!   directory).
//! * [`FileError`] is not fatal. A single unreadable or corrupt file must never
//!   stop a 200,000-file merge, so per-file failures are collected in an
//!   [`ErrorLog`] and reported at the end.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A fatal error that ends the run.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("input path does not exist: {0}")]
    MissingInput(PathBuf),

    #[error("input path is not a directory: {0}")]
    NotADirectory(PathBuf),

    #[error(
        "the output directory is also an input directory: {0}\n\
         choose an output directory that is not one of the sources"
    )]
    OutputIsInput(PathBuf),

    #[error("--prefer-source {0} is not one of the input directories")]
    PreferSourceNotAnInput(PathBuf),

    #[error("could not create the output directory {path}: {source}")]
    CreateOutput {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write the report to {path}: {source}")]
    WriteReport {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not resolve path {path}: {source}")]
    ResolvePath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Csv(#[from] csv::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A non-fatal, per-file failure.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileError {
    pub path: PathBuf,
    /// Pipeline stage that failed, for example `"hash"` or `"decode"`.
    pub stage: &'static str,
    pub message: String,
}

/// Thread-safe collector for [`FileError`] values.
///
/// Every parallel stage holds a shared reference to one of these. The lock is
/// only taken when a file actually fails, so it does not contend in normal use.
#[derive(Debug, Default)]
pub struct ErrorLog {
    inner: Mutex<Vec<FileError>>,
}

impl ErrorLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a failure and continue.
    pub fn push(&self, path: &Path, stage: &'static str, message: impl std::fmt::Display) {
        let message = message.to_string();
        tracing::debug!(path = %path.display(), stage, %message, "file error");
        self.inner
            .lock()
            .expect("error log mutex poisoned")
            .push(FileError {
                path: path.to_path_buf(),
                stage,
                message,
            });
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("error log mutex poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Consume the log, returning failures sorted by path for deterministic output.
    pub fn into_sorted_vec(self) -> Vec<FileError> {
        let mut errors = self.inner.into_inner().expect("error log mutex poisoned");
        errors.sort_by(|a, b| a.path.cmp(&b.path).then(a.stage.cmp(b.stage)));
        errors
    }
}

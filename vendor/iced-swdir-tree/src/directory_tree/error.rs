//! Crate-level error type.
//!
//! We wrap [`std::io::Error`] (via [`swdir::ScanError`]) in our own enum
//! rather than re-exporting swdir's error so the public API stays
//! self-contained and future-flexible. In particular, cloning is required
//! because iced messages are `Clone` — and `std::io::Error` is not
//! `Clone`, so we carry the [`io::ErrorKind`] and a formatted message
//! instead of the live error object.

use std::io;
use std::path::{Path, PathBuf};

/// Everything the widget can fail at.
///
/// `Clone` is implemented because iced messages flow across channels
/// and must be cloneable. The originating [`io::Error`] cannot be
/// cloned, so we preserve its [`io::ErrorKind`] and its formatted
/// message instead.
#[derive(Debug, Clone)]
pub enum Error {
    /// An I/O error surfaced while scanning a directory.
    ///
    /// `path` is always the path the error refers to (the directory
    /// being scanned, or a specific child entry).
    Io {
        /// Path the error occurred on.
        path: PathBuf,
        /// Kind of the underlying [`io::Error`] (e.g. `NotFound`,
        /// `PermissionDenied`).
        kind: io::ErrorKind,
        /// Human-readable rendering of the original I/O error.
        message: String,
    },
}

impl Error {
    /// Construct an [`Error::Io`] from the pieces of a failed scan.
    pub(crate) fn io(path: impl Into<PathBuf>, kind: io::ErrorKind, message: String) -> Self {
        Self::Io {
            path: path.into(),
            kind,
            message,
        }
    }

    /// Path associated with this error, if any.
    pub fn path(&self) -> &Path {
        match self {
            Self::Io { path, .. } => path,
        }
    }

    /// [`io::ErrorKind`] of the underlying I/O failure.
    pub fn io_kind(&self) -> io::ErrorKind {
        match self {
            Self::Io { kind, .. } => *kind,
        }
    }

    /// `true` if the error is a permission-denied failure — the widget
    /// uses this to gray out the offending directory in the view.
    pub fn is_permission_denied(&self) -> bool {
        self.io_kind() == io::ErrorKind::PermissionDenied
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, message, .. } => {
                write!(f, "I/O error at {}: {}", path.display(), message)
            }
        }
    }
}

impl std::error::Error for Error {}

/// Convert a [`swdir::ScanError`] into our crate error.
///
/// Kept private to keep swdir's types out of our public API.
impl From<&swdir::ScanError> for Error {
    fn from(e: &swdir::ScanError) -> Self {
        Self::io(e.path(), e.io_kind(), e.to_string())
    }
}

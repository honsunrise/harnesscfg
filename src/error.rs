//! Error type shared by every module.
//!
//! Messages name paths and reasons only. They never quote file contents:
//! configuration files routinely hold API keys, and parser diagnostics would
//! otherwise echo them into activation logs.

use std::path::{Path, PathBuf};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot parse {path} as {format}; left unchanged")]
    Parse { path: PathBuf, format: &'static str },

    #[error("cannot apply managed model fields to {path}: {reason}; left unchanged")]
    Sync { path: PathBuf, reason: String },

    #[error("{path} is a dangling symlink; left unchanged")]
    DanglingLink { path: PathBuf },

    #[error("{path} is a symlink maintained by the user; left unchanged")]
    UserLink { path: PathBuf },

    #[error("{path} already exists as a user directory; left unchanged")]
    UserDirectory { path: PathBuf },

    #[error("{path} is expected to be a directory; left unchanged")]
    NotDirectory { path: PathBuf },

    #[error("{path} cannot be copied safely: it is neither a regular file nor a directory")]
    Unsupported { path: PathBuf },

    #[error("{path} changed while harnesscfg was preparing it; switch again")]
    Concurrent { path: PathBuf },

    #[error("several candidates exist for one file: {}; keep exactly one", list(.paths))]
    Ambiguous { paths: Vec<PathBuf> },

    #[error("{path} is locked by OMP; switch again shortly, OMP can keep running")]
    Locked { path: PathBuf },

    #[error("cannot express this change in {format}: {reason}")]
    Unrepresentable {
        format: &'static str,
        reason: String,
    },

    #[error("invalid manifest {path}: {reason}")]
    Manifest { path: PathBuf, reason: String },

    #[error("invalid registry entry {id}: {reason}")]
    Registry { id: String, reason: String },

    #[error("{command} failed with {status}")]
    Command { command: String, status: String },

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Attach the path an I/O operation was acting on.
pub trait IoContext<T> {
    fn at(self, path: &Path) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

impl<T> IoContext<T> for rustix::io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|errno| Error::Io {
            path: path.to_path_buf(),
            source: errno.into(),
        })
    }
}

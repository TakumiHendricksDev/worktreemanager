//! The Code tab's view of a worktree: what is in a directory, and what is in a file.
//!
//! # What this crate does not do
//!
//! It never decides which files a worktree *has*. That is git's question — `.gitignore` is git's
//! grammar, and agreeing with it well enough to draw a tree is not a small job — so the listing
//! comes from `wtm-git`, and this crate is handed paths. What it does is the part git cannot:
//! looking at one directory the user opened inside something ignored, and reading a file.
//!
//! # Paths
//!
//! Every path crossing into this crate is relative to a worktree root and is checked by
//! [`resolve`] before anything touches the disk. See its documentation for the rule.

mod dir;
mod path;
mod read;

pub use dir::{Entry, EntryKind, Kinds, classify, list_dir};
pub use path::resolve;
pub use read::{Content, FileText, MAX_READ_BYTES, read_file, stat};

/// Why a file or directory could not be shown.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodeError {
    /// Not a path inside the worktree at all: absolute, climbing out with `..`, or containing NUL.
    #[error("`{0}` is not a path inside this worktree")]
    BadPath(String),
    #[error("`{0}` does not exist")]
    NotFound(String),
    #[error("`{0}` is not a directory")]
    NotADirectory(String),
    /// A directory, a FIFO, a socket: something that exists and is not a file to read.
    #[error("`{0}` is not a file")]
    NotAFile(String),
    #[error("could not read `{path}`: {message}")]
    Io { path: String, message: String },
}

impl CodeError {
    fn io(rel: &str, error: &std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Self::NotFound(rel.to_owned());
        }
        Self::Io {
            path: rel.to_owned(),
            message: error.to_string(),
        }
    }
}

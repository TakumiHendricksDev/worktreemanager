//! What git says about the files in a worktree, for the Code tab.
//!
//! Paths here are always relative to the worktree root and use `/`, exactly as git prints them.
//! They are handed to the frontend in that form, which is also the form an agent is told a file
//! by — an absolute path buries the part that identifies the file under the user's home directory.

/// A NUL-separated path listing from git.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathList {
    pub paths: Vec<String>,
    /// The listing was cut short — the runner caps what it captures — so `paths` is a prefix of
    /// what git printed, not all of it. The UI says so rather than presenting a partial tree as
    /// the whole one.
    pub truncated: bool,
}

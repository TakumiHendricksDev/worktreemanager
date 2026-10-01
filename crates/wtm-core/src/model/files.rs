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

/// How a file differs from the revision a Changes view compares against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// Moved, and possibly edited. `from` is where it was.
    Renamed {
        from: String,
    },
    /// Git knows nothing about it yet.
    Untracked,
    /// Anything else git reports — a type change, an unmerged path — shown as modified.
    Other,
}

/// One changed file, relative to the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
}

/// One region of a file that differs from the revision, as `git diff -U0` reports it.
///
/// Line numbers are 1-based. A count of zero means the side has no lines here: `new_lines == 0` is
/// a deletion that happened *after* line `new_start`, and `old_lines == 0` an insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// The lines this region replaced or deleted, without their `-`.
    pub removed: Vec<String>,
}

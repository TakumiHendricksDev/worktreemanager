//! Filesystem reads.
//!
//! Narrow on purpose. The domain needs to answer a handful of preflight questions
//! ("does this directory exist, and is it empty?") and to read a couple of
//! key/value files for display. It has no business writing anything — every
//! mutation the app performs goes through git or through a project's own command.
//!
//! That is also why this trait has no `write`: not an oversight, a boundary. If a
//! future feature needs to write a file, that is a use-case decision worth making
//! deliberately rather than something the domain can do incidentally.

use std::path::{Path, PathBuf};

use crate::error::ConfigError;

pub trait FileStore: Send + Sync {
    fn exists(&self, path: &Path) -> bool;

    fn is_dir(&self, path: &Path) -> bool;

    /// Whether a directory has no entries. `git worktree add` refuses a non-empty
    /// target, so preflight needs to distinguish "exists but empty" (fine) from
    /// "exists with contents" (fatal).
    fn is_dir_empty(&self, path: &Path) -> Result<bool, ConfigError>;

    fn read_to_string(&self, path: &Path) -> Result<String, ConfigError>;

    /// Parse a `KEY=value` file.
    ///
    /// Matches the semantics of the shell helper it replaces: the *last*
    /// assignment to a key wins, and one layer of surrounding single or double
    /// quotes is stripped. Comments and blank lines are ignored. No variable
    /// expansion — this reads files, it does not evaluate them.
    fn read_dotenv(
        &self,
        path: &Path,
    ) -> Result<std::collections::BTreeMap<String, String>, ConfigError>;

    /// Resolve to an absolute, normalized path.
    ///
    /// Must not require the path to exist — it is used on the *target* directory
    /// during planning, before anything is created. Should therefore normalize
    /// lexically (resolving `.` and `..`) rather than calling `canonicalize`.
    fn absolutize(&self, path: &Path) -> Result<PathBuf, ConfigError>;
}

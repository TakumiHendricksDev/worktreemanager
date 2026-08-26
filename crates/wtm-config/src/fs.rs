//! The real [`FileStore`].
//!
//! Read-only by design — see the port's documentation. The one piece of genuine logic
//! here is [`parse_dotenv`], whose semantics have to match the shell helper it
//! replaces, and [`absolutize`], which must work on a path that does not exist yet.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wtm_core::error::ConfigError;
use wtm_core::ports::fs::FileStore;

/// Filesystem-backed [`FileStore`].
#[derive(Debug, Clone, Default)]
pub struct RealFileStore;

impl RealFileStore {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

fn io_err(path: &Path, err: &std::io::Error) -> ConfigError {
    ConfigError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    }
}

impl FileStore for RealFileStore {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn is_dir_empty(&self, path: &Path) -> Result<bool, ConfigError> {
        let mut entries = std::fs::read_dir(path).map_err(|e| io_err(path, &e))?;
        Ok(entries.next().is_none())
    }

    fn read_to_string(&self, path: &Path) -> Result<String, ConfigError> {
        std::fs::read_to_string(path).map_err(|e| io_err(path, &e))
    }

    fn read_dotenv(&self, path: &Path) -> Result<BTreeMap<String, String>, ConfigError> {
        Ok(parse_dotenv(&self.read_to_string(path)?))
    }

    fn absolutize(&self, path: &Path) -> Result<PathBuf, ConfigError> {
        absolutize(path)
    }
}

/// Parse a `KEY=value` file.
///
/// Matches the shell helper it replaces:
///
/// - the **last** assignment to a key wins (`sed … | tail -1`),
/// - one layer of surrounding single or double quotes is stripped,
/// - `export KEY=value` is accepted, since env files get hand-edited,
/// - comments and blank lines are ignored,
/// - and **nothing is expanded**. Reading a file is not evaluating it: a literal
///   `${OTHER}` stays literal, because guessing at shell expansion semantics would be
///   worse than not doing it.
#[must_use]
pub fn parse_dotenv(contents: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();
        if key.is_empty() {
            continue;
        }

        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);

        out.insert(key.to_owned(), value.to_owned());
    }

    out
}

/// Resolve to an absolute, lexically normalized path.
///
/// Deliberately *not* `canonicalize`: this runs on the create target during planning,
/// before the directory exists, and `canonicalize` requires existence. It also must
/// not resolve symlinks — the `../{name}` layout is relative to the repo root as the
/// user sees it, and silently rewriting `/var` to `/private/var` would make every
/// path shown in the UI unrecognizable.
pub fn absolutize(path: &Path) -> Result<PathBuf, ConfigError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| io_err(path, &e))?
            .join(path)
    };

    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // POSIX: the root's parent is the root.
                Some(Component::RootDir) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }

    Ok(out)
}

/// A sibling of `path` that will not collide with another in-flight atomic write.
///
/// User config, trust, and the session store all used `path.with_extension("toml.tmp")`,
/// which is one name for every writer. Two saves at once would truncate each other's
/// temporary file, and a crash mid-write could leave a shared leftover. The pid and a
/// process-local counter keep the file in the same directory (so `rename` stays atomic)
/// without sharing a name.
pub fn unique_temp_path(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    path.with_file_name(format!("{name}.{}.{seq}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_last_assignment_wins() {
        // How a generated env file overrides a copied template: it appends.
        let env = parse_dotenv("HOST_PORT_WEB=8000\nHOST_PORT_WEB=8007\n");
        assert_eq!(env["HOST_PORT_WEB"], "8007");
    }

    #[test]
    fn dotenv_strips_one_layer_of_quotes() {
        let env = parse_dotenv("A=\"quoted\"\nB='single'\nC=\"\"nested\"\"\n");
        assert_eq!(env["A"], "quoted");
        assert_eq!(env["B"], "single");
        assert_eq!(env["C"], "\"nested\"");
    }

    #[test]
    fn dotenv_ignores_comments_blanks_and_malformed_lines() {
        let env = parse_dotenv("# a comment\n\n   \nNOT_AN_ASSIGNMENT\n=novalue\nA=1\n");
        assert_eq!(env.len(), 1);
        assert_eq!(env["A"], "1");
    }

    #[test]
    fn dotenv_accepts_export_and_values_containing_equals() {
        let env = parse_dotenv("export A=1\nDSN=postgres://u:p@h/db?x=1\n");
        assert_eq!(env["A"], "1");
        assert_eq!(env["DSN"], "postgres://u:p@h/db?x=1");
    }

    #[test]
    fn dotenv_does_not_expand() {
        assert_eq!(parse_dotenv("A=1\nB=${A}/x\n")["B"], "${A}/x");
    }

    #[test]
    fn dotenv_preserves_an_empty_value() {
        // Distinct from absent: a port var present but blank means something.
        let env = parse_dotenv("EMPTY=\n");
        assert_eq!(env.get("EMPTY").map(String::as_str), Some(""));
    }

    #[test]
    fn absolutize_resolves_the_sibling_layout_without_the_path_existing() {
        // The `../{name}` case, on a directory that has not been created yet.
        assert_eq!(
            absolutize(Path::new("/Users/dev/code/webapp/../ACME-1-slug")).unwrap(),
            PathBuf::from("/Users/dev/code/ACME-1-slug")
        );
        assert_eq!(
            absolutize(Path::new("/a/./b/../c")).unwrap(),
            PathBuf::from("/a/c")
        );
    }

    #[test]
    fn absolutize_never_climbs_past_the_root() {
        assert_eq!(
            absolutize(Path::new("/../../x")).unwrap(),
            PathBuf::from("/x")
        );
    }

    #[test]
    fn absolutize_makes_a_relative_path_absolute() {
        let resolved = absolutize(Path::new("relative/path")).unwrap();
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("relative/path"));
    }

    #[test]
    fn absolutize_does_not_resolve_symlinks() {
        // Rewriting /var to /private/var would make displayed paths unrecognizable.
        let path = Path::new("/var/folders/x");
        assert_eq!(absolutize(path).unwrap(), PathBuf::from("/var/folders/x"));
    }

    #[test]
    fn unique_temp_paths_stay_siblings_and_do_not_reuse_a_name() {
        let path = Path::new("/tmp/config.toml");
        let first = unique_temp_path(path);
        let second = unique_temp_path(path);
        assert_eq!(first.parent(), path.parent());
        assert_ne!(first, second);
        assert_ne!(first, path.with_extension("toml.tmp"));
    }

    #[test]
    fn real_store_reads_and_reports_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".env");
        std::fs::write(&file, "HOST_PORT_WEB=8007\n").unwrap();

        let store = RealFileStore::new();
        assert!(store.exists(&file));
        assert!(!store.is_dir(&file));
        assert!(store.is_dir(dir.path()));
        assert_eq!(store.read_dotenv(&file).unwrap()["HOST_PORT_WEB"], "8007");

        let missing = dir.path().join("nope");
        assert!(!store.exists(&missing));
        assert!(matches!(
            store.read_to_string(&missing),
            Err(ConfigError::Io { .. })
        ));
    }

    #[test]
    fn is_dir_empty_distinguishes_empty_from_populated() {
        // `git worktree add` tolerates an empty target and rejects a populated one, so
        // preflight has to tell them apart.
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        let full = dir.path().join("full");
        std::fs::create_dir(&empty).unwrap();
        std::fs::create_dir(&full).unwrap();
        std::fs::write(full.join("f"), "x").unwrap();

        let store = RealFileStore::new();
        assert!(store.is_dir_empty(&empty).unwrap());
        assert!(!store.is_dir_empty(&full).unwrap());
        assert!(store.is_dir_empty(&dir.path().join("nope")).is_err());
    }
}

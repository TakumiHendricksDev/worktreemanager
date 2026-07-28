//! Resolving and merging the four config layers.
//!
//! # Precedence
//!
//! Most specific first:
//!
//! | Layer | Path | Committed? |
//! |---|---|---|
//! | [`ConfigLayer::Local`] | `<git-common-dir>/wtm.local.toml` | no — inside `.git` |
//! | [`ConfigLayer::Repo`] | `<repo>/wtm.toml` | yes |
//! | [`ConfigLayer::User`] | `~/.config/wtm/config.toml`, `[defaults]` table | n/a |
//! | [`ConfigLayer::BuiltIn`] | compiled in | — |
//!
//! The local layer lives inside the git directory on purpose: it configures a
//! repository without adding a file to it, which is what makes the app usable against
//! repos you do not own. And it is keyed on `--git-common-dir` rather than `--git-dir`
//! so every worktree of a repo shares one config — inside a linked worktree `.git` is
//! a *file*, and `--git-dir` points at a per-worktree subdirectory, so the naive path
//! would silently differ depending on which worktree you opened.
//!
//! # Merging is per-key, not per-file
//!
//! Merge happens on `toml::Value`, before deserialization, so a local override can set
//! one key without restating the table around it. Arrays *replace* rather than
//! concatenate: a user redefining `[[field]]` means "this is the form", not "append
//! six more fields to whatever the repo said".

use std::path::{Path, PathBuf};

use toml::Value;
use wtm_core::error::{ConfigError, ConfigLayer};

/// The compiled-in defaults, so a repo with no config still gets a working form.
pub const BUILT_IN_DEFAULTS: &str = include_str!("../../../defaults/wtm.default.toml");

/// Filename of the committed, team-shared layer.
pub const REPO_FILENAME: &str = "wtm.toml";

/// Filename of the untracked, per-checkout layer.
pub const LOCAL_FILENAME: &str = "wtm.local.toml";

/// One layer's contribution.
#[derive(Debug, Clone)]
pub struct LoadedLayer {
    pub layer: ConfigLayer,
    /// `None` for the built-in layer, which has no file on disk.
    pub path: Option<PathBuf>,
    pub value: Value,
    /// Raw text, kept for the trust hash and for span-accurate error reporting.
    pub source: Option<String>,
}

/// Where each layer's file lives for a given repository.
#[derive(Debug, Clone)]
pub struct LayerPaths {
    pub repo: PathBuf,
    pub local: PathBuf,
    pub user: PathBuf,
}

impl LayerPaths {
    /// `git_common_dir` must come from `git rev-parse --git-common-dir`. See the
    /// module docs for why nothing else will do.
    #[must_use]
    pub fn new(repo_root: &Path, git_common_dir: &Path, user_config: &Path) -> Self {
        Self {
            repo: repo_root.join(REPO_FILENAME),
            local: git_common_dir.join(LOCAL_FILENAME),
            user: user_config.to_path_buf(),
        }
    }
}

/// Parse a TOML *document*, mapping a syntax error to a located [`ConfigError`].
///
/// Note `toml::from_str::<Table>` rather than `str::parse::<Value>()`: in toml 1.x the
/// `FromStr` impl for `Value` parses a single value, so a whole document fails with a
/// confusing "unexpected content, expected nothing".
pub fn parse(path: &Path, layer: ConfigLayer, source: &str) -> Result<Value, ConfigError> {
    toml::from_str::<toml::Table>(source)
        .map(Value::Table)
        .map_err(|e| {
            let span = e.span();
            // toml reports a byte span; convert to a line/column a person can act on.
            let (line, column) = span
                .map(|s| offset_to_line_column(source, s.start))
                .map_or((None, None), |(l, c)| (Some(l), Some(c)));
            ConfigError::Invalid {
                path: path.to_path_buf(),
                layer,
                line,
                column,
                key: None,
                message: e.message().to_owned(),
            }
        })
}

/// Parse a TOML document into a [`Value`], without location mapping.
///
/// The plain-`toml` entry point the rest of the crate and its tests share, so nobody
/// reaches for `str::parse` and hits the single-value `FromStr` impl.
pub fn document(source: &str) -> Result<Value, toml::de::Error> {
    toml::from_str::<toml::Table>(source).map(Value::Table)
}

/// Byte offset to 1-based line and column.
fn offset_to_line_column(source: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(source.len());
    let before = &source[..clamped];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit_once('\n')
        .map_or(clamped, |(_, tail)| tail.len())
        + 1;
    (line, column)
}

/// Deep-merge `overlay` onto `base`.
///
/// Tables merge key by key. Everything else — including arrays — replaces. Array
/// replacement is the important choice: `[[field]]` describes *the* form, so a local
/// override that lists two fields means two fields, not two appended to the repo's six.
/// Concatenating would make it impossible to remove or reorder a field.
#[must_use]
pub fn merge(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Table(mut base_table), Value::Table(overlay_table)) => {
            for (key, overlay_value) in overlay_table {
                let merged = match base_table.remove(&key) {
                    Some(base_value) => merge(base_value, overlay_value),
                    None => overlay_value,
                };
                base_table.insert(key, merged);
            }
            Value::Table(base_table)
        }
        (_, overlay) => overlay,
    }
}

/// Merge every loaded layer, weakest first.
#[must_use]
pub fn merge_all(layers: &[LoadedLayer]) -> Value {
    layers
        .iter()
        .fold(Value::Table(toml::map::Map::new()), |acc, layer| {
            merge(acc, layer.value.clone())
        })
}

/// Extract the per-project overrides a user config may carry.
///
/// The user layer is a whole app config (registered projects, theme, exec settings),
/// not a project config. Only two parts of it apply to a project: a global `[defaults]`
/// table, and a `[projects.<path>]` table for this specific repository — the latter
/// winning, since it is more specific.
#[must_use]
pub fn project_overrides_from_user(user: &Value, repo_root: &Path) -> Value {
    let empty = Value::Table(toml::map::Map::new());

    let defaults = user
        .get("defaults")
        .cloned()
        .unwrap_or_else(|| empty.clone());

    let specific = user
        .get("projects")
        .and_then(Value::as_table)
        .and_then(|projects| {
            let key = repo_root.to_string_lossy();
            projects.get(key.as_ref())
        })
        .and_then(Value::as_table)
        // The registration entry also holds bookkeeping (`name`, `added`); only the
        // `config` sub-table is project configuration.
        .and_then(|entry| entry.get("config"))
        .cloned()
        .unwrap_or(empty);

    merge(defaults, specific)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(toml_text: &str) -> Value {
        document(toml_text).unwrap()
    }

    #[test]
    fn tables_merge_key_by_key() {
        let base = table("[naming]\nbranch = 'a'\ndirectory = 'b'\n");
        let overlay = table("[naming]\nbranch = 'z'\n");
        let merged = merge(base, overlay);

        let naming = merged.get("naming").unwrap();
        assert_eq!(
            naming.get("branch").unwrap().as_str(),
            Some("z"),
            "overlay wins"
        );
        assert_eq!(
            naming.get("directory").unwrap().as_str(),
            Some("b"),
            "an untouched key must survive"
        );
    }

    #[test]
    fn arrays_replace_rather_than_concatenate() {
        // So a local override can remove or reorder form fields, not only add.
        let base = table("[[field]]\nkey = 'a'\n\n[[field]]\nkey = 'b'\n");
        let overlay = table("[[field]]\nkey = 'only'\n");
        let merged = merge(base, overlay);

        let fields = merged.get("field").unwrap().as_array().unwrap();
        assert_eq!(fields.len(), 1, "arrays must replace: {fields:?}");
        assert_eq!(fields[0].get("key").unwrap().as_str(), Some("only"));
    }

    #[test]
    fn nested_tables_merge_to_arbitrary_depth() {
        let merged = merge(table("[a.b.c]\nx = 1\ny = 2\n"), table("[a.b.c]\ny = 99\n"));
        let c = merged.get("a").unwrap().get("b").unwrap().get("c").unwrap();
        assert_eq!(c.get("x").unwrap().as_integer(), Some(1));
        assert_eq!(c.get("y").unwrap().as_integer(), Some(99));
    }

    #[test]
    fn a_scalar_replaces_a_table_and_vice_versa() {
        // Type changes are the overlay author's business, not something to reconcile.
        assert!(
            merge(table("a = 1\n"), table("[a]\nb = 2\n"))
                .get("a")
                .unwrap()
                .is_table()
        );
        assert_eq!(
            merge(table("[a]\nb = 2\n"), table("a = 1\n"))
                .get("a")
                .unwrap()
                .as_integer(),
            Some(1)
        );
    }

    #[test]
    fn merge_all_applies_layers_weakest_first() {
        let layers = vec![
            LoadedLayer {
                layer: ConfigLayer::BuiltIn,
                path: None,
                value: table("[naming]\nbranch = 'builtin'\ndirectory = 'builtin'\n"),
                source: None,
            },
            LoadedLayer {
                layer: ConfigLayer::Repo,
                path: Some(PathBuf::from("/r/wtm.toml")),
                value: table("[naming]\nbranch = 'repo'\n"),
                source: None,
            },
            LoadedLayer {
                layer: ConfigLayer::Local,
                path: Some(PathBuf::from("/r/.git/wtm.local.toml")),
                value: table("[naming]\nbranch = 'local'\n"),
                source: None,
            },
        ];

        let merged = merge_all(&layers);
        let naming = merged.get("naming").unwrap();
        assert_eq!(
            naming.get("branch").unwrap().as_str(),
            Some("local"),
            "most specific wins"
        );
        assert_eq!(naming.get("directory").unwrap().as_str(), Some("builtin"));
    }

    #[test]
    fn the_built_in_defaults_parse_and_provide_naming() {
        // A repo with no config at all must still get a usable form.
        let defaults = document(BUILT_IN_DEFAULTS).unwrap();
        assert!(
            defaults.get("naming").is_some(),
            "defaults must supply naming"
        );
        assert!(
            defaults
                .get("field")
                .and_then(Value::as_array)
                .is_some_and(|f| !f.is_empty()),
            "defaults must supply at least one form field"
        );
    }

    #[test]
    fn user_overrides_combine_global_defaults_with_a_project_entry() {
        let user = table(
            "[defaults.naming]\nbranch = 'from-defaults'\ndirectory = 'from-defaults'\n\n\
             [projects.\"/Users/dev/repo\".config.naming]\nbranch = 'from-project'\n",
        );
        let overrides = project_overrides_from_user(&user, Path::new("/Users/dev/repo"));
        let naming = overrides.get("naming").unwrap();
        assert_eq!(
            naming.get("branch").unwrap().as_str(),
            Some("from-project"),
            "the specific entry beats the global default"
        );
        assert_eq!(
            naming.get("directory").unwrap().as_str(),
            Some("from-defaults")
        );
    }

    #[test]
    fn user_overrides_ignore_a_different_projects_entry() {
        let user = table("[projects.\"/other/repo\".config.naming]\nbranch = 'nope'\n");
        let overrides = project_overrides_from_user(&user, Path::new("/Users/dev/repo"));
        assert!(overrides.get("naming").is_none());
    }

    #[test]
    fn user_overrides_ignore_registration_bookkeeping() {
        // `name` sits next to `config` in a registration entry and is not config.
        let user = table("[projects.\"/r\"]\nname = 'r'\nadded = '2026-01-01'\n");
        let overrides = project_overrides_from_user(&user, Path::new("/r"));
        assert!(overrides.get("name").is_none(), "got {overrides:?}");
    }

    #[test]
    fn a_syntax_error_reports_a_line_and_column() {
        let source = "[naming]\nbranch = 'unterminated\n";
        let err = parse(Path::new("/r/wtm.toml"), ConfigLayer::Repo, source).unwrap_err();
        match err {
            ConfigError::Invalid {
                line,
                column,
                layer,
                ..
            } => {
                assert!(
                    line.is_some(),
                    "a syntax error must carry a line to jump to"
                );
                assert!(column.is_some());
                assert_eq!(layer, ConfigLayer::Repo);
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn line_and_column_conversion_is_one_based() {
        let source = "abc\ndefgh\n";
        assert_eq!(offset_to_line_column(source, 0), (1, 1));
        assert_eq!(offset_to_line_column(source, 2), (1, 3));
        assert_eq!(offset_to_line_column(source, 4), (2, 1));
        assert_eq!(offset_to_line_column(source, 6), (2, 3));
        // Past the end must clamp rather than panic.
        assert_eq!(offset_to_line_column(source, 9_999).0, 3);
    }

    #[test]
    fn layer_paths_use_the_git_common_dir_for_the_local_layer() {
        // The whole point: shared by every worktree of the repo.
        let paths = LayerPaths::new(
            Path::new("/Users/dev/code/webapp"),
            Path::new("/Users/dev/code/webapp/.git"),
            Path::new("/Users/dev/.config/wtm/config.toml"),
        );
        assert_eq!(paths.repo, PathBuf::from("/Users/dev/code/webapp/wtm.toml"));
        assert_eq!(
            paths.local,
            PathBuf::from("/Users/dev/code/webapp/.git/wtm.local.toml")
        );
    }
}

//! The Code tab's commands: a worktree's tree, and what is in it.
//!
//! Every command takes a project and a worktree and checks the second belongs to the first
//! before touching the disk, the way the Database commands do. `list_worktree_files` trusts its
//! worktree id as a working directory, which is fine for a list of names; these read files.
//!
//! The view types live here rather than in `view.rs` because nothing else renders them. They are
//! still an external contract — `types.ts` mirrors them by hand — and `the_tree_view_is_camel_case`
//! pins the keys.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use wtm_core::ports::exec::CancelToken;

use crate::app::App;
use crate::commands::{AppState, Reply, blocking};
use crate::view::ErrorView;

/// A worktree's files, as git lists them, plus what the tree must draw differently.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeTreeView {
    /// Tracked files and untracked ones that are not ignored.
    pub files: Vec<String>,
    /// Ignored paths, each at its shallowest ignored level. A directory ends in `/`.
    pub ignored: Vec<String>,
    /// Listed paths that are directories on disk: submodules, and links to directories.
    pub dirs: Vec<String>,
    pub symlinks: Vec<String>,
    /// Listed, but deleted from the working tree.
    pub missing: Vec<String>,
    /// A listing was cut short, so the tree is not the whole worktree.
    pub truncated: bool,
}

/// One entry of a directory git did not list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeEntryView {
    pub name: String,
    /// `file`, `dir` or `other`.
    pub kind: &'static str,
    pub symlink: bool,
}

/// A file for the viewer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeFileView {
    /// `None` for a binary file, which is described rather than shown.
    pub text: Option<String>,
    pub size: u64,
    /// The text stops before the file does. See `wtm_code::MAX_READ_BYTES`.
    pub truncated: bool,
    pub mtime_ms: Option<u64>,
    pub symlink: bool,
    /// It resolves to somewhere outside the worktree, through a link.
    pub outside: bool,
}

/// Whether an open file changed, without reading it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeStatView {
    pub path: String,
    /// `false` when the file is gone, or is no longer a file.
    pub exists: bool,
    pub mtime_ms: Option<u64>,
    pub size: u64,
}

/// Find in Files' toggles, as the popup sends them.
// Four independent switches, each its own button in the popup. Folding any two into an enum would
// invent states the popup does not have.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeSearchOptionsView {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// Comma-separated globs; see `wtm_code::Mask`.
    pub mask: String,
    /// Also search what `.gitignore` hides, which is walked rather than listed and is bounded.
    pub include_ignored: bool,
}

/// One matching line. See `wtm_code::Hit` for what the numbers count.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeHitView {
    pub path: String,
    pub line: u32,
    pub text: String,
    pub offset: u32,
    pub ranges: Vec<[u32; 2]>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeSearchView {
    pub hits: Vec<CodeHitView>,
    pub matches: usize,
    pub files: usize,
    /// It stopped at a cap, so there are more matches than these.
    pub truncated: bool,
    /// The walk of ignored files stopped before it had seen them all.
    pub ignored_stopped: bool,
}

/// What a worktree has changed, for the Changes view and the tree's colours.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeChangesView {
    /// `branch` or `uncommitted`: the scope actually used. A branch with no base to compare with
    /// falls back to its uncommitted changes, and says so here.
    pub scope: &'static str,
    /// What the changes are against, to show: the base branch's name, or `HEAD`.
    pub against: String,
    /// The revision `code_file_diff` and `code_base_version` compare with — the merge base, or
    /// `HEAD`. Handed back by the frontend as it was given.
    pub rev: String,
    pub changes: Vec<CodeChangeView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeChangeView {
    pub path: String,
    /// `added`, `modified`, `deleted`, `renamed`, `untracked` or `other`.
    pub kind: &'static str,
    /// Where a renamed file was.
    pub from: Option<String>,
}

/// One region of a file that differs from the revision. See `wtm_core::model::Hunk`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeHunkView {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub removed: Vec<String>,
}

/// One definition, for Go to Class, Go to Symbol and ⌘-click.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeSymbolView {
    pub name: String,
    /// `class`, `function`, `method`, `interface`, `type`, `enum`, `struct`, `trait`, `module` or
    /// `constant`.
    pub kind: &'static str,
    pub container: Option<String>,
    pub path: String,
    pub line: u32,
}

impl From<wtm_code::Symbol> for CodeSymbolView {
    fn from(symbol: wtm_code::Symbol) -> Self {
        Self {
            kind: symbol.kind.as_str(),
            name: symbol.name,
            container: symbol.container,
            path: symbol.path,
            line: symbol.line,
        }
    }
}

impl From<wtm_code::CodeError> for ErrorView {
    fn from(err: wtm_code::CodeError) -> Self {
        let kind = match &err {
            wtm_code::CodeError::BadPath(_) => "badPath",
            wtm_code::CodeError::NotFound(_) => "notFound",
            wtm_code::CodeError::NotADirectory(_) => "notADirectory",
            wtm_code::CodeError::NotAFile(_) => "notAFile",
            wtm_code::CodeError::Io { .. } => "io",
            wtm_code::CodeError::BadQuery(_) => "badQuery",
            wtm_code::CodeError::Cancelled => "cancelled",
        };
        Self::new(kind, err.to_string())
    }
}

/// The directory a Code command may read, once the worktree is known to be the project's.
fn root_of(app: &App, project_id: &str, worktree_id: &str) -> Result<PathBuf, ErrorView> {
    let project = app.project(project_id)?;
    Ok(app.worktree(&project, worktree_id)?.path)
}

/// Everything the tree draws, in one round trip.
#[tauri::command]
pub async fn code_tree(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
) -> Reply<CodeTreeView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        let files = app.git.files(&root)?;
        let ignored = app.git.ignored(&root)?;
        let kinds = wtm_code::classify(
            &root,
            files.paths.iter().chain(&ignored.paths).map(String::as_str),
        );
        Ok(CodeTreeView {
            truncated: files.truncated || ignored.truncated,
            files: files.paths,
            ignored: ignored.paths,
            dirs: kinds.dirs,
            symlinks: kinds.symlinks,
            missing: kinds.missing,
        })
    })
    .await
}

/// One level of a directory the tree has no listing for: ignored, a submodule, or linked.
#[tauri::command]
pub async fn code_list_dir(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    dir: String,
) -> Reply<Vec<CodeEntryView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        Ok(wtm_code::list_dir(&root, &dir)?
            .into_iter()
            .map(|entry| CodeEntryView {
                name: entry.name,
                kind: match entry.kind {
                    wtm_code::EntryKind::File => "file",
                    wtm_code::EntryKind::Dir => "dir",
                    wtm_code::EntryKind::Other => "other",
                },
                symlink: entry.symlink,
            })
            .collect())
    })
    .await
}

/// A revision the frontend hands back must be one `code_changes` gave it: a commit id or `HEAD`.
///
/// `--end-of-options` already stops git reading it as a flag. This is the second half: nothing but
/// a hash or `HEAD` reaches git at all, so no `rev:path` spelling can name something else.
fn checked_rev(rev: &str) -> Result<&str, ErrorView> {
    let hash = rev.len() >= 7 && rev.chars().all(|c| c.is_ascii_hexdigit());
    if rev == "HEAD" || hash {
        Ok(rev)
    } else {
        Err(ErrorView::new(
            "badRevision",
            format!("`{rev}` is not a revision to compare with"),
        ))
    }
}

/// What the worktree has changed: against the base branch, or only what is uncommitted.
///
/// Against the base branch means against the *merge base* of `HEAD` and that branch, so the view
/// shows what this branch did and not what the base has done since it forked. It is compared with
/// the working tree, so committed and uncommitted work both count. Untracked files are added.
#[tauri::command]
pub async fn code_changes(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    scope: String,
) -> Reply<CodeChangesView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let worktrees = app.git.list_worktrees(&project.root)?;
        let root = worktrees
            .iter()
            .find(|w| w.id.as_str() == worktree_id)
            .map(|w| w.path.clone())
            .ok_or_else(|| wtm_core::error::WtmError::UnknownWorktree(worktree_id.clone()))?;

        let branch = if scope == "branch" {
            app.base_branch(&project, &worktrees).and_then(|base| {
                let fork = app.git.merge_base(&root, "HEAD", &base).ok().flatten()?;
                Some((fork.as_str().to_owned(), base))
            })
        } else {
            None
        };
        let (used, rev, against) = match branch {
            Some((rev, base)) => ("branch", rev, base),
            None => ("uncommitted", "HEAD".to_owned(), "HEAD".to_owned()),
        };

        // A repository with no commits has no `HEAD` to diff against; everything in it is new.
        let tracked = app.git.changed_paths(&root, &rev).unwrap_or_default();
        let mut changes: Vec<CodeChangeView> = tracked
            .into_iter()
            .map(|change| {
                let (kind, from) = match change.kind {
                    wtm_core::model::ChangeKind::Added => ("added", None),
                    wtm_core::model::ChangeKind::Modified => ("modified", None),
                    wtm_core::model::ChangeKind::Deleted => ("deleted", None),
                    wtm_core::model::ChangeKind::Renamed { from } => ("renamed", Some(from)),
                    wtm_core::model::ChangeKind::Untracked => ("untracked", None),
                    wtm_core::model::ChangeKind::Other => ("other", None),
                };
                CodeChangeView {
                    path: change.path,
                    kind,
                    from,
                }
            })
            .collect();
        changes.extend(
            app.git
                .untracked(&root)?
                .paths
                .into_iter()
                .map(|path| CodeChangeView {
                    path,
                    kind: "untracked",
                    from: None,
                }),
        );
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(CodeChangesView {
            scope: used,
            against,
            rev,
            changes,
        })
    })
    .await
}

/// How one file differs from the Changes view's revision, for the viewer's gutter.
#[tauri::command]
pub async fn code_file_diff(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    path: String,
    rev: String,
    from: Option<String>,
) -> Reply<Vec<CodeHunkView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        wtm_code::resolve(&root, &path)?;
        if let Some(from) = &from {
            wtm_code::resolve(&root, from)?;
        }
        let hunks = app
            .git
            .file_hunks(&root, checked_rev(&rev)?, &path, from.as_deref())?;
        Ok(hunks
            .into_iter()
            .map(|hunk| CodeHunkView {
                old_start: hunk.old_start,
                old_lines: hunk.old_lines,
                new_start: hunk.new_start,
                new_lines: hunk.new_lines,
                removed: hunk.removed,
            })
            .collect())
    })
    .await
}

/// A file as it was at the Changes view's revision: what a deleted file held.
#[tauri::command]
pub async fn code_base_version(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    path: String,
    rev: String,
) -> Reply<Option<String>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        wtm_code::resolve(&root, &path)?;
        Ok(app.git.show_file(&root, checked_rev(&rev)?, &path)?)
    })
    .await
}

/// The symbol indexes of the worktrees looked at most recently, newest last.
///
/// Two, because an index holds every definition in a worktree and a person reads one worktree at a
/// time — the second is the one they were in a moment ago. Older ones are rebuilt when they are
/// asked for again, which costs a read of their source files.
static SYMBOLS: parking_lot::Mutex<Vec<(String, wtm_code::SymbolIndex)>> =
    parking_lot::Mutex::new(Vec::new());

/// Run `with` on a worktree's symbol index, refreshed against git's file list when `refresh`.
///
/// Refreshing is a `stat` per file and a read of only the ones that changed, so it is asked for when
/// a palette opens rather than on every keystroke in it.
fn with_symbols<T>(
    app: &App,
    root: &std::path::Path,
    worktree_id: &str,
    refresh: bool,
    with: impl FnOnce(&wtm_code::SymbolIndex) -> T,
) -> Result<T, ErrorView> {
    let mut cache = SYMBOLS.lock();
    let known = cache.iter().position(|(id, _)| id == worktree_id);
    let (id, mut index) = match known {
        Some(at) => cache.remove(at),
        None => (worktree_id.to_owned(), wtm_code::SymbolIndex::default()),
    };
    if refresh || known.is_none() {
        index.update(root, &app.git.files(root)?.paths);
    }
    let answer = with(&index);
    cache.push((id, index));
    if cache.len() > 2 {
        cache.remove(0);
    }
    Ok(answer)
}

/// Go to Class (`types_only`) and Go to Symbol: the best definitions for a query.
#[tauri::command]
pub async fn code_symbols(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    query: String,
    types_only: bool,
    refresh: bool,
) -> Reply<Vec<CodeSymbolView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        with_symbols(&app, &root, &worktree_id, refresh, |index| {
            index
                .query(&query, types_only, 60)
                .into_iter()
                .map(CodeSymbolView::from)
                .collect()
        })
    })
    .await
}

/// Every definition with exactly this name: what ⌘-click on a name goes to.
#[tauri::command]
pub async fn code_definitions(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    name: String,
) -> Reply<Vec<CodeSymbolView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        with_symbols(&app, &root, &worktree_id, true, |index| {
            index
                .definitions(&name)
                .into_iter()
                .map(CodeSymbolView::from)
                .collect()
        })
    })
    .await
}

/// The search that is running, so the next one can stop it.
///
/// One for the whole app, not one per worktree: there is one Find in Files popup, and a query typed
/// into it supersedes the last whichever worktree that was for. A static rather than a field on
/// `App` because nothing else about the app has any business with it.
static CURRENT_SEARCH: parking_lot::Mutex<Option<CancelToken>> = parking_lot::Mutex::new(None);

/// Find in Files.
///
/// Starting a search cancels the one before it, which the popup does on every pause in typing; the
/// cancelled one answers `cancelled`, and the popup ignores answers it has moved past anyway.
#[tauri::command]
pub async fn code_search(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    query: String,
    options: CodeSearchOptionsView,
) -> Reply<CodeSearchView> {
    let cancel = CancelToken::new();
    if let Some(previous) = CURRENT_SEARCH.lock().replace(cancel.clone()) {
        previous.cancel();
    }
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        let mut files = app.git.files(&root)?.paths;
        let mut ignored_stopped = false;
        if options.include_ignored {
            let ignored = app.git.ignored(&root)?;
            let (more, stopped) =
                wtm_code::walk_ignored(&root, &ignored.paths, wtm_code::MAX_IGNORED_FILES, &|| {
                    cancel.is_cancelled()
                });
            files.extend(more);
            ignored_stopped = stopped;
        }
        let results = wtm_code::search(
            &root,
            &files,
            &query,
            &wtm_code::SearchOptions {
                case_sensitive: options.case_sensitive,
                whole_word: options.whole_word,
                regex: options.regex,
                mask: options.mask,
            },
            &cancel,
        )?;
        Ok(CodeSearchView {
            hits: results
                .hits
                .into_iter()
                .map(|hit| CodeHitView {
                    path: hit.path,
                    line: hit.line,
                    text: hit.text,
                    offset: hit.offset,
                    ranges: hit.ranges.into_iter().map(|(a, b)| [a, b]).collect(),
                })
                .collect(),
            matches: results.matches,
            files: results.files,
            truncated: results.truncated,
            ignored_stopped,
        })
    })
    .await
}

/// One file, for the viewer.
#[tauri::command]
pub async fn code_read_file(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    path: String,
) -> Reply<CodeFileView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        let file = wtm_code::read_file(&root, &path)?;
        Ok(CodeFileView {
            text: match file.content {
                wtm_code::Content::Text(text) => Some(text),
                wtm_code::Content::Binary => None,
            },
            size: file.size,
            truncated: file.truncated,
            mtime_ms: file.mtime_ms,
            symlink: file.symlink,
            outside: file.outside,
        })
    })
    .await
}

/// When each open file last changed, so a refresh re-reads only the ones that did.
#[tauri::command]
pub async fn code_stat(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    paths: Vec<String>,
) -> Reply<Vec<CodeStatView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        Ok(paths
            .into_iter()
            .map(|path| match wtm_code::stat(&root, &path) {
                Some((mtime_ms, size)) => CodeStatView {
                    path,
                    exists: true,
                    mtime_ms,
                    size,
                },
                None => CodeStatView {
                    path,
                    exists: false,
                    mtime_ms: None,
                    size: 0,
                },
            })
            .collect())
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// The serialized key names are an external contract with `types.ts`.
    #[test]
    fn the_tree_view_is_camel_case() {
        let view = CodeTreeView {
            files: vec![],
            ignored: vec![],
            dirs: vec![],
            symlinks: vec![],
            missing: vec![],
            truncated: false,
        };
        let json = serde_json::to_value(&view).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "dirs",
                "files",
                "ignored",
                "missing",
                "symlinks",
                "truncated"
            ]
        );
    }

    #[test]
    fn the_file_view_is_camel_case() {
        let view = CodeFileView {
            text: None,
            size: 0,
            truncated: false,
            mtime_ms: Some(1),
            symlink: false,
            outside: false,
        };
        let json = serde_json::to_value(&view).unwrap();
        let object = json.as_object().unwrap();
        assert!(object.contains_key("mtimeMs"), "{object:?}");
        assert!(!object.contains_key("mtime_ms"));
    }

    #[test]
    fn only_a_commit_id_or_head_is_accepted_as_a_revision_to_compare_with() {
        assert!(checked_rev("HEAD").is_ok());
        assert!(checked_rev("3f2a9c1").is_ok());
        assert!(checked_rev("3f2a9c1e0b9d7a6c5f4e3d2c1b0a998877665544").is_ok());
        for bad in [
            "--output=/tmp/x",
            "main",
            "HEAD~1",
            "abc",
            "3f2a9c1:secret",
            "",
        ] {
            assert!(checked_rev(bad).is_err(), "{bad}");
        }
    }
}

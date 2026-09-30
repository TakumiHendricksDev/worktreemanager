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

use serde::Serialize;

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

impl From<wtm_code::CodeError> for ErrorView {
    fn from(err: wtm_code::CodeError) -> Self {
        let kind = match &err {
            wtm_code::CodeError::BadPath(_) => "badPath",
            wtm_code::CodeError::NotFound(_) => "notFound",
            wtm_code::CodeError::NotADirectory(_) => "notADirectory",
            wtm_code::CodeError::NotAFile(_) => "notAFile",
            wtm_code::CodeError::Io { .. } => "io",
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
}

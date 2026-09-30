//! What is on disk behind a path git listed, and what is in a directory git did not list.

use std::fs;
use std::path::Path;

use crate::{CodeError, resolve};

/// What a directory entry is, once any symlink is followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// A socket, a FIFO, a device, or a link to nothing. Shown, never opened: reading a FIFO
    /// blocks until something writes to it.
    Other,
}

/// One entry of [`list_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub symlink: bool,
}

/// The children of one directory, unsorted.
///
/// One level only, and never recursive, because the directories this is for are the ones git
/// did not list — an ignored `node_modules`, a submodule, a linked directory — and walking one of
/// those is hundreds of thousands of entries, or a symlink loop. The tree asks again for each
/// level the user opens.
pub fn list_dir(root: &Path, rel: &str) -> Result<Vec<Entry>, CodeError> {
    let dir = resolve(root, rel)?;
    let metadata = fs::metadata(&dir).map_err(|e| CodeError::io(rel, &e))?;
    if !metadata.is_dir() {
        return Err(CodeError::NotADirectory(rel.to_owned()));
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| CodeError::io(rel, &e))? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        // `.git` is git's own, and in a linked worktree it is a one-line file pointing elsewhere.
        // Neither is anything a reviewer is looking for.
        if name == ".git" {
            continue;
        }
        let symlink = entry.file_type().is_ok_and(|t| t.is_symlink());
        entries.push(Entry {
            kind: kind_of(&entry.path()),
            name,
            symlink,
        });
    }
    Ok(entries)
}

/// The paths in a git listing that are not ordinary files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kinds {
    /// Directories behind a single listed path: a submodule, or a symlink to a directory. The
    /// tree shows them as folders whose contents come from [`list_dir`].
    pub dirs: Vec<String>,
    /// Paths that are themselves symlinks, whatever they point at.
    pub symlinks: Vec<String>,
    /// Listed but gone: tracked files deleted from the working tree. `git ls-files --cached`
    /// still reports them, and opening one would fail.
    pub missing: Vec<String>,
}

/// Sort a git listing's paths into the ones the tree has to draw differently.
///
/// Returns only the exceptions, since nearly every path is a plain file. A trailing `/` — how
/// `ls-files --directory` marks an ignored directory — is dropped before looking.
pub fn classify<'a>(root: &Path, paths: impl IntoIterator<Item = &'a str>) -> Kinds {
    let mut kinds = Kinds::default();
    for listed in paths {
        let rel = listed.strip_suffix('/').unwrap_or(listed);
        let Ok(path) = resolve(root, rel) else {
            continue;
        };
        match fs::symlink_metadata(&path) {
            Err(_) => kinds.missing.push(rel.to_owned()),
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    kinds.symlinks.push(rel.to_owned());
                }
                // A listed path that is a directory, whether by being one (a submodule's
                // gitlink) or by pointing at one. An ignored directory git already marked is
                // left alone: the tree knows it is a folder from the slash.
                if listed == rel && fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                    kinds.dirs.push(rel.to_owned());
                }
            }
        }
    }
    kinds
}

fn kind_of(path: &Path) -> EntryKind {
    match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => EntryKind::Dir,
        Ok(meta) if meta.is_file() => EntryKind::File,
        _ => EntryKind::Other,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn names(mut entries: Vec<Entry>) -> Vec<(String, EntryKind, bool)> {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
            .into_iter()
            .map(|e| (e.name, e.kind, e.symlink))
            .collect()
    }

    #[test]
    fn a_directory_lists_its_files_and_folders_one_level_deep() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg/lib")).unwrap();
        fs::write(dir.path().join("node_modules/pkg/index.js"), "").unwrap();

        assert_eq!(
            names(list_dir(dir.path(), "node_modules/pkg").unwrap()),
            [
                ("index.js".to_owned(), EntryKind::File, false),
                ("lib".to_owned(), EntryKind::Dir, false),
            ]
        );
    }

    #[test]
    fn a_symlinked_directory_is_listed_as_a_linked_folder_and_git_s_own_file_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("shared")).unwrap();
        fs::write(dir.path().join(".git"), "gitdir: /elsewhere\n").unwrap();

        assert_eq!(
            names(list_dir(dir.path(), "").unwrap()),
            [("shared".to_owned(), EntryKind::Dir, true)]
        );
    }

    #[test]
    fn listing_a_file_or_a_missing_directory_says_which() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();

        assert_eq!(
            list_dir(dir.path(), "a.txt"),
            Err(CodeError::NotADirectory("a.txt".to_owned()))
        );
        assert_eq!(
            list_dir(dir.path(), "gone"),
            Err(CodeError::NotFound("gone".to_owned()))
        );
    }

    #[test]
    fn classify_reports_only_the_paths_that_are_not_plain_files() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("plain.py"), "").unwrap();
        fs::create_dir(dir.path().join("vendored")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join(".claude")).unwrap();
        fs::write(elsewhere.path().join("note.md"), "").unwrap();
        std::os::unix::fs::symlink(
            elsewhere.path().join("note.md"),
            dir.path().join("linked.md"),
        )
        .unwrap();
        fs::create_dir(dir.path().join("node_modules")).unwrap();

        let kinds = classify(
            dir.path(),
            [
                "plain.py",
                "vendored",
                ".claude",
                "linked.md",
                "deleted.py",
                "node_modules/",
            ],
        );

        assert_eq!(kinds.dirs, ["vendored", ".claude"]);
        assert_eq!(kinds.symlinks, [".claude", "linked.md"]);
        assert_eq!(kinds.missing, ["deleted.py"]);
    }
}

//! Turning a path the frontend sent into one on disk.

use std::path::{Component, Path, PathBuf};

use crate::CodeError;

/// `root` joined with `rel`, if `rel` names something inside it.
///
/// `rel` must be relative and made only of ordinary components: no `..`, no `.`, no root, no
/// Windows prefix, and no NUL. The empty string is the root itself.
///
/// This is *lexical* containment, and deliberately so. The only caller is the app's own webview,
/// which can already read any file through the composer's attachments, so this is not the
/// boundary that keeps a hostile page out — nothing a page can reach calls it. What it stops is a
/// confused path: an agent's reply naming `../../.ssh/config`, turned into a link and clicked.
///
/// Symlinks are *not* resolved here, so a link inside the worktree that points outside it is
/// followed when read. That is the point of showing one — a `.claude` directory linked in from
/// elsewhere is exactly what a reviewer opens — and the reader reports it rather than refusing.
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf, CodeError> {
    if rel.contains('\0') {
        return Err(CodeError::BadPath(rel.replace('\0', "\\0")));
    }
    let relative = Path::new(rel);
    let mut joined = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => joined.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(CodeError::BadPath(rel.to_owned()));
            }
        }
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_relative_path_is_joined_onto_the_root() {
        assert_eq!(
            resolve(Path::new("/w"), "src/app.py").unwrap(),
            PathBuf::from("/w/src/app.py")
        );
    }

    #[test]
    fn the_empty_path_is_the_root() {
        assert_eq!(resolve(Path::new("/w"), "").unwrap(), PathBuf::from("/w"));
    }

    #[test]
    fn a_path_that_climbs_out_is_refused_wherever_the_climb_is() {
        for rel in ["../secret", "src/../../secret", "src/.."] {
            assert_eq!(
                resolve(Path::new("/w"), rel),
                Err(CodeError::BadPath(rel.to_owned())),
                "{rel}"
            );
        }
    }

    #[test]
    fn an_absolute_path_is_refused_rather_than_replacing_the_root() {
        // `Path::join` with an absolute path discards the root entirely, which is the bug this
        // function exists to make impossible.
        assert!(matches!(
            resolve(Path::new("/w"), "/etc/passwd"),
            Err(CodeError::BadPath(_))
        ));
    }

    #[test]
    fn a_nul_byte_is_refused() {
        assert!(matches!(
            resolve(Path::new("/w"), "src/a\0b"),
            Err(CodeError::BadPath(_))
        ));
    }

    #[test]
    fn a_dot_component_is_refused_because_git_never_prints_one() {
        assert!(matches!(
            resolve(Path::new("/w"), "./src"),
            Err(CodeError::BadPath(_))
        ));
    }
}

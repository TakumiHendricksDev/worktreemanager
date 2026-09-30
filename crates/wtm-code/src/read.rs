//! Reading one file for the viewer.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::{CodeError, resolve};

/// The most of a file the viewer is sent. Past this it shows the start and says it stopped.
///
/// Five mebibytes is a large source file — a generated bundle, a lockfile, a data fixture — and
/// still an instant parse for `CodeMirror`, which only highlights what is on screen. Anything
/// bigger is not being *read* by anyone, and sending it whole would sit in the IPC channel and
/// in the tab's memory for nothing.
pub const MAX_READ_BYTES: u64 = 5 * 1024 * 1024;

/// How far into a file to look for a NUL byte before calling it binary.
///
/// Git's own heuristic, and for the same reason: text almost never contains one, and almost
/// every binary format has one early.
const SNIFF_BYTES: usize = 8 * 1024;

/// What a file holds, as far as the viewer is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    /// Described, not shown: sending a binary file's bytes as text helps nobody.
    Binary,
}

/// A file's contents, as the viewer shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileText {
    pub content: Content,
    /// On disk, in bytes, which is more than the text when `truncated`.
    pub size: u64,
    pub truncated: bool,
    /// Milliseconds since the epoch, so the viewer can tell a changed file from an unchanged one
    /// without reading it again. `None` on a filesystem that keeps no times.
    pub mtime_ms: Option<u64>,
    /// The path is a link.
    pub symlink: bool,
    /// The file it resolves to is outside the worktree — through a link, somewhere on the way.
    pub outside: bool,
}

/// Read `rel` for display.
///
/// Refuses anything that is not a regular file once links are followed. A directory is the
/// obvious case; a FIFO is the important one, because opening it for reading blocks until
/// something writes to the other end, which would hang the command forever.
pub fn read_file(root: &Path, rel: &str) -> Result<FileText, CodeError> {
    let path = resolve(root, rel)?;
    let link = fs::symlink_metadata(&path).map_err(|e| CodeError::io(rel, &e))?;
    let meta = fs::metadata(&path).map_err(|e| CodeError::io(rel, &e))?;
    if !meta.is_file() {
        return Err(CodeError::NotAFile(rel.to_owned()));
    }

    let mut bytes = Vec::new();
    fs::File::open(&path)
        .and_then(|file| file.take(MAX_READ_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| CodeError::io(rel, &e))?;
    let truncated = bytes.len() as u64 > MAX_READ_BYTES;
    if truncated {
        // End on a line boundary, so the last line shown is a whole one.
        let cut = usize::try_from(MAX_READ_BYTES).unwrap_or(usize::MAX);
        bytes.truncate(cut);
        if let Some(end) = bytes.iter().rposition(|b| *b == b'\n') {
            bytes.truncate(end + 1);
        }
    }

    let binary = bytes.iter().take(SNIFF_BYTES).any(|b| *b == 0);
    let outside = match (fs::canonicalize(&path), fs::canonicalize(root)) {
        (Ok(file), Ok(root)) => !file.starts_with(root),
        _ => false,
    };

    Ok(FileText {
        content: if binary {
            Content::Binary
        } else {
            Content::Text(String::from_utf8_lossy(&bytes).into_owned())
        },
        size: meta.len(),
        truncated,
        mtime_ms: mtime_ms(&meta),
        symlink: link.file_type().is_symlink(),
        outside,
    })
}

/// When a file last changed and how big it is, or `None` when it is gone.
///
/// What a refresh asks of every open tab: cheaper than reading each one again, and enough to know
/// which ones did change.
#[must_use]
pub fn stat(root: &Path, rel: &str) -> Option<(Option<u64>, u64)> {
    let path = resolve(root, rel).ok()?;
    let meta = fs::metadata(path).ok()?;
    meta.is_file().then(|| (mtime_ms(&meta), meta.len()))
}

fn mtime_ms(meta: &fs::Metadata) -> Option<u64> {
    let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since.as_millis()).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_text_file_comes_back_whole_with_its_size_and_time() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("app.py"), "print('héllo')\n").unwrap();

        let file = read_file(dir.path(), "app.py").unwrap();

        assert_eq!(file.content, Content::Text("print('héllo')\n".to_owned()));
        assert_eq!(file.size, 16);
        assert!(!file.truncated && !file.symlink && !file.outside);
        assert!(file.mtime_ms.is_some());
    }

    #[test]
    fn a_file_with_a_nul_near_the_start_is_binary_and_sends_no_text() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("logo.png"),
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR",
        )
        .unwrap();

        let file = read_file(dir.path(), "logo.png").unwrap();

        assert_eq!(file.content, Content::Binary);
        assert_eq!(file.size, 16);
    }

    #[test]
    fn a_file_past_the_cap_is_cut_on_a_line_boundary_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let line = "x".repeat(99) + "\n";
        let lines = usize::try_from(MAX_READ_BYTES).unwrap() / line.len() + 10;
        fs::write(dir.path().join("big.txt"), line.repeat(lines)).unwrap();

        let file = read_file(dir.path(), "big.txt").unwrap();

        let Content::Text(text) = &file.content else {
            panic!("text")
        };
        assert!(file.truncated);
        assert!(text.len() as u64 <= MAX_READ_BYTES);
        assert!(text.ends_with('\n'), "the last line shown is a whole one");
        assert_eq!(file.size, (line.len() * lines) as u64);
    }

    #[test]
    fn a_directory_and_a_fifo_are_refused_rather_than_read() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        assert_eq!(
            read_file(dir.path(), "src"),
            Err(CodeError::NotAFile("src".to_owned()))
        );

        // A FIFO would block `open` until a writer arrived. Made with `mkfifo` through the one
        // runner every spawn goes through, rather than libc, which this crate does not link.
        let fifo = dir.path().join("pipe");
        let runner = wtm_exec::Runner::with_probed_path(None);
        let made = wtm_core::ports::exec::CommandRunner::run(
            &runner,
            &wtm_core::ports::exec::Invocation::new(
                vec!["mkfifo".to_owned(), fifo.to_string_lossy().into_owned()],
                dir.path(),
                5_000,
            ),
            &wtm_core::ports::exec::CancelToken::new(),
        );
        assert!(made.is_ok(), "mkfifo: {made:?}");
        assert_eq!(
            read_file(dir.path(), "pipe"),
            Err(CodeError::NotAFile("pipe".to_owned()))
        );
    }

    #[test]
    fn a_link_out_of_the_worktree_is_followed_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("settings.json"), "{}\n").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join(".claude")).unwrap();

        let file = read_file(dir.path(), ".claude/settings.json").unwrap();

        assert_eq!(file.content, Content::Text("{}\n".to_owned()));
        assert!(file.outside, "the file is outside the worktree");
        assert!(
            !file.symlink,
            "the file itself is not a link; its folder is"
        );
    }

    #[test]
    fn a_missing_file_is_not_found_and_stat_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_file(dir.path(), "gone.py"),
            Err(CodeError::NotFound("gone.py".to_owned()))
        );
        assert_eq!(stat(dir.path(), "gone.py"), None);
    }
}

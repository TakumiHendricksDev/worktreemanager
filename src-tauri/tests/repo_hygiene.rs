//! wtm must not know about any particular project.
//!
//! The design claim is that a project's entire worktree convention lives in its own TOML, and
//! that wtm's source contains nothing specific to any one of them. That claim held right up to
//! the point where the first real project's name, its ticket keys, its branch names and one
//! developer's home directory had been copied into test fixtures and doc comments across two
//! dozen files — none of it load-bearing, all of it published.
//!
//! So: a lint, not a promise. Every tracked text file is scanned for identifiers that belong to
//! a specific project or machine. It runs in `just check`, which means the next person to paste
//! a real path into a fixture finds out immediately rather than at `git push`.
//!
//! Adding to `FORBIDDEN` is cheap. Use it for anything that would embarrass you in a public
//! repository: employer names, internal hostnames, ticket keys, absolute home directories.

// `Command::new` is banned workspace-wide so every spawn in the *app* goes through
// `wtm-exec`'s wrapper and gets a timeout, a sanitized environment and a tracing span. None of
// that applies to a test asking git for its own file list, and routing this through the adapter
// would mean building one just to read a list of paths.
#![allow(clippy::unwrap_used, clippy::disallowed_methods)]

use std::path::{Path, PathBuf};

/// Identifiers that must not appear anywhere in the tree.
///
/// Assembled with `concat!` so this file does not match its own list — the test would
/// otherwise be its own only failure, which is a genuinely annoying five minutes.
///
/// `word` means "only when not part of a longer word". It exists for exactly one reason:
/// `appears` and `disappears` contain a certain fruit, and eleven of them are load-bearing
/// prose.
const FORBIDDEN: &[(&str, bool)] = &[
    (concat!("pe", "ars"), true),
    (concat!("can", "opy"), true),
    (concat!("canopy", "team"), false),
    (concat!("/Users/", "takumi"), false),
    (concat!("Sites/", "Canopy"), false),
];

/// Files exempt for a stated reason.
const SKIP_FILES: &[&str] = &[
    // This file, which necessarily names what it forbids.
    "repo_hygiene.rs",
    // Resolved dependency graph: crate names are not ours to choose.
    "Cargo.lock",
];

#[test]
fn no_project_specific_identifiers_are_committed() {
    let root = workspace_root();
    let mut findings: Vec<String> = Vec::new();

    for relative in tracked_files(&root) {
        if SKIP_FILES.contains(&file_name(&relative)) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&relative)) else {
            continue; // Binary, or deleted since it was staged: nothing to scan.
        };

        for (needle, word) in FORBIDDEN {
            for (number, line) in text.lines().enumerate() {
                if !contains(line, needle, *word) {
                    continue;
                }
                findings.push(format!(
                    "{}:{}: {}",
                    relative.display(),
                    number + 1,
                    line.trim().chars().take(110).collect::<String>()
                ));
            }
        }
    }

    assert!(
        findings.is_empty(),
        "{} project-specific reference(s) found. wtm is meant to be repository-agnostic; \
         use a neutral placeholder, or set the value from an environment variable if a test \
         genuinely needs a real checkout (see WTM_TEST_REPO).\n{}",
        findings.len(),
        findings.join("\n")
    );
}

/// Case-insensitive substring search, optionally requiring whole-word boundaries.
fn contains(line: &str, needle: &str, word: bool) -> bool {
    let haystack = line.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let bytes = haystack.as_bytes();

    let mut from = 0;
    while let Some(offset) = haystack[from..].find(&needle) {
        let start = from + offset;
        let end = start + needle.len();
        if !word {
            return true;
        }
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphabetic();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphabetic();
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Every path git tracks, relative to the workspace root.
///
/// Asked of git rather than walked from disk, which is the difference between checking what is
/// *committed* — what this test is named for and what actually gets published — and checking
/// whatever happens to be sitting in the working tree. Walking meant build output and vendored
/// code needed a skip list, and it still failed on things no skip list anticipates: a
/// `JetBrains` `.idea/workspace.xml` records the absolute path of the project you opened, so the
/// check went red over a gitignored file that was never going anywhere. Which editor someone
/// uses is not a repository-hygiene problem.
fn tracked_files(root: &Path) -> Vec<PathBuf> {
    // `-z`, because a filename may contain a newline and `lines()` would then invent two.
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .expect("git ls-files");

    assert!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

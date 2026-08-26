//! Parsers for git's machine-readable output.
//!
//! Pure functions over `&str`, so the tricky part — the grammar — is testable
//! without a repository, while the [`crate::cli`] adapter that produces the input is
//! tested against a real `git`.
//!
//! # Why `--porcelain -z` and never the human form
//!
//! `git worktree list` prints space-padded columns whose width is computed from the
//! longest path in the list. That means:
//!
//! - the column offsets change when an unrelated worktree is added,
//! - and a path containing a space is ambiguous with the column separator.
//!
//! Both are real: this machine has worktrees under `~/.codex/worktrees/…` and
//! `~/worktrees/…` alongside the repo's siblings, so the padding shifts constantly.
//! `--porcelain -z` gives NUL-terminated fields with records separated by an extra
//! NUL, which is unambiguous for any path.
//!
//! Verified byte layout (`git worktree list --porcelain -z | od -c`):
//!
//! ```text
//! worktree /Users/dev/code/webapp\0HEAD 15c2d425…\0branch refs/heads/main\0\0
//! worktree /Users/dev/.cache/agent-worktrees/a1/webapp\0HEAD 71421b0c…\0detached\0\0
//! ```

use std::path::PathBuf;

use wtm_core::error::GitError;
use wtm_core::model::{BranchRef, Checkout, CommitId, WorkingTreeStatus, Worktree, WorktreeId};

/// Prefix git uses for local branch refs.
const HEADS_PREFIX: &str = "refs/heads/";

/// Parse `git worktree list --porcelain -z`.
///
/// The first record is the main worktree — git documents this ordering, and
/// [`Worktree::is_main`] relies on it. Nothing else in the output identifies which
/// one is main.
pub fn parse_worktree_list(output: &str) -> Result<Vec<Worktree>, GitError> {
    let mut worktrees = Vec::new();

    for (index, record) in split_records(output).into_iter().enumerate() {
        let mut path: Option<PathBuf> = None;
        let mut head = None;
        let mut checkout = None;
        let mut is_bare = false;
        let mut locked = None;
        let mut prunable = None;

        for field in record {
            let (key, value) = split_field(field);
            match key {
                "worktree" => path = Some(PathBuf::from(value)),
                "HEAD" => head = Some(CommitId::new(value)),
                "branch" => {
                    checkout = Some(Checkout::Branch {
                        branch: BranchRef::new(value.strip_prefix(HEADS_PREFIX).unwrap_or(value)),
                    });
                }
                "detached" => checkout = Some(Checkout::Detached),
                "bare" => is_bare = true,
                // Both may appear with or without a trailing reason.
                "locked" => locked = Some(value.to_owned()),
                "prunable" => prunable = Some(value.to_owned()),
                // Unknown keys are ignored rather than fatal: git has added
                // attributes to this output before and will again, and refusing to
                // list any worktrees because of one unrecognized line would be a
                // bad trade.
                other => tracing::debug!(key = other, "ignoring unknown worktree attribute"),
            }
        }

        let path = path.ok_or_else(|| GitError::Unparsable {
            message: "worktree record has no `worktree` field".to_owned(),
            raw: output.replace('\0', "\\0"),
        })?;

        worktrees.push(Worktree {
            id: WorktreeId::from_path(&path),
            head,
            // A bare repository has neither a branch nor a detached HEAD; treating
            // the absence as detached is the honest reading — there is no branch
            // checked out here.
            checkout: checkout.unwrap_or(Checkout::Detached),
            is_main: index == 0,
            is_bare,
            locked,
            prunable,
            path,
        });
    }

    Ok(worktrees)
}

/// Parse `git status --porcelain=v1 -z`.
///
/// Entry format is `XY <path>` where `X` is the index status and `Y` the worktree
/// status. Renames and copies carry a second NUL-terminated path, which is consumed
/// and discarded — we only need counts.
pub fn parse_status(output: &str) -> WorkingTreeStatus {
    let mut status = WorkingTreeStatus::default();
    let mut fields = output.split('\0').filter(|f| !f.is_empty());

    while let Some(entry) = fields.next() {
        // `XY ` is three bytes minimum; anything shorter is not an entry.
        let mut chars = entry.chars();
        let (Some(x), Some(y)) = (chars.next(), chars.next()) else {
            continue;
        };

        if x == '?' && y == '?' {
            status.untracked += 1;
            continue;
        }
        if x == '!' && y == '!' {
            // Ignored, only present with --ignored. Not a change.
            continue;
        }

        // A renamed/copied entry carries one source path, whichever column reports the change.
        // Consuming only an index-side rename desynchronizes every entry after a worktree rename;
        // consuming one per column is also wrong because porcelain v1 defines one original path
        // per entry, not per status letter.
        if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            let _source = fields.next();
        }

        if x != ' ' {
            status.staged += 1;
        }
        if y != ' ' {
            // The same question `git diff-index --quiet HEAD` answers: tracked
            // content differs from HEAD. Untracked files deliberately do not set
            // this — a project's own removal guard usually checks only tracked
            // changes, so conflating them would refuse removals it would allow.
            status.dirty_tracked = true;
        }
    }

    status
}

/// Parse `git rev-list --left-right --count <base>...<branch>` into
/// `(ahead, behind)`.
///
/// The output is `<left>\t<right>`, where left counts commits reachable from the
/// first ref only and right from the second only. With `base...branch` that makes
/// left = behind and right = ahead — the opposite of the reading order, which is
/// exactly the sort of thing worth pinning down with a test.
pub fn parse_ahead_behind(output: &str) -> Result<(u32, u32), GitError> {
    let mut parts = output.split_whitespace();
    let behind = parts.next().and_then(|s| s.parse().ok());
    let ahead = parts.next().and_then(|s| s.parse().ok());

    match (ahead, behind) {
        (Some(ahead), Some(behind)) => Ok((ahead, behind)),
        _ => Err(GitError::Unparsable {
            message: "expected `<behind>\\t<ahead>` from rev-list --left-right --count".to_owned(),
            raw: output.to_owned(),
        }),
    }
}

/// Parse one branch name per line, dropping blanks and any `HEAD` alias.
///
/// `refs/remotes/<remote>/HEAD` is a symbolic ref, not a branch; offering it as a
/// base would produce a confusing detached checkout.
pub fn parse_branch_lines(output: &str, strip_remote: Option<&str>) -> Vec<BranchRef> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| match strip_remote {
            Some(remote) => line.strip_prefix(&format!("{remote}/")),
            None => Some(line),
        })
        .filter(|name| *name != "HEAD" && !name.ends_with("/HEAD"))
        .map(BranchRef::new)
        .collect()
}

/// Split NUL-terminated fields into records, where an empty field ends a record.
fn split_records(output: &str) -> Vec<Vec<&str>> {
    let mut records = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    for field in output.split('\0') {
        if field.is_empty() {
            if !current.is_empty() {
                records.push(std::mem::take(&mut current));
            }
        } else {
            current.push(field);
        }
    }
    // Tolerate output that is not terminated by the final double NUL.
    if !current.is_empty() {
        records.push(current);
    }

    records
}

/// Split `key value` into its parts; a bare key yields an empty value.
fn split_field(field: &str) -> (&str, &str) {
    field.split_once(' ').unwrap_or((field, ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real output captured from the reference repo, with the record separators
    /// spelled out. Includes a detached worktree, a worktree outside the repo's
    /// parent, and a directory whose name disagrees with its branch.
    const REAL_OUTPUT: &str = concat!(
        "worktree /Users/dev/code/webapp\0",
        "HEAD 15c2d42529198ad0bf56fd8ad123db8a08ee65e0\0",
        "branch refs/heads/main\0",
        "\0",
        "worktree /Users/dev/.cache/agent-worktrees/a1/webapp\0",
        "HEAD 71421b0c9b97d8f7d8dd9b434488292cecedf055\0",
        "detached\0",
        "\0",
        "worktree /Users/dev/code/ACME-4567-move-account-settings-to-the-spa-pattern\0",
        "HEAD 0122276878000000000000000000000000000000\0",
        "branch refs/heads/experiment/ACME-0000-move-account-setting-configurations\0",
        "\0",
        "worktree /Users/dev/worktrees/webapp-payments-retry\0",
        "HEAD 7f02356acc000000000000000000000000000000\0",
        "branch refs/heads/experiment/ACME-0000-add-idempotency-key-to-payments\0",
        "\0",
    );

    #[test]
    fn parses_real_output() {
        let worktrees = parse_worktree_list(REAL_OUTPUT).unwrap();
        assert_eq!(worktrees.len(), 4);

        let main = &worktrees[0];
        assert!(main.is_main, "the first record is always the main worktree");
        assert_eq!(main.path, PathBuf::from("/Users/dev/code/webapp"));
        assert_eq!(main.branch().map(BranchRef::as_str), Some("main"));
        assert_eq!(main.head.as_ref().unwrap().short(), "15c2d42529");
        assert!(!main.is_bare);
        assert!(main.locked.is_none());
    }

    #[test]
    fn a_detached_worktree_has_no_branch() {
        let worktrees = parse_worktree_list(REAL_OUTPUT).unwrap();
        let detached = &worktrees[1];
        assert_eq!(detached.checkout, Checkout::Detached);
        assert!(detached.branch().is_none(), "must not invent a branch");
        // Its directory is `webapp`, same as the main worktree's — another reason a
        // directory name is not an identity.
        assert_eq!(detached.dirname(), "webapp");
        assert!(!detached.is_main);
    }

    /// The most important case in this file.
    #[test]
    fn a_directory_name_that_disagrees_with_its_branch_is_reported_faithfully() {
        let worktrees = parse_worktree_list(REAL_OUTPUT).unwrap();
        let odd = &worktrees[2];
        assert_eq!(
            odd.dirname(),
            "ACME-4567-move-account-settings-to-the-spa-pattern"
        );
        assert_eq!(
            odd.branch().map(BranchRef::as_str),
            Some("experiment/ACME-0000-move-account-setting-configurations"),
            "the branch must come from git, never from the directory name"
        );
    }

    #[test]
    fn worktrees_outside_the_repo_parent_are_kept() {
        let worktrees = parse_worktree_list(REAL_OUTPUT).unwrap();
        assert_eq!(
            worktrees[3].path,
            PathBuf::from("/Users/dev/worktrees/webapp-payments-retry")
        );
    }

    #[test]
    fn handles_paths_containing_spaces() {
        // Precisely what the human-readable form cannot express.
        let output = "worktree /Users/dev/My Sites/a repo\0HEAD abc\0branch refs/heads/main\0\0";
        let worktrees = parse_worktree_list(output).unwrap();
        assert_eq!(
            worktrees[0].path,
            PathBuf::from("/Users/dev/My Sites/a repo")
        );
        assert_eq!(worktrees[0].dirname(), "a repo");
    }

    #[test]
    fn parses_bare_locked_and_prunable_attributes() {
        let output = concat!(
            "worktree /repos/bare.git\0bare\0",
            "\0",
            "worktree /wt/locked\0HEAD abc\0branch refs/heads/l\0locked because reasons\0",
            "\0",
            "worktree /wt/gone\0HEAD def\0branch refs/heads/g\0prunable gitdir file points to non-existent location\0",
            "\0",
        );
        let worktrees = parse_worktree_list(output).unwrap();

        assert!(worktrees[0].is_bare);
        assert!(worktrees[0].head.is_none());
        assert_eq!(
            worktrees[0].checkout,
            Checkout::Detached,
            "a bare repo has nothing checked out"
        );

        assert_eq!(worktrees[1].locked.as_deref(), Some("because reasons"));
        assert!(
            worktrees[2]
                .prunable
                .as_deref()
                .unwrap()
                .contains("non-existent"),
            "prunable reason should be preserved for the UI"
        );
    }

    #[test]
    fn a_bare_locked_attribute_with_no_reason_yields_an_empty_reason() {
        let output = "worktree /wt/a\0HEAD abc\0branch refs/heads/a\0locked\0\0";
        let worktrees = parse_worktree_list(output).unwrap();
        assert_eq!(worktrees[0].locked.as_deref(), Some(""));
    }

    #[test]
    fn branch_names_containing_slashes_survive_prefix_stripping() {
        let output = "worktree /wt/a\0HEAD abc\0branch refs/heads/task/ACME-1234-a-b-c\0\0";
        let worktrees = parse_worktree_list(output).unwrap();
        assert_eq!(
            worktrees[0].branch().map(BranchRef::as_str),
            Some("task/ACME-1234-a-b-c")
        );
    }

    #[test]
    fn unknown_attributes_are_ignored_rather_than_fatal() {
        // Forward compatibility: a future git adding a field must not break listing.
        let output = "worktree /wt/a\0HEAD abc\0branch refs/heads/a\0somethingnew value\0\0";
        assert_eq!(parse_worktree_list(output).unwrap().len(), 1);
    }

    #[test]
    fn empty_output_yields_no_worktrees() {
        assert!(parse_worktree_list("").unwrap().is_empty());
        assert!(parse_worktree_list("\0\0").unwrap().is_empty());
    }

    #[test]
    fn output_without_a_trailing_double_nul_still_parses() {
        let output = "worktree /wt/a\0HEAD abc\0branch refs/heads/a";
        assert_eq!(parse_worktree_list(output).unwrap().len(), 1);
    }

    #[test]
    fn a_record_without_a_worktree_field_is_an_error() {
        let err = parse_worktree_list("HEAD abc\0branch refs/heads/a\0\0").unwrap_err();
        assert!(matches!(err, GitError::Unparsable { .. }), "got {err:?}");
    }

    // ── status ────────────────────────────────────────────────────────────────

    #[test]
    fn status_counts_staged_dirty_and_untracked_separately() {
        // " M" worktree-modified, "M " staged, "??" untracked, "MM" both.
        let output = " M src/a.rs\0M  src/b.rs\0?? new.txt\0MM src/c.rs\0";
        let status = parse_status(output);
        assert!(
            status.dirty_tracked,
            "two entries have worktree modifications"
        );
        assert_eq!(status.staged, 2, "M  and MM");
        assert_eq!(status.untracked, 1);
        assert!(!status.is_clean());
    }

    #[test]
    fn a_worktree_with_only_untracked_files_is_not_tracked_dirty() {
        // Load-bearing distinction: a project's removal guard usually checks only
        // tracked changes, so conflating these would refuse removals it allows.
        let status = parse_status("?? a.txt\0?? b.txt\0");
        assert!(!status.dirty_tracked);
        assert_eq!(status.untracked, 2);
        assert!(!status.is_clean(), "still not clean overall");
    }

    #[test]
    fn a_rename_consumes_its_source_path_and_counts_once() {
        // "R  new\0old\0" — without consuming `old` it would be parsed as another
        // entry, inflating the counts.
        let status = parse_status("R  new/path.rs\0old/path.rs\0");
        assert_eq!(status.staged, 1);
        assert_eq!(
            status.untracked, 0,
            "the source path must not be read as an entry"
        );
    }

    #[test]
    fn a_worktree_rename_consumes_its_source_path_without_inflating_staged_changes() {
        let status = parse_status(" R new/path.rs\0old/path.rs\0");
        assert_eq!(status.staged, 0);
        assert!(status.dirty_tracked);
        assert_eq!(status.untracked, 0);
    }

    #[test]
    fn a_double_rename_status_consumes_one_source_path() {
        let status = parse_status("RR final/path.rs\0source.rs\0M  next.rs\0");
        assert_eq!(status.staged, 2, "the rename and the following staged file");
        assert!(status.dirty_tracked);
        assert_eq!(status.untracked, 0);
    }

    #[test]
    fn a_worktree_copy_consumes_its_source_path() {
        let status = parse_status(" C copied/path.rs\0source/path.rs\0");
        assert_eq!(status.staged, 0);
        assert!(status.dirty_tracked);
        assert_eq!(status.untracked, 0);
    }

    #[test]
    fn clean_status_is_clean() {
        let status = parse_status("");
        assert!(status.is_clean());
        assert!(!status.dirty_tracked);
    }

    #[test]
    fn ignored_entries_are_not_changes() {
        assert!(parse_status("!! target/\0").is_clean());
    }

    // ── ahead/behind ──────────────────────────────────────────────────────────

    #[test]
    fn ahead_behind_maps_left_to_behind_and_right_to_ahead() {
        // `rev-list --left-right --count base...branch` prints "<left>\t<right>".
        // With base on the left, left-only commits are ones we lack: behind.
        assert_eq!(parse_ahead_behind("3\t7\n").unwrap(), (7, 3));
        assert_eq!(parse_ahead_behind("0\t0").unwrap(), (0, 0));
    }

    #[test]
    fn malformed_ahead_behind_is_an_error_not_a_silent_zero() {
        // Silently reporting (0,0) would render a diverged branch as up to date.
        assert!(parse_ahead_behind("").is_err());
        assert!(parse_ahead_behind("5").is_err());
        assert!(parse_ahead_behind("a\tb").is_err());
    }

    // ── branches ──────────────────────────────────────────────────────────────

    #[test]
    fn branch_lines_are_trimmed_and_blanks_dropped() {
        let branches = parse_branch_lines("main\n\n  develop  \n", None);
        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0].as_str(), "main");
        assert_eq!(branches[1].as_str(), "develop");
    }

    #[test]
    fn remote_prefixes_are_stripped_and_head_aliases_dropped() {
        let branches = parse_branch_lines(
            "origin/main\norigin/develop\norigin/HEAD\norigin/task/ACME-1-x\n",
            Some("origin"),
        );
        let names: Vec<&str> = branches.iter().map(BranchRef::as_str).collect();
        assert_eq!(names, vec!["main", "develop", "task/ACME-1-x"]);
    }

    #[test]
    fn lines_for_another_remote_are_excluded_when_stripping() {
        // Only the requested remote's branches should survive.
        let branches = parse_branch_lines("origin/main\nupstream/main\n", Some("origin"));
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].as_str(), "main");
    }
}

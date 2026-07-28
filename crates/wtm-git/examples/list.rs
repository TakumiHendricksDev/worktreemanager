//! List a repository's worktrees through the real adapter stack.
//!
//! A smoke test against a genuine repository, which the temp-directory fixtures
//! cannot fully stand in for: real repos accumulate worktrees outside the parent
//! directory, detached checkouts, and directory names that no longer match their
//! branches.
//!
//! ```sh
//! cargo run -p wtm-git --example list -- /path/to/repo
//! ```

// The workspace bans `println!` because a GUI app's stdout goes nowhere useful. This
// is a command-line diagnostic whose entire output *is* stdout.
#![allow(clippy::print_stdout)]

use std::sync::Arc;

use wtm_core::ports::exec::CommandRunner;
use wtm_core::ports::git::{BranchFilter, Git};
use wtm_exec::Runner;
use wtm_git::GitCli;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target = std::env::args().nth(1).unwrap_or_else(|| ".".to_owned());
    let path = std::fs::canonicalize(&target)?;

    let runner = Arc::new(Runner::with_probed_path(None));
    println!("resolved PATH: {}\n", runner.resolved_path());

    let git = GitCli::new(runner as Arc<dyn CommandRunner>);
    let root = git.repo_root(&path)?;

    println!("repo root:      {}", root.display());
    println!("git common dir: {}\n", git.git_common_dir(&root)?.display());

    let worktrees = git.list_worktrees(&root)?;
    println!("{} worktree(s):\n", worktrees.len());

    for wt in &worktrees {
        let status = git.status(&wt.path).unwrap_or_default();
        let branch = wt
            .branch()
            .map_or("(detached)".to_owned(), |b| b.as_str().to_owned());

        let mut flags = Vec::new();
        if wt.is_main {
            flags.push("main".to_owned());
        }
        if wt.is_bare {
            flags.push("bare".to_owned());
        }
        if status.dirty_tracked {
            flags.push("dirty".to_owned());
        }
        if status.untracked > 0 {
            flags.push(format!("{} untracked", status.untracked));
        }
        if let Some(reason) = &wt.prunable {
            flags.push(format!("prunable: {reason}"));
        }

        // Printing the directory name and the branch side by side is the point: they
        // are unrelated facts, and a real repo proves it.
        println!("  {}", wt.dirname());
        println!("    path:   {}", wt.path.display());
        println!("    branch: {branch}");
        println!(
            "    head:   {}",
            wt.head.as_ref().map_or("-", |h| h.short())
        );
        if !flags.is_empty() {
            println!("    flags:  {}", flags.join(", "));
        }
        if wt.dirname() != branch.rsplit('/').next().unwrap_or(&branch) {
            println!("    note:   directory name does not match the branch");
        }
        println!();
    }

    let local = git.branches(&root, BranchFilter::Local)?;
    let all = git.branches(&root, BranchFilter::Both)?;
    println!(
        "{} local branch(es), {} including remote-only",
        local.len(),
        all.len()
    );

    Ok(())
}

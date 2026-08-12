//! The remove pipeline.
//!
//! # Why the app runs git itself
//!
//! A project's own removal command tends to have no non-interactive escape. The reference
//! script prompts `Delete branch <x>? [y/n]` with no `--yes` flag, and its `confirm()` helper
//! loops forever on EOF stdin. Delegating to it from a GUI means either a hang or a hijacked
//! terminal.
//!
//! So `strategy = "native"` is the default: the app runs `git worktree remove` and the prompt
//! becomes a checkbox on the dialog. The project's *teardown* still runs — `[[remove.pre]]`
//! steps go first, which is how containers get stopped and root-owned files get chowned back
//! before git tries to delete the directory.
//!
//! # Order matters
//!
//! Teardown, then remove, then optionally delete the branch. Reversing the first two leaves
//! `git worktree remove` fighting root-owned files a container created, which fails in a way
//! that looks like a permissions bug.

use std::sync::Arc;

use crate::error::WtmError;
use crate::model::{
    ExitOutcome, OnFailure, PlanWarning, PreflightItem, Project, RemoveStrategy, SessionId,
    Worktree,
};
use crate::ports::exec::{CancelToken, CommandRunner, Invocation};
use crate::ports::git::Git;
use crate::ports::progress::ProgressSink;
use crate::ports::pty::{PtyHost, PtySink};
use crate::ports::template::{Context, TemplateEngine};

/// Default deadline for a teardown step. Five minutes: `docker compose down -v` can take a
/// while on large volumes.
const DEFAULT_PRE_TIMEOUT_MS: u64 = 300_000;

/// What the caller wants removed.
#[derive(Debug, Clone)]
pub struct RemoveRequest {
    pub project: Project,
    pub worktree: Worktree,
    pub ambient: Context,
    /// Delete the branch as well. The GUI form of the shell's `confirm()` prompt.
    pub delete_branch: bool,
    /// Force past a dirty working tree. Checking `delete_branch` is itself the explicit decision
    /// to delete that branch after the unmerged warning has been shown.
    pub force: bool,
    /// Preflight ids the user acknowledged.
    pub acknowledged: Vec<String>,
}

/// What happened.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoveOutcome {
    Removed {
        /// True when the branch was deleted too.
        branch_deleted: bool,
        warnings: Vec<PlanWarning>,
    },
    /// A teardown step failed under `on_failure = "fail"`, so nothing was removed.
    TeardownFailed {
        session: Option<SessionId>,
        warnings: Vec<PlanWarning>,
    },
}

/// Remove a worktree.
pub struct RemovePipeline {
    pub git: Arc<dyn Git>,
    pub runner: Arc<dyn CommandRunner>,
    pub pty: Arc<dyn PtyHost>,
    pub engine: Arc<dyn TemplateEngine>,
}

impl std::fmt::Debug for RemovePipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemovePipeline").finish_non_exhaustive()
    }
}

impl RemovePipeline {
    /// What the confirmation dialog needs to say, without touching anything.
    ///
    /// Infallible today — a failed `git status` degrades to "clean" rather than propagating,
    /// because a worktree whose directory is already gone must still be removable. The
    /// `Result` stays because the next check added here plausibly will fail, and widening the
    /// signature later would churn every call site.
    #[allow(clippy::unnecessary_wraps)]
    pub fn preflight(&self, req: &RemoveRequest) -> Result<Vec<PreflightItem>, WtmError> {
        let project = &req.project;
        let mut items = Vec::new();

        if req.worktree.is_main {
            items.push(
                PreflightItem::error("is_main", "This is the repository's main worktree.")
                    .with_hint("git will not remove it, and neither will wtm."),
            );
            // No point checking anything else.
            return Ok(items);
        }

        let status = self.git.status(&req.worktree.path).unwrap_or_default();

        if project.remove.require_clean && status.dirty_tracked {
            items.push(
                PreflightItem::error("dirty", "This worktree has uncommitted changes.")
                    .overridable()
                    .with_hint("Commit or stash them first, or tick force to discard them."),
            );
        }
        if status.untracked > 0 {
            items.push(
                PreflightItem::warn(
                    "untracked",
                    format!("{} untracked file(s) will be deleted.", status.untracked),
                )
                .with_hint("git refuses without --force; these are not recoverable."),
            );
        }

        if req.delete_branch
            && project.remove.warn_if_unmerged
            && let Some(branch) = req.worktree.branch()
        {
            // Compare against the project's default base, since that is what "merged"
            // means for this workflow.
            let base = project
                .field(&project.create.base_field)
                .and_then(|f| f.default.as_ref())
                .map_or_else(|| "HEAD".to_owned(), crate::model::FieldDefault::as_string);

            if !self
                .git
                .is_merged(&project.root, branch, &base)
                .unwrap_or(false)
            {
                items.push(
                    PreflightItem::warn(
                        "unmerged",
                        format!("`{branch}` has commits that are not in `{base}`."),
                    )
                    .with_hint("Deleting the branch loses them."),
                );
            }
        }

        Ok(items)
    }

    /// Run teardown, remove the worktree, and optionally delete the branch.
    pub fn execute(
        &self,
        req: &RemoveRequest,
        progress: &dyn ProgressSink,
        sink: &Arc<dyn PtySink>,
        cancel: &CancelToken,
    ) -> Result<RemoveOutcome, WtmError> {
        let project = &req.project;
        let mut warnings = Vec::new();

        let blocking: Vec<PreflightItem> = self
            .preflight(req)?
            .into_iter()
            .filter(|item| {
                item.severity == crate::model::PreflightSeverity::Error
                    && !(item.overridable && (req.force || req.acknowledged.contains(&item.id)))
            })
            .collect();
        if !blocking.is_empty() {
            return Err(WtmError::Preflight(blocking));
        }

        let mut ctx = req.ambient.clone();
        ctx.insert(
            "worktree.path".to_owned(),
            req.worktree.path.to_string_lossy().into_owned(),
        );
        ctx.insert(
            "worktree.dirname".to_owned(),
            req.worktree.dirname().to_owned(),
        );
        ctx.insert(
            "worktree.branch".to_owned(),
            req.worktree
                .branch()
                .map(|b| b.as_str().to_owned())
                .unwrap_or_default(),
        );

        // ── teardown, before git touches the directory ──
        let total = u8::try_from(project.remove.pre.len()).unwrap_or(u8::MAX) + 2;
        for (index, step) in project.remove.pre.iter().enumerate() {
            cancel.check()?;

            if let Some(when) = &step.when {
                // A worktree that was never set up has nothing to tear down.
                if !self
                    .engine
                    .eval_bool("remove.pre.when", when, &ctx)
                    .unwrap_or(false)
                {
                    continue;
                }
            }

            let step_index = u8::try_from(index).unwrap_or(0) + 1;
            progress.stage("teardown", "Running project teardown", step_index, total);

            let argv: Vec<String> = step
                .run
                .iter()
                .enumerate()
                .map(|(i, template)| {
                    self.engine
                        .render(&format!("remove.pre[{index}][{i}]"), template, &ctx)
                })
                .collect::<Result<_, _>>()?;

            let cwd = match step.cwd {
                crate::model::CwdBase::Worktree => req.worktree.path.clone(),
                _ => project.root.clone(),
            };

            progress.emit(crate::ports::progress::ProgressEvent::CommandStarted {
                argv: argv.clone(),
                cwd: cwd.to_string_lossy().into_owned(),
            });

            let mut inv = Invocation::new(
                argv.clone(),
                cwd,
                step.timeout_ms.unwrap_or(DEFAULT_PRE_TIMEOUT_MS),
            );
            inv.env = step
                .env
                .iter()
                .filter_map(|(name, template)| {
                    self.engine
                        .render(&format!("remove.pre.env.{name}"), template, &ctx)
                        .ok()
                        .map(|value| (name.clone(), value))
                })
                .collect();

            let result = if step.pty {
                self.pty
                    .spawn(
                        &inv,
                        24,
                        100,
                        Some(req.worktree.id.as_str()),
                        Arc::clone(sink),
                    )
                    .and_then(|spawned| self.pty.wait(&spawned.session, cancel))
                    .map(|outcome| {
                        if outcome.is_success() {
                            Ok(())
                        } else {
                            Err(outcome)
                        }
                    })
            } else {
                self.runner.run(&inv, cancel).map(|_| Ok(()))
            };

            let failed = match result {
                Ok(Ok(())) => None,
                Ok(Err(outcome)) => Some(format!("{outcome:?}")),
                Err(err) => Some(err.to_string()),
            };

            if let Some(message) = failed {
                match step.on_failure {
                    // A stopped Docker daemon must not block removing a worktree.
                    OnFailure::Warn | OnFailure::Ignore | OnFailure::Keep => {
                        warnings.push(PlanWarning::new(
                            format!("teardown_{index}_failed"),
                            format!("`{}` failed: {message}", argv.join(" ")),
                        ));
                    }
                    OnFailure::Fail => {
                        return Ok(RemoveOutcome::TeardownFailed {
                            session: None,
                            warnings,
                        });
                    }
                }
            }
        }

        // ── remove ──
        cancel.check()?;
        progress.stage("remove", "Removing the worktree", total - 1, total);

        if project.remove.strategy == RemoveStrategy::Command
            && let Some(_command) = &project.remove.command
        {
            // Not reachable from the UI yet: the native path is the default and the one
            // that turns the branch prompt into a checkbox. Left explicit so the branch is
            // visible rather than silently falling through to native.
            return Err(WtmError::Validation(vec![crate::error::FieldProblem::new(
                "remove.strategy",
                "`strategy = \"command\"` is not supported yet; use the default \
                     `\"native\"`, which runs git directly and offers branch deletion as a \
                     checkbox.",
            )]));
        }

        self.git
            .remove_worktree(&project.root, &req.worktree.path, req.force)?;

        // ── the branch ──
        let mut branch_deleted = false;
        if req.delete_branch {
            progress.stage("branch", "Deleting the branch", total, total);
            if let Some(branch) = req.worktree.branch() {
                // Use `-D`, not `-d`, after the user explicitly checked branch deletion.
                //
                // This is not laziness: `git branch -d` refuses unless the branch is merged
                // into **HEAD**, while the question the user was actually asked — and the one
                // `warn_if_unmerged` answers — is whether it is merged into the project's
                // *base*. Those differ constantly: a branch cut from `origin/develop` is fully
                // contained in develop but not in the main checkout's `main`, so `-d` refuses
                // and the branch silently survives a removal the user explicitly requested.
                //
                // So: run our own merge check against the base, warn if it fails (already done
                // in `preflight`), and then honour the user's decision.
                match self.git.delete_branch(&project.root, branch, true) {
                    Ok(()) => branch_deleted = true,
                    Err(err) => warnings.push(PlanWarning::new(
                        "branch_delete_failed",
                        format!(
                            "The worktree was removed, but `{branch}` could not be deleted: {err}. \
                             Delete it with `git branch -D {branch}` once you are sure."
                        ),
                    )),
                }
            }
        }

        Ok(RemoveOutcome::Removed {
            branch_deleted,
            warnings,
        })
    }
}

/// Convenience so `ExitOutcome` reads naturally in the teardown result mapping.
impl ExitOutcome {
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Success => "succeeded".to_owned(),
            Self::Failed { code } => format!("exited {code}"),
            Self::Signalled { signal } => format!("killed by signal {signal}"),
            Self::TimedOut { after_ms } => format!("timed out after {after_ms}ms"),
            Self::Cancelled => "cancelled".to_owned(),
        }
    }
}

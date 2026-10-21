//! A review worktree: the create pipeline with a pull request as its input instead of the form.
//!
//! # The same boundary, a different input
//!
//! [`CreatePipeline::preview_review`] and [`CreatePipeline::execute_review`] are one planning
//! function with a stop-after, exactly like the form's pair, and planning mutates nothing: it reads
//! the remotes, the worktrees and whether the head commit is already here, and decides. The fetch,
//! the `git worktree add` and the optional setup are stages 7 to 9, after the same preflight gate.
//!
//! # Setup is off unless asked for
//!
//! A project's setup may clone a database, run migrations or allocate ports, and the code it runs
//! against is the branch just checked out. For your own work that is the point. For a review it is
//! someone else's branch, read before you have decided anything about it, so a review never runs
//! setup quietly: [`ReviewRequest::prepare_environment`] is the opt-in, it defaults to off in the
//! view, and the preview says which of the two will happen and shows the exact argv when it will.
//!
//! # Fetching a fork's commits
//!
//! An open pull request from a branch of the base repository is fetched by that branch's name,
//! which updates the remote-tracking branch the rest of the app already reads. A fork's branch is
//! not reachable from any remote you have, and adding the fork as one would change your config to
//! read one pull request, so its commits come from the base repository's `pull/<n>/head`, which the
//! forge keeps for every pull request. So does a closed or merged one's, whose branch may be gone.
//!
//! Either way the worktree is checked out at the head commit the forge reported, by id, never at a
//! branch tip. A push between resolving and fetching cannot change what you review: a moved head is
//! a preflight error when the caller pinned the one it showed, and a force-pushed one that the fetch
//! no longer brings is refused rather than replaced.
//!
//! # One worktree per pull request
//!
//! Before planning a new worktree, planning looks for one that already holds this pull request:
//! the review branch an earlier review made, the planned directory, the pull request's own branch
//! when it lives in this repository (your own pull request, read where you wrote it), or any
//! worktree whose `HEAD` is the head commit. Finding one plans no mutation at all, and executing
//! that plan returns it. A worktree that is behind the head is still the one used, with a warning:
//! moving it would mean resetting a checkout that may hold someone's work.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{CreatePipeline, STAGES, SetupStage, insert_planned_worktree, refuse_unacknowledged};
use crate::error::{GitError, WtmError};
use crate::flow::{PrState, PullRequest, remote_for};
use crate::model::{
    BranchRef, CommitId, CreateOutcome, DirBase, FormValues, PlanWarning, PreflightItem, Project,
    TrackMode, Worktree,
};
use crate::ports::exec::CancelToken;
use crate::ports::git::{AddOptions, BranchFilter};
use crate::ports::progress::ProgressSink;
use crate::ports::pty::PtySink;
use crate::ports::template::Context;

/// How the review worktree holds the head commit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewCheckout {
    /// Detached at the head commit. Nothing to name, nothing to clean up, and no branch a reflexive
    /// `git push` could send somewhere.
    #[default]
    Detached,
    /// On a new local branch, `pr-<number>`, with no upstream, for a review you expect to commit
    /// in. No upstream for the same reason the form's branches have none: a push from it must be a
    /// decision, not a default that lands on the author's branch.
    Branch,
}

/// What the caller wants checked out.
#[derive(Debug, Clone)]
pub struct ReviewRequest {
    /// A loaded, trusted project, as for [`super::CreateRequest`].
    pub project: Project,
    /// Ambient tokens the adapter layer supplies: `repo.*`, `vars.*`, `os.*`, `now.*`.
    pub ambient: Context,
    /// The pull request as the forge just described it.
    pub pull_request: PullRequest,
    /// The head the person was shown, if they were shown one. A different head now is a preflight
    /// error, so the worktree is never of a commit nobody looked at.
    pub expected_head: Option<CommitId>,
    pub checkout: ReviewCheckout,
    /// Run the project's setup in the new worktree. Off unless asked; see the module docs.
    pub prepare_environment: bool,
    /// Preflight ids the person acknowledged, as for the form.
    pub acknowledged: Vec<String>,
    pub rows: u16,
    pub cols: u16,
}

/// Where the head commit comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFetch {
    pub remote: String,
    /// The head branch, or `pull/<n>/head`. See the module docs for which.
    pub refspec: String,
}

/// Why an existing worktree is the one to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseReason {
    /// It is on the review branch an earlier review made.
    ReviewBranch,
    /// It is where a review worktree for this pull request goes.
    ReviewDirectory,
    /// It is on the pull request's own branch.
    PullRequestBranch,
    /// Its `HEAD` is the pull request's head commit.
    HeadCommit,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewTarget {
    /// Use a worktree that is already there. Nothing will be fetched, created or run.
    Reuse {
        worktree: Worktree,
        reason: ReuseReason,
        /// Whether it is at the head commit right now.
        at_head: bool,
    },
    /// Make one.
    Create {
        directory: PathBuf,
        /// `None` for a detached checkout.
        branch: Option<BranchRef>,
        /// The literal `git worktree add …` argv.
        git_argv: Vec<String>,
    },
}

/// What happens about the project's setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewSetup {
    /// The project declares setup, and this review will not run it.
    Skipped,
    /// The project declares none, so there is nothing to opt into.
    NotDeclared,
    /// An existing worktree is used as it is, so its environment is whatever it already has.
    NotApplicable,
    /// Setup runs after the worktree is made, exactly this.
    Run { argv: Vec<String>, cwd: PathBuf },
}

/// Everything decided before anything is touched.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPlan {
    pub number: u64,
    pub head: CommitId,
    /// `None` when reusing a worktree, or when no remote points at the base repository.
    pub fetch: Option<ReviewFetch>,
    /// Whether the head commit is already in this repository.
    pub head_present: bool,
    pub target: ReviewTarget,
    pub setup: ReviewSetup,
}

/// The review screen: exactly what will happen, before anything has.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPreview {
    pub plan: ReviewPlan,
    pub preflight: Vec<PreflightItem>,
    pub warnings: Vec<PlanWarning>,
}

/// What [`CreatePipeline::execute_review`] did.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewOutcome {
    /// An existing worktree, untouched.
    Reused { worktree: Worktree, at_head: bool },
    /// A new one, with the same partial outcomes as the form's when setup ran and failed.
    Created { outcome: CreateOutcome },
}

/// Everything planning produced, shared by preview and execute.
struct PlannedReview {
    preview: ReviewPreview,
    add: Option<AddOptions>,
    setup_base_ctx: Context,
}

/// The branch a [`ReviewCheckout::Branch`] review makes.
fn review_branch(number: u64) -> BranchRef {
    BranchRef::new(format!("pr-{number}"))
}

impl CreatePipeline {
    /// Plan a review worktree and return the review screen's contents. Mutates nothing.
    pub fn preview_review(
        &self,
        req: &ReviewRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<ReviewPreview, WtmError> {
        Ok(self.plan_review(req, progress, cancel)?.preview)
    }

    /// Plan again, then fetch, add and, only if asked, run setup.
    ///
    /// Re-plans rather than trusting a preview the caller holds, for the form's reason: the
    /// repository can change between the review screen and the click, and planning is free.
    pub fn execute_review(
        &self,
        req: &ReviewRequest,
        progress: &dyn ProgressSink,
        sink: Arc<dyn PtySink>,
        cancel: &CancelToken,
    ) -> Result<ReviewOutcome, WtmError> {
        let planned = self.plan_review(req, progress, cancel)?;
        refuse_unacknowledged(&planned.preview.preflight, &req.acknowledged)?;

        let plan = &planned.preview.plan;
        let root = &req.project.root;
        let add = match (&plan.target, planned.add) {
            (
                ReviewTarget::Reuse {
                    worktree, at_head, ..
                },
                _,
            ) => {
                progress.stage("done", "Done", STAGES, STAGES);
                return Ok(ReviewOutcome::Reused {
                    worktree: worktree.clone(),
                    at_head: *at_head,
                });
            }
            (ReviewTarget::Create { .. }, Some(add)) => add,
            (ReviewTarget::Create { .. }, None) => {
                unreachable!("planning makes add options for every worktree it plans to create")
            }
        };

        // ── 7. fetch ──
        if let Some(fetch) = &plan.fetch {
            progress.stage("fetch", "Fetching the pull request", 7, STAGES);
            cancel.check()?;
            if let Err(err) = self.git.fetch(root, &fetch.remote, &fetch.refspec) {
                // With the head already here, a stale remote-tracking branch is harmless, so this
                // is the base fetch's rule. Without it there is nothing to check out.
                if !plan.head_present {
                    return Err(err.into());
                }
                progress.warn(
                    "fetch_failed",
                    &format!(
                        "Could not fetch {} from {}; the head commit was already here. {err}",
                        fetch.refspec, fetch.remote
                    ),
                );
            }
        }
        // Checked after the fetch, not trusted from it: a force-push between resolving and
        // fetching leaves the old head unreachable, and git would otherwise fail later with
        // "invalid reference" about a commit id nobody recognises.
        if self.git.rev_parse(root, plan.head.as_str())?.is_none() {
            return Err(GitError::BadRef(format!(
                "{} (the head of pull request #{} is not in this repository after fetching; \
                 it may have been force-pushed, so look at the pull request again)",
                plan.head.as_str(),
                plan.number
            ))
            .into());
        }

        // ── 8. the first worktree mutation ──
        progress.stage("add", "Creating the review worktree", 8, STAGES);
        cancel.check()?;
        if let ReviewTarget::Create { git_argv, .. } = &plan.target {
            progress.emit(crate::ports::progress::ProgressEvent::CommandStarted {
                argv: git_argv.clone(),
                cwd: root.to_string_lossy().into_owned(),
            });
        }
        let worktree = match self.git.add_worktree(root, &add) {
            Ok(worktree) => worktree,
            Err(err) => {
                let _ = self.git.prune_worktrees(root);
                return Err(err.into());
            }
        };

        // ── 9. setup, only when asked ──
        if !req.prepare_environment {
            progress.stage("done", "Done", STAGES, STAGES);
            return Ok(ReviewOutcome::Created {
                outcome: CreateOutcome::Created {
                    worktree,
                    setup_session: None,
                },
            });
        }
        let values = FormValues::default();
        let outcome = self.setup_stage(
            &SetupStage {
                project: &req.project,
                base_ctx: &planned.setup_base_ctx,
                values: &values,
                rows: req.rows,
                cols: req.cols,
            },
            worktree,
            sink,
            progress,
            cancel,
        )?;
        Ok(ReviewOutcome::Created { outcome })
    }

    fn plan_review(
        &self,
        req: &ReviewRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<PlannedReview, WtmError> {
        let project = &req.project;
        let pr = &req.pull_request;
        let mut warnings = Vec::new();
        let mut preflight = Vec::new();

        // ── 1. validate ──
        progress.stage("validate", "Checking the pull request", 1, STAGES);
        cancel.check()?;
        if let Some(expected) = &req.expected_head
            && expected != &pr.head.sha
        {
            preflight.push(
                PreflightItem::error(
                    "head_moved",
                    format!(
                        "Pull request #{} moved from {} to {} since it was shown.",
                        pr.number,
                        expected.short(),
                        pr.head.sha.short()
                    ),
                )
                .with_hint("Look at it again before preparing a worktree for it."),
            );
        }
        if pr.state != PrState::Open {
            warnings.push(PlanWarning::new(
                "not_open",
                format!(
                    "Pull request #{} is {}. Its commits are fetched from pull/{}/head.",
                    pr.number,
                    match pr.state {
                        PrState::Merged => "merged",
                        PrState::Closed => "closed",
                        PrState::Open | PrState::Unknown => "in a state wtm does not recognise",
                    },
                    pr.number
                ),
            ));
        }

        // ── 4. probe ──
        progress.stage("probe", "Reading the repository", 4, STAGES);
        cancel.check()?;
        let remotes = self.git.remote_urls(&project.root)?;
        // As in the form: a hand-deleted checkout must not look like a worktree to reuse.
        let _ = self.git.prune_worktrees(&project.root);
        let worktrees = self.git.list_worktrees(&project.root)?;
        let local = self.git.branches(&project.root, BranchFilter::Local)?;
        let head_present = self
            .git
            .rev_parse(&project.root, pr.head.sha.as_str())?
            .is_some();

        // ── 5. plan ──
        progress.stage("plan", "Planning", 5, STAGES);
        cancel.check()?;
        let branch = review_branch(pr.number);
        let directory = self.review_directory(project, &req.ambient, pr)?;

        let (target, add) =
            if let Some((worktree, reason)) = find_reusable(&worktrees, pr, &branch, &directory) {
                let at_head = worktree.head.as_ref() == Some(&pr.head.sha);
                if !at_head {
                    warnings.push(PlanWarning::new(
                        "reused_behind_head",
                        format!(
                            "{} is at {}, and the pull request's head is {}. It is used as it is: \
                             wtm does not move a checkout that may hold work.",
                            worktree.path.display(),
                            worktree.head.as_ref().map_or("no commit", CommitId::short),
                            pr.head.sha.short()
                        ),
                    ));
                }
                let target = ReviewTarget::Reuse {
                    worktree,
                    reason,
                    at_head,
                };
                (target, None)
            } else {
                let branch = (req.checkout == ReviewCheckout::Branch).then_some(branch);
                let add = AddOptions {
                    path: directory.clone(),
                    branch: branch.clone(),
                    start_point: pr.head.sha.as_str().to_owned(),
                    track: TrackMode::NoTrack,
                    create_branch: branch.is_some(),
                };
                let target = ReviewTarget::Create {
                    git_argv: self.git.add_worktree_argv(&add),
                    directory: directory.clone(),
                    branch,
                };
                (target, Some(add))
            };
        let reusing = matches!(target, ReviewTarget::Reuse { .. });

        let fetch = if reusing {
            None
        } else if let Some(remote) = remote_for(&pr.base.repo, &remotes) {
            Some(ReviewFetch {
                remote,
                refspec: refspec_for(pr),
            })
        } else {
            let message = format!(
                "No remote of this repository points at {}, so the pull request's commits \
                 cannot be fetched.",
                pr.base.repo
            );
            if head_present {
                // Nothing needs fetching: the commit is already here.
                warnings.push(PlanWarning::new("no_remote", message));
            } else {
                preflight.push(PreflightItem::error("no_remote", message).with_hint(
                    "Add a remote for it, or open a pull request from this repository instead.",
                ));
            }
            None
        };

        let setup = if reusing {
            ReviewSetup::NotApplicable
        } else if project.setup.is_none() {
            ReviewSetup::NotDeclared
        } else if !req.prepare_environment {
            ReviewSetup::Skipped
        } else {
            // The planned worktree's tokens, for the preview; stage 9 renders again with the real
            // ones, as the form's does.
            let mut planned_ctx = req.ambient.clone();
            let branch = match &target {
                ReviewTarget::Create { branch, .. } => branch.as_ref(),
                ReviewTarget::Reuse { .. } => None,
            };
            insert_planned_worktree(&mut planned_ctx, &directory, branch);
            let rendered = self.render_setup(
                project,
                &planned_ctx,
                &FormValues::default(),
                Some(&directory),
            )?;
            match rendered {
                (Some(argv), Some(cwd)) => {
                    let fields = self.setup_form_fields(project);
                    if !fields.is_empty() {
                        warnings.push(PlanWarning::new(
                            "setup_reads_form_fields",
                            format!(
                                "This project's setup reads {} from the New Worktree form. A \
                                 review has no form, so {} empty.",
                                fields.join(", "),
                                if fields.len() == 1 {
                                    "it is"
                                } else {
                                    "they are"
                                }
                            ),
                        ));
                    }
                    ReviewSetup::Run { argv, cwd }
                }
                _ => ReviewSetup::NotDeclared,
            }
        };

        // ── 6b. preflight ──
        progress.stage("preflight", "Checking preconditions", 6, STAGES);
        if let ReviewTarget::Create {
            directory, branch, ..
        } = &target
        {
            if self.files.exists(directory) && !self.files.is_dir_empty(directory).unwrap_or(false)
            {
                preflight.push(
                    PreflightItem::error(
                        "dir_exists",
                        format!("{} already exists and is not empty.", directory.display()),
                    )
                    .with_hint(
                        "Remove it, or move it aside. git refuses to create a worktree over it.",
                    ),
                );
            }
            if let Some(branch) = branch
                && local.contains(branch)
            {
                preflight.push(
                    PreflightItem::error(
                        "branch_exists",
                        format!(
                            "A branch named `{branch}` already exists and is not checked out \
                             anywhere."
                        ),
                    )
                    .with_hint("Delete it, or check the pull request out detached instead."),
                );
            }
        }
        if let ReviewSetup::Run { argv, cwd } = &setup {
            preflight.extend(self.setup_program_missing(project, argv, Some(cwd)));
        }

        Ok(PlannedReview {
            preview: ReviewPreview {
                plan: ReviewPlan {
                    number: pr.number,
                    head: pr.head.sha.clone(),
                    fetch,
                    head_present,
                    target,
                    setup,
                },
                preflight,
                warnings,
            },
            add,
            setup_base_ctx: req.ambient.clone(),
        })
    }

    /// Where a review worktree goes: beside the project's other worktrees, named after the
    /// repository and the pull request (`webapp-pr-4031`), so it reads as what it is in the
    /// sidebar and two projects' reviews of the same number cannot collide.
    ///
    /// The project's `dir_base` decides the parent, as it does for the form. Its `naming.directory`
    /// template does not: that is built from form fields a review does not have.
    fn review_directory(
        &self,
        project: &Project,
        ctx: &Context,
        pr: &PullRequest,
    ) -> Result<PathBuf, WtmError> {
        let base = match &project.naming.dir_base {
            DirBase::RepoParent => project
                .root
                .parent()
                .map_or_else(|| project.root.clone(), std::path::Path::to_path_buf),
            DirBase::RepoRoot => project.root.clone(),
            DirBase::Custom(template) => {
                PathBuf::from(self.engine.render("naming.dir_base", template, ctx)?)
            }
        };
        let repo = project
            .root
            .file_name()
            .map_or_else(|| "review".to_owned(), |n| n.to_string_lossy().into_owned());
        Ok(self
            .files
            .absolutize(&base.join(format!("{repo}-pr-{}", pr.number)))?)
    }

    /// Form fields the setup command's templates read, which a review leaves empty.
    fn setup_form_fields(&self, project: &Project) -> Vec<String> {
        let Some(setup) = &project.setup else {
            return Vec::new();
        };
        let mut fields: Vec<String> = setup
            .command
            .run
            .iter()
            .chain(setup.command.env.values())
            .chain(
                setup
                    .command
                    .args_when
                    .iter()
                    .flat_map(|conditional| conditional.push.iter()),
            )
            .filter_map(|template| self.engine.referenced_tokens("setup", template).ok())
            .flatten()
            .filter(|token| project.field(token).is_some())
            .collect();
        fields.sort();
        fields.dedup();
        fields
    }
}

/// The refspec that brings the head commit. See the module docs for why a fork's, and a closed
/// pull request's, come from `pull/<n>/head`.
fn refspec_for(pr: &PullRequest) -> String {
    if pr.state == PrState::Open && !pr.cross_repository {
        pr.head.branch.clone()
    } else {
        format!("pull/{}/head", pr.number)
    }
}

/// An existing worktree that already holds this pull request. See the module docs for the order.
///
/// Among the matches, one already at the head wins over one behind it, so a fresh review worktree
/// is preferred to an old one on the same branch.
fn find_reusable(
    worktrees: &[Worktree],
    pr: &PullRequest,
    review_branch: &BranchRef,
    directory: &std::path::Path,
) -> Option<(Worktree, ReuseReason)> {
    let usable = || {
        worktrees
            .iter()
            .filter(|w| !w.is_bare && w.prunable.is_none())
    };
    let own_branch = BranchRef::new(pr.head.branch.as_str());
    let mut found: Vec<(&Worktree, ReuseReason)> = Vec::new();
    found.extend(
        usable()
            .filter(|w| w.branch() == Some(review_branch))
            .map(|w| (w, ReuseReason::ReviewBranch)),
    );
    found.extend(
        usable()
            .filter(|w| w.path == directory)
            .map(|w| (w, ReuseReason::ReviewDirectory)),
    );
    // A fork's branch name means nothing here: a fork's `main` is not your `main`.
    if !pr.cross_repository {
        found.extend(
            usable()
                .filter(|w| w.branch() == Some(&own_branch))
                .map(|w| (w, ReuseReason::PullRequestBranch)),
        );
    }
    found.extend(
        usable()
            .filter(|w| w.head.as_ref() == Some(&pr.head.sha))
            .map(|w| (w, ReuseReason::HeadCommit)),
    );

    let at_head = |w: &Worktree| w.head.as_ref() == Some(&pr.head.sha);
    found
        .iter()
        .find(|(w, _)| at_head(w))
        .or_else(|| found.first())
        .map(|(w, reason)| ((*w).clone(), *reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{ChecksSummary, PrBase, PrHead, RepoSlug};
    use crate::model::{Checkout, WorktreeId};

    fn base() -> RepoSlug {
        RepoSlug::new("github.com", "acme", "webapp")
    }

    fn pr(cross_repository: bool, state: PrState) -> PullRequest {
        PullRequest {
            number: 4031,
            url: "https://github.com/acme/webapp/pull/4031".to_owned(),
            title: "CSV export".to_owned(),
            author: Some("sam".to_owned()),
            state,
            draft: false,
            head: PrHead {
                repo: Some(RepoSlug::new(
                    "github.com",
                    if cross_repository { "sam" } else { "acme" },
                    "webapp",
                )),
                branch: "main".to_owned(),
                sha: CommitId::new("7c9e1d0000000000000000000000000000000000"),
            },
            base: PrBase {
                repo: base(),
                branch: "main".to_owned(),
                sha: None,
            },
            cross_repository,
            maintainer_can_modify: false,
            requested_reviewers: Vec::new(),
            checks: ChecksSummary::default(),
        }
    }

    #[test]
    fn an_open_pull_request_from_this_repository_is_fetched_by_its_branch_and_anything_else_by_its_pull_ref()
     {
        assert_eq!(refspec_for(&pr(false, PrState::Open)), "main");
        assert_eq!(refspec_for(&pr(true, PrState::Open)), "pull/4031/head");
        assert_eq!(refspec_for(&pr(false, PrState::Merged)), "pull/4031/head");
        assert_eq!(refspec_for(&pr(false, PrState::Closed)), "pull/4031/head");
    }

    fn worktree(path: &str, branch: Option<&str>, head: &str) -> Worktree {
        Worktree {
            id: WorktreeId::from_path(std::path::Path::new(path)),
            path: PathBuf::from(path),
            head: Some(CommitId::new(head)),
            checkout: branch.map_or(Checkout::Detached, |b| Checkout::Branch {
                branch: BranchRef::new(b),
            }),
            is_main: false,
            is_bare: false,
            locked: None,
            prunable: None,
        }
    }

    #[test]
    fn a_forks_branch_name_never_picks_your_worktree_of_the_same_name() {
        // The fork's head branch is `main`; so is the main checkout. Only a same-repository pull
        // request's branch is evidence.
        let worktrees = [worktree("/x/webapp", Some("main"), "1111")];
        let dir = PathBuf::from("/x/webapp-pr-4031");
        let review = review_branch(4031);
        assert!(find_reusable(&worktrees, &pr(true, PrState::Open), &review, &dir).is_none());
        let (found, reason) =
            find_reusable(&worktrees, &pr(false, PrState::Open), &review, &dir).unwrap();
        assert_eq!(found.path, PathBuf::from("/x/webapp"));
        assert_eq!(reason, ReuseReason::PullRequestBranch);
    }

    #[test]
    fn a_worktree_already_at_the_head_wins_over_an_older_review_of_the_same_pull_request() {
        let head = "7c9e1d0000000000000000000000000000000000";
        let worktrees = [
            worktree(
                "/x/webapp-pr-4031",
                None,
                "0ld0000000000000000000000000000000000000",
            ),
            worktree("/x/elsewhere", None, head),
        ];
        let (found, reason) = find_reusable(
            &worktrees,
            &pr(true, PrState::Open),
            &review_branch(4031),
            std::path::Path::new("/x/webapp-pr-4031"),
        )
        .unwrap();
        assert_eq!(found.path, PathBuf::from("/x/elsewhere"));
        assert_eq!(reason, ReuseReason::HeadCommit);

        let (found, reason) = find_reusable(
            &worktrees[..1],
            &pr(true, PrState::Open),
            &review_branch(4031),
            std::path::Path::new("/x/webapp-pr-4031"),
        )
        .unwrap();
        assert_eq!(found.path, PathBuf::from("/x/webapp-pr-4031"));
        assert_eq!(reason, ReuseReason::ReviewDirectory);
    }
}

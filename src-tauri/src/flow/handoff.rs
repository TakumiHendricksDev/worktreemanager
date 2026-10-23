//! The hand-over: what an implementation left for you to test, and a revision when testing it shows
//! something to change.
//!
//! # Evidence, not a verdict
//!
//! The panel sets out what is known, from where it is known: the diff from where implementing began,
//! the checks the app ran with what they printed, what the review left and why, and, for each
//! acceptance criterion, the writer's own word beside the findings that cite it. Nothing here says a
//! criterion is met: that is yours to judge. What the app does say is when its evidence no longer
//! describes the worktree — you edited by hand, or committed since — because a check that passed on
//! another tree is not a check of this one.
//!
//! # A revision is the same run, the same writer, the same conversation
//!
//! Revise sends your words to the writer that did the work, in its conversation, under the writer
//! lease the hand-over gave up, and the run goes back to Implementing. What follows is what followed
//! the first round: the app's commits, the checks, the review, a fix loop whose bounds start again,
//! because a revision is new work. The pull request's feedback is revised the same way, from the
//! Inbox, once the work is published.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use wtm_core::flow::{
    CheckRun, Coverage, FindingId, FlowDir, FlowEvent, FlowId, ImplementChange, ImplementOutcome,
    InboxKind, LeaseKind, LeaseRelease, NewRound, Phase, RoundKind, StepChange, StepStatus,
    coverage,
};
use wtm_core::ports::{ImplementStore, PlanStore, RunStore};
use wtm_git::push::{self, DiffStyle};

use super::fix::{FixFile, PATCH_LIMIT};
use super::{Engine, Refusal, text};

/// How much of a check's output the panel shows: its end, where a failure says what failed.
const TAIL_LINES: usize = 40;

/// One check as the hand-over shows it: its latest run, the end of what it printed, and whether the
/// worktree changed since in a way that touches it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckEvidence {
    pub run: CheckRun,
    pub tail: Option<String>,
    pub stale: bool,
}

/// What changed in the worktree since the checks ran and the review read it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SinceEvidence {
    pub paths: Vec<String>,
    /// False when git listed only part of them.
    pub complete: bool,
    /// Findings whose lines changed since they were reported.
    pub findings: Vec<FindingId>,
}

/// Everything the hand-over panel shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
pub struct HandoffView {
    pub flow: FlowId,
    pub key: String,
    pub project: String,
    /// The worktree's id, for its environment's controls.
    pub worktree: String,
    pub branch: Option<String>,
    /// `HEAD` when implementing began: what the diff is measured from.
    pub base_sha: String,
    pub head_sha: String,
    pub files: Vec<FixFile>,
    /// The diff to read, with context; cut at [`PATCH_LIMIT`] bytes.
    pub patch: String,
    pub truncated: bool,
    /// Changed in the worktree and not committed: your edits, or anything a writer left.
    pub uncommitted: Vec<String>,
    pub coverage: Vec<Coverage>,
    /// The latest run of each check Verifying runs, in their configured order.
    pub checks: Vec<CheckEvidence>,
    /// Set when the worktree no longer is what the checks and the review saw.
    pub since: Option<SinceEvidence>,
    /// Whether Revise would send a revision now.
    pub can_revise: bool,
}

/// The end of a check's output: its last [`TAIL_LINES`] lines.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

impl Engine {
    /// Whether `flow` is where a revision may be asked for: handed over, or published, and still.
    pub(super) fn revisable(&self, flow: FlowId) -> Result<(), String> {
        if self.is_running(flow) {
            return Err("its writer is working now".to_owned());
        }
        let state = self.state(flow)?;
        if state.end.is_some() {
            return Err("this run has ended".to_owned());
        }
        if !matches!(
            state.phase,
            Phase::ReadyToTest | Phase::Publishing | Phase::InReview
        ) {
            return Err(format!(
                "a revision follows the hand-over; this run is at {:?}",
                state.phase
            ));
        }
        if let Some(hold) = &state.hold {
            return Err(format!(
                "this run is waiting ({:?}); settle that first",
                hold.kind()
            ));
        }
        if self.publishing_now(flow) {
            return Err("its pull request is being written to right now".to_owned());
        }
        let implementation = self
            .db()?
            .implementation(flow)
            .map_err(text)?
            .ok_or("nothing was implemented to revise")?;
        if implementation.outcome != Some(ImplementOutcome::Ready) {
            return Err("the implementation has not been handed over".to_owned());
        }
        Ok(())
    }

    /// The hand-over of an issue run's implementation.
    ///
    /// # Errors
    ///
    /// When the run implemented nothing, or its worktree or record cannot be read.
    pub fn handoff_view(&self, flow: FlowId) -> Result<HandoffView, String> {
        let db = self.db()?;
        let implementation = db
            .implementation(flow)
            .map_err(text)?
            .ok_or("this run has implemented nothing yet")?;
        let project = self
            .app
            .project(implementation.project.as_str())
            .map_err(text)?;
        let worktree = self
            .app
            .worktree(&project, &implementation.worktree)
            .map_err(|e| format!("the issue's worktree is not available: {e}"))?;
        let runner = self.app.runner.as_ref();
        let base = implementation.base_sha.clone();
        let head = self
            .app
            .git
            .rev_parse(&worktree.path, "HEAD")
            .map_err(text)?
            .map_or_else(|| base.clone(), |c| c.as_str().to_owned());
        let (files, patch, truncated) = if head == base {
            (Vec::new(), String::new(), false)
        } else {
            let mut patch = push::diff(runner, &worktree.path, &base, &head, DiffStyle::Reading)
                .map_err(text)?;
            let truncated = patch.len() > PATCH_LIMIT;
            if truncated {
                let mut end = PATCH_LIMIT;
                while !patch.is_char_boundary(end) {
                    end -= 1;
                }
                patch.truncate(end);
            }
            let files = push::numstat(runner, &worktree.path, &base, &head)
                .map_err(text)?
                .into_iter()
                .map(|(path, added, removed)| FixFile {
                    path,
                    added,
                    removed,
                })
                .collect();
            (files, patch, truncated)
        };
        let uncommitted = wtm_git::checkpoint::changed(runner, &worktree.path).map_err(text)?;

        let rounds = db.rounds(flow).map_err(text)?;
        let plan = db
            .plans(flow)
            .map_err(text)?
            .into_iter()
            .find(|p| p.revision == implementation.plan_revision);
        let (findings, records) = match implementation.review_flow {
            Some(review) => (
                db.findings(review).map_err(text)?,
                db.checks(review).map_err(text)?,
            ),
            None => (Vec::new(), Vec::new()),
        };
        let coverage = plan.map_or_else(Vec::new, |plan| {
            coverage(&plan.plan.acceptance_criteria, &rounds, &findings)
        });

        let staleness = implementation
            .review_flow
            .and_then(|review| self.staleness(review).ok());
        let configured: Vec<String> = Self::implementer_of(&project)
            .zip(project.workflow.as_ref())
            .map(|((implement, _), workflow)| {
                implement
                    .checks(workflow)
                    .into_iter()
                    .map(|(id, _)| id.to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut latest: BTreeMap<&str, &CheckRun> = BTreeMap::new();
        for run in &records {
            latest.insert(run.check.as_str(), run);
        }
        let dir = implementation
            .review_flow
            .map(|review| FlowDir::new(&self.app.config.paths().flow_dir, review));
        let checks = configured
            .iter()
            .filter_map(|id| latest.get(id.as_str()))
            .map(|run| CheckEvidence {
                tail: run
                    .output
                    .as_ref()
                    .zip(dir.as_ref())
                    .and_then(|(blob, dir)| std::fs::read_to_string(dir.resolve(&blob.path)).ok())
                    .map(|out| tail(&out)),
                stale: staleness
                    .as_ref()
                    .is_some_and(|s| s.stale_checks.contains(&run.id)),
                run: (*run).clone(),
            })
            .collect();
        let since = staleness.filter(|s| s.changed).map(|s| SinceEvidence {
            paths: s.paths,
            complete: s.complete,
            findings: s.stale_findings,
        });
        Ok(HandoffView {
            flow,
            key: implementation.key.clone(),
            project: implementation.project.as_str().to_owned(),
            worktree: implementation.worktree.clone(),
            branch: worktree.branch().map(|b| b.as_str().to_owned()),
            base_sha: base,
            head_sha: head,
            files,
            patch,
            truncated,
            uncommitted,
            coverage,
            checks,
            since,
            can_revise: self.revisable(flow).is_ok(),
        })
    }

    /// Send the implementation back to its writer with `note`: what testing it showed, or the pull
    /// request's feedback. The run goes back to Implementing, under the writer lease again.
    ///
    /// # Errors
    ///
    /// Why it did not: no note, nothing handed over, the run is working or waiting, the provider
    /// cannot be used, or someone else holds the worktree. Nothing was recorded.
    pub fn revise(self: &Arc<Self>, flow: FlowId, note: &str) -> Result<(), Refusal> {
        let note = note.trim();
        if note.is_empty() {
            return Err("say what to change: a revision is sent in your words".into());
        }
        let _one = self.starting.lock();
        self.revisable(flow)?;
        let db = self.db()?;
        let implementation = db
            .implementation(flow)
            .map_err(text)?
            .ok_or("nothing was implemented to revise")?;
        let project = self
            .app
            .project(implementation.project.as_str())
            .map_err(text)?;
        let (_, spec) = Self::implementer_of(&project)
            .ok_or_else(|| Refusal::from(super::implement::no_implementer()))?;
        self.host.preflight(&[spec.provider.id()])?;
        let worktree = self
            .app
            .worktree(&project, &implementation.worktree)
            .map_err(|e| format!("the issue's worktree is not available: {e}"))?;
        let taken =
            self.take_writer_lease(flow, &worktree.path.to_string_lossy(), &implementation.role)?;
        let recorded = (|| -> Result<(), String> {
            let rounds = db.rounds(flow).map_err(text)?;
            if !rounds.iter().any(|r| r.ended_at.is_none()) {
                db.start_round(
                    &NewRound {
                        flow,
                        kind: RoundKind::Revise,
                        plan_revision: implementation.plan_revision,
                        findings: Vec::new(),
                        checks: Vec::new(),
                        note: Some(note.to_owned()),
                    },
                    self.now(),
                )
                .map_err(text)?;
            }
            // What the last hand-over left stays recorded until the next one replaces it: the
            // revision's turn tells the writer about it.
            db.change_implementation(
                flow,
                &ImplementChange {
                    outcome: wtm_core::flow::Field::Clear,
                    ..ImplementChange::default()
                },
                self.now(),
            )
            .map_err(text)?;
            if let Some(step) = db
                .steps(flow)
                .map_err(text)?
                .into_iter()
                .find(|s| s.role == implementation.role)
            {
                db.update_step(
                    step.id,
                    &StepChange::status(StepStatus::Pending).detail("revision asked".to_owned()),
                    self.now(),
                )
                .map_err(text)?;
            }
            self.record(
                flow,
                &FlowEvent::Advanced {
                    to: Phase::Implementing,
                },
            )?;
            Ok(())
        })();
        if let Err(reason) = recorded {
            if taken {
                self.release_leases_of(flow, LeaseKind::Writer, LeaseRelease::Failed);
            }
            return Err(reason.into());
        }
        self.settle(flow, &[InboxKind::ReviewReady, InboxKind::Failure]);
        tracing::info!(%flow, "sent a revision to the implementer");
        self.notice.changed(flow);
        Ok(self.spawn_implementation(flow, super::implement::WriteTurn::Next)?)
    }
}

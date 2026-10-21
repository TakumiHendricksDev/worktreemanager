//! What a review concluded, whether it still holds, and running it again where it does not.
//!
//! # Consolidated as each role submits
//!
//! A role's result is consolidated with the others' the moment it is accepted, not once the run
//! ends, so a run that a quit cut off still shows the findings of the roles that finished, merged
//! the way a finished run's are. The merge itself is `wtm_core::flow::consolidate`; this gathers
//! the reports from each step's current result and saves what changed. A step's current result
//! is the one its newest accepted attempt submitted, which is also what makes a report's identity:
//! that attempt and its place in the submission.
//!
//! # Stale is measured, never assumed
//!
//! Whether a finding still describes the code is a question for git, asked when the Work view asks:
//! the worktree's tree now, uncommitted and untracked work included, against the tree each report
//! was found on. A check ran against a whole tree, and a test can depend on any file in it, so a
//! check is stale after any change. Nothing here watches the worktree: the window asks again when
//! the worktree's listing changes, which it learns on focus, never on a timer (ARCHITECTURE §8).
//!
//! # A rerun is the same review, moved forward
//!
//! Rerunning records a new snapshot on the same run and runs again only the roles whose files
//! changed since they read them, plus any that failed, since a failed role has no result to keep.
//! The others keep their results and their findings, which go on being compared with the trees
//! they were found on. One run rather than a new one because the findings, their dismissals and
//! the rules saved from them are about one piece of work, and splitting them across runs would
//! leave the Reviews tab choosing which run to believe.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use wtm_core::flow::knowledge::{RuleQuery, select};
use wtm_core::flow::{
    CheckRunId, Evidence, Field, FindingId, FindingRecord, FlowDir, FlowEvent, FlowId, HoldKind,
    InboxKind, Invocation, InvocationStatus, Phase, ReportedFinding, ReviewInvocation, RoleInput,
    RoleProvider, RoleSpec, RuleId, RuleKind, ScratchLabel, Snapshot, SnapshotId, Source, Step,
    StepChange, StepStatus, Submission, TreeCheck, WorkflowSpec, consolidate, finding_is_stale,
    reconcile, role_is_affected,
};
use wtm_core::ports::{RuleStore, RunStore};
use wtm_core::usecase::snapshot::Snapshots;

use super::{Engine, Refusal, RunCtx, text};

/// A check a run's roles may ask for, as the Reviews tab lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckChoice {
    pub id: String,
    pub label: String,
    /// Services it needs, which wtm does not start yet: such a check is always skipped.
    pub needs: Vec<String>,
    /// The roles that may run it.
    pub roles: Vec<String>,
}

/// What a run's roles were given to judge the change by.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    /// Rule revisions any of its roles were given, in rule order.
    pub rules: Vec<AppliedRule>,
    /// Repository instruction files any role's brief carried, in the order they were first given.
    pub sources: Vec<String>,
    /// Skills its roles name, from your config.
    pub skills: Vec<AppliedSkill>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedRule {
    pub rule: RuleId,
    pub revision: u32,
    pub kind: RuleKind,
    /// The rule as that revision stated it.
    pub text: String,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedSkill {
    pub role: String,
    pub provider: RoleProvider,
    pub skill: String,
}

/// Which roles a rerun runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rerun {
    /// Those whose files changed since they read them, and those that failed.
    Affected,
    /// Every role the run has.
    All,
}

/// Whether a run's results still describe the worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Staleness {
    pub flow: FlowId,
    /// The worktree's tree now, uncommitted and untracked work included.
    pub tree_sha: String,
    pub head_sha: Option<String>,
    /// Whether the worktree differs from the run's latest snapshot.
    pub changed: bool,
    /// The files that differ from it.
    pub paths: Vec<String>,
    /// False when git listed more changed files than could be kept: every result is then stale.
    pub complete: bool,
    pub stale_findings: Vec<FindingId>,
    pub stale_checks: Vec<CheckRunId>,
    /// The roles Rerun affected would run, in the run's order.
    pub affected: Vec<String>,
}

/// The newest attempt at `step` that handed in a result.
fn submitted_by(invocations: &[Invocation], step: &Step) -> Option<Invocation> {
    invocations
        .iter()
        .rev()
        .find(|i| i.step == step.id && i.status == InvocationStatus::Submitted)
        .cloned()
}

/// The checks the run's roles may ask for, in the order the roles name them.
#[must_use]
pub fn expected_checks(workflow: Option<&WorkflowSpec>, steps: &[Step]) -> Vec<CheckChoice> {
    let Some(workflow) = workflow else {
        return Vec::new();
    };
    let mut out: Vec<CheckChoice> = Vec::new();
    for step in steps
        .iter()
        .filter(|s| !matches!(s.status, StepStatus::Replaced))
    {
        let Some(role) = workflow.role.get(&step.role) else {
            continue;
        };
        for id in &role.checks {
            let Some(check) = workflow.check.get(id) else {
                continue;
            };
            if let Some(known) = out.iter_mut().find(|choice| &choice.id == id) {
                if !known.roles.contains(&step.role) {
                    known.roles.push(step.role.clone());
                }
                continue;
            }
            out.push(CheckChoice {
                id: id.clone(),
                label: check.label.clone().unwrap_or_else(|| id.clone()),
                needs: check.needs.clone(),
                roles: vec![step.role.clone()],
            });
        }
    }
    out
}

impl Engine {
    /// Consolidate a run's current results into its findings and save what changed. Logged rather
    /// than returned: a role's result is already safe in its step, and the next submission or the
    /// end of the run consolidates again.
    pub(super) fn consolidate(&self, flow: FlowId) {
        if let Err(reason) = self.consolidate_now(flow) {
            tracing::warn!(%flow, %reason, "could not consolidate a review's findings");
        }
    }

    fn consolidate_now(&self, flow: FlowId) -> Result<(), String> {
        let _one = self.consolidating.lock();
        let db = self.db()?;
        let steps = db.steps(flow).map_err(text)?;
        let invocations = db.invocations(flow).map_err(text)?;
        let mut trees: BTreeMap<SnapshotId, String> = BTreeMap::new();
        let mut evidence = Vec::new();
        for step in steps
            .iter()
            .filter(|s| !matches!(s.status, StepStatus::Replaced))
        {
            let Some(Submission::Findings(report)) = &step.result else {
                continue;
            };
            let Some(attempt) = submitted_by(&invocations, step) else {
                continue;
            };
            let Some(snapshot) = attempt.snapshot else {
                continue;
            };
            let tree = if let Some(tree) = trees.get(&snapshot) {
                tree.clone()
            } else {
                let tree = db
                    .snapshot(snapshot)
                    .map_err(text)?
                    .map(|s| s.tree_sha)
                    .ok_or_else(|| format!("snapshot {snapshot} is missing"))?;
                trees.insert(snapshot, tree.clone());
                tree
            };
            for (ordinal, reported) in (0u32..).zip(&report.findings) {
                evidence.push(Evidence {
                    source: Source {
                        step: step.id,
                        invocation: attempt.id,
                        ordinal,
                        role: step.role.clone(),
                        provider: step.provider,
                        model: step.model.clone(),
                        workspace: step.workspace,
                        tree_sha: tree.clone(),
                    },
                    reported: reported.clone(),
                });
            }
        }
        let saved = db.findings(flow).map_err(text)?;
        let change = reconcile(&saved, &evidence, &consolidate(&evidence));
        if change.create.is_empty() && change.supersede.is_empty() {
            return Ok(());
        }
        db.save_consolidation(flow, &change, self.now())
            .map_err(text)?;
        self.notice.changed(flow);
        Ok(())
    }

    /// What a role reported last time, when that was on another tree: a rerun asks it to check
    /// each one against the code as it is now.
    pub(super) fn earlier_findings(&self, step: &Step, tree: &str) -> Vec<ReportedFinding> {
        let Some(Submission::Findings(report)) = &step.result else {
            return Vec::new();
        };
        let Ok(db) = self.db() else {
            return Vec::new();
        };
        let read = db
            .invocations(step.flow)
            .ok()
            .and_then(|all| submitted_by(&all, step))
            .and_then(|attempt| attempt.snapshot)
            .and_then(|id| db.snapshot(id).ok().flatten());
        match read {
            Some(snapshot) if snapshot.tree_sha != tree => report.findings.clone(),
            _ => Vec::new(),
        }
    }

    /// The knowledge section of a role's brief, and the instruction files it carries.
    ///
    /// The instructions are read from `cwd`, the checkout the role reads, so they are the snapshot's
    /// and not whatever the worktree holds by the time the role starts. The rule revisions chosen
    /// are recorded against this attempt before the role sees them: what a review was told is
    /// evidence, like what it read.
    pub(super) fn knowledge_for(
        &self,
        ctx: &RunCtx,
        step: &Step,
        spec: &RoleSpec,
        cwd: &Path,
        attempt: u32,
    ) -> (Option<String>, Vec<String>) {
        let wants_instructions = spec.inputs.contains(&RoleInput::Instructions);
        let query = RuleQuery::for_role(spec, ctx.changed.as_deref().unwrap_or(&[]));
        if !wants_instructions && query.is_none() {
            return (None, Vec::new());
        }
        let instructions = if wants_instructions {
            match crate::knowledge::instruction_set(self.app.git.as_ref(), cwd, &ctx.patterns) {
                Ok((set, _)) => Some(set),
                Err(error) => {
                    tracing::warn!(flow = %ctx.flow, role = %step.role, error = %error.message,
                        "could not read the instruction sources for a role");
                    None
                }
            }
        } else {
            None
        };
        let rules = query.and_then(|query| {
            let db = self.db().ok()?;
            let all = db.rules(&ctx.project_id).ok()?;
            let selection = select(&all, &ctx.project_id, &query);
            if let Err(error) = db.record_applications(
                &ReviewInvocation {
                    flow: ctx.flow,
                    role: step.role.clone(),
                    attempt,
                },
                &selection.applied,
                self.now(),
            ) {
                tracing::warn!(flow = %ctx.flow, role = %step.role, %error,
                    "could not record the rules a role was given");
            }
            Some((selection, all))
        });
        let section = wtm_core::flow::knowledge::brief(
            instructions.as_ref(),
            rules
                .as_ref()
                .map(|(selection, all)| (selection, all.as_slice())),
        );
        let sources = instructions
            .map(|set| set.documents.into_iter().map(|d| d.path).collect())
            .unwrap_or_default();
        (Some(section), sources)
    }

    /// What the run's roles were given: the rule revisions recorded against their attempts, the
    /// instruction files their briefs carried, and the skills their roles name.
    pub(super) fn applied(
        &self,
        flow: FlowId,
        workflow: Option<&WorkflowSpec>,
        steps: &[Step],
        invocations: &[Invocation],
    ) -> Result<Applied, String> {
        let db = self.db()?;
        let mut rules: BTreeMap<(RuleId, u32), AppliedRule> = BTreeMap::new();
        for application in db.applications(flow).map_err(text)? {
            let key = (application.rule, application.revision);
            if let Some(known) = rules.get_mut(&key) {
                if !known.roles.contains(&application.invocation.role) {
                    known.roles.push(application.invocation.role.clone());
                }
                continue;
            }
            let Some(revision) = db
                .rule(application.rule)
                .map_err(text)?
                .and_then(|rule| rule.revision(application.revision).cloned())
            else {
                continue;
            };
            rules.insert(
                key,
                AppliedRule {
                    rule: application.rule,
                    revision: application.revision,
                    kind: revision.draft.kind,
                    text: revision.draft.text,
                    roles: vec![application.invocation.role],
                },
            );
        }
        let mut sources: Vec<String> = Vec::new();
        for path in invocations.iter().flat_map(|i| &i.instruction_sources) {
            if !sources.contains(path) {
                sources.push(path.clone());
            }
        }
        let skills = workflow.map_or_else(Vec::new, |workflow| {
            steps
                .iter()
                .filter(|s| !matches!(s.status, StepStatus::Skipped | StepStatus::Replaced))
                .filter_map(|step| Some((step, workflow.role.get(&step.role)?)))
                .flat_map(|(step, role)| {
                    role.skill_names().map(|skill| AppliedSkill {
                        role: step.role.clone(),
                        provider: role.provider,
                        skill: skill.to_owned(),
                    })
                })
                .collect()
        });
        Ok(Applied {
            rules: rules.into_values().collect(),
            sources,
            skills,
        })
    }

    /// Whether a run's results still describe its worktree, and what Rerun affected would run.
    ///
    /// # Errors
    ///
    /// When the run, its snapshot or its worktree cannot be found, or git cannot read the tree.
    pub fn staleness(&self, flow: FlowId) -> Result<Staleness, String> {
        let db = self.db()?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let snapshot = review
            .snapshot
            .and_then(|id| db.snapshot(id).ok().flatten())
            .ok_or("this review has no snapshot yet")?;
        let project = self.app.project(review.project.as_str()).map_err(text)?;
        let worktree = self
            .app
            .worktree(&project, &review.worktree)
            .map_err(|e| format!("the worktree is no longer there: {e}"))?;
        let dir = FlowDir::new(&self.app.config.paths().flow_dir, flow);
        let label = ScratchLabel::new(&format!(
            "stale-{}",
            self.scratch.fetch_add(1, Ordering::SeqCst)
        ))
        .ok_or("a scratch label")?;
        let now = self
            .app
            .git
            .snapshot_tree(&worktree.path, &dir.scratch_index(&label))
            .map_err(text)?;
        let head = self
            .app
            .git
            .rev_parse(&worktree.path, "HEAD")
            .ok()
            .flatten()
            .map(|commit| commit.as_str().to_owned());

        let mut compared: BTreeMap<String, TreeCheck> = BTreeMap::new();
        let mut compare = |tree: &str| -> TreeCheck {
            if let Some(known) = compared.get(tree) {
                return known.clone();
            }
            let check = if tree == now {
                TreeCheck::Unchanged
            } else {
                match self.app.git.tree_changes(&worktree.path, tree, &now) {
                    Ok(listed) => TreeCheck::Changed {
                        tree_sha: now.clone(),
                        paths: listed.paths.into_iter().collect(),
                        complete: !listed.truncated,
                    },
                    // A tree git can no longer read, collected since: say nothing holds rather
                    // than guess that it does.
                    Err(_) => TreeCheck::Changed {
                        tree_sha: now.clone(),
                        paths: BTreeSet::new(),
                        complete: false,
                    },
                }
            };
            compared.insert(tree.to_owned(), check.clone());
            check
        };

        let overall = compare(&snapshot.tree_sha);
        let findings = db.findings(flow).map_err(text)?;
        let stale_findings = findings
            .iter()
            .filter(|record| finding_is_stale(record, |tree| Some(compare(tree))))
            .map(|record| record.finding.id)
            .collect();
        let stale_checks = db
            .checks(flow)
            .map_err(text)?
            .iter()
            .filter(|run| compare(&run.tree_sha).is_stale())
            .map(|run| run.id)
            .collect();
        let affected = Self::affected_roles(&db, flow, &project, &snapshot, &mut compare)?;
        let (paths, complete) = match &overall {
            TreeCheck::Unchanged => (Vec::new(), true),
            TreeCheck::Changed {
                paths, complete, ..
            } => (paths.iter().cloned().collect(), *complete),
        };
        Ok(Staleness {
            flow,
            tree_sha: now,
            head_sha: head,
            changed: overall.is_stale(),
            paths,
            complete,
            stale_findings,
            stale_checks,
            affected,
        })
    }

    /// The roles Rerun affected would run: those whose files changed since the tree they read,
    /// those with a report that went stale, skipped ones whose files now changed, and failed ones.
    ///
    /// A role's paths say what it was asked to review, not what it may report on: a conventions
    /// role scoped to `web/**` can still see, and report, a bug in a file the change touched
    /// elsewhere. Its paths alone would leave that report stale with nothing offering to run it
    /// again, which is what the first real run of this showed. So a stale report of its own makes a
    /// role affected too.
    fn affected_roles(
        db: &wtm_appdb::AppDb,
        flow: FlowId,
        project: &wtm_core::model::Project,
        snapshot: &Snapshot,
        compare: &mut dyn FnMut(&str) -> TreeCheck,
    ) -> Result<Vec<String>, String> {
        let Some(workflow) = project.workflow.as_ref() else {
            return Ok(Vec::new());
        };
        let steps = db.steps(flow).map_err(text)?;
        let invocations = db.invocations(flow).map_err(text)?;
        let mut reported_stale: BTreeSet<wtm_core::flow::StepId> = BTreeSet::new();
        for record in db.findings(flow).map_err(text)? {
            // A dismissed finding is settled: its file changing is no reason to run its role.
            if record.finding.dismissed.is_some() {
                continue;
            }
            for evidence in &record.evidence {
                if evidence.is_stale(&compare(&evidence.source.tree_sha)) {
                    reported_stale.insert(evidence.source.step);
                }
            }
        }
        let mut out = Vec::new();
        for step in &steps {
            let Some(role) = workflow.role.get(&step.role) else {
                continue;
            };
            let affected = match step.status {
                StepStatus::Replaced | StepStatus::Cancelled => false,
                StepStatus::Failed if step.result.is_none() => true,
                StepStatus::Skipped => {
                    !role.paths.is_empty()
                        && role_is_affected(
                            &role.paths,
                            role.workspace(),
                            &compare(&snapshot.tree_sha),
                        )
                }
                _ if step.result.is_some() => {
                    let read = submitted_by(&invocations, step)
                        .and_then(|attempt| attempt.snapshot)
                        .and_then(|id| db.snapshot(id).ok().flatten())
                        .map_or_else(|| snapshot.tree_sha.clone(), |s| s.tree_sha);
                    reported_stale.contains(&step.id)
                        || role_is_affected(&role.paths, step.workspace, &compare(&read))
                }
                _ => false,
            };
            if affected {
                out.push(step.role.clone());
            }
        }
        Ok(out)
    }

    /// Run a consolidated review's roles again against a new snapshot of its worktree.
    ///
    /// # Errors
    ///
    /// When the review is running, waiting on something, has ended, or has nothing to rerun; when a
    /// provider would not bill a subscription; or when the snapshot cannot be taken.
    pub fn rerun(self: &Arc<Self>, flow: FlowId, scope: Rerun) -> Result<(), Refusal> {
        if self.is_running(flow) {
            return Err("this review is still running".into());
        }
        let db = self.db()?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let state = self.state(flow)?;
        if state.end.is_some() {
            return Err("this review has ended; start a new one".into());
        }
        if let Some(hold) = &state.hold {
            return Err(match hold.kind() {
                HoldKind::Interrupted | HoldKind::WaitingCapacity => {
                    "this review has roles left to run; resume it first".to_owned()
                }
                other => format!("this review is waiting on {other:?}"),
            }
            .into());
        }
        if state.phase != Phase::Consolidated {
            return Err("only a finished review can be run again".into());
        }
        let project = self.app.project(review.project.as_str()).map_err(text)?;
        let workflow = project
            .workflow
            .clone()
            .ok_or("the project's workflow config has gone")?;
        let worktree = self
            .app
            .worktree(&project, &review.worktree)
            .map_err(|e| format!("the worktree is no longer there: {e}"))?;
        let plan = wtm_core::flow::plan(&workflow, &review.recipe, None).map_err(text)?;
        let providers: Vec<&str> = plan
            .providers(&workflow)
            .into_iter()
            .map(RoleProvider::id)
            .collect();
        self.host.preflight(&providers)?;

        let steps = db.steps(flow).map_err(text)?;
        let roles: BTreeSet<String> = match scope {
            Rerun::Affected => self.staleness(flow)?.affected.into_iter().collect(),
            Rerun::All => steps
                .iter()
                .filter(|s| !matches!(s.status, StepStatus::Replaced))
                .map(|s| s.role.clone())
                .collect(),
        };
        if roles.is_empty() {
            return Err(
                "nothing the review's roles read has changed since they ran, so there is nothing \
                 to run again"
                    .into(),
            );
        }

        let dir = FlowDir::new(&self.app.config.paths().flow_dir, flow);
        let snapshots = Snapshots {
            git: Arc::clone(&self.app.git),
            files: Arc::clone(&self.app.files),
        };
        let label = ScratchLabel::new(&format!(
            "rerun-{}",
            self.scratch.fetch_add(1, Ordering::SeqCst)
        ))
        .ok_or("a scratch label")?;
        let snapshot = snapshots
            .take(&worktree.path, &review.base, &dir, &label)
            .map_err(|e| format!("the snapshot could not be taken: {e}"))?;
        let current = review
            .snapshot
            .and_then(|id| db.snapshot(id).ok().flatten());
        if current.as_ref() != Some(&snapshot) {
            db.record_snapshot(flow, &snapshot, self.now())
                .map_err(text)?;
        }

        // Planned against the new snapshot's changes, so a role none of whose files are in the
        // change any more is skipped again rather than run over nothing.
        let known: Option<BTreeSet<String>> = self
            .app
            .git
            .tree_changes(&worktree.path, &snapshot.base_sha, &snapshot.tree_sha)
            .ok()
            .filter(|listed| !listed.truncated)
            .map(|listed| listed.paths.into_iter().collect());
        let replanned =
            wtm_core::flow::plan(&workflow, &review.recipe, known.as_ref()).map_err(text)?;
        let skipped: BTreeSet<&str> = replanned
            .stages
            .iter()
            .flat_map(|stage| &stage.skipped)
            .map(|role| role.id.as_str())
            .collect();
        for step in steps.iter().filter(|s| roles.contains(&s.role)) {
            let change = if skipped.contains(step.role.as_str()) {
                StepChange::status(StepStatus::Skipped).detail("none of its paths changed")
            } else {
                StepChange {
                    detail: Field::Clear,
                    resets_at: Field::Clear,
                    skill_problem: Field::Clear,
                    ..StepChange::status(StepStatus::Pending)
                }
            };
            db.update_step(step.id, &change, self.now()).map_err(text)?;
            let _ = db.resolve_inbox(flow, InboxKind::Failure, Some(&step.role), self.now());
        }
        self.record(
            flow,
            &FlowEvent::Advanced {
                to: Phase::Reviewing,
            },
        )?;
        self.notice.changed(flow);
        Ok(self.spawn(flow)?)
    }

    /// Dismiss a finding, saying why, or take a dismissal back with `None`.
    ///
    /// # Errors
    ///
    /// When the reason is blank or the finding does not exist.
    pub fn dismiss(
        &self,
        finding: FindingId,
        reason: Option<&str>,
    ) -> Result<FindingRecord, String> {
        let reason = reason.map(str::trim);
        if reason.is_some_and(str::is_empty) {
            return Err(
                "say why it is dismissed: the reason is what a later review reads".to_owned(),
            );
        }
        let record = self
            .db()?
            .dismiss_finding(finding, reason, self.now())
            .map_err(text)?;
        self.notice.changed(record.finding.flow);
        Ok(record)
    }
}

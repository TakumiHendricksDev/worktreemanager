//! The create pipeline.
//!
//! # The invariant that makes this tractable
//!
//! > **Stages 1–6 perform zero mutations. Every mutating operation is in stage 7 or later.**
//!
//! Three things follow from that one line, and they are why the code is shaped this way:
//!
//! - [`CreatePipeline::preview`] and [`CreatePipeline::execute`] are the *same* function with
//!   a stop-after flag, so the review screen cannot drift from what actually runs.
//! - A failed preview is infinitely retryable with nothing to clean up, which is what lets
//!   the form re-preview on every change.
//! - The review screen can show the exact `git worktree add` argv and the exact setup argv
//!   *before* anything has happened.
//!
//! `no_mutation_before_stage_seven` in the tests asserts it directly, against a fake git that
//! records every mutating call.
//!
//! # Why setup failure is not an error
//!
//! By the time a project's setup command fails it may have written an environment file,
//! allocated ports, copied editor config and cloned a multi-gigabyte database volume. Removing
//! the worktree to leave a tidy-looking failure would leak those resources and destroy work
//! that is usually one command from fixed. So stage 9 returns a *successful*
//! [`CreateOutcome::SetupFailed`] carrying the worktree and a set of [`Remedy`] options.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json_path::JsonPath;

use crate::error::{FieldProblem, WtmError};
use crate::model::{
    BranchChoice, BranchPlan, BranchRef, BranchScope, CreateOutcome, CreatePlan, DirBase,
    ExistingBranchBehavior, ExitOutcome, FieldValue, FormValues, LookupErrorPolicy, LookupFormat,
    LookupSpec, PlanPreview, PlanWarning, PreflightItem, Project, SessionId, TrackMode,
    TrackModeSpec, Worktree,
};
use crate::ports::clock::Clock;
use crate::ports::exec::{CancelToken, CommandRunner, Invocation};
use crate::ports::fs::FileStore;
use crate::ports::git::{AddOptions, BranchFilter, Git};
use crate::ports::progress::ProgressSink;
use crate::ports::pty::{PtyHost, PtySink};
use crate::ports::template::{Context, TemplateEngine};

/// Total stage count, for progress reporting.
const STAGES: u8 = 10;

/// Default deadline for a lookup command when its config does not set one.
///
/// A lookup is typically a network call to an issue tracker, so this is generous — but it is
/// still finite, because a hung `acli` must not wedge the form.
const DEFAULT_LOOKUP_TIMEOUT_MS: u64 = 20_000;

/// Default deadline for a setup command. Thirty minutes: a project's setup may clone
/// multi-gigabyte container volumes.
const DEFAULT_SETUP_TIMEOUT_MS: u64 = 1_800_000;

/// What the caller wants built.
#[derive(Debug, Clone)]
pub struct CreateRequest {
    /// A loaded, trusted project. Trust is enforced by `ConfigStore::load`, so reaching here
    /// with an untrusted config is impossible by construction.
    pub project: Project,
    pub values: FormValues,
    /// Ambient tokens the adapter layer supplies: `repo.*`, `vars.*`, `os.*`, `now.*`.
    pub ambient: Context,
    /// Set when the user chose to adopt an existing branch from the review screen — the GUI
    /// form of the shell's numbered stdin picker.
    pub adopt_branch: Option<String>,
    /// Preflight ids the user explicitly acknowledged. Only `overridable` items can be
    /// waived; a hard failure ignores this.
    pub acknowledged: Vec<String>,
    /// Terminal size for the setup session.
    pub rows: u16,
    pub cols: u16,
}

/// What [`CreatePipeline::run_setup`] needs, separate from how it reports.
///
/// A struct rather than six positional parameters: the two `u16`s next to each other are exactly
/// the kind of pair that gets transposed silently, and a caller passing `&worktree, &project`
/// the wrong way round would still compile if these were both `&`-of-the-same-shape.
#[derive(Debug, Clone, Copy)]
pub struct SetupRequest<'a> {
    pub project: &'a Project,
    pub worktree: &'a Worktree,
    /// Ambient tokens the adapter layer supplies: `repo.*`, `vars.*`, `os.*`, `now.*`.
    pub ambient: &'a Context,
    /// Appended to the rendered argv — how `--force` reaches a re-run.
    pub extra_args: &'a [String],
    pub rows: u16,
    pub cols: u16,
}

/// The ports the pipeline needs.
pub struct CreatePipeline {
    pub git: Arc<dyn Git>,
    pub runner: Arc<dyn CommandRunner>,
    pub pty: Arc<dyn PtyHost>,
    pub engine: Arc<dyn TemplateEngine>,
    pub files: Arc<dyn FileStore>,
    pub clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for CreatePipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatePipeline").finish_non_exhaustive()
    }
}

/// Everything stages 1–6 produced, shared by `preview` and `execute`.
struct Planned {
    preview: PlanPreview,
    add: AddOptions,
    fetch: Option<(String, String)>,
}

/// Resolve `remote/ref` only when the prefix names a configured remote.
///
/// Local wins deliberately. A repository can have both a remote named `epic` and a local branch
/// named `epic/thing-api`; selecting the branch the picker showed must not turn into a network
/// operation merely because its first path component also has another meaning.
fn fetch_target(
    base_ref: &str,
    local: &[BranchRef],
    remotes: &[String],
) -> Option<(String, String)> {
    if local.iter().any(|branch| branch.as_str() == base_ref) {
        return None;
    }
    let (remote, branch) = base_ref.split_once('/')?;
    remotes
        .iter()
        .any(|candidate| candidate == remote)
        .then(|| (remote.to_owned(), branch.to_owned()))
}

impl CreatePipeline {
    // ── stages 1–6b: no mutations ────────────────────────────────────────────

    /// Run stages 1–6b and return the review screen's contents.
    ///
    /// Safe to call as often as the form changes: nothing here touches the repository.
    pub fn preview(
        &self,
        req: &CreateRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<PlanPreview, WtmError> {
        Ok(self.plan(req, progress, cancel)?.preview)
    }

    fn plan(
        &self,
        req: &CreateRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<Planned, WtmError> {
        let project = &req.project;

        // ── 1. validate ──
        progress.stage("validate", "Checking the form", 1, STAGES);
        cancel.check()?;
        let values = self.normalize_and_validate(project, &req.values, &req.ambient)?;

        let mut ctx = req.ambient.clone();
        for (key, value) in &values.normalized {
            ctx.insert(key.clone(), value.as_template_string());
        }

        // ── 2. enrich ──
        progress.stage("enrich", "Looking up issue metadata", 2, STAGES);
        cancel.check()?;
        let mut warnings = Vec::new();
        let lookups = self.run_lookups(project, &mut ctx, progress, cancel, &mut warnings)?;

        // ── 3. computed ──
        progress.stage("computed", "Deriving names", 3, STAGES);
        cancel.check()?;
        let computed = self.evaluate_computed(project, &mut ctx)?;

        // ── 4. probe ──
        progress.stage("probe", "Reading the repository", 4, STAGES);
        cancel.check()?;
        let base_ref = values.effective_str(&project.create.base_field);
        let local = self.git.branches(&project.root, BranchFilter::Local)?;
        let remote = self.git.branches(&project.root, BranchFilter::Remote)?;
        let remotes = self.git.remotes(&project.root)?;
        let worktrees = self.git.list_worktrees(&project.root)?;
        let base_commit = self.git.rev_parse(&project.root, &base_ref)?;

        // ── 5. plan ──
        progress.stage("plan", "Planning", 5, STAGES);
        cancel.check()?;

        let branch_choices =
            self.branch_choices(project, &ctx, &local, &remote, &computed_dir_ctx(&ctx))?;

        let (branch_plan, directory) = if let Some(chosen) = &req.adopt_branch {
            {
                let choice = branch_choices
                    .iter()
                    .find(|c| c.branch.as_str() == chosen)
                    .ok_or_else(|| {
                        WtmError::Validation(vec![FieldProblem::new(
                            &project.create.base_field,
                            format!("`{chosen}` is no longer an available branch"),
                        )])
                    })?;
                let plan = if choice.remote_only {
                    BranchPlan::AdoptRemote {
                        branch: choice.branch.clone(),
                        remote: "origin".to_owned(),
                    }
                } else {
                    BranchPlan::UseLocal {
                        branch: choice.branch.clone(),
                    }
                };
                (plan, choice.directory.clone())
            }
        } else {
            {
                let branch = self.render_branch(project, &ctx)?;
                let directory = self.render_directory(project, &ctx)?;
                let track = match project.create.track {
                    TrackModeSpec::NoTrack => TrackMode::NoTrack,
                    TrackModeSpec::Track => TrackMode::Track,
                    TrackModeSpec::Detach => TrackMode::Detach,
                };
                (BranchPlan::Create { branch, track }, directory)
            }
        };

        let add = Self::add_options(&branch_plan, &directory, &base_ref, &local);
        let git_argv = self.git.add_worktree_argv(&add);

        // Setup argv is rendered with the *planned* worktree in scope, since that is exactly
        // what stage 9 will do — the review screen must show the real command.
        let mut setup_ctx = ctx.clone();
        insert_planned_worktree(&mut setup_ctx, &directory, branch_plan.branch());
        let (setup_argv, setup_cwd) = self.render_setup(project, &setup_ctx, &values)?;

        let fetch = project
            .create
            .fetch_base
            .then(|| fetch_target(&base_ref, &local, &remotes))
            .flatten();
        let plan = CreatePlan {
            branch_plan: branch_plan.clone(),
            directory: directory.clone(),
            base_ref: base_ref.clone(),
            base_commit: base_commit.clone(),
            will_fetch: fetch.is_some(),
            git_argv,
            setup_argv: setup_argv.clone(),
            setup_cwd: setup_cwd.clone(),
        };

        // ── 6b. preflight ──
        progress.stage("preflight", "Checking preconditions", 6, STAGES);
        let preflight = self.preflight(project, &plan, &worktrees, &local, base_commit.is_some());

        Ok(Planned {
            preview: PlanPreview {
                plan,
                preflight,
                warnings,
                lookups,
                computed,
                branch_choices,
                naming_fields: self.naming_fields(project),
            },
            add,
            fetch,
        })
    }

    /// Form fields that feed the branch and directory templates, directly or through
    /// `[computed]`.
    ///
    /// Answers "which of these inputs stop mattering if I adopt an existing branch?" —
    /// adopting supplies both the branch and the directory, so anything that only fed those
    /// two templates has nothing left to affect.
    ///
    /// Read off the project's own templates via [`TemplateEngine::referenced_tokens`], never
    /// a hardcoded list, because which fields exist is a `wtm.toml` decision. A template that
    /// fails to parse contributes nothing rather than failing the plan: this drives a UI hint,
    /// and validation has already rejected a broken template long before here.
    fn naming_fields(&self, project: &Project) -> Vec<String> {
        let mut pending: Vec<String> = Vec::new();
        let push = |template: &str, key: &str, out: &mut Vec<String>| {
            if let Ok(tokens) = self.engine.referenced_tokens(key, template) {
                out.extend(tokens);
            }
        };

        push(&project.naming.branch, "naming.branch", &mut pending);
        push(&project.naming.directory, "naming.directory", &mut pending);

        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut fields: BTreeSet<String> = BTreeSet::new();

        while let Some(token) = pending.pop() {
            if !seen.insert(token.clone()) {
                continue;
            }

            // `computed.slug` is not itself an input — follow it to whatever it is built from.
            if let Some(name) = token.strip_prefix("computed.") {
                if let Some(spec) = project.computed.iter().find(|c| c.key == name) {
                    push(&spec.template, "computed", &mut pending);
                }
                continue;
            }

            // Anything else is an input only if the form actually declares it; `repo.root`
            // and friends are ambient and not something the user can change here.
            if project.field(&token).is_some() {
                fields.insert(token);
            }
        }

        fields.into_iter().collect()
    }

    // ── stages 7–10: mutations ───────────────────────────────────────────────

    /// Run the whole pipeline.
    ///
    /// Re-runs stages 1–6b first — deliberately, rather than trusting a preview the caller
    /// may be holding. The repository can change between the review screen and the click, and
    /// re-planning is cheap because it mutates nothing.
    pub fn execute(
        &self,
        req: &CreateRequest,
        progress: &dyn ProgressSink,
        sink: Arc<dyn PtySink>,
        cancel: &CancelToken,
    ) -> Result<CreateOutcome, WtmError> {
        let planned = self.plan(req, progress, cancel)?;
        let project = &req.project;

        // Anything not acknowledged, or not waivable at all, stops us here — with nothing to
        // undo, which is the entire point of the 1–6 boundary.
        let blocking: Vec<PreflightItem> = planned
            .preview
            .preflight
            .iter()
            .filter(|item| {
                item.severity == crate::model::PreflightSeverity::Error
                    && !(item.overridable && req.acknowledged.contains(&item.id))
            })
            .cloned()
            .collect();
        if !blocking.is_empty() {
            return Err(WtmError::Preflight(blocking));
        }

        // ── 7. fetch ──
        if let Some((remote, branch)) = &planned.fetch {
            progress.stage("fetch", "Fetching the base branch", 7, STAGES);
            cancel.check()?;
            // Non-fatal on purpose: working from a slightly stale base beats refusing to
            // work offline. The review screen already showed the cached commit.
            if let Err(err) = self.git.fetch(&project.root, remote, branch) {
                progress.warn(
                    "fetch_failed",
                    &format!("Could not fetch {remote}/{branch}; using the cached ref. {err}"),
                );
            }
        }

        // ── 8. the first mutation ──
        progress.stage("add", "Creating the worktree", 8, STAGES);
        cancel.check()?;
        progress.emit(crate::ports::progress::ProgressEvent::CommandStarted {
            argv: planned.preview.plan.git_argv.clone(),
            cwd: project.root.to_string_lossy().into_owned(),
        });

        let worktree = match self.git.add_worktree(&project.root, &planned.add) {
            Ok(worktree) => worktree,
            Err(err) => {
                // git cleans up its own partial directory. Prune any admin entry it may have
                // recorded, so a retry is not blocked by a ghost.
                let _ = self.git.prune_worktrees(&project.root);
                return Err(err.into());
            }
        };

        // ── 9. setup ──
        let Some(setup) = &project.setup else {
            progress.stage("done", "Done", STAGES, STAGES);
            return Ok(CreateOutcome::Created {
                worktree,
                setup_session: None,
            });
        };

        progress.stage("setup", "Running project setup", 9, STAGES);

        let mut setup_ctx = req.ambient.clone();
        for (key, value) in &planned.preview.plan_values() {
            setup_ctx.insert(key.clone(), value.clone());
        }
        insert_actual_worktree(&mut setup_ctx, &worktree);

        let Some(argv) = planned.preview.plan.setup_argv.clone() else {
            progress.stage("done", "Done", STAGES, STAGES);
            return Ok(CreateOutcome::Created {
                worktree,
                setup_session: None,
            });
        };
        let cwd = planned
            .preview
            .plan
            .setup_cwd
            .clone()
            .unwrap_or_else(|| project.root.clone());

        let mut inv = Invocation::new(
            argv.clone(),
            cwd.clone(),
            setup.command.timeout_ms.unwrap_or(DEFAULT_SETUP_TIMEOUT_MS),
        );
        inv.env = self.render_env(&setup.command.env, &setup_ctx, "setup");

        progress.emit(crate::ports::progress::ProgressEvent::CommandStarted {
            argv: argv.clone(),
            cwd: cwd.to_string_lossy().into_owned(),
        });

        let spawned =
            match self
                .pty
                .spawn(&inv, req.rows, req.cols, Some(worktree.id.as_str()), sink)
            {
                Ok(spawned) => spawned,
                Err(err) => {
                    // The worktree exists and is fine; only setup could not start. Report it as a
                    // partial outcome so the remedies are offered.
                    progress.warn("setup_spawn_failed", &err.to_string());
                    return Ok(CreateOutcome::SetupFailed {
                        worktree,
                        session: SessionId::new("none"),
                        outcome: ExitOutcome::Failed { code: -1 },
                        remedies: CreateOutcome::default_remedies(),
                    });
                }
            };

        progress.emit(crate::ports::progress::ProgressEvent::SessionStarted {
            session: spawned.session.as_str().to_owned(),
        });

        let outcome = self.pty.wait(&spawned.session, cancel)?;

        // ── 10. done ──
        progress.stage("done", "Done", STAGES, STAGES);

        Ok(match outcome {
            ExitOutcome::Success => CreateOutcome::Created {
                worktree,
                setup_session: Some(spawned.session),
            },
            ExitOutcome::Cancelled => CreateOutcome::Cancelled {
                worktree: Some(worktree),
                session: Some(spawned.session),
            },
            failed => CreateOutcome::SetupFailed {
                worktree,
                session: spawned.session,
                outcome: failed,
                remedies: CreateOutcome::default_remedies(),
            },
        })
    }

    /// Re-run only stage 9 against an existing worktree.
    ///
    /// This is both the `RetrySetup` remedy and the "adopt a worktree created outside the app"
    /// entry point. One implementation, two callers — the alternative would be a bespoke retry
    /// path that drifts from the real one.
    pub fn run_setup(
        &self,
        req: &SetupRequest<'_>,
        sink: Arc<dyn PtySink>,
        progress: &dyn ProgressSink,
    ) -> Result<(SessionId, ExitOutcome), WtmError> {
        let SetupRequest {
            project,
            worktree,
            ambient,
            extra_args,
            rows,
            cols,
        } = *req;

        let setup = project.setup.as_ref().ok_or_else(|| {
            WtmError::Validation(vec![FieldProblem::new(
                "setup",
                "this project declares no setup command",
            )])
        })?;

        let mut ctx = ambient.clone();
        insert_actual_worktree(&mut ctx, worktree);

        let mut argv = self.render_argv(&setup.command.run, &ctx, "setup.run")?;
        argv.extend_from_slice(extra_args);

        let cwd = resolve_cwd(&setup.command.cwd, project, Some(worktree));
        let mut inv = Invocation::new(
            argv,
            cwd,
            setup.command.timeout_ms.unwrap_or(DEFAULT_SETUP_TIMEOUT_MS),
        );
        inv.env = self.render_env(&setup.command.env, &ctx, "setup");

        let spawned = self
            .pty
            .spawn(&inv, rows, cols, Some(worktree.id.as_str()), sink)?;

        progress.emit(crate::ports::progress::ProgressEvent::SessionStarted {
            session: spawned.session.as_str().to_owned(),
        });

        let outcome = self.pty.wait(&spawned.session, &CancelToken::new())?;
        Ok((spawned.session, outcome))
    }

    // ── stage implementations ────────────────────────────────────────────────

    /// Apply each field's `normalize` template, then validate.
    ///
    /// Normalization runs first because validation must judge the *effective* value — a
    /// pattern of `^[A-Z]+-[0-9]+$` has to see `ACME-1234`, not the `1234` that was typed.
    fn normalize_and_validate(
        &self,
        project: &Project,
        values: &FormValues,
        ambient: &Context,
    ) -> Result<FormValues, WtmError> {
        let mut out = values.clone();

        // Raw values are visible to a `normalize` template, so one field can normalize using
        // another's input.
        let mut ctx = ambient.clone();
        for (key, value) in &values.raw {
            ctx.insert(key.clone(), value.as_template_string());
        }

        for field in &project.fields {
            let raw = values
                .raw
                .get(&field.key)
                .cloned()
                .unwrap_or(FieldValue::Empty);
            let normalized = match &field.normalize {
                Some(template) => {
                    let key = format!("field.{}.normalize", field.key);
                    let rendered = self.engine.render(&key, template, &ctx)?;
                    FieldValue::from(rendered)
                }
                None => raw,
            };
            out.set_normalized(field.key.clone(), normalized);
        }

        // Re-render the context with normalized values so `required_when` sees them.
        let mut check_ctx = ambient.clone();
        for (key, value) in &out.normalized {
            check_ctx.insert(key.clone(), value.as_template_string());
        }

        let mut problems = Vec::new();
        for field in &project.fields {
            let value = out.effective(&field.key);

            let required = field.required
                || match &field.required_when {
                    Some(expression) => self.engine.eval_bool(
                        &format!("field.{}.required_when", field.key),
                        expression,
                        &check_ctx,
                    )?,
                    None => false,
                };

            if required && value.is_empty() {
                problems.push(FieldProblem::new(&field.key, "This is required."));
                continue;
            }
            if value.is_empty() {
                continue;
            }

            if let Some(pattern) = &field.pattern {
                let rendered = value.as_template_string();
                let matches = self
                    .engine
                    .eval_bool(
                        &format!("field.{}.pattern", field.key),
                        &format!("subject | matches({})", quote(pattern)),
                        &{
                            let mut ctx = Context::new();
                            ctx.insert("subject".to_owned(), rendered.clone());
                            ctx
                        },
                    )
                    .unwrap_or(true);
                if !matches {
                    problems.push(FieldProblem::new(
                        &field.key,
                        field
                            .pattern_message
                            .clone()
                            .unwrap_or_else(|| format!("Must match {pattern}")),
                    ));
                }
            }
        }

        if problems.is_empty() {
            Ok(out)
        } else {
            Err(WtmError::Validation(problems))
        }
    }

    /// Run each applicable `[[lookup]]` and map its output onto tokens.
    fn run_lookups(
        &self,
        project: &Project,
        ctx: &mut Context,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
        warnings: &mut Vec<PlanWarning>,
    ) -> Result<BTreeMap<String, String>, WtmError> {
        let mut resolved = BTreeMap::new();

        for lookup in &project.lookups {
            if let Some(when) = &lookup.command.when
                && !self
                    .engine
                    .eval_bool(&format!("lookup.{}.when", lookup.id), when, ctx)?
            {
                continue;
            }

            progress.emit(crate::ports::progress::ProgressEvent::LookupStarted {
                id: lookup.id.clone(),
            });

            let tokens = match self.execute_lookup(project, lookup, ctx, cancel) {
                Ok(tokens) => tokens,
                Err(err) => match lookup.on_error {
                    LookupErrorPolicy::Fail => return Err(err),
                    LookupErrorPolicy::Warn => {
                        // A tracker outage must not stop you making a worktree. Fall back and
                        // say so on the review screen.
                        warnings.push(PlanWarning::new(
                            format!("lookup_{}_failed", lookup.id),
                            format!(
                                "`{}` could not be reached, so its fallback values were used. {err}",
                                lookup.id
                            ),
                        ));
                        Self::lookup_fallbacks(lookup)
                    }
                },
            };

            progress.emit(crate::ports::progress::ProgressEvent::LookupFinished {
                id: lookup.id.clone(),
                tokens: tokens.clone(),
            });

            for (key, value) in tokens {
                let token = format!("lookup.{}.{key}", lookup.id);
                ctx.insert(token.clone(), value.clone());
                resolved.insert(token, value);
            }
        }

        Ok(resolved)
    }

    fn execute_lookup(
        &self,
        project: &Project,
        lookup: &LookupSpec,
        ctx: &Context,
        cancel: &CancelToken,
    ) -> Result<BTreeMap<String, String>, WtmError> {
        let argv = self.render_argv(
            &lookup.command.run,
            ctx,
            &format!("lookup.{}.run", lookup.id),
        )?;
        let cwd = resolve_cwd(&lookup.command.cwd, project, None);

        let mut inv = Invocation::new(
            argv,
            cwd,
            lookup
                .command
                .timeout_ms
                .unwrap_or(DEFAULT_LOOKUP_TIMEOUT_MS),
        );
        inv.env = self.render_env(&lookup.command.env, ctx, &format!("lookup.{}", lookup.id));

        let output = self.runner.run(&inv, cancel)?;

        let mut tokens = BTreeMap::new();
        match lookup.format {
            LookupFormat::Text => {
                let text = output.stdout.trim().to_owned();
                for (name, mapping) in &lookup.map {
                    tokens.insert(name.clone(), self.apply_mapping(&text, mapping)?);
                }
                if lookup.map.is_empty() {
                    tokens.insert("value".to_owned(), text);
                }
            }
            LookupFormat::Json => {
                let json: serde_json::Value =
                    serde_json::from_str(&output.stdout).map_err(|e| {
                        WtmError::Render(crate::error::RenderError::Eval {
                            key: format!("lookup.{}", lookup.id),
                            message: format!("output was not valid JSON: {e}"),
                        })
                    })?;

                for (name, mapping) in &lookup.map {
                    let raw = JsonPath::parse(&mapping.path)
                        .ok()
                        .and_then(|path| path.query(&json).at_most_one().ok().flatten())
                        .map(|value| match value {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    tokens.insert(name.clone(), self.apply_mapping(&raw, mapping)?);
                }
            }
        }

        Ok(tokens)
    }

    /// Apply a mapping's transforms, rewrites and fallback to an extracted value.
    fn apply_mapping(
        &self,
        raw: &str,
        mapping: &crate::model::LookupMapping,
    ) -> Result<String, WtmError> {
        let mut value = raw.to_owned();
        for filter in &mapping.transform {
            value = self.engine.apply_filter(filter, &value)?;
        }
        // Rewrites encode a project's vocabulary as data — the shell's
        // `case "sub-task") issue_type="subtask"`.
        for rewrite in &mapping.rewrite {
            if value == rewrite.from {
                value.clone_from(&rewrite.to);
            }
        }
        if value.is_empty() {
            value = mapping.fallback.clone().unwrap_or_default();
        }
        Ok(value)
    }

    fn lookup_fallbacks(lookup: &LookupSpec) -> BTreeMap<String, String> {
        lookup
            .map
            .iter()
            .map(|(name, mapping)| (name.clone(), mapping.fallback.clone().unwrap_or_default()))
            .collect()
    }

    /// Evaluate `[[computed]]` in declaration order, each visible to the next.
    fn evaluate_computed(
        &self,
        project: &Project,
        ctx: &mut Context,
    ) -> Result<BTreeMap<String, String>, WtmError> {
        let mut resolved = BTreeMap::new();
        for computed in &project.computed {
            let key = format!("computed.{}", computed.key);
            let value = self.engine.render(&key, &computed.template, ctx)?;
            ctx.insert(key.clone(), value.clone());
            resolved.insert(key, value);
        }
        Ok(resolved)
    }

    fn render_branch(
        &self,
        project: &Project,
        ctx: &Context,
    ) -> Result<crate::model::BranchRef, WtmError> {
        let rendered = self
            .engine
            .render("naming.branch", &project.naming.branch, ctx)?;
        let trimmed = rendered.trim();

        if trimmed.is_empty() {
            return Err(WtmError::Render(crate::error::RenderError::Unusable {
                key: "naming.branch".to_owned(),
                rendered,
                message: "the branch name rendered empty".to_owned(),
            }));
        }

        // The backstop against a corrupt name. An empty slug turns
        // `{type}/{key}-{slug}` into `experiment/ACME-0000-`, which git accepts and nothing
        // else catches.
        if let Some(pattern) = &project.naming.branch_must_match {
            let ok = self
                .engine
                .eval_bool(
                    "naming.branch_must_match",
                    &format!("subject | matches({})", quote(pattern)),
                    &{
                        let mut c = Context::new();
                        c.insert("subject".to_owned(), trimmed.to_owned());
                        c
                    },
                )
                .unwrap_or(true);
            if !ok {
                return Err(WtmError::Render(crate::error::RenderError::Unusable {
                    key: "naming.branch".to_owned(),
                    rendered: trimmed.to_owned(),
                    message: format!(
                        "does not match the project's required branch pattern `{pattern}` — \
                         usually a missing title or an issue summary that slugified to nothing"
                    ),
                }));
            }
        }

        Ok(crate::model::BranchRef::new(trimmed))
    }

    fn render_directory(&self, project: &Project, ctx: &Context) -> Result<PathBuf, WtmError> {
        let name = self
            .engine
            .render("naming.directory", &project.naming.directory, ctx)?
            .trim()
            .to_owned();

        if name.is_empty() {
            return Err(WtmError::Render(crate::error::RenderError::Unusable {
                key: "naming.directory".to_owned(),
                rendered: name,
                message: "the directory name rendered empty".to_owned(),
            }));
        }

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

        Ok(self.files.absolutize(&base.join(name))?)
    }

    /// Existing branches the user could adopt instead of creating a new one.
    fn branch_choices(
        &self,
        project: &Project,
        ctx: &Context,
        local: &[crate::model::BranchRef],
        remote: &[crate::model::BranchRef],
        _dir_ctx: &Context,
    ) -> Result<Vec<BranchChoice>, WtmError> {
        let mut choices = Vec::new();

        for matcher in &project.create.existing_branch_match {
            if matcher.behavior == ExistingBranchBehavior::Ignore {
                continue;
            }
            let pattern = self.engine.render(
                "create.existing_branch_match.pattern",
                &matcher.pattern,
                ctx,
            )?;

            let candidates: Vec<(&crate::model::BranchRef, bool)> = match matcher.scope {
                BranchScope::Local => local.iter().map(|b| (b, false)).collect(),
                BranchScope::Remote => remote.iter().map(|b| (b, true)).collect(),
                BranchScope::LocalAndRemote => local
                    .iter()
                    .map(|b| (b, false))
                    // Local wins on a name collision: it is the one actually checked out, and
                    // offering the same name twice in a picker is confusing.
                    .chain(
                        remote
                            .iter()
                            .filter(|b| !local.contains(b))
                            .map(|b| (b, true)),
                    )
                    .collect(),
            };

            for (branch, remote_only) in candidates {
                if !glob_matches(&pattern, branch.as_str()) {
                    continue;
                }
                if choices.iter().any(|c: &BranchChoice| c.branch == *branch) {
                    continue;
                }

                // An adopted branch's directory comes from its own template, with
                // `matched_branch` in scope.
                let mut adopt_ctx = ctx.clone();
                adopt_ctx.insert("matched_branch".to_owned(), branch.as_str().to_owned());

                let directory = if let Some(template) = &matcher.directory {
                    {
                        let name = self
                            .engine
                            .render(
                                "create.existing_branch_match.directory",
                                template,
                                &adopt_ctx,
                            )?
                            .trim()
                            .to_owned();
                        let base = project
                            .root
                            .parent()
                            .map_or_else(|| project.root.clone(), std::path::Path::to_path_buf);
                        self.files.absolutize(&base.join(name))?
                    }
                } else {
                    // The shell's default: the branch name minus its `type/` prefix.
                    {
                        let base = project
                            .root
                            .parent()
                            .map_or_else(|| project.root.clone(), std::path::Path::to_path_buf);
                        self.files.absolutize(&base.join(branch.without_prefix()))?
                    }
                };

                choices.push(BranchChoice {
                    branch: branch.clone(),
                    remote_only,
                    directory,
                });
            }
        }

        Ok(choices)
    }

    fn add_options(
        plan: &BranchPlan,
        directory: &std::path::Path,
        base_ref: &str,
        local: &[crate::model::BranchRef],
    ) -> AddOptions {
        match plan {
            BranchPlan::Create { branch, track } => AddOptions {
                path: directory.to_path_buf(),
                branch: Some(branch.clone()),
                start_point: base_ref.to_owned(),
                track: *track,
                create_branch: true,
            },
            BranchPlan::UseLocal { branch } => AddOptions {
                path: directory.to_path_buf(),
                branch: Some(branch.clone()),
                start_point: branch.as_str().to_owned(),
                track: TrackMode::Detach,
                // Already exists locally, so check it out rather than creating it.
                create_branch: false,
            },
            BranchPlan::AdoptRemote { branch, remote } => AddOptions {
                path: directory.to_path_buf(),
                branch: Some(branch.clone()),
                start_point: format!("{remote}/{}", branch.as_str()),
                // Tracking is exactly what you want when adopting a remote branch.
                track: TrackMode::Track,
                create_branch: !local.contains(branch),
            },
            BranchPlan::Detach => AddOptions {
                path: directory.to_path_buf(),
                branch: None,
                start_point: base_ref.to_owned(),
                track: TrackMode::Detach,
                create_branch: false,
            },
        }
    }

    fn render_setup(
        &self,
        project: &Project,
        ctx: &Context,
        values: &FormValues,
    ) -> Result<(Option<Vec<String>>, Option<PathBuf>), WtmError> {
        let Some(setup) = &project.setup else {
            return Ok((None, None));
        };

        let mut argv = self.render_argv(&setup.command.run, ctx, "setup.run")?;

        // `args_when` is how a boolean field becomes a flag, with no Rust knowing the flag
        // exists.
        for conditional in &setup.command.args_when {
            let mut when_ctx = ctx.clone();
            for (key, value) in &values.normalized {
                when_ctx.insert(key.clone(), value.as_template_string());
            }
            if self
                .engine
                .eval_bool("setup.args_when", &conditional.when, &when_ctx)?
            {
                for template in &conditional.push {
                    argv.push(self.engine.render("setup.args_when", template, ctx)?);
                }
            }
        }

        Ok((
            Some(argv),
            Some(resolve_cwd(&setup.command.cwd, project, None)),
        ))
    }

    fn render_argv(
        &self,
        templates: &[String],
        ctx: &Context,
        key: &str,
    ) -> Result<Vec<String>, WtmError> {
        templates
            .iter()
            .enumerate()
            .map(|(index, template)| {
                self.engine
                    .render(&format!("{key}[{index}]"), template, ctx)
                    .map_err(Into::into)
            })
            .collect()
    }

    fn render_env(
        &self,
        env: &BTreeMap<String, String>,
        ctx: &Context,
        key: &str,
    ) -> BTreeMap<String, String> {
        env.iter()
            .filter_map(|(name, template)| {
                self.engine
                    .render(&format!("{key}.env.{name}"), template, ctx)
                    .ok()
                    .map(|value| (name.clone(), value))
            })
            .collect()
    }

    /// Everything that must be true before the first mutation.
    ///
    /// Every check here is a condition that would otherwise surface as a confusing git error
    /// halfway through, or — worse — as a half-created worktree.
    fn preflight(
        &self,
        project: &Project,
        plan: &CreatePlan,
        worktrees: &[Worktree],
        local: &[crate::model::BranchRef],
        base_resolved: bool,
    ) -> Vec<PreflightItem> {
        let mut items = Vec::new();

        // The target directory.
        if self.files.exists(&plan.directory) {
            let empty = self.files.is_dir_empty(&plan.directory).unwrap_or(false);
            if empty {
                items.push(
                    PreflightItem::warn(
                        "dir_exists_empty",
                        format!("{} already exists but is empty.", plan.directory.display()),
                    )
                    .with_hint("git will use it as-is."),
                );
            } else {
                items.push(
                    PreflightItem::error(
                        "dir_exists",
                        format!(
                            "{} already exists and is not empty.",
                            plan.directory.display()
                        ),
                    )
                    .with_hint(
                        "Pick a different title, or remove the existing directory first. \
                         git refuses to create a worktree over it.",
                    ),
                );
            }
        }

        // A branch cannot be checked out in two worktrees at once — git refuses outright, so
        // catching it here saves a confusing failure after the fetch.
        if let Some(branch) = plan.branch_plan.branch() {
            if let Some(holder) = worktrees.iter().find(|w| w.branch() == Some(branch)) {
                items.push(
                    PreflightItem::error(
                        "branch_in_use",
                        format!(
                            "`{branch}` is already checked out at {}.",
                            holder.path.display()
                        ),
                    )
                    .with_hint("Open that worktree instead, or choose a different branch."),
                );
            }

            if matches!(plan.branch_plan, BranchPlan::Create { .. }) && local.contains(branch) {
                items.push(
                    PreflightItem::error(
                        "branch_exists",
                        format!("A branch named `{branch}` already exists."),
                    )
                    .with_hint("Adopt it from the list above instead of creating a new one."),
                );
            }
        }

        if !base_resolved {
            items.push(
                PreflightItem::error(
                    "base_unresolved",
                    format!("`{}` does not resolve to a commit.", plan.base_ref),
                )
                .with_hint("Fetch first, or pick a base that exists locally."),
            );
        }

        // Every configured program must be findable, or setup fails after the worktree
        // exists — the messiest possible time.
        if let Some(argv) = &plan.setup_argv
            && let Some(program) = argv.first()
        {
            let is_path = program.contains('/');
            let found = if is_path {
                let candidate = if program.starts_with('/') {
                    PathBuf::from(program)
                } else {
                    plan.setup_cwd
                        .clone()
                        .unwrap_or_else(|| project.root.clone())
                        .join(program)
                };
                self.files.exists(&candidate)
            } else {
                self.runner.which(program).is_some()
            };

            if !found {
                items.push(
                    PreflightItem::error(
                        "setup_program_missing",
                        format!("`{program}` was not found."),
                    )
                    .overridable()
                    .with_hint(
                        "Create the worktree anyway and run setup later, or check \
                             `just doctor` — a GUI app may not inherit your shell's PATH.",
                    ),
                );
            }
        }

        items
    }
}

impl PlanPreview {
    /// The token values the plan was built from, for reuse in stage 9.
    fn plan_values(&self) -> BTreeMap<String, String> {
        let mut out = self.lookups.clone();
        out.extend(self.computed.clone());
        out
    }
}

/// Where a command runs.
fn resolve_cwd(
    base: &crate::model::CwdBase,
    project: &Project,
    worktree: Option<&Worktree>,
) -> PathBuf {
    use crate::model::CwdBase;
    match base {
        CwdBase::RepoRoot | CwdBase::MainWorktree | CwdBase::Custom(_) => project.root.clone(),
        CwdBase::Worktree => worktree.map_or_else(|| project.root.clone(), |w| w.path.clone()),
    }
}

/// Tokens for a worktree that does not exist yet.
fn insert_planned_worktree(
    ctx: &mut Context,
    directory: &std::path::Path,
    branch: Option<&crate::model::BranchRef>,
) {
    ctx.insert(
        "worktree.path".to_owned(),
        directory.to_string_lossy().into_owned(),
    );
    ctx.insert(
        "worktree.dirname".to_owned(),
        directory
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    ctx.insert(
        "worktree.branch".to_owned(),
        branch.map(|b| b.as_str().to_owned()).unwrap_or_default(),
    );
    ctx.insert("worktree.head".to_owned(), String::new());
}

/// Tokens for a worktree that now exists.
fn insert_actual_worktree(ctx: &mut Context, worktree: &Worktree) {
    ctx.insert(
        "worktree.path".to_owned(),
        worktree.path.to_string_lossy().into_owned(),
    );
    ctx.insert("worktree.dirname".to_owned(), worktree.dirname().to_owned());
    ctx.insert(
        "worktree.branch".to_owned(),
        worktree
            .branch()
            .map(|b| b.as_str().to_owned())
            .unwrap_or_default(),
    );
    ctx.insert(
        "worktree.head".to_owned(),
        worktree
            .head
            .as_ref()
            .map(|h| h.as_str().to_owned())
            .unwrap_or_default(),
    );
}

/// Context for a directory template. Currently the same as the caller's.
fn computed_dir_ctx(ctx: &Context) -> Context {
    ctx.clone()
}

/// Quote a string as a template literal.
///
/// Single quotes, with any internal single quote escaped — patterns routinely contain
/// backslashes and `$`, which would otherwise be re-interpreted.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Match a glob containing only `*` wildcards.
///
/// Deliberately not a full glob implementation. Config patterns in practice are `*KEY*`, and a
/// predictable two-line matcher beats pulling in a dependency whose corner cases nobody will
/// remember.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }

    let mut rest = text;

    // A pattern not starting with `*` must match from the beginning.
    if let Some(first) = parts.first()
        && !first.is_empty()
    {
        match rest.strip_prefix(first) {
            Some(tail) => rest = tail,
            None => return false,
        }
    }

    // Middle segments must appear in order.
    for part in &parts[1..parts.len().saturating_sub(1)] {
        if part.is_empty() {
            continue;
        }
        match rest.find(part) {
            Some(index) => rest = &rest[index + part.len()..],
            None => return false,
        }
    }

    // A pattern not ending with `*` must match to the end.
    match parts.last() {
        Some(last) if !last.is_empty() => rest.ends_with(last) && rest.len() >= last.len(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_handles_the_patterns_configs_actually_use() {
        assert!(glob_matches("*ACME-1234*", "task/ACME-1234-slug"));
        assert!(glob_matches("*ACME-1234*", "ACME-1234"));
        assert!(!glob_matches("*ACME-1234*", "task/ACME-8268-slug"));

        assert!(glob_matches("task/*", "task/anything"));
        assert!(!glob_matches("task/*", "bug/anything"));

        assert!(glob_matches("*-slug", "task/x-slug"));
        assert!(!glob_matches("*-slug", "task/x-other"));

        // No wildcard is an exact match.
        assert!(glob_matches("main", "main"));
        assert!(!glob_matches("main", "mainline"));

        // A bare `*` matches everything.
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("*", ""));
    }

    #[test]
    fn quoting_survives_a_pattern_full_of_backslashes() {
        // Branch patterns are regexes; naive quoting would let `\d` become an escape.
        assert_eq!(quote("^[a-z]+$"), "'^[a-z]+$'");
        assert_eq!(quote(r"\d+"), r"'\\d+'");
        assert_eq!(quote("it's"), r"'it\'s'");
    }
}

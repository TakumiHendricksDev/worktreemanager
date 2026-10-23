//! A review's environment: checks that need services, the approval that starts them, and runtime
//! roles, which exercise the running app.
//!
//! The rules are `wtm_core::flow::runtime`'s; this is where they meet the supervisor, the inbox and
//! a browser pane. In short:
//!
//! - **The gate asks now.** Before a check that needs services runs, and before a runtime role
//!   starts, each service it needs is asked once, fresh, through the supervisor
//!   ([`Environment::gate`]). Only when every one answers does anything run, and the record keeps
//!   which services answered and at which address.
//! - **Blocked is a state, and asking is the default.** A check whose services are not up is
//!   recorded blocked and never started. Under `ask` the run files one approval, "start the
//!   environment and run what it blocks", which pins the hash of exactly what would start and run.
//!   Under `auto`, which only your own config can set, the services are started and the gate asked
//!   again. A runtime role waits for the same approval, its run held `Blocked(env)`, the way a role
//!   waits for capacity.
//! - **A runtime role's pane is incognito.** wtm opens it on the service's verified preview, in a
//!   store WebKit keeps in memory and drops with the pane, and refuses to go on if the host opened
//!   it anywhere else. Its token may drive that one pane and no other. Every screenshot it takes is
//!   kept as evidence (`s<id>`), and wtm takes one itself when the page first loads.
//! - **The tree is checked again at the end.** A runtime role reads the worktree, not a snapshot,
//!   because the app runs from there; if the worktree changed while it ran, its step says so, and
//!   its results read as stale.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde::Serialize;
use wtm_core::flow::RunPart;
use wtm_core::flow::{
    BlockCause, CheckRun, CheckSpec, CheckStatus, FlowEvent, FlowId, GatedService, Hold, HoldKind,
    InboxKind, InboxTarget, InvocationId, NewApproval, NewInboxItem, NewScreenshot, PlannedCheck,
    PlannedStart, RoleKind, RoleSpec, RuntimeDriver, ScratchLabel, Screenshot, ScreenshotId,
    ServiceState, ShotTaker, StartRequest, StartServices, Step, StepChange, StepStatus,
    blocked_detail, driver_for, gate_passes, has_playwright, runtime_needs, start_binding,
};
use wtm_core::ports::{InboxStore, OutboxStore, RunStore};
use wtm_core::usecase::snapshot::Snapshots;

use super::host::{PaneStore, RuntimePane};
use super::{Engine, Run, RunCtx, text};
use crate::app::FlowRole;

/// The supervisor, as the engine reaches it: asking services, starting them, and rendering what
/// starting them would run.
///
/// A port rather than the supervisor itself because the supervisor holds the engine (its approvals
/// and inbox items are the engine's flows), so the engine holds this weakly, attached once both
/// exist; and because a test can then gate a check on a service without a window.
pub trait Environment: Send + Sync {
    /// Ask each of `services` once, now, whether it answers its health check. A service the
    /// project does not declare, or one not running, comes back as not up, with why.
    fn gate(&self, project: &str, worktree: &str, services: &[String]) -> Vec<GatedService>;

    /// Start those of `services` not already running and wait until each has answered or given up,
    /// then ask the gate.
    fn bring_up(&self, project: &str, worktree: &str, services: &[String]) -> Vec<GatedService>;

    /// What starting `services` would run: each one's declared start command and the same command
    /// rendered, which an approval pins the hash of.
    ///
    /// # Errors
    ///
    /// When a service is not declared or its command does not render.
    fn plan(
        &self,
        project: &str,
        worktree: &str,
        services: &[String],
    ) -> Result<Vec<(PlannedStart, Vec<String>)>, String>;

    /// The preview address of a healthy `service`, fetched just now and answering.
    ///
    /// # Errors
    ///
    /// When it is not healthy, declares no preview, or did not answer.
    fn preview(&self, project: &str, worktree: &str, service: &str) -> Result<String, String>;
}

/// What the Reviews tab and the inbox show of an approval to start a run's environment, before
/// you give it: exactly what would start and run, and the hash Approve sends back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartView {
    pub flow: FlowId,
    pub request: StartRequest,
    /// The pin the approval covers. Approving sends it back, and a different one is refused.
    pub binding: String,
}

/// How a runtime role's wait for its environment ended.
pub(super) enum EnvReady {
    /// Every service it needs answered.
    Up(Vec<GatedService>),
    /// Cancelled, declined or quitting: the role does not run, and its step already says why.
    Stopped,
}

/// A runtime role's attempt, set up: how it exercises the app, and the pane it drives when that
/// is a browser.
#[derive(Debug, Clone)]
pub struct RuntimeSetup {
    pub driver: RuntimeDriver,
    pub pane: Option<RuntimePane>,
    /// What wtm saw before the role touched anything.
    pub opening: Option<Screenshot>,
    /// The services the gate found up when the role started.
    pub services: Vec<GatedService>,
}

/// How long a step's detail says a role waits for its environment.
const WAITING: &str = "It waits for you to start the environment: Reviews › Start environment and \
                       run, or the inbox.";

impl Engine {
    /// Give the engine its environment. Called once, after the supervisor is made.
    pub fn attach_environment(&self, environment: &Arc<dyn Environment>) {
        *self.environment.lock() = Some(Arc::downgrade(environment));
    }

    fn environment(&self) -> Option<Arc<dyn Environment>> {
        self.environment.lock().as_ref().and_then(Weak::upgrade)
    }

    /// The gate, or every service reported not up when there is no supervisor to ask.
    pub(super) fn gate(
        &self,
        project: &str,
        worktree: &str,
        needs: &[String],
    ) -> Vec<GatedService> {
        match self.environment() {
            Some(env) => env.gate(project, worktree, needs),
            None => needs
                .iter()
                .map(|service| GatedService {
                    service: service.clone(),
                    state: ServiceState::Stopped,
                    detail: Some("this wtm has no environment supervisor".to_owned()),
                    url: None,
                })
                .collect(),
        }
    }

    pub(super) fn bring_up(
        &self,
        project: &str,
        worktree: &str,
        needs: &[String],
    ) -> Vec<GatedService> {
        match self.environment() {
            Some(env) => env.bring_up(project, worktree, needs),
            None => self.gate(project, worktree, needs),
        }
    }

    // ═══════════════════════════════ runtime roles ═══════════════════════════════

    /// How a runtime role exercises the app in this worktree.
    pub(super) fn runtime_driver(worktree: &Path, spec: &RoleSpec) -> RuntimeDriver {
        let found = std::fs::read_to_string(worktree.join("package.json"))
            .is_ok_and(|text| has_playwright(&text));
        driver_for(spec, found)
    }

    /// Wait, if it has to, until every service a runtime role needs answers.
    ///
    /// Under `auto` the services are started first. Otherwise, or when starting them did not bring
    /// them up, the step says it is blocked, the run is held `Blocked(env)`, and one approval is
    /// filed; the role waits for it the way a role waits for capacity, woken by the approval (or
    /// a resume) to ask the gate again.
    pub(super) fn runtime_ready(
        &self,
        ctx: &RunCtx,
        run: &Run,
        step: &Step,
        spec: &RoleSpec,
    ) -> EnvReady {
        let driver = Self::runtime_driver(&ctx.worktree_path, spec);
        let needs = runtime_needs(&ctx.workflow, spec, driver);
        if needs.is_empty() {
            return EnvReady::Up(Vec::new());
        }
        let mut gated = self.gate(&ctx.project, &ctx.worktree, &needs);
        if gate_passes(&gated) {
            return EnvReady::Up(gated);
        }
        if ctx.workflow.start_services.unwrap_or_default() == StartServices::Auto {
            self.update_step(
                step,
                &StepChange::default().detail(format!("starting {}", names(&needs))),
            );
            gated = self.bring_up(&ctx.project, &ctx.worktree, &needs);
            if gate_passes(&gated) {
                self.update_step(
                    step,
                    &StepChange {
                        detail: wtm_core::flow::Field::Clear,
                        ..StepChange::default()
                    },
                );
                return EnvReady::Up(gated);
            }
        }

        self.update_step(
            step,
            &StepChange::status(StepStatus::Pending)
                .detail(format!("{}. {WAITING}", blocked_detail(&gated))),
        );
        self.hold_blocked(ctx.flow);
        self.file_start(ctx.flow);
        loop {
            std::thread::sleep(Duration::from_millis(self.tuning.tick_ms));
            if self.quitting() {
                return EnvReady::Stopped;
            }
            if run.cancel.load(Ordering::SeqCst) {
                self.update_step(step, &StepChange::status(StepStatus::Cancelled));
                self.release_blocked(ctx.flow);
                return EnvReady::Stopped;
            }
            if run.declined.swap(false, Ordering::SeqCst) {
                self.update_step(
                    step,
                    &StepChange::status(StepStatus::Cancelled).detail(format!(
                        "{} — you chose not to start it, so it did not run",
                        blocked_detail(&gated)
                    )),
                );
                self.release_blocked(ctx.flow);
                return EnvReady::Stopped;
            }
            if run.wake.swap(false, Ordering::SeqCst) {
                gated = self.gate(&ctx.project, &ctx.worktree, &needs);
                if gate_passes(&gated) {
                    break;
                }
                self.update_step(
                    step,
                    &StepChange::default().detail(format!("{}. {WAITING}", blocked_detail(&gated))),
                );
                self.file_start(ctx.flow);
            }
        }
        self.update_step(
            step,
            &StepChange {
                detail: wtm_core::flow::Field::Clear,
                ..StepChange::default()
            },
        );
        self.release_blocked(ctx.flow);
        EnvReady::Up(gated)
    }

    fn hold_blocked(&self, flow: FlowId) {
        let _held = self.holds.lock();
        let Ok(state) = self.state(flow) else { return };
        if state.end.is_none() && state.hold.is_none() {
            let _ = self.record(
                flow,
                &FlowEvent::Held {
                    hold: Hold::Blocked {
                        cause: BlockCause::Env,
                    },
                },
            );
        }
    }

    pub(super) fn release_blocked(&self, flow: FlowId) {
        let _held = self.holds.lock();
        let Ok(state) = self.state(flow) else { return };
        if matches!(
            state.hold,
            Some(Hold::Blocked {
                cause: BlockCause::Env
            })
        ) {
            let _ = self.record(
                flow,
                &FlowEvent::Released {
                    hold: HoldKind::Blocked,
                },
            );
        }
    }

    /// Set up a runtime role's attempt: for a browser, open its incognito pane on the verified
    /// preview and take the first picture.
    ///
    /// # Errors
    ///
    /// When the preview does not answer, the pane does not open, or it opened anywhere but an
    /// incognito store — which is refused rather than driven, since a pane in the persistent jar
    /// is one a signed-in session could be driven from.
    pub(super) fn open_runtime(
        &self,
        ctx: &RunCtx,
        step: &Step,
        spec: &RoleSpec,
        invocation: InvocationId,
        services: Vec<GatedService>,
    ) -> Result<RuntimeSetup, String> {
        let driver = Self::runtime_driver(&ctx.worktree_path, spec);
        if driver == RuntimeDriver::Playwright {
            return Ok(RuntimeSetup {
                driver,
                pane: None,
                opening: None,
                services,
            });
        }
        let service = spec
            .service
            .as_deref()
            .ok_or("a runtime role that drives a browser pane names its service")?;
        // The setting that turns agents' browser tools off covers this pane too, and is said
        // before any session opens rather than as a refusal in the middle of its turn.
        if !self.app.browser_tools_enabled() {
            return Err(
                "browser tools are turned off in Settings (\"Agents may use browser \
                        panes\"), so a runtime role has no pane to drive"
                    .to_owned(),
            );
        }
        let env = self
            .environment()
            .ok_or("this wtm has no environment supervisor to verify a preview")?;
        let url = env.preview(&ctx.project, &ctx.worktree, service)?;
        let pane = self
            .host
            .open_runtime_browser(&ctx.project, &ctx.worktree, &url)?;
        if pane.store != PaneStore::Incognito {
            self.host.close_runtime_browser(&pane.id);
            return Err(format!(
                "the browser pane opened in {:?}, not an incognito store, so it was closed \
                 rather than driven",
                pane.store
            ));
        }
        let opening = match self.host.screenshot(&pane.id) {
            Ok((png, at)) => self
                .keep_shot(
                    ctx.flow,
                    Some(step.id),
                    Some(invocation),
                    Some(&step.role),
                    &at,
                    ShotTaker::Wtm,
                    &png,
                )
                .ok(),
            Err(e) => {
                tracing::warn!(flow = %ctx.flow, error = %e, "could not take the opening screenshot");
                None
            }
        };
        Ok(RuntimeSetup {
            driver,
            pane: Some(pane),
            opening,
            services,
        })
    }

    /// Close a runtime role's pane, and its store with it.
    pub(super) fn close_runtime(&self, setup: &RuntimeSetup) {
        if let Some(pane) = &setup.pane {
            self.host.close_runtime_browser(&pane.id);
        }
    }

    /// At the end of a role that read the worktree: did the worktree change while it ran?
    ///
    /// Its reports already go stale on any change (a role that ran the app may have read any
    /// file), so the staleness the Reviews tab shows follows by itself; this records, on the step,
    /// that it happened during the run, which is a different fact from a change made since.
    pub(super) fn recheck_tree(&self, ctx: &RunCtx, step: &Step) {
        let snapshots = Snapshots {
            git: Arc::clone(&self.app.git),
            files: Arc::clone(&self.app.files),
        };
        let Some(label) = ScratchLabel::new(&format!(
            "end-{}",
            self.scratch.fetch_add(1, Ordering::SeqCst)
        )) else {
            return;
        };
        match snapshots.check(&ctx.worktree_path, &ctx.snapshot, &ctx.dir, &label) {
            Ok(check) if check.is_stale() => {
                let paths = check.changed_paths();
                let shown: Vec<String> = paths.iter().take(3).map(|p| format!("`{p}`")).collect();
                let more = paths.len().saturating_sub(shown.len());
                let files = if shown.is_empty() {
                    "files git could not list".to_owned()
                } else if more > 0 {
                    format!("{} and {more} more", shown.join(", "))
                } else {
                    shown.join(", ")
                };
                self.update_step(
                    step,
                    &StepChange::default().detail(format!(
                        "the worktree changed while it ran ({files}), so what it saw may not be \
                         this code; its results are stale"
                    )),
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(flow = %ctx.flow, error = %e, "could not check the tree again");
            }
        }
    }

    // ═══════════════════════════════ screenshots ═══════════════════════════════

    #[allow(clippy::too_many_arguments)]
    fn keep_shot(
        &self,
        flow: FlowId,
        step: Option<wtm_core::flow::StepId>,
        invocation: Option<InvocationId>,
        role: Option<&str>,
        url: &str,
        taken_by: ShotTaker,
        png: &[u8],
    ) -> Result<Screenshot, String> {
        let db = self.db()?;
        let dir = wtm_core::flow::FlowDir::new(&self.app.config.paths().flow_dir, flow);
        let relative = format!(
            "screenshots/{}-{}.png",
            invocation.map_or(0, |i| i.0),
            self.now()
        );
        let path = dir.resolve(&relative);
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, png))
            .map_err(|e| format!("could not keep the screenshot: {e}"))?;
        let digest = self.app.files.digest(&path).map_err(text)?;
        let shot = db
            .record_screenshot(
                &NewScreenshot {
                    flow,
                    step,
                    invocation,
                    role: role.map(str::to_owned),
                    url: url.to_owned(),
                    taken_by,
                    image: wtm_core::flow::BlobRef {
                        path: relative,
                        sha256: digest.sha256,
                        bytes: digest.bytes,
                    },
                },
                self.now(),
            )
            .map_err(text)?;
        self.notice.changed(flow);
        Ok(shot)
    }

    /// Keep a screenshot a runtime role took of its pane, as evidence it can cite. `None` when the
    /// role is not one of this engine's live runtime attempts.
    pub fn keep_role_screenshot(
        &self,
        role: &FlowRole,
        png: &[u8],
        url: &str,
    ) -> Option<Result<Screenshot, String>> {
        let live = self
            .live
            .lock()
            .get(&(role.flow, role.role.clone()))
            .cloned()?;
        live.browser.as_ref()?;
        Some(self.keep_shot(
            role.flow,
            Some(live.step),
            Some(live.invocation),
            Some(&role.role),
            url,
            ShotTaker::Role,
            png,
        ))
    }

    /// A screenshot's PNG, base64, for the Reviews tab: the webview cannot read the data directory.
    ///
    /// # Errors
    ///
    /// When the run has no such screenshot, or its file is gone or no longer what was kept.
    pub fn screenshot_png(&self, flow: FlowId, id: ScreenshotId) -> Result<String, String> {
        let db = self.db()?;
        let shot = db
            .screenshots(flow)
            .map_err(text)?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("run {flow} has no screenshot {}", id.label()))?;
        let dir = wtm_core::flow::FlowDir::new(&self.app.config.paths().flow_dir, flow);
        let path = dir.resolve(&shot.image.path);
        let digest = self.app.files.digest(&path).map_err(text)?;
        if digest.sha256 != shot.image.sha256 {
            return Err(format!(
                "{} changed on disk after it was kept, so it is not shown",
                id.label()
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", id.label()))?;
        Ok(crate::pty_bridge::base64_encode(&bytes))
    }

    // ═══════════════════════════════ the approval ═══════════════════════════════

    /// Every check of a run whose latest record is blocked: what an approval would run.
    pub(super) fn blocked_checks(&self, flow: FlowId) -> Vec<CheckRun> {
        let Ok(db) = self.db() else {
            return Vec::new();
        };
        let mut latest: BTreeMap<String, CheckRun> = BTreeMap::new();
        for run in db.checks(flow).unwrap_or_default() {
            latest.insert(run.check.clone(), run);
        }
        latest
            .into_values()
            .filter(|run| run.status == CheckStatus::Blocked)
            .collect()
    }

    /// Runtime roles of a run waiting for their environment.
    fn waiting_roles(&self, flow: FlowId) -> Vec<Step> {
        let Ok(db) = self.db() else {
            return Vec::new();
        };
        db.steps(flow)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                s.status == StepStatus::Pending
                    && s.detail
                        .as_deref()
                        .is_some_and(|d| d.starts_with("blocked: environment not started"))
            })
            .collect()
    }

    /// What approving a start would start and run now, and the pin it would cover. `None` when
    /// nothing in the run is blocked.
    ///
    /// # Errors
    ///
    /// When the run, its project or a service cannot be read.
    pub fn pending_start(&self, flow: FlowId) -> Result<Option<StartView>, String> {
        let checks = self.blocked_checks(flow);
        let roles = self.waiting_roles(flow);
        if checks.is_empty() && roles.is_empty() {
            return Ok(None);
        }
        let db = self.db()?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let project = self.app.project(review.project.as_str()).map_err(text)?;
        let Some(workflow) = self.kept_workflow(flow, RunPart::Review, Some(&project)) else {
            return Err("the project's workflow config has gone".to_owned());
        };
        let worktree = self
            .app
            .worktree(&project, &review.worktree)
            .map_err(|e| format!("the worktree is no longer there: {e}"))?;

        let mut needs: BTreeSet<String> = BTreeSet::new();
        let mut planned = Vec::new();
        let mut rendered_checks = Vec::new();
        for run in &checks {
            let Some(spec) = workflow.check.get(&run.check) else {
                continue;
            };
            needs.extend(spec.needs.iter().cloned());
            planned.push(PlannedCheck {
                check: run.check.clone(),
                label: spec.label.clone().unwrap_or_else(|| run.check.clone()),
                argv: spec.run.clone(),
                needs: spec.needs.clone(),
            });
            rendered_checks.push((run.check.clone(), spec.run.clone()));
        }
        let mut role_ids = Vec::new();
        for step in &roles {
            if let Some(spec) = workflow.role.get(&step.role)
                && spec.kind == RoleKind::Runtime
            {
                let driver = Self::runtime_driver(&worktree.path, spec);
                needs.extend(runtime_needs(&workflow, spec, driver));
                role_ids.push(step.role.clone());
            }
        }
        let needs: Vec<String> = needs.into_iter().collect();
        let env = self
            .environment()
            .ok_or("this wtm has no environment supervisor")?;
        let services = env.plan(review.project.as_str(), &review.worktree, &needs)?;
        let binding = start_binding(
            &services
                .iter()
                .map(|(s, argv)| (s.service.clone(), argv.clone()))
                .collect::<Vec<_>>(),
            &rendered_checks,
            &role_ids,
            &worktree.path.to_string_lossy(),
        );
        let pin = binding
            .0
            .get(&wtm_core::flow::Pinned::Payload)
            .cloned()
            .unwrap_or_default();
        Ok(Some(StartView {
            flow,
            request: StartRequest {
                project: review.project.as_str().to_owned(),
                worktree: review.worktree.clone(),
                services: services.into_iter().map(|(s, _)| s).collect(),
                checks: planned,
                roles: role_ids,
            },
            binding: pin,
        }))
    }

    /// File, once per run while it is open, the approval that starts its environment.
    pub(super) fn file_start(&self, flow: FlowId) {
        let view = match self.pending_start(flow) {
            Ok(Some(view)) => view,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(%flow, error = %e, "could not plan starting a run's environment");
                return;
            }
        };
        let services: Vec<String> = view
            .request
            .services
            .iter()
            .map(|s| s.label.clone())
            .collect();
        // One item per run while it is open, so it names only the services: whatever is blocked
        // after it was filed joins the same item, and the card it opens is read live.
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Approval,
            target: InboxTarget::Flow,
            title: format!("Start {} and run what it blocks?", services.join(", ")),
            body: format!(
                "Blocked: environment not started. This review has work waiting for {} to answer \
                 its health check. Approving starts {}, waits for {} to answer, then runs what was \
                 blocked. Nothing has run, and nothing reads as passed.",
                services.join(", "),
                if services.len() == 1 { "it" } else { "them" },
                if services.len() == 1 { "it" } else { "each" },
            ),
        });
    }

    /// Approve starting a run's environment, as `binding` showed it, and run what it blocks.
    ///
    /// # Errors
    ///
    /// When nothing is blocked, or what it would start or run changed since you looked: then
    /// nothing is recorded or run, and the view to look at again is the new one.
    pub fn start_environment(self: &Arc<Self>, flow: FlowId, binding: &str) -> Result<(), String> {
        let view = self
            .pending_start(flow)?
            .ok_or("nothing in this review is waiting for its environment")?;
        if view.binding != binding {
            return Err(
                "what it would start or run changed since you looked (its config or this \
                 worktree's .env), so nothing ran; look again"
                    .to_owned(),
            );
        }
        let db = self.db()?;
        let approval = NewApproval::services(
            flow,
            &view.request,
            wtm_core::flow::Binding::default()
                .with(wtm_core::flow::Pinned::Payload, view.binding.clone()),
        )
        .map_err(text)?;
        db.approve(&approval, &[], self.now()).map_err(text)?;
        let _ = db.resolve_inbox(
            flow,
            InboxKind::Approval,
            Some(&InboxTarget::Flow),
            self.now(),
        );
        self.notice.changed(flow);

        let engine = Arc::clone(self);
        std::thread::Builder::new()
            .name(format!("flow-{flow}-env"))
            .spawn(move || engine.run_started(flow, &view.request))
            .map_err(|e| format!("could not start the environment's thread: {e}"))?;
        Ok(())
    }

    /// After an approval: start the services, then run each blocked check again through the gate,
    /// then wake the runtime roles waiting for them.
    fn run_started(self: &Arc<Self>, flow: FlowId, request: &StartRequest) {
        let services: Vec<String> = request.services.iter().map(|s| s.service.clone()).collect();
        let gated = self.bring_up(&request.project, &request.worktree, &services);
        tracing::info!(%flow, up = gate_passes(&gated), "started a review's environment");
        let blocked = self.blocked_checks(flow);
        for run in blocked {
            if let Err(e) = self.run_unblocked(flow, request, &run) {
                tracing::warn!(%flow, check = %run.check, error = %e, "a blocked check did not run");
            }
        }
        let woke = self
            .runs
            .lock()
            .get(&flow)
            .map(|run| run.wake.store(true, Ordering::SeqCst))
            .is_some();
        if !woke && !request.roles.is_empty() {
            tracing::info!(%flow, "no run thread to wake; its runtime roles run on resume");
        }
        self.notice.changed(flow);
    }

    /// Run a check that was blocked, in the worktree its services run from, through the gate again.
    fn run_unblocked(
        &self,
        flow: FlowId,
        request: &StartRequest,
        blocked: &CheckRun,
    ) -> Result<(), String> {
        let project = self.app.project(&request.project).map_err(text)?;
        let workflow = self.run_workflow(flow, RunPart::Review, &project)?;
        let spec: CheckSpec =
            workflow.check.get(&blocked.check).cloned().ok_or_else(|| {
                format!("`{}` is no longer in your workflow config", blocked.check)
            })?;
        let worktree = self
            .app
            .worktree(&project, &request.worktree)
            .map_err(|e| format!("the worktree is no longer there: {e}"))?;
        let dir = wtm_core::flow::FlowDir::new(&self.app.config.paths().flow_dir, flow);
        // Run in the worktree, against the tree it holds now: the snapshot checkout a role read
        // from is gone with its stage, and the services run from the worktree anyway.
        let label = ScratchLabel::new(&format!(
            "check-{}",
            self.scratch.fetch_add(1, Ordering::SeqCst)
        ))
        .ok_or("a scratch label")?;
        let tree = self
            .app
            .git
            .snapshot_tree(&worktree.path, &dir.scratch_index(&label))
            .map_err(text)?;
        let gated = self.gate(&request.project, &request.worktree, &spec.needs);
        let place = super::checks::CheckPlace {
            flow,
            project: request.project.clone(),
            step: blocked.step,
            invocation: blocked.invocation,
            role: blocked.role.clone(),
            cwd: worktree.path.clone(),
            tree_sha: tree,
            dir,
        };
        self.gated_run(
            &blocked.check,
            &spec,
            &place,
            gated,
            Some("run after you approved starting its services"),
        )
        .map(|_| ())
    }

    /// Decline starting a run's environment: what was blocked stays blocked, and a runtime role
    /// waiting for it does not run.
    ///
    /// # Errors
    ///
    /// When the store cannot be written.
    pub fn decline_environment(&self, flow: FlowId) -> Result<(), String> {
        let db = self.db()?;
        db.resolve_inbox(
            flow,
            InboxKind::Approval,
            Some(&InboxTarget::Flow),
            self.now(),
        )
        .map_err(text)?;
        if let Some(run) = self.runs.lock().get(&flow) {
            run.declined.store(true, Ordering::SeqCst);
        }
        self.notice.changed(flow);
        Ok(())
    }
}

fn names(services: &[String]) -> String {
    services
        .iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

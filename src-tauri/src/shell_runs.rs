//! Immutable reviewed commands, with authorization separate from shell admission.
//!
//! No operation in this registry starts a process. Execution consumes a one-use
//! capability only after the app rechecks target identity, config and requester.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use wtm_core::model::ExitOutcome;
use wtm_core::ports::config::ConfigStore;
use wtm_exec::shell::{Interpreter, ScriptFile, canonical_script};
use wtm_exec::shell_frames::Framing;

use crate::app::App;
use crate::shell_output::Output;

const MAX_RUNS: usize = 256;
const MAX_PENDING_PER_HOME: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub key: String,
    pub project_id: String,
    pub worktree_id: String,
    pub command: String,
    pub interpreter: String,
    /// `auto`, `new`, or an explicit live shell session id from the review sheet.
    pub shell: String,
}

impl Request {
    fn same_payload(&self, other: &Self) -> bool {
        self.project_id == other.project_id
            && self.worktree_id == other.worktree_id
            && self.command == other.command
            && self.interpreter == other.interpreter
            && self.shell == other.shell
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Human,
    Home(String),
}

impl Source {
    pub fn home(&self) -> Option<&str> {
        match self {
            Self::Human => None,
            Self::Home(home) => Some(home),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub project: String,
    pub worktree: String,
    pub directory: PathBuf,
    device: u64,
    inode: u64,
    created: std::time::SystemTime,
    revision: wtm_config::ExecutionRevision,
}

impl Target {
    pub fn resolve(app: &App, project_id: &str, worktree_id: &str) -> Result<Self, String> {
        let registered = app.config.projects().map_err(|e| e.to_string())?;
        if !registered
            .iter()
            .any(|root| root.to_string_lossy() == project_id)
        {
            return Err("Choose a registered repository.".into());
        }
        let project = app.project(project_id).map_err(|e| e.to_string())?;
        let worktree = app
            .worktree(&project, worktree_id)
            .map_err(|e| e.to_string())?;
        let directory = std::fs::canonicalize(&worktree.path).map_err(|e| e.to_string())?;
        let metadata = std::fs::metadata(&directory).map_err(|e| e.to_string())?;
        if !metadata.is_dir() {
            return Err("The worktree directory is gone.".into());
        }
        Ok(Self {
            project: project_id.into(),
            worktree: worktree_id.into(),
            directory,
            device: metadata.dev(),
            inode: metadata.ino(),
            created: metadata.created().map_err(|e| e.to_string())?,
            revision: app
                .config
                .execution_revision(&project.root)
                .map_err(|e| e.to_string())?,
        })
    }

    pub fn revalidate(&self, app: &App) -> Result<(), String> {
        if Self::resolve(app, &self.project, &self.worktree)? != *self {
            return Err(
                "The worktree or configuration changed. Review a new request before running."
                    .into(),
            );
        }
        Ok(())
    }
}

pub struct Prepared {
    pub request: Request,
    pub source: Source,
    pub target: Target,
    pub interpreter: Interpreter,
    pub executable: PathBuf,
    pub script: ScriptFile,
    allow_grant: bool,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl Prepared {
    pub fn create(app: &App, mut request: Request, source: Source) -> Result<Self, String> {
        if request.key.is_empty()
            || request.key.len() > 128
            || request.key.chars().any(char::is_control)
        {
            return Err("A command needs a bounded, stable request key.".into());
        }
        request.command = canonical_script(&request.command)?;
        let interpreter = Interpreter::parse(&request.interpreter)?;
        let target = Target::resolve(app, &request.project_id, &request.worktree_id)?;
        let executable = if interpreter == Interpreter::Sh {
            PathBuf::from("/bin/sh")
        } else {
            app.runner
                .which(interpreter.name())
                .ok_or_else(|| format!("{} is not installed.", interpreter.name()))?
        };
        let script = ScriptFile::create(&request.command)?;
        script.check(
            app.runner.as_ref(),
            interpreter,
            &executable,
            &target.directory,
        )?;
        let allow_grant = source.home().is_none_or(|home| {
            matches!(request.shell.as_str(), "auto" | "new")
                || app
                    .shell_admission
                    .get(&request.shell)
                    .is_some_and(|shell| shell.owned_untouched(home))
        });
        Ok(Self {
            request,
            source,
            target,
            interpreter,
            executable,
            script,
            allow_grant,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Prepared,
    AwaitingApproval,
    AwaitingPane,
    OpeningShell,
    Reserved,
    Running,
    Completed,
    Denied,
    Cancelled,
    Interrupted,
}

impl Phase {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Denied | Self::Cancelled | Self::Interrupted
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: String,
    pub request: Request,
    pub requester: Option<String>,
    pub directory: String,
    pub phase: Phase,
    pub session: Option<String>,
    pub outcome: Option<ExitOutcome>,
    pub problem: Option<String>,
}

pub struct State {
    pub phase: Phase,
    pub session: Option<String>,
    pub capability: Option<String>,
    pub child: Option<u32>,
    pub helper: Option<u32>,
    pub control: Option<UnixStream>,
    pub output: Output,
    pub problem: Option<String>,
    pub grant: bool,
    pub watched: bool,
}

pub struct Run {
    pub id: String,
    pub prepared: Prepared,
    pub framing: Framing,
    pub state: Mutex<State>,
    pub changed: Condvar,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

impl Run {
    pub fn view(&self) -> View {
        let state = self.state.lock();
        let output = state.output.read(0).ok();
        View {
            id: self.id.clone(),
            request: self.prepared.request.clone(),
            requester: self.prepared.source.home().map(str::to_owned),
            directory: self
                .prepared
                .target
                .directory
                .to_string_lossy()
                .into_owned(),
            phase: state.phase,
            session: state.session.clone(),
            outcome: output.and_then(|output| output.outcome),
            problem: state.problem.clone(),
        }
    }

    pub fn approve(&self) -> Result<bool, String> {
        let mut state = self.state.lock();
        match state.phase {
            Phase::Prepared | Phase::AwaitingApproval => {
                state.phase = Phase::AwaitingPane;
                self.changed.notify_all();
                Ok(true)
            }
            Phase::Denied | Phase::Cancelled | Phase::Interrupted => {
                Err("This request is no longer available. Nothing was resubmitted.".into())
            }
            _ => Ok(false),
        }
    }

    pub fn deny(&self) -> Result<(), String> {
        let mut state = self.state.lock();
        if !matches!(state.phase, Phase::Prepared | Phase::AwaitingApproval) {
            return Err("This request is no longer waiting for approval.".into());
        }
        state.phase = Phase::Denied;
        state.problem = Some("The user denied this command.".into());
        state.output.interrupt("The user denied this command.");
        self.changed.notify_all();
        Ok(())
    }

    pub fn interrupt(&self, problem: &str) {
        let mut state = self.state.lock();
        if !state.phase.terminal() {
            state.phase = Phase::Interrupted;
            state.problem = Some(problem.into());
            state.output.interrupt(problem);
            self.changed.notify_all();
        }
    }
}

#[derive(Default)]
struct Entries {
    runs: BTreeMap<String, Arc<Run>>,
    keys: BTreeMap<(Source, String), String>,
    capabilities: BTreeMap<String, String>,
    active: BTreeMap<String, String>,
}

#[derive(Default)]
pub struct Registry {
    entries: Mutex<Entries>,
    grants: Mutex<Vec<(String, Target)>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("runs", &self.entries.lock().runs.len())
            .finish_non_exhaustive()
    }
}

impl Registry {
    pub fn prepare(&self, prepared: Prepared) -> Result<Arc<Run>, String> {
        let mut entries = self.entries.lock();
        let key = (prepared.source.clone(), prepared.request.key.clone());
        if let Some(id) = entries.keys.get(&key) {
            let run = entries
                .runs
                .get(id)
                .ok_or("This request has already settled.")?;
            if !run.prepared.request.same_payload(&prepared.request)
                || run.prepared.target != prepared.target
            {
                return Err(
                    "This request key already belongs to a different command or target.".into(),
                );
            }
            return Ok(Arc::clone(run));
        }
        let home = prepared.source.home();
        if entries.keys.len() >= 2048 {
            return Err("The request-key limit for this app launch has been reached.".into());
        }
        let siblings: Vec<_> = entries
            .runs
            .values()
            .filter(|run| run.prepared.source == prepared.source)
            .collect();
        if home.is_some() {
            let pending = siblings
                .iter()
                .find(|run| {
                    run.prepared.request.same_payload(&prepared.request)
                        && run.prepared.target == prepared.target
                        && run.state.lock().phase == Phase::AwaitingApproval
                })
                .map(|run| Arc::clone(run));
            if let Some(pending) = pending {
                entries.keys.insert(key, pending.id.clone());
                return Ok(pending);
            }
            if siblings.iter().any(|run| {
                run.prepared.request.command == prepared.request.command
                    && run.prepared.target == prepared.target
                    && run.state.lock().phase == Phase::Denied
            }) {
                return Err(
                    "The user denied this command. Do not resubmit it under a new key.".into(),
                );
            }
            if siblings
                .iter()
                .filter(|run| run.state.lock().phase == Phase::AwaitingApproval)
                .count()
                >= MAX_PENDING_PER_HOME
            {
                return Err(
                    "Home already has eight pending commands. Wait for their decisions.".into(),
                );
            }
        }
        if entries.runs.len() >= MAX_RUNS {
            return Err("The command history limit for this app launch has been reached.".into());
        }
        let grant =
            prepared.allow_grant && home.is_some_and(|home| self.has_grant(home, &prepared.target));
        let phase = if home.is_some() && !grant {
            Phase::AwaitingApproval
        } else {
            Phase::Prepared
        };
        let framing = Framing::default();
        let run = Arc::new(Run {
            id: format!("run-{}", uuid::Uuid::new_v4()),
            prepared,
            state: Mutex::new(State {
                phase,
                session: None,
                capability: None,
                child: None,
                helper: None,
                control: None,
                output: Output::new(framing.clone()),
                problem: None,
                grant,
                watched: false,
            }),
            framing,
            changed: Condvar::new(),
        });
        entries.keys.insert(key, run.id.clone());
        entries.runs.insert(run.id.clone(), Arc::clone(&run));
        Ok(run)
    }

    pub fn get(&self, id: &str) -> Result<Arc<Run>, String> {
        self.entries
            .lock()
            .runs
            .get(id)
            .cloned()
            .ok_or_else(|| "That run is gone. Runs never resume after wtm quits.".into())
    }

    pub fn all(&self) -> Vec<Arc<Run>> {
        self.entries.lock().runs.values().cloned().collect()
    }

    pub fn reserve(&self, run: &Arc<Run>, session: &str) -> Result<String, String> {
        let mut entries = self.entries.lock();
        let mut state = run.state.lock();
        if state.phase != Phase::AwaitingPane {
            return Err("This run has already been dispatched or cancelled.".into());
        }
        if entries.active.contains_key(session) {
            return Err("This shell already has a run.".into());
        }
        let capability = uuid::Uuid::new_v4().simple().to_string();
        state.phase = Phase::Reserved;
        state.session = Some(session.into());
        state.capability = Some(capability.clone());
        entries
            .capabilities
            .insert(capability.clone(), run.id.clone());
        entries.active.insert(session.into(), run.id.clone());
        Ok(capability)
    }

    pub fn by_capability(&self, capability: &str) -> Result<Arc<Run>, String> {
        let entries = self.entries.lock();
        entries
            .capabilities
            .get(capability)
            .and_then(|id| entries.runs.get(id))
            .cloned()
            .ok_or_else(|| "This shell-run capability is gone or already used.".into())
    }

    pub fn consume_capability(&self, capability: &str) {
        self.entries.lock().capabilities.remove(capability);
    }

    pub fn active(&self, session: &str) -> Option<Arc<Run>> {
        let entries = self.entries.lock();
        entries
            .active
            .get(session)
            .and_then(|id| entries.runs.get(id))
            .cloned()
    }

    pub fn release(&self, run: &Run) {
        let mut entries = self.entries.lock();
        entries.capabilities.retain(|_, id| id != &run.id);
        entries.active.retain(|_, id| id != &run.id);
    }

    /// Called only by the human IPC route, never the Home tool dispatcher.
    pub fn set_grant(&self, home: &str, target: Target, allow: bool) {
        let mut grants = self.grants.lock();
        grants.retain(|(owner, existing)| {
            owner != home
                || existing.project != target.project
                || existing.worktree != target.worktree
        });
        if allow {
            grants.push((home.into(), target));
        }
    }

    pub fn grants_for(&self, home: &str) -> Vec<Target> {
        self.grants
            .lock()
            .iter()
            .filter(|(owner, _)| owner == home)
            .map(|(_, target)| target.clone())
            .collect()
    }

    pub fn revoke_grant(&self, home: &str, project: &str, worktree: &str) {
        self.grants.lock().retain(|(owner, target)| {
            owner != home || target.project != project || target.worktree != worktree
        });
    }

    pub fn has_grant(&self, home: &str, target: &Target) -> bool {
        self.grants
            .lock()
            .iter()
            .any(|(owner, existing)| owner == home && existing == target)
    }

    pub fn forget_home(&self, home: &str) {
        self.grants.lock().retain(|(owner, _)| owner != home);
        for run in self.all() {
            if run.prepared.source.home() == Some(home) {
                run.interrupt("Home closed before this command settled.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtm_config::AppPaths;
    use wtm_core::ports::config::TrustDecision;
    use wtm_testkit::GitFixture;

    struct Setup {
        app: App,
        fixture: GitFixture,
        root: String,
        _config: tempfile::TempDir,
    }

    impl Setup {
        fn new() -> Self {
            let fixture = GitFixture::new();
            fixture.commit(
                "wtm.toml",
                "# commands prepared in an isolated repository\n",
                "add config",
            );
            let config = tempfile::tempdir().unwrap();
            let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();
            let root = app
                .register(fixture.root())
                .unwrap()
                .to_string_lossy()
                .into_owned();
            Self {
                app,
                fixture,
                root,
                _config: config,
            }
        }

        fn prepared(&self, key: &str, source: Source) -> Prepared {
            Prepared::create(
                &self.app,
                Request {
                    key: key.into(),
                    project_id: self.root.clone(),
                    worktree_id: self.root.clone(),
                    command: "printf 'reviewed\\n'\n".into(),
                    interpreter: "sh".into(),
                    shell: "auto".into(),
                },
                source,
            )
            .unwrap()
        }
    }

    #[test]
    fn preparation_cannot_execute_and_authorization_is_idempotent() {
        let setup = Setup::new();
        let registry = Registry::default();
        let mut prepared = setup.prepared("first", Source::Human);
        prepared.request.command = "touch executed".into();
        let prepared = Prepared::create(&setup.app, prepared.request, Source::Human).unwrap();
        let run = registry.prepare(prepared).unwrap();
        assert_eq!(run.view().phase, Phase::Prepared);
        assert!(registry.reserve(&run, "shell").is_err());
        assert!(!setup.fixture.root().join("executed").exists());
        assert!(run.approve().unwrap());
        assert!(!run.approve().unwrap());
        assert_eq!(run.view().phase, Phase::AwaitingPane);
        let capability = registry.reserve(&run, "shell").unwrap();
        assert!(Arc::ptr_eq(
            &run,
            &registry.by_capability(&capability).unwrap()
        ));
        assert!(registry.reserve(&run, "other-shell").is_err());
        registry.consume_capability(&capability);
        assert!(registry.by_capability(&capability).is_err());
        registry.release(&run);
        assert!(registry.active("shell").is_none());

        assert!(!setup.fixture.root().join("executed").exists());
    }

    #[test]
    fn duplicate_keys_return_the_original_run_and_cannot_change_its_payload() {
        let setup = Setup::new();
        let registry = Registry::default();
        let first = registry
            .prepare(setup.prepared("same", Source::Human))
            .unwrap();
        let duplicate = registry
            .prepare(setup.prepared("same", Source::Human))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &duplicate));
        let mut different = setup.prepared("same", Source::Human);
        different.request.shell = "new".into();
        assert!(registry.prepare(different).is_err());
        let other_home = registry
            .prepare(setup.prepared("same", Source::Home("home".into())))
            .unwrap();
        assert_ne!(other_home.id, first.id);
        assert_eq!(other_home.view().phase, Phase::AwaitingApproval);
        let repeated = registry
            .prepare(setup.prepared("another-key", Source::Home("home".into())))
            .unwrap();
        assert!(Arc::ptr_eq(&other_home, &repeated));
        let repeated_again = registry
            .prepare(setup.prepared("another-key", Source::Home("home".into())))
            .unwrap();
        assert!(Arc::ptr_eq(&other_home, &repeated_again));
    }

    #[test]
    fn a_denied_home_command_cannot_be_retried_under_a_new_key() {
        let setup = Setup::new();
        let registry = Registry::default();
        let source = Source::Home("home".into());
        let run = registry
            .prepare(setup.prepared("first", source.clone()))
            .unwrap();
        run.deny().unwrap();
        assert!(run.approve().is_err());
        assert!(registry.prepare(setup.prepared("retry", source)).is_err());
        assert_eq!(run.view().phase, Phase::Denied);
    }

    #[test]
    fn a_temporary_grant_belongs_to_one_home_and_disappears_when_it_closes() {
        let setup = Setup::new();
        let registry = Registry::default();
        let prepared = setup.prepared("first", Source::Home("home".into()));
        let target = prepared.target.clone();
        registry.set_grant("home", target.clone(), true);
        let run = registry.prepare(prepared).unwrap();
        assert_eq!(run.view().phase, Phase::Prepared);
        assert!(!registry.has_grant("other", &target));
        let other = registry
            .prepare(setup.prepared("first", Source::Home("other".into())))
            .unwrap();
        assert_eq!(other.view().phase, Phase::AwaitingApproval);
        registry.forget_home("home");
        assert!(!registry.has_grant("home", &target));
        assert_eq!(run.view().phase, Phase::Interrupted);
        assert_eq!(other.view().phase, Phase::AwaitingApproval);
    }

    #[test]
    fn config_changes_and_new_trust_decisions_invalidate_the_prepared_target() {
        let setup = Setup::new();
        let prepared = setup.prepared("first", Source::Human);
        prepared.target.revalidate(&setup.app).unwrap();
        let config = PathBuf::from(&setup.root).join("wtm.toml");
        std::fs::write(&config, "# changed\n").unwrap();
        assert!(prepared.target.revalidate(&setup.app).is_err());
        let second = setup.prepared("second", Source::Human);
        setup
            .app
            .config
            .set_trust(&config, TrustDecision::Approve)
            .unwrap();
        assert!(second.target.revalidate(&setup.app).is_err());
        setup
            .app
            .config
            .unregister_project(std::path::Path::new(&setup.root))
            .unwrap();
        assert!(Prepared::create(&setup.app, second.request, Source::Human).is_err());
    }

    #[test]
    fn a_worktree_grant_cannot_approve_an_explicit_unknown_or_user_owned_shell() {
        let setup = Setup::new();
        let registry = Registry::default();
        let mut request = setup.prepared("explicit", Source::Human).request;
        request.shell = "not-a-home-owned-shell".into();
        let prepared = Prepared::create(&setup.app, request, Source::Home("home".into())).unwrap();
        registry.set_grant("home", prepared.target.clone(), true);
        assert_eq!(
            registry.prepare(prepared).unwrap().view().phase,
            Phase::AwaitingApproval
        );
    }

    #[test]
    fn revoking_one_worktree_grant_cannot_leave_it_active_or_change_another_homes_grant() {
        let setup = Setup::new();
        let registry = Registry::default();
        let target = setup.prepared("target", Source::Human).target;
        registry.set_grant("home", target.clone(), true);
        registry.set_grant("other", target.clone(), true);
        registry.revoke_grant("home", &target.project, &target.worktree);
        assert!(!registry.has_grant("home", &target));
        assert!(registry.has_grant("other", &target));
        let prepared = setup.prepared("after-revoke", Source::Home("home".into()));
        assert_eq!(
            registry.prepare(prepared).unwrap().view().phase,
            Phase::AwaitingApproval
        );
    }

    #[test]
    fn a_fresh_registry_does_not_restore_runs_or_approvals() {
        let setup = Setup::new();
        let registry = Registry::default();
        let run = registry
            .prepare(setup.prepared("first", Source::Home("home".into())))
            .unwrap();
        let fresh = Registry::default();
        assert!(fresh.get(&run.id).is_err());
        assert!(fresh.all().is_empty());
        assert!(!fresh.has_grant("home", &run.prepared.target));
    }
    #[test]
    fn batch_interruption_closes_every_active_control_without_rewriting_finished_runs() {
        use std::io::Read;
        let setup = Setup::new();
        let registry = Registry::default();
        let mut runs = Vec::new();
        let mut readers = Vec::new();
        for key in ["first", "second"] {
            let run = registry
                .prepare(setup.prepared(key, Source::Human))
                .unwrap();
            let (control, reader) = std::os::unix::net::UnixStream::pair().unwrap();
            reader
                .set_read_timeout(Some(std::time::Duration::from_millis(250)))
                .unwrap();
            run.state.lock().phase = Phase::Running;
            run.state.lock().control = Some(control);
            runs.push(run);
            readers.push(reader);
        }
        let finished = registry
            .prepare(setup.prepared("finished", Source::Human))
            .unwrap();
        {
            let mut state = finished.state.lock();
            state.output.push(finished.framing.begin().as_bytes());
            state.output.push(finished.framing.end().as_bytes());
            state
                .output
                .finish(wtm_core::model::ExitOutcome::Failed { code: 7 });
            state.phase = Phase::Completed;
        }
        runs.push(finished.clone());
        crate::shell_control::interrupt_many(&runs, "Worktree removed", &[]);
        for mut reader in readers {
            assert_eq!(reader.read(&mut [0_u8; 1]).unwrap(), 0);
        }
        for run in &runs[..2] {
            let state = run.state.lock();
            assert_eq!(state.phase, Phase::Interrupted);
            assert!(state.output.complete());
            assert_eq!(state.problem.as_deref(), Some("Worktree removed"));
        }
        assert_eq!(finished.view().phase, Phase::Completed);
        assert_eq!(
            finished.view().outcome,
            Some(wtm_core::model::ExitOutcome::Failed { code: 7 })
        );
    }
}

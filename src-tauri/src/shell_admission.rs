//! Prompt reservations are independent of terminal output and agent credentials.
//!
//! The per-shell gate orders human input against capability consumption. Global
//! registry locks only find an Arc; no socket or PTY I/O holds the registry lock.

use std::collections::BTreeMap;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use wtm_exec::shell_integration::ShellIntegration;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Unknown,
    Busy,
    Ready,
    Reserved,
    Running,
    Closed,
}

struct Reservation {
    capability: String,
    generation: u64,
    epoch: u64,
    consumed: bool,
}

struct State {
    stream: Option<UnixStream>,
    prompt: Option<(u64, u32)>,
    generation: u64,
    reservation: Option<Reservation>,
    closed: bool,
    adopted: bool,
    focus: u64,
    created: u64,
}

pub struct Shell {
    pub project: String,
    pub worktree: String,
    pub owner: Option<String>,
    pub integration: ShellIntegration,
    state: Mutex<State>,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell")
            .field("project", &self.project)
            .field("worktree", &self.worktree)
            .finish_non_exhaustive()
    }
}

impl Shell {
    pub fn new(
        project: &str,
        worktree: &str,
        owner: Option<String>,
        integration: ShellIntegration,
    ) -> Self {
        Self {
            project: project.into(),
            worktree: worktree.into(),
            owner,
            integration,
            state: Mutex::new(State {
                stream: None,
                prompt: None,
                generation: 0,
                reservation: None,
                closed: false,
                adopted: false,
                focus: 0,
                created: 0,
            }),
        }
    }

    pub fn connect(&self, stream: UnixStream) -> Result<(), String> {
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.to_string())?;
        let mut state = self.state.lock();
        if state.closed || state.stream.is_some() {
            return Err("This shell integration connection is no longer available.".into());
        }
        state.stream = Some(stream);
        Ok(())
    }

    pub fn prompt(&self, epoch: u64, jobs: u32) {
        let mut state = self.state.lock();
        if !state.closed {
            state.prompt = Some((epoch, jobs));
        }
    }

    pub fn busy(&self) {
        self.state.lock().prompt = None;
    }

    pub fn availability(&self, foreground: bool) -> Availability {
        let state = self.state.lock();
        Self::availability_of(&state, foreground)
    }

    fn availability_of(state: &State, foreground: bool) -> Availability {
        if state.closed {
            Availability::Closed
        } else if let Some(reservation) = &state.reservation {
            if reservation.consumed {
                Availability::Running
            } else {
                Availability::Reserved
            }
        } else if state.stream.is_none() {
            Availability::Unknown
        } else if !foreground || !matches!(state.prompt, Some((_, 0))) {
            Availability::Busy
        } else {
            Availability::Ready
        }
    }

    /// The caller checks the real foreground group just before reserving. The
    /// widget checks again at consumption, so a stale OS snapshot cannot inject.
    pub fn reserve(&self, capability: &str, foreground: bool) -> Result<(), String> {
        let (mut stream, epoch) = {
            let mut state = self.state.lock();
            if Self::availability_of(&state, foreground) != Availability::Ready {
                return Err(
                    "This shell is busy or its prompt cannot be verified. Choose a new shell."
                        .into(),
                );
            }
            let epoch = state.prompt.map_or(0, |(epoch, _)| epoch);
            let stream = state
                .stream
                .as_ref()
                .ok_or("The shell disconnected.")?
                .try_clone()
                .map_err(|e| e.to_string())?;
            state.reservation = Some(Reservation {
                capability: capability.into(),
                generation: state.generation,
                epoch,
                consumed: false,
            });
            (stream, epoch)
        };
        if let Err(error) = writeln!(stream, "run {epoch} {capability}") {
            self.release(capability);
            return Err(error.to_string());
        }
        Ok(())
    }

    /// Call only after revalidating the immutable run, target and authorization.
    /// A UI keystroke queued after reservation invalidates the capability even if
    /// the widget has not received that keystroke yet.
    pub fn consume(&self, capability: &str) -> Result<(), String> {
        self.consume_if(capability, || Ok(()))
    }

    /// Acquire the input sequencer before authorization locks. A blocked PTY
    /// writer must not leave its output reader waiting on a run-state lock held
    /// by the consumer which is itself waiting for this input sequencer.
    pub fn consume_if(
        &self,
        capability: &str,
        authorize: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = self.state.lock();
        let valid = !state.closed
            && state.reservation.as_ref().is_some_and(|r| {
                r.capability == capability
                    && !r.consumed
                    && r.generation == state.generation
                    && state.prompt == Some((r.epoch, 0))
            });
        if !valid {
            return Err("The shell changed before Run was accepted. Nothing ran.".into());
        }
        authorize()?;
        if let Some(reservation) = &mut state.reservation {
            reservation.consumed = true;
        }
        state.prompt = None;
        Ok(())
    }

    pub fn release(&self, capability: &str) {
        let mut state = self.state.lock();
        if state
            .reservation
            .as_ref()
            .is_some_and(|r| r.capability == capability)
        {
            state.reservation = None;
        }
    }

    pub fn manual_input<T>(&self, write: impl FnOnce() -> T) -> T {
        let mut state = self.state.lock();
        state.generation += 1;
        state.adopted = true;
        state.prompt = None;
        write()
    }

    pub fn can_close_for(&self, home: &str, foreground: bool) -> bool {
        let state = self.state.lock();
        self.owner.as_deref() == Some(home)
            && !state.adopted
            && Self::availability_of(&state, foreground) == Availability::Ready
    }

    pub fn focus(&self, order: u64) {
        self.state.lock().focus = order;
    }

    pub fn focus_order(&self) -> u64 {
        self.state.lock().focus
    }

    pub fn close(&self) {
        let stream = {
            let mut state = self.state.lock();
            state.closed = true;
            state.prompt = None;
            state.stream.take()
        };
        if let Some(stream) = stream {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

#[derive(Default)]
pub struct Registry {
    shells: Mutex<BTreeMap<String, Arc<Shell>>>,
    pending: Mutex<BTreeMap<String, Arc<Shell>>>,
    next: AtomicU64,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("shells", &self.shells.lock().len())
            .finish_non_exhaustive()
    }
}

impl Registry {
    /// Published before PTY spawn: the startup socket may connect before spawn
    /// has returned a session id, but cannot authorize a run before registration.
    pub fn prepare(&self, shell: Arc<Shell>) {
        self.pending
            .lock()
            .insert(shell.integration.token().into(), shell);
    }

    pub fn register(&self, session: &str, shell: Arc<Shell>) {
        shell.state.lock().created = self.next.fetch_add(1, Ordering::Relaxed);
        let token = shell.integration.token().to_owned();
        self.shells.lock().insert(session.into(), shell);
        self.pending.lock().remove(&token);
    }

    pub fn abandon(&self, shell: &Shell) {
        self.pending.lock().remove(shell.integration.token());
        shell.close();
    }

    pub fn get(&self, session: &str) -> Option<Arc<Shell>> {
        self.shells.lock().get(session).cloned()
    }

    pub fn for_token(&self, token: &str) -> Option<Arc<Shell>> {
        // Check registered first, then pending, then registered again to cover a
        // registration moving the entry between those two brief lookups.
        let registered = || {
            self.shells
                .lock()
                .values()
                .find(|s| s.integration.token() == token)
                .cloned()
        };
        registered()
            .or_else(|| self.pending.lock().get(token).cloned())
            .or_else(registered)
    }

    pub fn candidates(
        &self,
        project: &str,
        worktree: &str,
        home: Option<&str>,
    ) -> Vec<(String, Arc<Shell>)> {
        let candidates: Vec<_> = self
            .shells
            .lock()
            .iter()
            .filter(|(_, s)| s.project == project && s.worktree == worktree)
            .map(|(id, s)| (id.clone(), Arc::clone(s)))
            .collect();
        let mut candidates: Vec<_> = candidates
            .into_iter()
            .filter(|(_, shell)| {
                home.is_none_or(|home| {
                    shell.owner.as_deref() == Some(home) && !shell.state.lock().adopted
                })
            })
            .collect();
        candidates.sort_by_key(|(_, shell)| {
            let state = shell.state.lock();
            (std::cmp::Reverse(state.focus), state.created)
        });
        candidates
    }

    pub fn close(&self, session: &str) {
        let shell = self.shells.lock().remove(session);
        if let Some(shell) = shell {
            shell.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::path::Path;

    fn shell(project: &str, worktree: &str, home: Option<&str>) -> Arc<Shell> {
        let integration = ShellIntegration::create(
            Path::new("/unused/socket"),
            Path::new("/unused/helper"),
            Path::new("/unused/profiles"),
        )
        .unwrap();
        Arc::new(Shell::new(
            project,
            worktree,
            home.map(str::to_owned),
            integration,
        ))
    }

    fn ready(shell: &Shell) -> BufReader<UnixStream> {
        let (server, client) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        shell.connect(server).unwrap();
        shell.prompt(7, 0);
        BufReader::new(client)
    }

    #[test]
    fn unknown_foreground_and_background_states_refuse_before_sending_a_control_message() {
        let shell = shell("project", "worktree", None);
        assert_eq!(shell.availability(true), Availability::Unknown);
        assert!(shell.reserve("first", true).is_err());
        let _reader = ready(&shell);
        assert!(shell.reserve("first", false).is_err());
        shell.prompt(7, 1);
        assert!(shell.reserve("first", true).is_err());
        shell.busy();
        assert!(shell.reserve("first", true).is_err());
        shell.close();
        shell.prompt(8, 0);
        assert!(shell.reserve("first", true).is_err());
    }

    #[test]
    fn a_prompt_has_one_reservation_and_its_capability_can_be_consumed_only_once() {
        let shell = shell("project", "worktree", None);
        let mut reader = ready(&shell);
        shell.reserve("first", true).unwrap();
        assert!(shell.reserve("second", true).is_err());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "run 7 first\n");
        assert!(shell.consume("second").is_err());
        shell.consume("first").unwrap();
        assert_eq!(shell.availability(false), Availability::Running);
        assert!(shell.consume("first").is_err());
        shell.release("second");
        assert_eq!(shell.availability(false), Availability::Running);
        shell.release("first");
        assert_eq!(shell.availability(true), Availability::Busy);
    }

    #[test]
    fn manual_input_invalidates_a_pending_run_even_if_a_delayed_prompt_report_arrives() {
        let shell = shell("project", "worktree", Some("home"));
        let _reader = ready(&shell);
        assert!(shell.can_close_for("home", true));
        shell.reserve("first", true).unwrap();
        shell.manual_input(|| ());
        shell.prompt(7, 0);
        assert!(shell.consume("first").is_err());
        shell.release("first");
        assert!(!shell.can_close_for("home", true));
    }

    #[test]
    fn a_new_prompt_or_closed_shell_cannot_consume_a_stale_reservation() {
        let shell = shell("project", "worktree", None);
        let _reader = ready(&shell);
        shell.reserve("first", true).unwrap();
        shell.prompt(8, 0);
        assert!(shell.consume("first").is_err());
        shell.prompt(7, 0);
        shell.close();
        assert!(shell.consume("first").is_err());
    }

    #[test]
    fn candidates_stay_in_the_target_scope_and_use_focus_then_creation_order() {
        let registry = Registry::default();
        let first = shell("project", "worktree", Some("home"));
        let second = shell("project", "worktree", None);
        let other = shell("project", "other", Some("home"));
        registry.prepare(Arc::clone(&first));
        assert!(registry.for_token(first.integration.token()).is_some());
        registry.register("z-first", Arc::clone(&first));
        registry.register("a-second", Arc::clone(&second));
        registry.register("other", other);
        let ids = |home| {
            registry
                .candidates("project", "worktree", home)
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(None), ["z-first", "a-second"]);
        second.focus(1);
        assert_eq!(ids(None), ["a-second", "z-first"]);
        assert_eq!(ids(Some("home")), ["z-first"]);
        assert!(ids(Some("other-home")).is_empty());
        first.manual_input(|| ());
        assert!(ids(Some("home")).is_empty());
        registry.close("z-first");
        assert!(registry.for_token(first.integration.token()).is_none());
        assert!(registry.get("z-first").is_none());
    }
}

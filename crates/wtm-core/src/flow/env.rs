//! Environments: the services a worktree runs while you test it, and the rules wtm supervises them
//! by.
//!
//! # Declared, never guessed
//!
//! A project says what its environment is: `[[environment.service]]` for something that runs (a web
//! server, a worker), `[[environment.resource]]` for something services share or own (a database),
//! and `[[environment.command]]` for a one-off against them (a migration). wtm knows how to start,
//! watch and stop a declared process; it never knows what one *is*, which keeps ARCHITECTURE §1's
//! "tools, not projects".
//!
//! # Ports are read, never allocated
//!
//! A service's `ports` are templates, typically `{{ env.WEB_PORT }}`, rendered from the worktree's
//! own `.env` or display sources. A project that allocates ports per worktree does it in its own
//! setup script, and a second allocator here would be the bug ARCHITECTURE §4 warns about: two
//! implementations of one collision-avoidance algorithm. wtm only checks, before a start, that the
//! ports are free, and when one is not it says who holds it ([`PortConflict`]). It never kills a
//! holder: the only processes it signals are ones it started, matched by pid *and* start time
//! ([`recover`]).
//!
//! # Health is a bounded question, asked after a start
//!
//! Every service declares how to tell that it is up, by a URL on this machine or a command, and
//! wtm asks it on a schedule that backs off and ends ([`probe_delays`]). There is no timer that
//! keeps asking afterwards: ARCHITECTURE §8 bans polling, and a health light that silently went
//! stale would be worse than one that says when it was last checked. A service that exits is seen
//! the moment it exits, because wtm is its parent.
//!
//! # A destructive command on a shared resource is approved every time
//!
//! A migration against this worktree's own database is your business. The same migration against a
//! database every worktree shares can break all of them, so it waits for an approval of exactly
//! what will run ([`needs_approval`], [`command_binding`]), and an approval covers one run.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::approval::{ApprovalId, Binding, Pinned};
use super::publish::sha256_of;
use super::state::FlowId;

/// How long a service gets to become healthy when its config does not say. A cold JavaScript dev
/// server can take most of a minute on its first build.
pub const DEFAULT_HEALTH_TIMEOUT_MS: u64 = 60 * 1000;

/// The longest a config may ask wtm to wait for health. Ten minutes separates slow from broken,
/// and a typo of an extra zero would otherwise keep a service "starting" for most of a day.
pub const MAX_HEALTH_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// How long a `[[environment.command]]` may run when its config does not say.
pub const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// How long a `stop` command may run. A `docker compose stop` takes seconds; a stop that takes
/// minutes is a stop that is not working, and wtm signals what it can instead.
pub const STOP_TIMEOUT_MS: u64 = 30 * 1000;

/// The first wait before asking a service whether it is up, in milliseconds. Short, because a
/// small server answers within a few hundred milliseconds and a health light that lags its server
/// by seconds reads as slow.
pub const FIRST_PROBE_DELAY_MS: u64 = 250;

/// The longest wait between two health questions. The schedule doubles up to this and stays there.
pub const MAX_PROBE_DELAY_MS: u64 = 4_000;

// ═══════════════════════════════ the schema ═══════════════════════════════

/// The `[environment]` table.
///
/// Arrays of tables, as the plan wrote them, so they *replace* across config layers rather than
/// merge: a local layer that declares `[[environment.service]]` declares the whole list. That is
/// the right default here and the wrong one for agents (see `AgentSpec`), because a service list is
/// one design: a local override that kept the repository's `web` beside its own `web-dev` would run
/// both on one port.
///
/// Every argv here is a command wtm runs, so a repository's `wtm.toml` that declares any of it is
/// behind the trust prompt (`wtm-config`'s `collect_environment_processes`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSpec {
    #[serde(default, rename = "service", skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceSpec>,
    #[serde(default, rename = "resource", skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceSpec>,
    #[serde(default, rename = "command", skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<EnvCommandSpec>,
    /// Ask `docker ps` as well as `lsof` who holds a port that is in use.
    ///
    /// Opt-in rather than automatic, because on a machine where Docker is installed but stopped the
    /// first `docker` call can take seconds to fail, and a project that runs nothing in containers
    /// would pay that on every conflict.
    #[serde(default)]
    pub docker: bool,
}

/// One process a worktree runs while you test it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    pub id: String,
    /// What the Environment tab calls it. Falls back to the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The argv that starts it, each element a template, run in the worktree. Never a shell string;
    /// write `["sh", "-c", "…"]` to ask for one.
    pub start: Vec<String>,
    /// The argv that stops it. Without one, wtm ends the process group it started, `SIGTERM` then
    /// `SIGKILL`; with one, it runs this first and ends the group afterwards anyway, since a stop
    /// that left something behind would leave it running with nothing watching it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// The ports it listens on, each a template that renders to a port number, such as
    /// `{{ env.WEB_PORT }}`. Checked before a start; never allocated (see the module docs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    pub health: HealthSpec,
    /// The page to open once it is healthy, as a template: `http://localhost:{{ env.WEB_PORT }}`.
    /// Opened only after wtm has fetched it and it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// A command whose output is the service's log, for a detached service, whose `start` returns
    /// as soon as the container is up. wtm runs it beside the service, appended to the same log,
    /// and ends it when the service stops. A service wtm holds a process for logs what it prints,
    /// so `logs` is refused there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<Vec<String>>,
    /// `start` returns once the service is running somewhere else, as `docker compose up -d` does.
    ///
    /// wtm then holds no process for it, so it cannot end one: `stop` is required, quitting wtm
    /// leaves the service running, and the next launch lists it as still up.
    #[serde(default)]
    pub detached: bool,
}

impl ServiceSpec {
    #[must_use]
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }
}

/// How to tell that a service is up: exactly one of `url` and `command`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSpec {
    /// A URL on this machine that answers 2xx or 3xx once the service is up. Templated. Only a
    /// loopback host is accepted ([`loopback_url`]): a health check is not a way to reach the
    /// network, and ARCHITECTURE §6a lists every way wtm does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// An argv that exits 0 once the service is up, for one that speaks no HTTP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// How long, from the start, before a service that has not answered is unhealthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl HealthSpec {
    #[must_use]
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_HEALTH_TIMEOUT_MS)
    }
}

/// Whether a resource is one worktree's own or every worktree's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// This worktree's own: a database volume cloned for it, a cache in its directory.
    PerWorktree,
    /// Shared with every other worktree, so damage to it is damage to all of them.
    Shared,
}

/// Something services use: a database, a cache, a queue.
///
/// wtm starts nothing for a resource. It exists so a command can say what it acts on, and so the
/// Environment tab can say which of a worktree's things are its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub isolation: Isolation,
}

impl ResourceSpec {
    #[must_use]
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }
}

/// A one-off command against a worktree's environment, such as a migration or a reset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvCommandSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Spelled `run` like every other command in a config, so the trust walk that finds any `run`
    /// array finds this one without being taught.
    pub run: Vec<String>,
    /// It destroys or rewrites data. On a shared resource that means an approval every time.
    #[serde(default)]
    pub destructive: bool,
    /// The `[[environment.resource]]` ids it acts on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl EnvCommandSpec {
    #[must_use]
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }

    #[must_use]
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_COMMAND_TIMEOUT_MS)
    }
}

/// A mistake in `[environment]`, with the key it is under.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{key}: {message}")]
pub struct EnvironmentError {
    /// Relative to `environment`, such as `service.web.health`.
    pub key: String,
    pub message: String,
}

impl EnvironmentError {
    fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            message: message.into(),
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn check_argv(key: &str, argv: &[String], what: &str) -> Result<(), EnvironmentError> {
    if argv.is_empty() {
        return Err(EnvironmentError::new(
            key,
            format!("{what} needs a program, as `[\"just\", \"up\"]`"),
        ));
    }
    if argv.iter().any(|word| word.trim().is_empty()) {
        return Err(EnvironmentError::new(
            key,
            "an argument is empty; an empty argv element is passed to the program as written and \
             is almost never meant",
        ));
    }
    Ok(())
}

/// Whether `template` begins as an http(s) URL. A template's host may be a token, so the loopback
/// check proper happens once it is rendered ([`loopback_url`]); this catches `ftp://` and a bare
/// `localhost:3000` while the file is still open in an editor.
fn starts_as_http(template: &str) -> bool {
    let lower = template.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

impl EnvironmentSpec {
    /// Whether nothing is declared, so the Work view has no Environment to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.services.is_empty() && self.resources.is_empty() && self.commands.is_empty()
    }

    #[must_use]
    pub fn service(&self, id: &str) -> Option<&ServiceSpec> {
        self.services.iter().find(|s| s.id == id)
    }

    #[must_use]
    pub fn resource(&self, id: &str) -> Option<&ResourceSpec> {
        self.resources.iter().find(|r| r.id == id)
    }

    #[must_use]
    pub fn command(&self, id: &str) -> Option<&EnvCommandSpec> {
        self.commands.iter().find(|c| c.id == id)
    }

    /// Everything the file alone decides: ids, argvs, health, references.
    ///
    /// # Errors
    ///
    /// The first problem, with its key.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        let mut seen = BTreeSet::new();
        for service in &self.services {
            let key = format!("service.{}", service.id);
            if !valid_id(&service.id) {
                return Err(EnvironmentError::new(
                    "service.id",
                    format!(
                        "`{}` is not a service id: use letters, digits, `_` and `-`",
                        service.id
                    ),
                ));
            }
            if !seen.insert(service.id.as_str()) {
                return Err(EnvironmentError::new(
                    key,
                    "two services share this id, and a check's `needs` could not say which",
                ));
            }
            check_argv(&format!("{key}.start"), &service.start, "a service")?;
            if let Some(stop) = &service.stop {
                check_argv(&format!("{key}.stop"), stop, "`stop`")?;
            }
            if let Some(logs) = &service.logs {
                check_argv(&format!("{key}.logs"), logs, "`logs`")?;
            }
            if service.logs.is_some() && !service.detached {
                return Err(EnvironmentError::new(
                    format!("{key}.logs"),
                    "`logs` is for a detached service; a service wtm holds a process for already \
                     logs what it prints",
                ));
            }
            if service.detached && service.stop.is_none() {
                return Err(EnvironmentError::new(
                    format!("{key}.stop"),
                    "a detached service needs `stop`: wtm holds no process for it, so without one \
                     nothing could stop it",
                ));
            }
            if service.ports.iter().any(|port| port.trim().is_empty()) {
                return Err(EnvironmentError::new(
                    format!("{key}.ports"),
                    "a port is empty; write the template that renders it, as `{{ env.WEB_PORT }}`",
                ));
            }
            validate_health(&key, &service.health)?;
            if let Some(preview) = &service.preview
                && !starts_as_http(preview)
            {
                return Err(EnvironmentError::new(
                    format!("{key}.preview"),
                    "a preview is an http:// or https:// address on this machine",
                ));
            }
        }

        let mut seen = BTreeSet::new();
        for resource in &self.resources {
            if !valid_id(&resource.id) {
                return Err(EnvironmentError::new(
                    "resource.id",
                    format!(
                        "`{}` is not a resource id: use letters, digits, `_` and `-`",
                        resource.id
                    ),
                ));
            }
            if !seen.insert(resource.id.as_str()) {
                return Err(EnvironmentError::new(
                    format!("resource.{}", resource.id),
                    "two resources share this id",
                ));
            }
        }

        let mut seen = BTreeSet::new();
        for command in &self.commands {
            let key = format!("command.{}", command.id);
            if !valid_id(&command.id) {
                return Err(EnvironmentError::new(
                    "command.id",
                    format!(
                        "`{}` is not a command id: use letters, digits, `_` and `-`",
                        command.id
                    ),
                ));
            }
            if !seen.insert(command.id.as_str()) {
                return Err(EnvironmentError::new(key, "two commands share this id"));
            }
            check_argv(&format!("{key}.run"), &command.run, "a command")?;
            if command.timeout_ms == Some(0) {
                return Err(EnvironmentError::new(
                    format!("{key}.timeout_ms"),
                    "a command needs some time to run",
                ));
            }
            for resource in &command.resources {
                if self.resource(resource).is_none() {
                    return Err(EnvironmentError::new(
                        format!("{key}.resources"),
                        format!(
                            "`{resource}` is not a declared `[[environment.resource]]`; a \
                             destructive command's approval depends on what it acts on"
                        ),
                    ));
                }
            }
            if command.destructive && command.resources.is_empty() {
                return Err(EnvironmentError::new(
                    format!("{key}.resources"),
                    "a destructive command has to name the resources it acts on, so wtm can tell \
                     whether it reaches one every worktree shares",
                ));
            }
        }
        Ok(())
    }
}

fn validate_health(key: &str, health: &HealthSpec) -> Result<(), EnvironmentError> {
    let key = format!("{key}.health");
    match (&health.url, &health.command) {
        (Some(_), Some(_)) => {
            return Err(EnvironmentError::new(
                key,
                "give `url` or `command`, not both: a service that answered one and not the other \
                 would be neither healthy nor not",
            ));
        }
        (None, None) => {
            return Err(EnvironmentError::new(
                key,
                "give `url` or `command`: without a way to ask, wtm could only guess that the \
                 service is up",
            ));
        }
        (Some(url), None) if !starts_as_http(url) => {
            return Err(EnvironmentError::new(
                format!("{key}.url"),
                "a health URL is an http:// or https:// address on this machine",
            ));
        }
        (None, Some(command)) => check_argv(&format!("{key}.command"), command, "a health check")?,
        (Some(_), None) => {}
    }
    match health.timeout_ms {
        Some(0) => Err(EnvironmentError::new(
            format!("{key}.timeout_ms"),
            "a service needs some time to start",
        )),
        Some(ms) if ms > MAX_HEALTH_TIMEOUT_MS => Err(EnvironmentError::new(
            format!("{key}.timeout_ms"),
            format!(
                "at most {} minutes: a service that is not up by then is not starting",
                MAX_HEALTH_TIMEOUT_MS / 60_000
            ),
        )),
        _ => Ok(()),
    }
}

// ═══════════════════════════════ a service's state ═══════════════════════════════

/// Where a service is, as the Environment tab shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// Not running, as far as wtm knows: never started, or stopped.
    Stopped,
    /// Started, and its health not answered yet.
    Starting,
    /// It answered its health check.
    Healthy,
    /// It is running, and it did not answer its health check in time, or stopped answering when
    /// asked again.
    Unhealthy,
    /// It could not be started, or it exited without being asked to.
    Failed,
}

/// What happened to a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEvent {
    /// You asked for it to start, and its process is running.
    Started,
    /// Its health check answered.
    Answered,
    /// Its health check did not answer before its deadline, or a recheck failed.
    Unanswered,
    /// Its process exited, and nobody asked it to.
    Exited,
    /// It could not be started at all: a missing program, a failed detached start.
    StartFailed,
    /// You, or quitting, stopped it.
    Stopped,
}

/// A step a service's state does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a {from:?} service cannot take {event:?}")]
pub struct ServiceTransitionError {
    pub from: ServiceState,
    pub event: ServiceEvent,
}

impl ServiceState {
    /// Whether wtm believes a process of this service is running.
    #[must_use]
    pub fn is_running(self) -> bool {
        matches!(self, Self::Starting | Self::Healthy | Self::Unhealthy)
    }

    /// The state after `event`.
    ///
    /// # Why a late answer is refused rather than ignored
    ///
    /// A health check that answers after the service was stopped, or after it exited, is about a
    /// process that is gone. Taking it would turn a stopped service green. The supervisor checks
    /// the generation of the start a probe belongs to, and this refusal is the second line behind
    /// that.
    ///
    /// # Errors
    ///
    /// When the step does not exist from this state.
    pub fn after(self, event: ServiceEvent) -> Result<Self, ServiceTransitionError> {
        use ServiceEvent as E;
        use ServiceState as S;
        let next = match (self, event) {
            (S::Stopped | S::Failed, E::Started) => S::Starting,
            (S::Starting | S::Healthy | S::Unhealthy, E::Answered) => S::Healthy,
            (S::Starting | S::Healthy | S::Unhealthy, E::Unanswered) => S::Unhealthy,
            (S::Stopped | S::Failed, E::StartFailed)
            | (S::Starting | S::Healthy | S::Unhealthy, E::Exited) => S::Failed,
            (_, E::Stopped) => S::Stopped,
            (from, event) => return Err(ServiceTransitionError { from, event }),
        };
        Ok(next)
    }
}

/// When to ask a service whether it is up: the wait before each question, in milliseconds.
///
/// Doubling from [`FIRST_PROBE_DELAY_MS`] up to [`MAX_PROBE_DELAY_MS`], and ending once the waits
/// reach `timeout_ms`. A small server is seen up within a quarter of a second; a slow one is asked
/// every four seconds rather than hammered; and the list ends, so a service that never answers
/// becomes unhealthy at its deadline and nothing goes on asking.
#[must_use]
pub fn probe_delays(timeout_ms: u64) -> Vec<u64> {
    let mut delays = Vec::new();
    let mut waited = 0u64;
    let mut next = FIRST_PROBE_DELAY_MS;
    while waited < timeout_ms {
        let delay = next.min(timeout_ms - waited);
        delays.push(delay);
        waited += delay;
        next = (next * 2).min(MAX_PROBE_DELAY_MS);
    }
    delays
}

/// Whether an HTTP status says the service is up. A redirect counts: a dev server that sends `/`
/// to `/login` is up.
#[must_use]
pub fn healthy_status(status: u16) -> bool {
    (200..400).contains(&status)
}

// ═══════════════════════════════ addresses ═══════════════════════════════

/// A rendered URL that names this machine, checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LoopbackUrl(String);

impl LoopbackUrl {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a rendered URL is not one wtm will fetch or open.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlRefusal {
    #[error("`{0}` is not an http:// or https:// address")]
    NotHttp(String),
    #[error("`{0}` names another machine; only localhost addresses are fetched or opened")]
    NotLoopback(String),
    #[error("`{0}` carries a user name or password, which a local preview never needs")]
    Credentials(String),
}

/// `url`, if it is http(s) on this machine: `localhost` or a `*.localhost` name, `127.0.0.0/8`, or
/// `[::1]`.
///
/// # Why only loopback
///
/// A health URL is fetched without a click and a preview opens in a pane that shares cookies with
/// your other browser panes. Both are templates over a worktree's `.env`, which anyone who can
/// commit a setup script can write. Restricting them to this machine keeps a health check from
/// being a way to make a request elsewhere, which ARCHITECTURE §6a would otherwise have to list.
///
/// # Errors
///
/// [`UrlRefusal`], naming the URL.
pub fn loopback_url(url: &str) -> Result<LoopbackUrl, UrlRefusal> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
        .ok_or_else(|| UrlRefusal::NotHttp(url.to_owned()))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err(UrlRefusal::Credentials(url.to_owned()));
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    let loopback = host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || is_loopback_v4(host);
    if loopback {
        Ok(LoopbackUrl(url.to_owned()))
    } else {
        Err(UrlRefusal::NotLoopback(url.to_owned()))
    }
}

fn is_loopback_v4(host: &str) -> bool {
    let octets: Vec<&str> = host.split('.').collect();
    octets.len() == 4
        && octets[0] == "127"
        && octets
            .iter()
            .all(|octet| !octet.is_empty() && octet.parse::<u8>().is_ok())
}

/// A rendered port template as a port number.
///
/// # Errors
///
/// When it is empty, not a number, or 0. The message is a sentence for the Environment tab.
pub fn port_number(rendered: &str) -> Result<u16, String> {
    let trimmed = rendered.trim();
    if trimmed.is_empty() {
        return Err("renders to nothing; is the variable in this worktree's .env yet?".to_owned());
    }
    match trimmed.parse::<u16>() {
        Ok(0) | Err(_) => Err(format!(
            "renders to `{trimmed}`, which is not a port number"
        )),
        Ok(port) => Ok(port),
    }
}

// ═══════════════════════════════ who holds a port ═══════════════════════════════

/// Something listening on a port a service needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortOwner {
    /// From `lsof`. Absent when only Docker could say.
    pub pid: Option<u32>,
    /// The program's name as `lsof` reports it, or the container's image for Docker.
    pub command: String,
    /// The container publishing the port, when Docker was asked and one does.
    pub container: Option<String>,
    /// A service wtm itself started, in this worktree or another, when the pid is one of them.
    pub service: Option<OwnedService>,
}

/// A service wtm started, named for a conflict report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedService {
    pub worktree: String,
    pub service: String,
}

/// A port a service needs that something else holds. Reported, never resolved by wtm: the holder
/// may be another worktree's server, your own terminal's, or a database you rely on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortConflict {
    pub port: u16,
    /// Empty when the port is held and neither `lsof` nor Docker could say by what, which happens
    /// for another user's process or a socket still closing.
    pub owners: Vec<PortOwner>,
}

/// The listeners in `lsof -F pcn` output: `p<pid>` opens a process, `c<command>` names it.
///
/// Machine output rather than the table, for ARCHITECTURE §4's reason: a human-readable column
/// layout changes width with its contents.
#[must_use]
pub fn parse_lsof(output: &str) -> Vec<PortOwner> {
    let mut owners: Vec<PortOwner> = Vec::new();
    for line in output.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => {
                if let Ok(pid) = value.parse::<u32>()
                    && !owners.iter().any(|o| o.pid == Some(pid))
                {
                    owners.push(PortOwner {
                        pid: Some(pid),
                        command: String::new(),
                        container: None,
                        service: None,
                    });
                }
            }
            "c" => {
                if let Some(last) = owners.last_mut()
                    && last.command.is_empty()
                {
                    value.clone_into(&mut last.command);
                }
            }
            _ => {}
        }
    }
    owners
}

/// The containers publishing host `port`, from `docker ps --format '{{.Names}}\t{{.Image}}\t{{.Ports}}'`.
///
/// A ports column reads `0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp`: the host side is before the
/// arrow, after its last colon.
#[must_use]
pub fn containers_on(output: &str, port: u16) -> Vec<PortOwner> {
    let wanted = port.to_string();
    output
        .lines()
        .filter_map(|line| {
            let mut columns = line.split('\t');
            let name = columns.next()?.trim();
            let image = columns.next()?.trim();
            let ports = columns.next().unwrap_or_default();
            let publishes = ports.split(',').any(|mapping| {
                mapping
                    .split("->")
                    .next()
                    .and_then(|host| host.rsplit(':').next())
                    .is_some_and(|host_port| host_port.trim() == wanted)
                    && mapping.contains("->")
            });
            (publishes && !name.is_empty()).then(|| PortOwner {
                pid: None,
                command: image.to_owned(),
                container: Some(name.to_owned()),
                service: None,
            })
        })
        .collect()
}

/// Whether an `lsof` owner is Docker's own port forwarder, whose pid says nothing about which
/// container holds the port.
#[must_use]
pub fn is_docker_forwarder(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    ["docker", "vpnkit", "com.docke", "orbstack"]
        .iter()
        .any(|name| lower.contains(name))
}

// ═══════════════════════════════ owning a process ═══════════════════════════════

/// A process wtm started, as it can be recognised again: its pid, and when it started.
///
/// A pid alone is not an identity. After a crash, or a long sleep, the pid a service had may
/// belong to something else entirely, and signalling it would kill a process wtm never started.
/// The start time, as the operating system reports it, tells the two apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessIdentity {
    pub pid: u32,
    /// Opaque: compared, never parsed.
    pub started: String,
}

/// What launch recovery makes of a service a previous run of wtm started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    /// Its process is still running: the same pid, started at the same time. wtm takes it back.
    Adopt,
    /// Nothing runs under its pid now. It ended while wtm was not watching.
    Exited,
    /// Something else runs under its pid now. That process is not wtm's, and is never signalled.
    Reused,
}

/// Compare a recorded process with what runs under its pid now (`None`: nothing does).
#[must_use]
pub fn recover(recorded: &ProcessIdentity, now: Option<&ProcessIdentity>) -> Recovery {
    match now {
        None => Recovery::Exited,
        Some(now) if now.pid == recorded.pid && now.started == recorded.started => Recovery::Adopt,
        Some(_) => Recovery::Reused,
    }
}

// ═══════════════════════════════ commands and approval ═══════════════════════════════

/// Whether running `command` needs an approval each time: it is destructive and acts on a shared
/// resource. A resource id the spec does not declare counts as shared, because not knowing what a
/// destructive command reaches is not a reason to let it through.
#[must_use]
pub fn needs_approval(spec: &EnvironmentSpec, command: &EnvCommandSpec) -> bool {
    command.destructive
        && command.resources.iter().any(|id| {
            spec.resource(id)
                .is_none_or(|resource| resource.isolation == Isolation::Shared)
        })
}

/// The binding an approval to run a command covers: the hash of exactly what runs, the rendered
/// argv and its directory.
///
/// Rendered, not declared: the template is what the trust prompt showed, but `{{ env.DB_URL }}`
/// can point somewhere else by the time you approve, and an approval of the words is not an
/// approval of the database they name now. The values themselves never leave the app (ARCHITECTURE
/// §6a); only their hash is kept.
#[must_use]
pub fn command_binding(argv: &[String], cwd: &str) -> Binding {
    let exact = serde_json::json!({ "argv": argv, "cwd": cwd });
    Binding::default().with(Pinned::Payload, sha256_of(exact.to_string().as_bytes()))
}

/// What an approval to run an environment command was of, kept with it.
///
/// The declared argv, never the rendered one: the window shows what you approved, and a rendered
/// argv can hold a password from `.env`. [`command_binding`] carries the rendered one's hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRequest {
    pub project: String,
    pub worktree: String,
    pub command: String,
    pub label: String,
    /// As the config declares it, templates and all.
    pub argv: Vec<String>,
    /// The shared resources that make it need approval, by label.
    pub shared: Vec<String>,
}

// ═══════════════════════════════ what the store keeps ═══════════════════════════════

/// A service of one worktree, as the store keeps it: what wtm last knew of it, and the process
/// that lets a relaunch find it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRecord {
    /// The project's root.
    pub project: String,
    /// The worktree's path.
    pub worktree: String,
    pub service: String,
    pub state: ServiceState,
    /// The process group wtm started and holds: the service itself, or, for a detached service,
    /// the `logs` command wtm runs beside it. `None` when wtm holds nothing for it.
    pub process: Option<ProcessIdentity>,
    pub detached: bool,
    /// Where its output goes.
    pub log: String,
    /// Why it is unhealthy or failed, or what answered its health check, in a sentence.
    pub detail: Option<String>,
    pub started_at: Option<UnixMs>,
    pub updated_at: UnixMs,
}

id! {
    /// One run of an environment command, as the store numbers it.
    CommandRunId
}

/// Where one run of an environment command is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandRunState {
    /// A destructive command on a shared resource, waiting for you.
    AwaitingApproval,
    /// You said no. Nothing ran.
    Declined,
    /// What it would run changed between the request and your approval. Nothing ran.
    Stale,
    Running,
    Succeeded,
    Failed,
}

impl CommandRunState {
    /// Whether this run is over.
    #[must_use]
    pub fn is_over(self) -> bool {
        !matches!(self, Self::AwaitingApproval | Self::Running)
    }

    /// Whether a run may move from this state to `to`. A run that is over stays over: an approval
    /// covers one run, so approving again is a new request.
    #[must_use]
    pub fn can_move(self, to: Self) -> bool {
        use CommandRunState as S;
        matches!(
            (self, to),
            (S::AwaitingApproval, S::Declined | S::Stale | S::Running)
                | (S::Running, S::Succeeded | S::Failed)
        )
    }
}

/// A run to record, not yet numbered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCommandRun {
    /// The worktree's environment flow.
    pub flow: FlowId,
    pub command: String,
    pub label: String,
    /// As declared. See [`CommandRequest`] for why never rendered.
    pub argv: Vec<String>,
    /// The hash of what will run ([`command_binding`]).
    pub binding: Binding,
    /// `AwaitingApproval` or `Running`: a run starts either waiting for you or under way.
    pub state: CommandRunState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRun {
    pub id: CommandRunId,
    pub flow: FlowId,
    pub command: String,
    pub label: String,
    pub argv: Vec<String>,
    pub binding: Binding,
    pub state: CommandRunState,
    /// The approval it ran under, when it needed one.
    pub approval: Option<ApprovalId>,
    pub exit_code: Option<i32>,
    /// The end of what it printed.
    pub output: Option<String>,
    pub requested_at: UnixMs,
    pub finished_at: Option<UnixMs>,
}

/// A change to a run: where it moves, and what it left.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandRunChange {
    pub state: Option<CommandRunState>,
    pub approval: Option<ApprovalId>,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
}

/// Why a store refused to move a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("a {from:?} command run cannot become {to:?}")]
pub struct CommandRunRefused {
    pub from: CommandRunState,
    pub to: CommandRunState,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(toml_like: serde_json::Value) -> EnvironmentSpec {
        serde_json::from_value(toml_like).unwrap()
    }

    fn web() -> serde_json::Value {
        serde_json::json!({
            "id": "web",
            "start": ["python3", "-m", "http.server", "{{ env.WEB_PORT }}"],
            "ports": ["{{ env.WEB_PORT }}"],
            "health": { "url": "http://localhost:{{ env.WEB_PORT }}/healthz" },
            "preview": "http://localhost:{{ env.WEB_PORT }}",
        })
    }

    #[test]
    fn a_service_a_shared_resource_and_a_destructive_command_validate() {
        let spec = spec(serde_json::json!({
            "service": [web()],
            "resource": [{ "id": "db", "isolation": "shared" }],
            "command": [{ "id": "reset", "run": ["just", "reset-db"], "destructive": true, "resources": ["db"] }],
        }));
        spec.validate().unwrap();
        assert_eq!(
            spec.service("web").unwrap().health.timeout_ms(),
            DEFAULT_HEALTH_TIMEOUT_MS
        );
        assert!(needs_approval(&spec, spec.command("reset").unwrap()));
    }

    #[test]
    fn a_service_has_to_say_how_to_tell_it_is_up_and_only_one_way() {
        let mut neither = web();
        neither["health"] = serde_json::json!({});
        let error = spec(serde_json::json!({ "service": [neither] }))
            .validate()
            .unwrap_err();
        assert_eq!(error.key, "service.web.health");

        let mut both = web();
        both["health"] = serde_json::json!({ "url": "http://localhost:1/", "command": ["true"] });
        let error = spec(serde_json::json!({ "service": [both] }))
            .validate()
            .unwrap_err();
        assert!(error.message.contains("not both"), "{error}");
    }

    #[test]
    fn a_detached_service_has_to_say_how_to_stop_it() {
        let mut detached = web();
        detached["detached"] = serde_json::json!(true);
        let error = spec(serde_json::json!({ "service": [detached] }))
            .validate()
            .unwrap_err();
        assert_eq!(error.key, "service.web.stop");
    }

    #[test]
    fn a_destructive_command_names_declared_resources() {
        let unnamed = spec(serde_json::json!({
            "command": [{ "id": "reset", "run": ["x"], "destructive": true }],
        }));
        assert_eq!(
            unnamed.validate().unwrap_err().key,
            "command.reset.resources"
        );

        let undeclared = spec(serde_json::json!({
            "command": [{ "id": "reset", "run": ["x"], "resources": ["db"] }],
        }));
        assert!(undeclared.validate().unwrap_err().message.contains("`db`"));
    }

    #[test]
    fn health_and_preview_must_start_as_web_addresses() {
        let mut ftp = web();
        ftp["health"] = serde_json::json!({ "url": "ftp://localhost/x" });
        assert_eq!(
            spec(serde_json::json!({ "service": [ftp] }))
                .validate()
                .unwrap_err()
                .key,
            "service.web.health.url"
        );
        let mut bare = web();
        bare["preview"] = serde_json::json!("localhost:3000");
        assert_eq!(
            spec(serde_json::json!({ "service": [bare] }))
                .validate()
                .unwrap_err()
                .key,
            "service.web.preview"
        );
    }

    #[test]
    fn a_health_deadline_is_bounded_both_ways() {
        let mut zero = web();
        zero["health"]["timeout_ms"] = serde_json::json!(0);
        assert!(
            spec(serde_json::json!({ "service": [zero] }))
                .validate()
                .is_err()
        );
        let mut day = web();
        day["health"]["timeout_ms"] = serde_json::json!(MAX_HEALTH_TIMEOUT_MS + 1);
        assert!(
            spec(serde_json::json!({ "service": [day] }))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn two_services_with_one_id_are_refused() {
        let error = spec(serde_json::json!({ "service": [web(), web()] }))
            .validate()
            .unwrap_err();
        assert_eq!(error.key, "service.web");
    }

    #[test]
    fn only_a_destructive_command_on_a_shared_resource_needs_approval_each_time() {
        let spec = spec(serde_json::json!({
            "resource": [
                { "id": "db", "isolation": "shared" },
                { "id": "cache", "isolation": "per_worktree" },
            ],
            "command": [
                { "id": "reset", "run": ["x"], "destructive": true, "resources": ["db"] },
                { "id": "wipe", "run": ["x"], "destructive": true, "resources": ["cache"] },
                { "id": "both", "run": ["x"], "destructive": true, "resources": ["cache", "db"] },
                { "id": "seed", "run": ["x"], "resources": ["db"] },
            ],
        }));
        spec.validate().unwrap();
        let approval = |id: &str| needs_approval(&spec, spec.command(id).unwrap());
        assert!(approval("reset"));
        assert!(
            !approval("wipe"),
            "its own database is this worktree's business"
        );
        assert!(approval("both"), "one shared resource is enough");
        assert!(!approval("seed"), "not destructive");
    }

    #[test]
    fn a_service_moves_through_its_states_and_a_late_answer_cannot_turn_a_stopped_one_green() {
        use ServiceEvent as E;
        use ServiceState as S;
        let started = S::Stopped.after(E::Started).unwrap();
        assert_eq!(started, S::Starting);
        assert_eq!(started.after(E::Answered).unwrap(), S::Healthy);
        assert_eq!(started.after(E::Unanswered).unwrap(), S::Unhealthy);
        assert_eq!(S::Unhealthy.after(E::Answered).unwrap(), S::Healthy);
        assert_eq!(S::Healthy.after(E::Exited).unwrap(), S::Failed);
        assert_eq!(S::Failed.after(E::Started).unwrap(), S::Starting);
        assert_eq!(S::Healthy.after(E::Stopped).unwrap(), S::Stopped);
        assert_eq!(S::Stopped.after(E::StartFailed).unwrap(), S::Failed);

        assert!(S::Stopped.after(E::Answered).is_err());
        assert!(S::Failed.after(E::Answered).is_err());
        assert!(
            S::Healthy.after(E::Started).is_err(),
            "one process per service"
        );
        assert!(S::Stopped.after(E::Exited).is_err());
    }

    #[test]
    fn the_health_schedule_backs_off_and_ends_at_its_deadline() {
        let delays = probe_delays(10_000);
        assert_eq!(&delays[..5], &[250, 500, 1_000, 2_000, 4_000]);
        assert_eq!(delays.iter().sum::<u64>(), 10_000, "{delays:?}");
        assert!(delays.iter().all(|d| *d <= MAX_PROBE_DELAY_MS));

        let longest = probe_delays(MAX_HEALTH_TIMEOUT_MS);
        assert_eq!(longest.iter().sum::<u64>(), MAX_HEALTH_TIMEOUT_MS);
        assert!(longest.len() < 160, "bounded: {} questions", longest.len());

        assert_eq!(probe_delays(100), vec![100]);
    }

    #[test]
    fn a_redirect_is_up_and_a_server_error_is_not() {
        assert!(healthy_status(200));
        assert!(healthy_status(302));
        assert!(!healthy_status(404));
        assert!(!healthy_status(500));
        assert!(!healthy_status(0));
    }

    #[test]
    fn only_addresses_on_this_machine_are_fetched_or_opened() {
        for ok in [
            "http://localhost:8123/healthz",
            "https://app.localhost/",
            "http://127.0.0.1:9",
            "http://127.1.2.3",
            "http://[::1]:8080/x",
            "HTTP://LOCALHOST:1",
        ] {
            assert!(loopback_url(ok).is_ok(), "{ok}");
        }
        assert!(matches!(
            loopback_url("http://example.com/"),
            Err(UrlRefusal::NotLoopback(_))
        ));
        assert!(matches!(
            loopback_url("http://localhost.example.com/"),
            Err(UrlRefusal::NotLoopback(_))
        ));
        assert!(matches!(
            loopback_url("http://127.0.0.1.nip.io/"),
            Err(UrlRefusal::NotLoopback(_))
        ));
        assert!(matches!(
            loopback_url("http://user:pw@localhost/"),
            Err(UrlRefusal::Credentials(_))
        ));
        assert!(matches!(
            loopback_url("file:///etc/passwd"),
            Err(UrlRefusal::NotHttp(_))
        ));
    }

    #[test]
    fn a_port_template_must_render_to_a_port() {
        assert_eq!(port_number(" 8123 "), Ok(8123));
        assert!(port_number("").unwrap_err().contains(".env"));
        assert!(port_number("0").is_err());
        assert!(port_number("70000").is_err());
        assert!(port_number("web").is_err());
    }

    #[test]
    fn lsof_machine_output_names_each_listener_once() {
        let owners = parse_lsof(
            "p7781\ncpostgres\nf5\nn*:8125\np7781\ncpostgres\nn[::1]:8125\np42\ncpython3.12\nn127.0.0.1:8125\n",
        );
        assert_eq!(owners.len(), 2);
        assert_eq!(owners[0].pid, Some(7781));
        assert_eq!(owners[0].command, "postgres");
        assert_eq!(owners[1].command, "python3.12");
        assert!(parse_lsof("").is_empty());
    }

    #[test]
    fn docker_ps_names_the_container_publishing_a_host_port() {
        let ps = "api-db-1\tpostgres:16\t0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp\n\
                  cache\tredis:7\t6379/tcp\n\
                  web\tnginx\t127.0.0.1:15432->80/tcp\n";
        let owners = containers_on(ps, 5432);
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].container.as_deref(), Some("api-db-1"));
        assert_eq!(owners[0].command, "postgres:16");
        assert!(
            containers_on(ps, 6379).is_empty(),
            "exposed but not published"
        );
        assert!(
            containers_on(ps, 80).is_empty(),
            "the container's side is not the host's"
        );
    }

    #[test]
    fn a_pid_is_ours_only_while_it_started_when_ours_did() {
        let ours = ProcessIdentity {
            pid: 4242,
            started: "Tue Oct 21 14:02:03 2026".to_owned(),
        };
        assert_eq!(recover(&ours, Some(&ours.clone())), Recovery::Adopt);
        assert_eq!(recover(&ours, None), Recovery::Exited);
        let stranger = ProcessIdentity {
            pid: 4242,
            started: "Tue Oct 21 18:40:11 2026".to_owned(),
        };
        assert_eq!(recover(&ours, Some(&stranger)), Recovery::Reused);
    }

    #[test]
    fn an_approved_run_happens_once_and_a_finished_run_stays_finished() {
        use CommandRunState as S;
        assert!(S::AwaitingApproval.can_move(S::Running));
        assert!(S::AwaitingApproval.can_move(S::Declined));
        assert!(S::AwaitingApproval.can_move(S::Stale));
        assert!(S::Running.can_move(S::Succeeded));
        assert!(
            !S::Succeeded.can_move(S::Running),
            "approving again is a new request"
        );
        assert!(!S::Declined.can_move(S::Running));
        assert!(!S::Stale.can_move(S::Running));
        assert!(
            !S::AwaitingApproval.can_move(S::Succeeded),
            "it has to run first"
        );
        assert!(S::Failed.is_over() && !S::Running.is_over());
    }

    #[test]
    fn an_approval_binds_the_rendered_command_so_a_changed_value_makes_it_stale() {
        let argv = |url: &str| {
            vec![
                "psql".to_owned(),
                url.to_owned(),
                "-f".to_owned(),
                "reset.sql".to_owned(),
            ]
        };
        let approved = command_binding(&argv("postgres://localhost/app_a"), "/w/a");
        assert_eq!(
            approved,
            command_binding(&argv("postgres://localhost/app_a"), "/w/a")
        );
        assert_ne!(
            approved,
            command_binding(&argv("postgres://localhost/app_b"), "/w/a")
        );
        assert_ne!(
            approved,
            command_binding(&argv("postgres://localhost/app_a"), "/w/b")
        );
        assert!(
            !format!("{approved:?}").contains("app_a"),
            "only a hash is kept"
        );
    }
}

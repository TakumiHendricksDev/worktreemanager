//! Checks that need services, and the runtime role: what the health gate found, what starting
//! the environment for them is an approval of, and the screenshots a runtime role keeps.
//!
//! # "Not running" is never a pass, and never a quiet skip
//!
//! A browser test against nothing fails for a reason that says nothing about the change, and
//! passing it would be worse. So a check that needs services runs only once every one of them
//! answers its health check *now* — asked again, not read from what the Environment tab last
//! knew, because a server that died an hour ago still reads healthy until somebody asks. One that
//! does not is **blocked**, with the services that were missing named, and what happens next is
//! your setting: `ask` (the default) files one approval to start them and run what was blocked,
//! and `auto` starts them itself, only for checks your own config says may.
//!
//! # A runtime role reads the running app
//!
//! A static role reads a snapshot of the code. A runtime role exercises the app the worktree
//! serves: through the project's own Playwright when it has one, as configured checks run by the
//! app, or else by driving a browser pane wtm opens for it on a verified preview address, in a
//! store of its own that is thrown away with the pane (never the browser panes' persistent jar,
//! where a signed-in tab is a session anyone driving the pane could use). It runs after the static
//! roles, on the worktree itself, and the worktree's tree is checked again when it finishes,
//! because what it ran may have changed underneath it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::approval::{Binding, Pinned};
use super::env::ServiceState;
use super::publish::sha256_of;
use super::recipe::{RoleSpec, RuntimeDriver, WorkflowSpec};
use super::review::BlobRef;
use super::run::{InvocationId, StepId};
use super::state::FlowId;

/// A service a check or a runtime role needed, as the health gate found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatedService {
    pub service: String,
    pub state: ServiceState,
    /// What its health check answered just now, or why it is not up.
    pub detail: Option<String>,
    /// The address its health was asked at, when that is an address. With the answer, this is the
    /// evidence of what was up when the check ran.
    pub url: Option<String>,
}

impl GatedService {
    /// Whether it answered its health check when the gate asked.
    #[must_use]
    pub fn is_up(&self) -> bool {
        self.state == ServiceState::Healthy
    }
}

/// Whether every service a check needs answered: the only condition under which it runs.
#[must_use]
pub fn gate_passes(services: &[GatedService]) -> bool {
    services.iter().all(GatedService::is_up)
}

/// Why a check or a runtime role did not run, in the words the Reviews tab and the role both read.
///
/// "blocked: environment not started" leads every time, so the one fact that matters reads the
/// same wherever it is shown; the services follow, each with what is wrong with it.
#[must_use]
pub fn blocked_detail(services: &[GatedService]) -> String {
    let missing: Vec<String> = services
        .iter()
        .filter(|s| !s.is_up())
        .map(|s| {
            let state = match s.state {
                ServiceState::Stopped => "is stopped",
                ServiceState::Starting => "is still starting",
                ServiceState::Healthy => "answered",
                ServiceState::Unhealthy => "is not answering",
                ServiceState::Failed => "failed",
            };
            match s.detail.as_deref() {
                Some(detail) if s.state != ServiceState::Stopped => {
                    format!("`{}` {state} ({detail})", s.service)
                }
                _ => format!("`{}` {state}", s.service),
            }
        })
        .collect();
    if missing.is_empty() {
        return "blocked: environment not started".to_owned();
    }
    format!("blocked: environment not started — {}", missing.join(", "))
}

/// Whether a `package.json` declares the project's own Playwright test runner.
///
/// Only `@playwright/test`, in `dependencies` or `devDependencies`: the bare `playwright` library
/// is a browser driver a project may use for anything, and its presence says nothing about a
/// suite of end-to-end tests to run. A file that is not JSON declares nothing.
#[must_use]
pub fn has_playwright(package_json: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(package_json) else {
        return false;
    };
    ["dependencies", "devDependencies"].iter().any(|table| {
        value
            .get(table)
            .and_then(serde_json::Value::as_object)
            .is_some_and(|deps| deps.contains_key("@playwright/test"))
    })
}

/// How a runtime role exercises the app: what its config says, or else Playwright when the
/// worktree has it and the role has checks to run with it, and a browser pane otherwise.
///
/// Playwright needs both. A role with no checks has nothing to run through it, and a project
/// without `@playwright/test` cannot run what a check says, so either way the browser pane is
/// what is left — which still needs the service the role names, and config loading holds a role
/// that may fall back to the pane to naming one.
#[must_use]
pub fn driver_for(spec: &RoleSpec, has_playwright: bool) -> RuntimeDriver {
    spec.driver
        .unwrap_or(if has_playwright && !spec.checks.is_empty() {
            RuntimeDriver::Playwright
        } else {
            RuntimeDriver::Browser
        })
}

/// The services a runtime role needs answering before it starts: the one whose preview it drives,
/// or every service its checks need when Playwright runs them.
#[must_use]
pub fn runtime_needs(
    workflow: &WorkflowSpec,
    spec: &RoleSpec,
    driver: RuntimeDriver,
) -> Vec<String> {
    let mut needs: BTreeSet<String> = BTreeSet::new();
    match driver {
        RuntimeDriver::Browser => needs.extend(spec.service.iter().cloned()),
        RuntimeDriver::Playwright => {
            for check in &spec.checks {
                if let Some(check) = workflow.check.get(check) {
                    needs.extend(check.needs.iter().cloned());
                }
            }
        }
    }
    needs.into_iter().collect()
}

/// One service an approval would start, as its config declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedStart {
    pub service: String,
    pub label: String,
    /// Declared, templates and all, for the reason [`super::env::CommandRequest`] gives.
    pub argv: Vec<String>,
}

/// One blocked check an approval would run once its services answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedCheck {
    pub check: String,
    pub label: String,
    pub argv: Vec<String>,
    pub needs: Vec<String>,
}

/// What an approval to start a run's environment is of: the services it starts, and what it then
/// runs — the checks that were blocked, and the runtime roles waiting for them.
///
/// Kept with the approval so the window can show exactly what you said yes to. The binding pins
/// the hash of the *rendered* argvs ([`start_binding`]), so a `.env` changed between looking and
/// approving runs nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    pub project: String,
    pub worktree: String,
    pub services: Vec<PlannedStart>,
    pub checks: Vec<PlannedCheck>,
    /// Runtime roles that wait on these services, by role id.
    pub roles: Vec<String>,
}

impl StartRequest {
    /// Whether approving it would do anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty() && self.roles.is_empty()
    }
}

/// The pin of an approval to start services and run what they block: the hash of exactly what
/// would run, rendered, and where.
#[must_use]
pub fn start_binding(
    services: &[(String, Vec<String>)],
    checks: &[(String, Vec<String>)],
    roles: &[String],
    worktree: &str,
) -> Binding {
    let exact = serde_json::json!({
        "services": services,
        "checks": checks,
        "roles": roles,
        "cwd": worktree,
    });
    Binding::default().with(Pinned::Payload, sha256_of(exact.to_string().as_bytes()))
}

id! {
    /// One screenshot a runtime role kept as evidence, as the store numbers it.
    ScreenshotId
}

impl ScreenshotId {
    /// How a role cites it in a finding's `evidence`, and how the Reviews tab finds it again.
    #[must_use]
    pub fn label(self) -> String {
        format!("s{}", self.0)
    }

    /// The id in a citation such as `s12`, or `None` for anything else a role wrote there.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.trim().strip_prefix('s')?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok().map(Self)
    }
}

/// A picture of the page a runtime role was driving, kept under the flow's directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Screenshot {
    pub id: ScreenshotId,
    pub flow: FlowId,
    pub step: Option<StepId>,
    pub invocation: Option<InvocationId>,
    pub role: Option<String>,
    /// The page's address when it was taken.
    pub url: String,
    /// Who took it: the role, or wtm when the pane first loaded.
    pub taken_by: ShotTaker,
    pub image: BlobRef,
    pub taken_at: UnixMs,
}

/// Who asked for a screenshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotTaker {
    /// wtm, once the verified preview had loaded and before the role touched it.
    Wtm,
    /// The role, through `browser_screenshot`.
    Role,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewScreenshot {
    pub flow: FlowId,
    pub step: Option<StepId>,
    pub invocation: Option<InvocationId>,
    pub role: Option<String>,
    pub url: String,
    pub taken_by: ShotTaker,
    pub image: BlobRef,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::recipe::{CheckSpec, RoleKind};

    fn service(id: &str, state: ServiceState, detail: Option<&str>) -> GatedService {
        GatedService {
            service: id.to_owned(),
            state,
            detail: detail.map(str::to_owned),
            url: None,
        }
    }

    #[test]
    fn a_check_runs_only_when_every_service_it_needs_answered() {
        assert!(gate_passes(&[service("web", ServiceState::Healthy, None)]));
        assert!(!gate_passes(&[
            service("web", ServiceState::Healthy, None),
            service("api", ServiceState::Starting, None),
        ]));
        assert!(!gate_passes(&[service(
            "web",
            ServiceState::Unhealthy,
            None
        )]));
    }

    #[test]
    fn a_blocked_detail_leads_with_the_environment_and_names_what_is_missing() {
        let detail = blocked_detail(&[
            service("web", ServiceState::Stopped, Some("stopped by you")),
            service("api", ServiceState::Unhealthy, Some("answered 503")),
            service("db", ServiceState::Healthy, Some("answered 200")),
        ]);
        assert_eq!(
            detail,
            "blocked: environment not started — `web` is stopped, `api` is not answering \
             (answered 503)"
        );
    }

    #[test]
    fn playwright_is_the_test_runner_package_and_not_the_bare_library() {
        assert!(has_playwright(
            r#"{"devDependencies": {"@playwright/test": "^1.48.0"}}"#
        ));
        assert!(has_playwright(
            r#"{"dependencies": {"@playwright/test": "1.48.0"}}"#
        ));
        assert!(!has_playwright(
            r#"{"devDependencies": {"playwright": "^1.48.0"}}"#
        ));
        assert!(!has_playwright("not json"));
    }

    fn runtime_role(checks: &[&str], service: Option<&str>) -> RoleSpec {
        serde_json::from_value(serde_json::json!({
            "objective": "Click through the app",
            "provider": "claude",
            "kind": "runtime",
            "inputs": ["diff"],
            "checks": checks,
            "service": service,
        }))
        .unwrap()
    }

    #[test]
    fn a_runtime_role_uses_playwright_only_when_the_project_has_it_and_the_role_has_checks() {
        let role = runtime_role(&["e2e"], Some("web"));
        assert_eq!(role.kind, RoleKind::Runtime);
        assert_eq!(driver_for(&role, true), RuntimeDriver::Playwright);
        assert_eq!(driver_for(&role, false), RuntimeDriver::Browser);
        assert_eq!(
            driver_for(&runtime_role(&[], Some("web")), true),
            RuntimeDriver::Browser
        );
        let mut told = runtime_role(&["e2e"], Some("web"));
        told.driver = Some(RuntimeDriver::Playwright);
        assert_eq!(driver_for(&told, false), RuntimeDriver::Playwright);
    }

    #[test]
    fn a_runtime_role_needs_its_service_for_the_pane_and_its_checks_services_for_playwright() {
        let mut workflow = WorkflowSpec::default();
        workflow.check.insert(
            "e2e".to_owned(),
            CheckSpec {
                label: None,
                run: vec!["npx".to_owned(), "playwright".to_owned(), "test".to_owned()],
                timeout_ms: None,
                needs: vec!["web".to_owned(), "api".to_owned()],
                start_services: None,
                paths: Vec::new(),
            },
        );
        let role = runtime_role(&["e2e"], Some("web"));
        assert_eq!(
            runtime_needs(&workflow, &role, RuntimeDriver::Browser),
            ["web"]
        );
        assert_eq!(
            runtime_needs(&workflow, &role, RuntimeDriver::Playwright),
            ["api", "web"]
        );
    }

    #[test]
    fn approving_a_start_pins_what_would_run_so_a_changed_port_runs_nothing() {
        let services = |port: &str| {
            vec![(
                "web".to_owned(),
                vec![
                    "python3".to_owned(),
                    "server.py".to_owned(),
                    port.to_owned(),
                ],
            )]
        };
        let checks = vec![("e2e".to_owned(), vec!["just".to_owned(), "e2e".to_owned()])];
        let asked = start_binding(&services("18431"), &checks, &[], "/w");
        assert_eq!(asked, start_binding(&services("18431"), &checks, &[], "/w"));
        assert_ne!(asked, start_binding(&services("18432"), &checks, &[], "/w"));
        assert_ne!(asked, start_binding(&services("18431"), &[], &[], "/w"));
    }

    #[test]
    fn a_screenshot_is_cited_as_s_and_its_number() {
        assert_eq!(ScreenshotId(12).label(), "s12");
        assert_eq!(ScreenshotId::parse("s12"), Some(ScreenshotId(12)));
        assert_eq!(ScreenshotId::parse(" s3 "), Some(ScreenshotId(3)));
        assert_eq!(ScreenshotId::parse("c12"), None);
        assert_eq!(ScreenshotId::parse("s"), None);
        assert_eq!(ScreenshotId::parse("s1x"), None);
    }
}

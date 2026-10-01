//! What each provider account has left, in one record per provider for the whole app.
//!
//! # Rust's, not a pane's
//!
//! The figures belong to an account, and several panes, and windows, share one. They reach wtm
//! two ways. A running session reports them in passing (Claude on every turn, Codex on a rolling
//! notification), and the bridge files that report here rather than in the session's stream: see
//! `AgentEvent::LimitsUpdated` for why a replayed copy must not be able to overwrite a fresh one.
//! Or the usage view asks a provider directly. Codex and Claude can both answer that in full, and
//! Claude's answer is the only place its per-model weekly windows appear (Fable's, say). Cursor
//! names its plan and nothing else.
//!
//! Every change is announced as [`USAGE_LIMITS_EVENT`] carrying that provider's whole record, the
//! same shape `code:comments` uses, so a window mirrors it rather than merging changes itself.
//!
//! # In memory
//!
//! Not written to disk. A reload of the window keeps the record, because Rust outlives it. A
//! relaunch starts empty, and that is the honest state: yesterday's five-hour figure says nothing
//! about today's.
//!
//! # Asked only when someone looks
//!
//! Nothing here runs on a schedule. The view asks when it opens, and when its Refresh is pressed,
//! the "on demand and on window focus" rule ARCHITECTURE §8 sets for everything else. Each ask
//! starts a short-lived CLI, and the CLI is what talks to its own provider, as it does for every
//! session wtm opens; this module sends nothing anywhere itself.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use wtm_core::model::UsageLimits;
use wtm_core::ports::{CancelToken, Invocation};

use crate::app::App;
use crate::commands::{AppState, Reply, blocking};
use crate::view::ErrorView;

/// A provider's record changed. Payload: that provider's whole [`AccountUsage`].
pub const USAGE_LIMITS_EVENT: &str = "usage:limits";

/// How long `claude auth status` gets. It reads a local file and answers in about a second.
const CLAUDE_TIMEOUT_MS: u64 = 10_000;

/// How long Claude's `get_usage` ask gets. The CLI starts in about a second, but the answer can
/// mean a request to Claude's usage endpoint when the CLI's own reading is stale.
const CLAUDE_USAGE_TIMEOUT_MS: u64 = 20_000;

/// How long `cursor-agent about` gets.
///
/// It answers in half a second when warm, but Cursor's app-managed CLI can cold-start through its
/// agent-worker extension, which is why the capability probe gives Cursor twenty seconds as well.
const CURSOR_TIMEOUT_MS: u64 = 20_000;

/// One provider's record.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub provider: String,
    pub limits: UsageLimits,
    /// Unix milliseconds at which the windows were last reported, by a session or by an ask.
    ///
    /// Separate from [`Self::asked_at`] because for Claude the two are unrelated: asking gets its
    /// plan, and only a turn gets its windows. A view that showed "updated just now" after asking
    /// Claude for its plan would be vouching for figures an hour old.
    pub reported_at: Option<u64>,
    /// Unix milliseconds at which the provider was last asked directly, successfully or not.
    pub asked_at: Option<u64>,
    /// Why the last ask failed, in the CLI's own words. Cleared by the next one that works.
    pub error: Option<String>,
}

impl AccountUsage {
    fn new(provider: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            limits: UsageLimits::empty(provider),
            reported_at: None,
            asked_at: None,
            error: None,
        }
    }
}

/// Every provider's record, keyed by catalogue id.
#[derive(Debug, Default)]
pub struct Registry {
    accounts: parking_lot::Mutex<BTreeMap<String, AccountUsage>>,
}

impl Registry {
    /// Fold in what a running session reported, and return the provider's record.
    pub fn report(&self, limits: UsageLimits, now_ms: u64) -> AccountUsage {
        let mut accounts = self.accounts.lock();
        let account = accounts
            .entry(limits.provider.clone())
            .or_insert_with(|| AccountUsage::new(&limits.provider));
        if !limits.windows.is_empty() {
            account.reported_at = Some(now_ms);
        }
        account.limits.absorb(limits, false);
        account.clone()
    }

    /// Fold in the answer to asking a provider directly, and return its record.
    ///
    /// `complete` is true when the answer lists every window the account has, which only Codex's
    /// can. A failed ask keeps the figures already held: a Codex that cannot be reached right now
    /// has not changed what the last reading said, and the view shows the error beside it.
    pub fn answer(
        &self,
        provider: &str,
        answer: Result<(UsageLimits, bool), String>,
        now_ms: u64,
    ) -> AccountUsage {
        let mut accounts = self.accounts.lock();
        let account = accounts
            .entry(provider.to_owned())
            .or_insert_with(|| AccountUsage::new(provider));
        account.asked_at = Some(now_ms);
        match answer {
            Ok((limits, complete)) => {
                if complete || !limits.windows.is_empty() {
                    account.reported_at = Some(now_ms);
                }
                account.limits.absorb(limits, complete);
                account.error = None;
            }
            Err(error) => account.error = Some(error),
        }
        account.clone()
    }

    /// Every record held, in catalogue-id order.
    pub fn all(&self) -> Vec<AccountUsage> {
        self.accounts.lock().values().cloned().collect()
    }
}

/// File a session's report and announce the provider's record to every window.
pub fn report(handle: &AppHandle, app: &App, limits: UsageLimits) {
    let account = app.usage.report(limits, app.clock.now_unix_ms());
    announce(handle, &account);
}

fn announce(handle: &AppHandle, account: &AccountUsage) {
    if let Err(err) = handle.emit(USAGE_LIMITS_EVENT, account) {
        // The window is gone. The record is still here for the next one to ask for.
        tracing::debug!(error = %err, "could not announce usage limits");
    }
}

/// Ask a provider for whatever it will say without a session, and say whether that is the whole
/// list of its windows.
fn ask(app: &Arc<App>, provider: &str) -> Result<(UsageLimits, bool), String> {
    let entry = wtm_agent::entry(provider)
        .ok_or_else(|| format!("`{provider}` is not an agent this build of wtm knows"))?;
    let program = app
        .agent_executable(entry)
        .ok_or_else(|| format!("{} is not installed", entry.label))?;
    let program = program.to_string_lossy();

    match provider {
        wtm_agent::claude::ID => {
            let usage = Invocation::new(
                wtm_agent::claude::usage_argv(&program),
                std::env::temp_dir(),
                CLAUDE_USAGE_TIMEOUT_MS,
            )
            .with_stdin(wtm_agent::claude::usage_stdin());
            match run_invocation(app, &usage).and_then(|out| wtm_agent::claude::parse_usage(&out)) {
                Ok(answer) => Ok(answer),
                // A CLI from before `get_usage`, or one that could not reach its endpoint, can still
                // name the plan. That is what this view showed before it asked the fuller question.
                Err(error) => {
                    tracing::debug!(%error, "claude get_usage failed; asking auth status instead");
                    let out = run(
                        app,
                        wtm_agent::claude::auth_status_argv(&program),
                        CLAUDE_TIMEOUT_MS,
                    )?;
                    Ok((wtm_agent::claude::parse_auth_status(&out), false))
                }
            }
        }
        wtm_agent::codex::ID => {
            let reply = crate::commands::ask_codex(app, "account/rateLimits/read")?;
            Ok((wtm_agent::codex::parse_rate_limits(&reply)?, true))
        }
        wtm_agent::cursor::ID => {
            let out = run(
                app,
                wtm_agent::cursor::about_argv(&program),
                CURSOR_TIMEOUT_MS,
            )?;
            Ok((wtm_agent::cursor::parse_about(&out), false))
        }
        _ => Err(format!("{} has no usage to read", entry.label)),
    }
}

/// [`ask`], reachable from an integration test, which is the only place a real CLI's answer can be
/// checked. The command around it needs a running Tauri runtime.
///
/// # Errors
///
/// What [`ask`] says when the provider cannot be asked or refuses.
pub fn ask_for_test(app: &Arc<App>, provider: &str) -> Result<(UsageLimits, bool), String> {
    ask(app, provider)
}

/// Run a one-shot CLI command and return its stdout, or the end of what it said when it failed.
fn run(app: &App, argv: Vec<String>, timeout_ms: u64) -> Result<String, String> {
    run_invocation(
        app,
        &Invocation::new(argv, std::env::temp_dir(), timeout_ms),
    )
}

/// [`run`], for an invocation that needs more than an argv: one given input on stdin.
fn run_invocation(app: &App, inv: &Invocation) -> Result<String, String> {
    let out = app
        .runner
        .run_allow_failure(inv, &CancelToken::new())
        .map_err(|e| e.to_string())?;
    if out.is_success() {
        return Ok(out.stdout);
    }
    let said = if out.stderr.trim().is_empty() {
        out.stdout
    } else {
        out.stderr
    };
    Err(said
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("the command failed and said nothing")
        .trim()
        .to_owned())
}

/// Every provider's record as it stands, without asking anyone.
///
/// What a window seeds itself with when it opens, so a reload, or a pane popped out into its own
/// window, shows what the app already knows rather than nothing.
#[tauri::command]
pub async fn usage_limits(app: AppState<'_>) -> Reply<Vec<AccountUsage>> {
    Ok(app.usage.all())
}

/// Ask one provider directly, fold its answer in, and return its record.
///
/// One provider per call rather than all of them, because their speeds differ: Codex has to start
/// an app server, which starts its MCP servers, while Claude answers from a file. The view asks each
/// in parallel and fills each section in as it lands.
///
/// A failed ask is not an error reply. It comes back as the record with `error` set, which keeps
/// the last good figures on screen beside the reason.
#[tauri::command]
pub async fn refresh_usage_limits(
    handle: AppHandle,
    app: AppState<'_>,
    agent_id: String,
) -> Reply<AccountUsage> {
    let app = Arc::clone(&app);
    blocking(move || {
        if wtm_agent::entry(&agent_id).is_none() {
            return Err(ErrorView::new(
                "exec",
                format!("`{agent_id}` is not an agent this build of wtm knows how to drive"),
            ));
        }
        let answer = ask(&app, &agent_id);
        let account = app.usage.answer(&agent_id, answer, app.clock.now_unix_ms());
        announce(&handle, &account);
        Ok(account)
    })
    .await
}

#[cfg(test)]
mod tests {
    use wtm_core::model::LimitWindow;

    use super::*;

    fn weekly(used: f64) -> UsageLimits {
        UsageLimits {
            windows: vec![LimitWindow {
                minutes: Some(10_080),
                scope: None,
                used_percent: used,
                resets_at: Some(900),
            }],
            ..UsageLimits::empty("codex")
        }
    }

    #[test]
    fn asking_claude_for_its_plan_does_not_vouch_for_windows_it_did_not_report() {
        let registry = Registry::default();
        registry.report(
            UsageLimits {
                provider: "claude".to_owned(),
                ..weekly(40.0)
            },
            1_000,
        );
        let account = registry.answer(
            "claude",
            Ok((
                UsageLimits {
                    plan: Some("Team".to_owned()),
                    ..UsageLimits::empty("claude")
                },
                false,
            )),
            5_000,
        );

        assert_eq!(account.reported_at, Some(1_000));
        assert_eq!(account.asked_at, Some(5_000));
        assert_eq!(account.limits.plan.as_deref(), Some("Team"));
        assert_eq!(account.limits.windows.len(), 1);
    }

    #[test]
    fn a_failed_ask_keeps_the_last_figures_and_says_why() {
        let registry = Registry::default();
        registry.answer("codex", Ok((weekly(6.0), true)), 1_000);
        let account = registry.answer("codex", Err("codex is not logged in".to_owned()), 2_000);

        assert_eq!(account.error.as_deref(), Some("codex is not logged in"));
        assert_eq!(account.reported_at, Some(1_000));
        assert_eq!(account.limits.windows, weekly(6.0).windows);

        let recovered = registry.answer("codex", Ok((weekly(7.0), true)), 3_000);
        assert_eq!(recovered.error, None);
    }

    #[test]
    fn a_session_report_with_only_a_plan_does_not_move_the_reported_time() {
        let registry = Registry::default();
        let account = registry.report(
            UsageLimits {
                plan: Some("Pro".to_owned()),
                ..UsageLimits::empty("codex")
            },
            1_000,
        );
        assert_eq!(account.reported_at, None);
    }
}

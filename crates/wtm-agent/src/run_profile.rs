//! What a workflow run's agent CLI is started with, so that it bills a plan and holds no credential
//! it could use behind the app's back.
//!
//! Two pieces, both applied before a run session's process exists:
//!
//! - [`scrub`], the environment. It removes every variable that bills an API account or carries a
//!   forge, SSH or tracker credential, and points `gh` at an empty config directory.
//! - [`check`], the subscription preflight. It asks each CLI, with that same environment, how it is
//!   signed in, and a run refuses unless Claude is on claude.ai and Codex on `ChatGPT`.
//!
//! # Why both, when either sounds sufficient
//!
//! Each one misses something the other catches. Measured against Claude Code 2.1.296:
//!
//! - **The check cannot see most of what the scrub removes.** With `ANTHROPIC_AUTH_TOKEN` or
//!   `ANTHROPIC_BASE_URL` set, `claude auth status` still answers `"authMethod": "claude.ai"` and
//!   mentions neither. With `ANTHROPIC_API_KEY` set it *also* says `claude.ai`, and adds only an
//!   `apiKeySource` field, while `-p` quietly bills the key. Codex's `login status` reports its
//!   stored sign-in and ignores `OPENAI_API_KEY` and `CODEX_API_KEY` altogether.
//! - **The scrub cannot see a key that is not in the environment.** An `apiKeyHelper`, a Console
//!   login, or `codex login --with-api-key` are all stored by the CLI itself. Only asking finds them.
//!
//! # Manual panes are untouched
//!
//! Nothing here is applied to a pane a person opens. A repository's `[agent.<id>.env]` stays the
//! way to point a pane at a local model with `ANTHROPIC_BASE_URL`, and a pane keeps whatever key
//! wtm's own environment holds. A run session is the one place the app starts an agent with nobody
//! watching, which is the only reason it gets a stricter environment. ARCHITECTURE §6a's rule is
//! about values leaving the machine; this is about whose account pays when they do.
//!
//! # Stated limits
//!
//! A scrubbed environment removes credentials *by default*. It does not stop a process from reading
//! `~/.config/gh` or a keychain on purpose, which the F2 spike showed a sandboxed Bash can do. That
//! is the OS sandbox's job, and a run profile carries one; this module is not a substitute for it.

use std::path::Path;

use wtm_core::model::{AuthCheck, AuthMethod};
use wtm_core::ports::exec::{CancelToken, CommandRunner, EnvScrub, Invocation, Output};

use crate::{claude, codex};

/// How long a status command gets. Both read a local file and answer in about a second.
const STATUS_TIMEOUT_MS: u64 = 10_000;

/// The plan sign-in each provider has to report before a run may use it.
const CLAUDE_SUBSCRIPTION: &str = "claude.ai";
const CODEX_SUBSCRIPTION: &str = "ChatGPT";

/// Variables that decide who pays for Claude's tokens, or where they are sent.
#[must_use]
pub fn claude_billing() -> EnvScrub {
    EnvScrub::default()
        // `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` bill an API account, and
        // `ANTHROPIC_BASE_URL` sends every request to another server, with the claude.ai token in
        // its headers. The whole family rather than those three, because `ANTHROPIC_CUSTOM_HEADERS`
        // can carry a gateway's credential just as well, and a run takes its model from argv, so
        // nothing in the family is something it needs from the app's environment.
        .remove_prefix("ANTHROPIC_")
        // Bedrock, Vertex, Foundry, a gateway, and whatever route the next release adds. The check
        // would refuse these as `third_party`; removing them means a run uses the plan instead.
        .remove_prefix("CLAUDE_CODE_USE_")
}

/// Variables that decide who pays for Codex's tokens.
#[must_use]
pub fn codex_billing() -> EnvScrub {
    EnvScrub::default()
        // `OPENAI_API_KEY`, and `OPENAI_BASE_URL` for the same reason as Anthropic's.
        .remove_prefix("OPENAI_")
        // Exact names, because the rest of `CODEX_*` is the CLI's own plumbing: `CODEX_HOME` is
        // where its ChatGPT sign-in lives, and removing that would sign it out.
        .remove("CODEX_API_KEY")
        .remove("CODEX_ACCESS_TOKEN")
}

/// Credentials for everything a run must reach only through the app: the forge, SSH, the tracker.
fn credentials() -> EnvScrub {
    EnvScrub::default()
        // `gh` reads `GH_TOKEN`, `GH_ENTERPRISE_TOKEN`, `GITHUB_TOKEN` and
        // `GITHUB_ENTERPRISE_TOKEN`, and other tools read `GITHUB_PAT`. Two prefixes cover every
        // spelling, including `GH_CONFIG_DIR`, which [`scrub`] then sets on purpose.
        .remove_prefix("GH_")
        .remove_prefix("GITHUB_")
        .remove("CODEX_GITHUB_PERSONAL_ACCESS_TOKEN")
        // The agent socket signs and authenticates as the user. `~/.ssh/config` can still name the
        // agent directly, so this removes the default route, not every route; see the module docs.
        .remove("SSH_AUTH_SOCK")
        // `acli` keeps its own login, but it reads `ACLI_CONFIG_DIR` and `ATLASSIAN_*` overrides,
        // and the tracker tools around it read `JIRA_API_TOKEN` and `CONFLUENCE_API_TOKEN`.
        .remove_prefix("ACLI_")
        .remove_prefix("ATLASSIAN_")
        .remove_prefix("JIRA_")
        .remove_prefix("CONFLUENCE_")
}

/// The environment scrub every run session gets, whichever provider it runs.
///
/// Both providers' billing variables go from both, because a Claude run has no use for an `OpenAI`
/// key and the reverse.
///
/// `gh_config_dir` must be an empty directory the app owns. `gh` then finds no account rather than
/// the user's, which turns an attempt to use it into "not logged in" instead of an action taken as
/// the user. `GIT_TERMINAL_PROMPT=0` makes a git credential prompt fail rather than wait for a
/// terminal nobody is watching.
#[must_use]
pub fn scrub(gh_config_dir: &Path) -> EnvScrub {
    claude_billing()
        .and(codex_billing())
        .and(credentials())
        .set("GH_CONFIG_DIR", gh_config_dir.to_string_lossy())
        .set("GIT_TERMINAL_PROMPT", "0")
}

/// The billing variables for `provider`, or `None` for one with no subscription check.
#[must_use]
pub fn billing(provider: &str) -> Option<EnvScrub> {
    match provider {
        claude::ID => Some(claude_billing()),
        codex::ID => Some(codex_billing()),
        _ => None,
    }
}

/// Which of `names` would change how `provider` bills, sorted. For [`AuthCheck::inherited`].
pub fn inherited<'a>(provider: &str, names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let Some(billing) = billing(provider) else {
        return Vec::new();
    };
    let mut found: Vec<String> = names
        .into_iter()
        .filter(|name| billing.removes(name))
        .map(str::to_owned)
        .collect();
    found.sort();
    found
}

/// How Claude is signed in, out of `claude auth status --json`.
///
/// Read from stdout whatever the exit code: a signed-out CLI exits 1 and still prints the JSON.
#[must_use]
pub fn read_claude_status(stdout: &str) -> AuthMethod {
    let Ok(reply) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return AuthMethod::Unreadable;
    };
    let text = |field: &str| {
        reply
            .get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    };

    if reply.get("loggedIn").and_then(serde_json::Value::as_bool) == Some(false) {
        return AuthMethod::SignedOut;
    }
    // Ahead of `authMethod`, because that field still says `claude.ai` when a key is set, and `-p`
    // bills the key.
    if let Some(source) = text("apiKeySource").filter(|source| *source != "none") {
        return AuthMethod::ApiKey {
            source: Some(source.to_owned()),
        };
    }
    match text("authMethod") {
        Some(CLAUDE_SUBSCRIPTION) if text("apiProvider") == Some("firstParty") => {
            AuthMethod::Subscription {
                service: CLAUDE_SUBSCRIPTION.to_owned(),
            }
        }
        Some("api_key") => AuthMethod::ApiKey { source: None },
        Some("api_key_helper") => AuthMethod::ApiKey {
            source: Some("apiKeyHelper".to_owned()),
        },
        Some("none") => AuthMethod::SignedOut,
        // `third_party` says nothing a person could act on; the provider it names does.
        Some("third_party") => AuthMethod::Other {
            method: text("apiProvider").unwrap_or("third_party").to_owned(),
        },
        // `claude.ai` on a provider other than Anthropic's own lands here too, under that provider's
        // name, rather than passing as a subscription it cannot be.
        Some(CLAUDE_SUBSCRIPTION) => AuthMethod::Other {
            method: text("apiProvider")
                .unwrap_or(CLAUDE_SUBSCRIPTION)
                .to_owned(),
        },
        Some(other) => AuthMethod::Other {
            method: other.to_owned(),
        },
        None => AuthMethod::Unreadable,
    }
}

/// How Codex is signed in, out of `codex login status`.
///
/// Codex 0.160 prints its one line on stderr, so both streams are read. Matched by the start of the
/// line and nothing more, because an API-key sign-in reads `Logged in using an API key - sk-…`
/// followed by part of the key, and none of that may be kept.
#[must_use]
pub fn read_codex_status(out: &Output) -> AuthMethod {
    for line in out.stderr.lines().chain(out.stdout.lines()) {
        let line = line.trim().to_ascii_lowercase();
        if line.starts_with("logged in using chatgpt") {
            return AuthMethod::Subscription {
                service: CODEX_SUBSCRIPTION.to_owned(),
            };
        }
        if line.starts_with("logged in using an api key") {
            return AuthMethod::ApiKey { source: None };
        }
        if line.starts_with("not logged in") {
            return AuthMethod::SignedOut;
        }
    }
    AuthMethod::Unreadable
}

/// Why a run on `provider` signed in with `method` would refuse to start, or `None` if it may.
#[must_use]
pub fn refusal(provider: &str, method: &AuthMethod) -> Option<String> {
    let label = crate::entry(provider).map_or(provider, |entry| entry.label);
    let required = match provider {
        claude::ID => CLAUDE_SUBSCRIPTION,
        codex::ID => CODEX_SUBSCRIPTION,
        _ => {
            return Some(format!(
                "Workflow runs can't use {label}: wtm has no way to confirm it bills a subscription."
            ));
        }
    };
    match method {
        AuthMethod::Subscription { service } if service == required => None,
        _ => Some(format!(
            "Workflow runs won't start until {label} is signed in with {required}."
        )),
    }
}

/// Ask `provider`'s CLI, at `program`, how it is signed in, with `scrub` applied as a run would.
///
/// `inherited` is passed in rather than read here, because reading the app's environment is the
/// composition root's business: this crate is handed ports and never looks at the process.
pub fn check(
    runner: &dyn CommandRunner,
    provider: &str,
    program: &str,
    scrub: &EnvScrub,
    inherited: Vec<String>,
) -> AuthCheck {
    let method = match status_argv(provider, program) {
        None => AuthMethod::Unreadable,
        Some(argv) => {
            let inv = Invocation::new(argv, std::env::temp_dir(), STATUS_TIMEOUT_MS)
                .with_scrub(scrub.clone());
            match runner.run_allow_failure(&inv, &CancelToken::new()) {
                Ok(out) if provider == claude::ID => read_claude_status(&out.stdout),
                Ok(out) => read_codex_status(&out),
                Err(error) => {
                    tracing::debug!(provider, %error, "could not ask how the CLI is signed in");
                    AuthMethod::Unreadable
                }
            }
        }
    };
    AuthCheck {
        provider: provider.to_owned(),
        refusal: refusal(provider, &method),
        summary: method.summary(),
        method,
        inherited,
    }
}

/// Every check's refusal, joined, or `Ok` when every provider a run uses may be used.
///
/// # Errors
///
/// The refusals, one sentence each, in the order the checks were given.
pub fn require_subscriptions(checks: &[AuthCheck]) -> Result<(), String> {
    let refusals: Vec<&str> = checks
        .iter()
        .filter_map(|check| check.refusal.as_deref())
        .collect();
    if refusals.is_empty() {
        Ok(())
    } else {
        Err(refusals.join(" "))
    }
}

fn status_argv(provider: &str, program: &str) -> Option<Vec<String>> {
    match provider {
        claude::ID => Some(claude::auth_status_argv(program)),
        codex::ID => Some(codex::login_status_argv(program)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use wtm_testkit::FakeRunner;

    use super::*;

    /// `claude auth status --json` from Claude Code 2.1.296, signed in on a Team plan, with the
    /// account's details replaced. Only the shape matters.
    const CLAUDE_SIGNED_IN: &str = r#"{
      "loggedIn": true,
      "authMethod": "claude.ai",
      "apiProvider": "firstParty",
      "analyticsDisabled": false,
      "email": "someone@example.com",
      "orgName": "Example",
      "subscriptionType": "team"
    }"#;

    /// The same CLI and account, run with `ANTHROPIC_API_KEY` set to a dummy value. Note that
    /// `authMethod` does not change.
    const CLAUDE_WITH_A_KEY: &str = r#"{
      "loggedIn": true,
      "authMethod": "claude.ai",
      "apiProvider": "firstParty",
      "apiKeySource": "ANTHROPIC_API_KEY",
      "subscriptionType": "team"
    }"#;

    /// With `CLAUDE_CODE_USE_BEDROCK=1`.
    const CLAUDE_ON_BEDROCK: &str = r#"{
      "loggedIn": true,
      "authMethod": "third_party",
      "apiProvider": "bedrock",
      "analyticsDisabled": true
    }"#;

    /// With an empty `CLAUDE_CONFIG_DIR`. Exits 1.
    const CLAUDE_SIGNED_OUT: &str = r#"{
      "loggedIn": false,
      "authMethod": "none",
      "apiProvider": "firstParty",
      "analyticsDisabled": false
    }"#;

    fn stderr(text: &str) -> Output {
        Output {
            code: 0,
            stdout: String::new(),
            stderr: text.to_owned(),
            duration_ms: 1,
        }
    }

    #[test]
    fn claude_on_its_own_claude_ai_sign_in_is_a_subscription_runs_may_use() {
        let method = read_claude_status(CLAUDE_SIGNED_IN);
        assert_eq!(
            method,
            AuthMethod::Subscription {
                service: "claude.ai".to_owned()
            }
        );
        assert_eq!(refusal(claude::ID, &method), None);
    }

    #[test]
    fn claude_reporting_claude_ai_with_a_key_source_is_read_as_the_key_it_would_bill() {
        // The trap this module exists for: the method still says claude.ai.
        let method = read_claude_status(CLAUDE_WITH_A_KEY);
        assert_eq!(
            method,
            AuthMethod::ApiKey {
                source: Some("ANTHROPIC_API_KEY".to_owned())
            }
        );
        assert_eq!(
            refusal(claude::ID, &method).as_deref(),
            Some("Workflow runs won't start until Claude Code is signed in with claude.ai.")
        );
    }

    #[test]
    fn claude_through_a_cloud_provider_is_named_by_that_provider_and_refused() {
        let method = read_claude_status(CLAUDE_ON_BEDROCK);
        assert_eq!(
            method,
            AuthMethod::Other {
                method: "bedrock".to_owned()
            }
        );
        assert!(refusal(claude::ID, &method).is_some());
    }

    #[test]
    fn claude_signed_out_or_on_a_helper_or_answering_garbage_is_refused() {
        assert_eq!(read_claude_status(CLAUDE_SIGNED_OUT), AuthMethod::SignedOut);
        assert_eq!(
            read_claude_status(r#"{"loggedIn":true,"authMethod":"api_key_helper"}"#),
            AuthMethod::ApiKey {
                source: Some("apiKeyHelper".to_owned())
            }
        );
        // A long-lived token from `claude setup-token` is refused too. The plan asks for a claude.ai
        // login, and wtm cannot tell from here which account that token bills.
        assert_eq!(
            read_claude_status(r#"{"loggedIn":true,"authMethod":"oauth_token"}"#),
            AuthMethod::Other {
                method: "oauth_token".to_owned()
            }
        );
        assert_eq!(read_claude_status("not json"), AuthMethod::Unreadable);
        assert_eq!(read_claude_status("{}"), AuthMethod::Unreadable);
    }

    #[test]
    fn codex_on_chatgpt_is_a_subscription_runs_may_use() {
        let method = read_codex_status(&stderr("Logged in using ChatGPT\n"));
        assert_eq!(
            method,
            AuthMethod::Subscription {
                service: "ChatGPT".to_owned()
            }
        );
        assert_eq!(refusal(codex::ID, &method), None);
    }

    #[test]
    fn codex_on_an_api_key_is_refused_and_no_part_of_the_key_is_kept() {
        // 0.160.1's own wording, which ends in a masked key.
        let method = read_codex_status(&stderr("Logged in using an API key - sk-dumm***-key\n"));
        assert_eq!(method, AuthMethod::ApiKey { source: None });
        let check = AuthCheck {
            provider: codex::ID.to_owned(),
            refusal: refusal(codex::ID, &method),
            summary: method.summary(),
            method,
            inherited: Vec::new(),
        };
        let serialized = serde_json::to_string(&check).unwrap();
        assert!(
            !serialized.contains("sk-"),
            "nothing of the key may reach the view: {serialized}"
        );
        assert!(check.refusal.is_some());
    }

    #[test]
    fn codex_signed_out_or_saying_something_new_is_refused() {
        assert_eq!(
            read_codex_status(&stderr("Not logged in\n")),
            AuthMethod::SignedOut
        );
        assert_eq!(
            read_codex_status(&stderr("Logged in using something else entirely\n")),
            AuthMethod::Unreadable
        );
        assert!(refusal(codex::ID, &AuthMethod::Unreadable).is_some());
    }

    #[test]
    fn a_provider_with_no_subscription_check_is_refused_by_name() {
        assert_eq!(
            refusal(crate::cursor::ID, &AuthMethod::Unreadable).as_deref(),
            Some(
                "Workflow runs can't use Cursor Agent: wtm has no way to confirm it bills a subscription."
            )
        );
    }

    #[test]
    fn the_preflight_asks_with_the_runs_own_scrub_and_reads_the_answer() {
        let runner = FakeRunner::scripted(vec![FakeRunner::ok(CLAUDE_SIGNED_IN)]);
        let scrub = scrub(Path::new("/empty/gh"));

        let check = check(&runner, claude::ID, "/bin/claude", &scrub, Vec::new());

        assert_eq!(check.refusal, None);
        assert_eq!(check.summary, "Signed in with claude.ai");
        let calls = runner.calls();
        assert_eq!(
            calls[0].argv,
            ["/bin/claude", "auth", "status", "--json"].map(str::to_owned)
        );
        assert_eq!(
            calls[0].scrub, scrub,
            "asked with anything else, the answer would describe some other environment"
        );
    }

    #[test]
    fn a_cli_that_cannot_be_asked_is_refused_rather_than_assumed_signed_in() {
        let runner = FakeRunner::scripted(vec![Err(wtm_core::error::ExecError::Timeout {
            argv: "codex login status".to_owned(),
            timeout_ms: STATUS_TIMEOUT_MS,
        })]);
        let check = check(
            &runner,
            codex::ID,
            "codex",
            &EnvScrub::default(),
            Vec::new(),
        );
        assert_eq!(check.method, AuthMethod::Unreadable);
        assert!(check.refusal.is_some());
    }

    #[test]
    fn a_run_is_refused_when_any_provider_it_uses_is_refused() {
        let ok = AuthCheck {
            provider: claude::ID.to_owned(),
            method: AuthMethod::Subscription {
                service: "claude.ai".to_owned(),
            },
            summary: String::new(),
            refusal: None,
            inherited: Vec::new(),
        };
        let refused = AuthCheck {
            provider: codex::ID.to_owned(),
            method: AuthMethod::SignedOut,
            summary: String::new(),
            refusal: Some("Codex is not signed in.".to_owned()),
            inherited: Vec::new(),
        };
        assert_eq!(require_subscriptions(std::slice::from_ref(&ok)), Ok(()));
        assert_eq!(
            require_subscriptions(&[ok, refused]),
            Err("Codex is not signed in.".to_owned())
        );
    }

    #[test]
    fn the_run_scrub_removes_every_billing_and_credential_family_named_in_the_plan() {
        let scrub = scrub(Path::new("/empty/gh"));
        for name in [
            "GH_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "SSH_AUTH_SOCK",
            "JIRA_API_TOKEN",
            "ATLASSIAN_API_TOKEN",
            "ACLI_CONFIG_DIR",
        ] {
            assert!(scrub.removes(name), "{name} must not reach a run");
        }
        // What a CLI needs to find its own subscription sign-in.
        for name in ["HOME", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "PATH", "TMPDIR"] {
            assert!(!scrub.removes(name), "{name} must survive");
        }
        assert_eq!(
            scrub.values().get("GH_CONFIG_DIR").map(String::as_str),
            Some("/empty/gh")
        );
        assert_eq!(
            scrub
                .values()
                .get("GIT_TERMINAL_PROMPT")
                .map(String::as_str),
            Some("0")
        );
    }

    #[test]
    fn inherited_billing_variables_are_named_per_provider() {
        let names = [
            "HOME",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GH_TOKEN",
            "ANTHROPIC_BASE_URL",
        ];
        assert_eq!(
            inherited(claude::ID, names),
            ["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL"].map(str::to_owned)
        );
        assert_eq!(
            inherited(codex::ID, names),
            ["OPENAI_API_KEY"].map(str::to_owned)
        );
        assert!(inherited(crate::cursor::ID, names).is_empty());
    }
}

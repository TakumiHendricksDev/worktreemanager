//! Who pays for an agent CLI's tokens, as the CLI itself reports it.
//!
//! # Why this exists
//!
//! A workflow run is started by the app rather than by a person at a prompt, so nobody is there to
//! notice that it is billing an API account. Claude Code's `-p` mode uses `ANTHROPIC_API_KEY`
//! whenever it is set, without asking, and both CLIs can be signed in with a key instead of a plan.
//! So a run asks each CLI how it is signed in before it starts, and refuses anything but a
//! subscription. This module is the vocabulary for the answer; reading it out of each CLI belongs to
//! `wtm-agent`, which knows their wire formats.
//!
//! # Why not a field on [`UsageLimits`](super::UsageLimits)
//!
//! Limits are figures that change with every turn and merge from several reports. How an account is
//! signed in is one fact, read in one place, and replaced whole each time it is read.

use serde::{Deserialize, Serialize};

/// How a provider's CLI is signed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthMethod {
    /// A plan's sign-in: claude.ai for Claude, `ChatGPT` for Codex. Tokens come out of the plan.
    Subscription {
        service: String,
    },
    /// An API key, billed per token. `source` is where the CLI says the key came from, such as
    /// `ANTHROPIC_API_KEY` or `apiKeyHelper`, when it says.
    ApiKey {
        source: Option<String>,
    },
    /// Some other route, in the CLI's own word for it: `oauth_token`, `bedrock`, `vertex`.
    ///
    /// Kept verbatim rather than mapped to a friendlier name, because every one of these is refused
    /// and the CLI's own word is the one a person can search for.
    Other {
        method: String,
    },
    SignedOut,
    /// The CLI answered in a shape this build cannot read.
    ///
    /// Carries nothing from the answer on purpose: Codex prints part of an API key in its status
    /// line, and an unreadable answer is exactly the one nobody has checked for that.
    Unreadable,
}

impl AuthMethod {
    /// Whether tokens come out of a plan rather than being billed per token.
    #[must_use]
    pub fn is_subscription(&self) -> bool {
        matches!(self, Self::Subscription { .. })
    }

    /// The method as the start of a sentence: "Signed in with claude.ai".
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Subscription { service } => format!("Signed in with {service}"),
            Self::ApiKey {
                source: Some(source),
            } => format!("Using an API key from {source}"),
            Self::ApiKey { source: None } => "Using an API key".to_owned(),
            Self::Other { method } => format!("Signed in through {method}"),
            Self::SignedOut => "Not signed in".to_owned(),
            Self::Unreadable => "Signed in some way wtm could not read".to_owned(),
        }
    }
}

/// What the subscription preflight found for one provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCheck {
    /// The catalogue id of the provider asked.
    pub provider: String,
    /// What the CLI reported, asked with a workflow run's environment.
    pub method: AuthMethod,
    /// [`AuthMethod::summary`], so the view words it the way the refusal does.
    pub summary: String,
    /// Why a workflow run on this provider would refuse to start. `None` when it may.
    pub refusal: Option<String>,
    /// Names, never values, of variables in wtm's own environment that change how this provider
    /// bills. A pane opened from wtm inherits them; a workflow run has them removed.
    ///
    /// Listed because the check above runs with them removed, so it would otherwise say "claude.ai"
    /// while every pane opened from the same app was quietly using a key.
    pub inherited: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_subscription_counts_as_one() {
        assert!(
            AuthMethod::Subscription {
                service: "claude.ai".to_owned()
            }
            .is_subscription()
        );
        for method in [
            AuthMethod::ApiKey { source: None },
            AuthMethod::Other {
                method: "oauth_token".to_owned(),
            },
            AuthMethod::SignedOut,
            AuthMethod::Unreadable,
        ] {
            assert!(!method.is_subscription(), "{method:?} must not count");
        }
    }

    #[test]
    fn a_method_crosses_the_ipc_boundary_as_a_tagged_union() {
        let json = serde_json::to_value(AuthMethod::ApiKey {
            source: Some("ANTHROPIC_API_KEY".to_owned()),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "api_key", "source": "ANTHROPIC_API_KEY"})
        );
    }
}

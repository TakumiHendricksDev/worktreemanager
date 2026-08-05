//! What a provider can do, and the two very different ways of finding out.
//!
//! # Why this is not one mechanism
//!
//! Codex **advertises** its models. `model/list` returns each one with the effort ladder it actually
//! supports, and those ladders differ *within* the provider — verified against `codex-cli 0.144.6`:
//!
//! | model | efforts |
//! |---|---|
//! | `gpt-5.6-sol` | low, medium, high, xhigh, max, **ultra** |
//! | `gpt-5.6-luna` | low, medium, high, xhigh, max |
//! | `gpt-5.5` | low, medium, high, xhigh |
//!
//! Claude Code advertises nothing. Its efforts are a fixed five and its models are not enumerable
//! from the CLI at all, so the only honest source is a table compiled into this build.
//!
//! Pretending those are the same would mean either hardcoding Codex's list — wrong the week a model
//! ships — or claiming Claude's is live, which would make a stale table look like the CLI's fault.
//! So [`AgentCapability::models_are_live`] says which, and the UI can say "as reported by codex"
//! against "as of this wtm build".
//!
//! # Why the query lives in `src-tauri` and only its parser lives here
//!
//! Asking Codex means spawning a short-lived app server, and this crate deliberately cannot spawn —
//! its `Cargo.toml` has no `wtm-exec`, which is the proof. So the composition root drives the
//! process and hands the reply to [`crate::codex::parse_models`], which is pure and testable.

use wtm_core::model::{AgentCapability, AgentModel, EffortOption};

/// Claude Code's capabilities, as of this build.
///
/// # Why a table and not a query
///
/// There is no `model/list` here. `--model` takes an alias or a full id and the CLI validates it at
/// startup, so the only way to enumerate is to know — and knowing goes stale. Two things make that
/// survivable: the aliases (`opus`, `sonnet`, `haiku`) always resolve to the current model of that
/// tier, so an alias-first list ages far better than a list of dated ids; and the field is a free
/// string end to end, so a model this table has never heard of still works if the user types it.
///
/// `models_are_live: false` is what lets the UI say so rather than presenting this as the CLI's
/// answer.
#[must_use]
pub fn claude_capability() -> AgentCapability {
    // Five rungs, fixed, and the same for every model — the opposite of the other provider, where
    // the ladder is per model. Verified from `--help`: `--effort <low|medium|high|xhigh|max>`.
    let efforts = || -> Vec<EffortOption> {
        [
            ("low", "Fast, lighter reasoning"),
            ("medium", "Balances speed and depth"),
            ("high", "Greater reasoning depth"),
            ("xhigh", "Extra depth"),
            ("max", "Maximum depth, for the hardest problems"),
        ]
        .iter()
        .map(|(effort, description)| EffortOption {
            effort: (*effort).to_owned(),
            description: Some((*description).to_owned()),
        })
        .collect()
    };

    let model = |id: &str, label: &str, description: &str, is_default: bool| AgentModel {
        id: id.to_owned(),
        label: label.to_owned(),
        description: Some(description.to_owned()),
        is_default,
        default_effort: Some("high".to_owned()),
        efforts: efforts(),
    };

    AgentCapability {
        // Aliases first, deliberately: each resolves to the current model of its tier, so this list
        // stays true across releases in a way a list of dated ids would not.
        models: vec![
            model("opus", "Opus", "The most capable tier", true),
            model("sonnet", "Sonnet", "Balanced capability and speed", false),
            model("haiku", "Haiku", "Fastest and cheapest", false),
            model("fable", "Fable", "The newest tier", false),
            model(
                "opusplan",
                "Opus (plan) / Sonnet",
                "Opus while planning, Sonnet to execute",
                false,
            ),
        ],
        // The CLI's own spelling. `manual` is what `--help` lists and `default` is what the init
        // message reports; both mean "ask", and passing neither is what this app does by default —
        // see `ProviderEntry::default_mode` for why saying nothing is right here.
        modes: [
            "default",
            "acceptEdits",
            "plan",
            "dontAsk",
            "bypassPermissions",
        ]
        .iter()
        .map(|m| (*m).to_owned())
        .collect(),
        models_are_live: false,
        flags: [(
            "ultracode".to_owned(),
            // Not a sixth effort rung, however much the name suggests one — it is a boolean setting
            // meaning "xhigh plus standing dynamic-workflow orchestration", and the CLI refuses it
            // outright if effort resolves below xhigh. Codex's `ultra` *is* a rung. Two similar
            // names, two different things, and conflating them would mislabel behaviour on one side.
            "xhigh effort plus standing workflow orchestration. Needs effort at xhigh or above."
                .to_owned(),
        )]
        .into_iter()
        .collect(),
    }
}

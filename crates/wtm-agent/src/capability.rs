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

use wtm_core::model::{AgentCapability, AgentMode, AgentModel, EffortOption, ModeRisk};

/// The effort that means "and orchestrate workflows", which the `--effort` flag will not accept.
///
/// Exported because [`crate::claude`] has to recognise it when building argv and cannot import a
/// literal twice without the two spellings drifting.
pub const ULTRACODE: &str = "ultracode";

/// Claude Code's capabilities, as of this build.
///
/// # Why a table and not a query
///
/// There is no `model/list` here. `--model` takes an alias or a full id and the CLI validates it at
/// startup, so the only way to enumerate is to know — and knowing goes stale. The ids below are
/// aliases (`opus`, `sonnet`, `haiku`), which always resolve to the current model of their tier, and
/// the field is a free string end to end, so a model this table has never heard of still works if
/// the user types it.
///
/// # The labels carry versions and the ids deliberately do not
///
/// `Opus 5` rather than `Opus`, because "Opus" alone does not say which one you are about to spend
/// money on, and the tier names have outlived several models. That does give up some of what the
/// alias-first scheme bought: the *label* now needs a hand-bump when the next tier ships, even
/// though the id it names will keep resolving correctly. It is disclosed rather than hidden —
/// `models_are_live: false` puts "as of this build" on the control itself, which is exactly the
/// caveat this trade-off needs.
#[must_use]
pub fn claude_capability() -> AgentCapability {
    // Fixed, and the same for every model — the opposite of the other provider, where the ladder is
    // per model. The first five are `--help`'s own list: `--effort <low|medium|high|xhigh|max>`.
    //
    // `ultracode` is the sixth because the CLI itself puts it there — its interactive `/effort`
    // prints `[low|medium|high|xhigh|max|ultracode|auto]`. It is *not* more raw reasoning than
    // `max`, which is why the description says what it actually is; it is the top of the ladder
    // because that is where the tool that owns the ladder puts it. It is also the one rung
    // `--effort` will not take, so `claude::argv` translates it. See `ULTRACODE`.
    let efforts = || -> Vec<EffortOption> {
        [
            ("low", "Fast, lighter reasoning"),
            ("medium", "Balances speed and depth"),
            ("high", "Greater reasoning depth"),
            ("xhigh", "Extra depth"),
            ("max", "Maximum depth, for the hardest problems"),
            (
                ULTRACODE,
                "Max depth, plus standing multi-agent workflow orchestration",
            ),
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
            model("opus", "Opus 5", "The most capable tier", true),
            model("sonnet", "Sonnet 5", "Balanced capability and speed", false),
            model("haiku", "Haiku 4.5", "Fastest and cheapest", false),
            model("fable", "Fable 5", "The newest tier", false),
            model(
                "opusplan",
                "Opus 5 (plan) / Sonnet 5",
                "Opus while planning, Sonnet to execute",
                false,
            ),
        ],
        // Every value `--permission-mode` accepts, verified against `claude --help` on 2.1.221:
        // `(choices: "acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan")`.
        //
        // The list this replaced had `default`, which the flag rejects — the init message reports
        // `default` but the flag spells the same thing `manual` — and was missing `auto` entirely.
        // Both were wrong in the direction that fails at spawn time with the CLI's own error.
        //
        // **No mode is marked default, and that is deliberate.** `ProviderEntry::default_mode` is
        // `None` for this provider so wtm passes no `--permission-mode` at all, leaving whatever
        // the user set in `~/.claude/settings.json` intact. Marking one here would make the picker
        // send it on every session and quietly override that setting. The pane learns the real
        // answer instead: `init` reports `permissionMode`, and `SessionReady` carries it back.
        modes: vec![
            mode(
                "manual",
                "Manual",
                "Ask before every edit and command",
                ModeRisk::Normal,
                false,
            ),
            mode(
                "acceptEdits",
                "Accept edits",
                "Edit files without asking; still ask before running commands",
                ModeRisk::Elevated,
                false,
            ),
            mode(
                "plan",
                "Plan",
                "Research and propose a plan, changing nothing until it is approved",
                ModeRisk::Normal,
                false,
            ),
            mode(
                "auto",
                "Auto",
                "Decide which actions are safe and only ask about the rest",
                ModeRisk::Elevated,
                false,
            ),
            mode(
                "dontAsk",
                "Don't ask",
                "Never prompt — anything not allowed by a rule is denied outright",
                ModeRisk::Elevated,
                false,
            ),
            mode(
                "bypassPermissions",
                "Bypass permissions",
                "Skip every permission check. For sandboxes with no network access",
                ModeRisk::Unsandboxed,
                false,
            ),
        ],
        models_are_live: false,
    }
}

/// One entry of a mode table, so the six literals below read as data rather than as six structs.
fn mode(id: &str, label: &str, description: &str, risk: ModeRisk, is_default: bool) -> AgentMode {
    AgentMode {
        id: id.to_owned(),
        label: label.to_owned(),
        description: Some(description.to_owned()),
        is_default,
        risk,
    }
}

/// Codex's approval and sandbox settings, as the three presets its own TUI offers.
///
/// # Why presets and not two controls
///
/// The protocol has two independent axes — `approvalPolicy` (`untrusted` | `on-request` | `never`)
/// and `sandbox` (`read-only` | `workspace-write` | `danger-full-access`) — and wtm was sending
/// only the first, so the sandbox was whatever `~/.codex/config.toml` happened to say. Exposing
/// both as separate pickers would offer nine combinations, most of which are incoherent
/// (`never` + `read-only` is an agent that cannot act and will not ask), and would need a second
/// control in a toolbar that is already four wide on a quarter-width pane.
///
/// So: the three combinations Codex itself names, under one control, matching Claude's one control.
/// The expansion back into two fields happens in [`crate::codex`], which is where the protocol is
/// already being spoken.
///
/// Unlike the model list this is a compiled table on both providers, because `permissionProfile/list`
/// answers with the user's *named profiles* rather than with the vocabulary the protocol accepts.
#[must_use]
pub fn codex_modes() -> Vec<AgentMode> {
    vec![
        mode(
            "read-only",
            "Read only",
            "Read anything; ask before editing a file or running a command",
            ModeRisk::Normal,
            false,
        ),
        mode(
            "auto",
            "Auto",
            "Edit and run inside the worktree without asking; ask to leave it",
            ModeRisk::Elevated,
            true,
        ),
        mode(
            "full-access",
            "Full access",
            "No sandbox and no approvals. Anything, anywhere, unattended",
            ModeRisk::Unsandboxed,
            false,
        ),
    ]
}

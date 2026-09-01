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

use wtm_core::model::{AgentCapability, AgentMode, AgentModel, Effort, EffortOption, ModeRisk};

/// The effort that means "and orchestrate workflows", which the `--effort` flag will not accept.
///
/// Exported because [`crate::claude`] has to recognise it when building argv and cannot import a
/// literal twice without the two spellings drifting.
pub const ULTRACODE: &str = "ultracode";

/// Codex's own top rung, which is that provider's analogue of [`ULTRACODE`].
///
/// A constant for the same reason: [`carried_effort`] has to recognise it, and a literal spelled in
/// two files is one that drifts.
pub const CODEX_ULTRA: &str = "ultra";

/// The rung wtm starts a session on when it gets to choose.
///
/// **This is wtm's editorial answer, not either CLI's.** Both providers advertise a lower default —
/// Claude's table said `high`, Codex's `model/list` says `medium` for `gpt-5.6-sol` — and neither is
/// what someone driving several agents against a worktree wants: the whole point of the tool is
/// running work that takes a while, so the depth that pays for itself is the one above the CLI's
/// interactive default. Written down once, here, because the picker's seed and the spawn path both
/// have to agree and they reach it by different routes.
pub const PREFERRED_EFFORT: &str = "xhigh";

/// The shared rungs, weakest first — the vocabulary both providers spell the same way.
///
/// Neither provider's *top* rung is on this list, and that is the point: `ultracode` and
/// [`CODEX_ULTRA`] change what the agent *does*, not how hard it thinks, so they are not
/// comparable levels. See [`carried_effort`].
const LADDER: &[&str] = &["low", "medium", "high", "xhigh", "max"];

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

    // `PREFERRED_EFFORT` unconditionally, because every ladder above is the same six rungs and
    // `xhigh` is one of them. The per-model check that `preferred_effort` does is for the other
    // provider, where the ladder differs between models.
    let model = |id: &str, label: &str, description: &str, is_default: bool| AgentModel {
        id: id.to_owned(),
        label: label.to_owned(),
        description: Some(description.to_owned()),
        is_default,
        implied_mode: None,
        default_effort: Some(PREFERRED_EFFORT.to_owned()),
        efforts: efforts(),
    };

    AgentCapability {
        // Aliases first, deliberately: each resolves to the current model of its tier, so this list
        // stays true across releases in a way a list of dated ids would not. Opus 4.8 is the one
        // pinned id, and the exception is the point: no alias reaches it — `opus` now means the
        // current tier — so offering the previous generation at all means naming it. Hand-remove
        // when the CLI drops the id.
        models: vec![
            model("opus", "Opus 5", "The most capable tier", true),
            model("sonnet", "Sonnet 5", "Balanced capability and speed", false),
            model("haiku", "Haiku 4.5", "Fastest and cheapest", false),
            model("fable", "Fable 5.1", "The newest tier", false),
            AgentModel {
                // The one model whose meaning includes a mode: the CLI resolves `opusplan` to
                // Opus only while `permissionMode == "plan"` and to Sonnet otherwise — read off
                // 2.1.231's own model resolver. Without the implication, picking it in an
                // accept-edits pane is Sonnet for everything, and the label would be a lie.
                implied_mode: Some("plan".to_owned()),
                ..model(
                    "opusplan",
                    "Opus 5 (plan) / Sonnet 5",
                    "Opus while planning, Sonnet to execute; picking it switches the pane to Plan mode",
                    false,
                )
            },
            model(
                "claude-opus-4-8",
                "Opus 4.8",
                "The previous Opus generation",
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
                "Decide which tool permissions need approval; clarification questions are separate",
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
        // The one provider that has a high-speed mode at all. See `claude::flag_settings` for how
        // it is turned on and why the spawn-time half is not optional.
        supports_fast: true,
    }
}

/// The mode `model` only makes sense in, when it has one — in the provider's own spelling.
///
/// Compiled tables only: Codex advertises no such coupling, and asking it would cost a process.
/// The callers are the two places a mode is decided — the spawn path's resolution layering and
/// the picker's model change — which reach the same table by different routes and must agree.
#[must_use]
pub fn implied_mode(provider: &str, model: &str) -> Option<String> {
    if provider != crate::claude::ID {
        return None;
    }
    claude_capability()
        .models
        .into_iter()
        .find(|m| m.id == model)
        .and_then(|m| m.implied_mode)
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
/// The protocol has three independent axes: approval policy, approval reviewer, and sandbox. Wtm
/// once sent only the first, so the rest came from `~/.codex/config.toml` and the same picker value
/// behaved differently between machines. Exposing the axes as separate pickers would offer a pile
/// of incoherent combinations and would need more controls in a toolbar that is already four wide.
///
/// So: the three combinations Codex itself names, under one control, matching Claude's one control.
/// The expansion back into those fields happens in [`crate::codex`], where the protocol is
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
            "Edit and run in the worktree; permission requests are reviewed automatically. Clarification questions are separate",
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

/// The rungs every Codex model supports, weakest first.
///
/// A compiled floor, and the only thing [`carried_effort`] can honestly use. Asking the real
/// question would mean spawning a `codex app-server` per handoff — the query
/// [`crate::codex::parse_models`] parses costs a process — and a handoff is answered on a socket
/// thread while a model waits on the other end of it.
///
/// So this is the **intersection** of the ladders in the table at the top of this module, not the
/// union: `gpt-5.5` stops at `xhigh`, so `max` is not here. Erring permissive would mean a
/// `turn/start` that the server rejects; erring strict costs one rung of depth on the models that
/// have it. One of those is a failure and the other is a rounding error.
#[must_use]
pub fn codex_effort_floor() -> Vec<EffortOption> {
    ["low", "medium", "high", "xhigh"]
        .iter()
        .map(|effort| EffortOption {
            effort: (*effort).to_owned(),
            description: None,
        })
        .collect()
}

/// The rung wtm seeds `model` on: [`PREFERRED_EFFORT`] when that model's own ladder has it, and the
/// model's advertised default otherwise.
///
/// Per model rather than per provider, because Codex's ladders differ *within* the provider — see
/// the table at the top of this module. A model that never advertised a default and lacks the
/// preferred rung gets `None`, which is what it had before.
#[must_use]
pub fn preferred_effort(model: &AgentModel) -> Option<Effort> {
    if model.efforts.iter().any(|e| e.effort == PREFERRED_EFFORT) {
        return Some(PREFERRED_EFFORT.to_owned());
    }
    model.default_effort.clone()
}

/// Apply [`preferred_effort`] across a whole capability.
///
/// # Why this is a second pass and not part of the query
///
/// [`crate::codex::parse_models`] reports what the CLI said, under `models_are_live: true`. Baking
/// a preference into it would make the parser lie about its own source — and its test asserts the
/// advertised value precisely so that a CLI which changes its defaults is visible. So the CLI's
/// answer is parsed faithfully and *then* overridden here, where the override is the subject of the
/// function rather than a side effect of one.
pub fn prefer_effort(capability: &mut AgentCapability) {
    for model in &mut capability.models {
        model.default_effort = preferred_effort(model);
    }
}

/// The rung to start `to` on when a session on `from` hands off to it.
///
/// `None` when nothing sensible carries, which the caller reads as "use the target's own default" —
/// the only other honest answer.
///
/// # Why a top rung is demoted rather than translated
///
/// `ultracode` and [`CODEX_ULTRA`] look like counterparts and are not. Both switch on delegation:
/// `ultracode` is `max` plus standing multi-agent orchestration (see the ladder above, and
/// [`crate::claude::argv`], which translates it into a settings key rather than a depth), and
/// Codex's is that provider's equivalent. Mapping one to the other would mean an agent asked for a
/// code review quietly starting a fleet of sub-agents at a cost nobody authorised — the user turned
/// that on for **one** provider, deliberately, and a handoff is not consent to turn it on for
/// another. `max` is the highest rung that means only "think harder" on both sides.
///
/// # Why it clamps down and never up
///
/// The target's ladder is the vocabulary its protocol accepts, so a rung it does not have is a
/// spawn that fails or a frame it rejects. Stepping down to the nearest rung it does have keeps the
/// user's intent — "spend more here than the default" — without inventing a level.
#[must_use]
pub fn carried_effort(from: &str, to: &str, effort: &str) -> Option<Effort> {
    // Same provider: nothing to translate, and a Codex-to-Codex handoff must not be demoted by the
    // conservative floor below.
    if from == to {
        return Some(effort.to_owned());
    }

    let requested = if effort == ULTRACODE || effort == CODEX_ULTRA {
        "max"
    } else {
        effort
    };
    let wanted = LADDER.iter().position(|rung| *rung == requested)?;

    let accepts: Vec<Effort> = match to {
        crate::codex::ID => codex_effort_floor().into_iter().map(|e| e.effort).collect(),
        crate::cursor::ID => LADDER.iter().map(|effort| (*effort).to_owned()).collect(),
        crate::claude::ID => claude_capability()
            .models
            .first()
            .map(|m| m.efforts.iter().map(|e| e.effort.clone()).collect())
            .unwrap_or_default(),
        // An agent this build does not know. Its ladder is not ours to guess at.
        _ => return None,
    };

    // The highest rung the target accepts at or below the one asked for.
    LADDER
        .iter()
        .take(wanted + 1)
        .rev()
        .find(|rung| accepts.iter().any(|a| a == *rung))
        .map(|rung| (*rung).to_owned())
}

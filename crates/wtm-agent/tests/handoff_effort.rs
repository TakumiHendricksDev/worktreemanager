//! Carrying an effort across a handoff, where the two providers do not speak the same ladder.
//!
//! # Why this is its own file
//!
//! Every case here is *cross*-provider, so it belongs in neither provider's mapping tests. The
//! ladders are the ones documented in `capability.rs` and verified against `codex-cli 0.144.6`:
//! Claude offers a fixed `low…max` plus `ultracode`, Codex offers a per-model ladder that reaches
//! `ultra` on `gpt-5.6-sol` and stops at `xhigh` on `gpt-5.5`.
//!
//! # The two properties worth protecting
//!
//! **A rung must never be invented.** The target's ladder is the vocabulary its protocol accepts, so
//! carrying a rung it does not have is a spawn that fails or a frame it rejects — and it fails
//! *after* a pane has opened, which is the worse way to find out.
//!
//! **A top rung must never cross.** `ultracode` and `ultra` both switch on delegation rather than
//! depth. Mapping one to the other would mean an agent asked for a code review quietly starting a
//! fleet of sub-agents at a cost nobody authorised for that provider.

// `unwrap_used` is banned in the app so a failure carries a message. In an assertion it adds noise
// without adding information — a panic is the failure report either way. An integration test is its
// own crate, so the allowance has to be stated here.
#![allow(clippy::unwrap_used)]

use pretty_assertions::assert_eq;

use wtm_agent::capability::{CODEX_ULTRA, ULTRACODE, carried_effort, codex_effort_floor};
use wtm_agent::{claude, codex};

/// Every rung the target advertises, for the assertions that check membership rather than a literal.
fn ladder_of(provider: &str) -> Vec<String> {
    match provider {
        codex::ID => codex_effort_floor().into_iter().map(|e| e.effort).collect(),
        claude::ID => wtm_agent::claude_capability().models[0]
            .efforts
            .iter()
            .map(|e| e.effort.clone())
            .collect(),
        other => panic!("no ladder for `{other}`"),
    }
}

#[test]
fn a_handoff_to_the_same_provider_carries_the_effort_untouched() {
    // The case the conservative clamping below must not touch. A Codex pane handing off to Codex is
    // talking to its own protocol, so `ultra` is a rung it can actually use — demoting it because
    // some *other* Codex model lacks it would spend less than the user asked for, on a session where
    // the caller already proved the rung works.
    assert_eq!(
        carried_effort(codex::ID, codex::ID, CODEX_ULTRA).as_deref(),
        Some(CODEX_ULTRA)
    );
    assert_eq!(
        carried_effort(claude::ID, claude::ID, ULTRACODE).as_deref(),
        Some(ULTRACODE)
    );
    assert_eq!(
        carried_effort(claude::ID, claude::ID, "low").as_deref(),
        Some("low")
    );
}

#[test]
fn a_top_rung_is_carried_as_max_rather_than_the_other_providers_own_top_rung() {
    // Both directions of the trap. `ultracode` → `ultra` is the tempting mapping, because the
    // descriptions read as near-synonyms; it is wrong because both change *what the agent does*.
    // `max` is the highest rung that means only "think harder" on either side.
    assert_eq!(
        carried_effort(claude::ID, codex::ID, ULTRACODE).as_deref(),
        Some("xhigh"),
        "demoted to `max`, then clamped to what every Codex model accepts"
    );
    assert_eq!(
        carried_effort(codex::ID, claude::ID, CODEX_ULTRA).as_deref(),
        Some("max"),
        "Claude's ladder has `max`, so no clamping is needed after the demotion"
    );

    // Stated as its own assertion because it is the security-shaped half of the rule: neither
    // provider's delegation rung may appear on the other side of a handoff, whatever the path.
    for effort in [ULTRACODE, CODEX_ULTRA] {
        let to_codex = carried_effort(claude::ID, codex::ID, effort);
        let to_claude = carried_effort(codex::ID, claude::ID, effort);
        assert!(
            to_codex.as_deref() != Some(CODEX_ULTRA) && to_claude.as_deref() != Some(ULTRACODE),
            "`{effort}` must not cross providers as a delegation rung"
        );
    }
}

#[test]
fn an_effort_the_target_cannot_reach_steps_down_to_the_nearest_rung_it_can() {
    // `max` is real on Claude and absent from the Codex floor, because `gpt-5.5` stops at `xhigh`.
    // Stepping down keeps the intent — spend more here than the default — without naming a level the
    // protocol would refuse.
    assert_eq!(
        carried_effort(claude::ID, codex::ID, "max").as_deref(),
        Some("xhigh")
    );

    // A rung both sides have passes through unchanged, in both directions.
    for rung in ["low", "medium", "high", "xhigh"] {
        assert_eq!(
            carried_effort(claude::ID, codex::ID, rung).as_deref(),
            Some(rung)
        );
        assert_eq!(
            carried_effort(codex::ID, claude::ID, rung).as_deref(),
            Some(rung)
        );
    }
}

#[test]
fn an_effort_this_build_has_never_heard_of_is_refused_so_the_target_uses_its_own_default() {
    // A rung from a CLI newer than this build. Guessing at where it sits on the ladder would be
    // inventing a level; `None` falls through to the layers `session_request_for` already had, which
    // end at the compiled default. Same answer for an agent id this build does not know — that
    // provider's ladder is not ours to assume.
    assert_eq!(carried_effort(claude::ID, codex::ID, "extreme"), None);
    assert_eq!(
        carried_effort(claude::ID, "some-future-agent", "high"),
        None
    );
}

#[test]
fn every_rung_the_carry_ladder_can_return_is_one_the_target_advertises() {
    // The property that makes the rest of this file redundant if it holds, and the one that would
    // catch a future rung added to one ladder and not the other. Walks every source rung on both
    // providers against both targets.
    let sources = [
        (claude::ID, ladder_of(claude::ID)),
        (codex::ID, ladder_of(codex::ID)),
    ];

    for (from, rungs) in &sources {
        for target in [claude::ID, codex::ID] {
            let accepts = ladder_of(target);
            for rung in rungs {
                let Some(carried) = carried_effort(from, target, rung) else {
                    continue;
                };
                assert!(
                    accepts.contains(&carried),
                    "{from} → {target} carried `{rung}` as `{carried}`, which {target} does not offer"
                );
            }
        }
    }
}

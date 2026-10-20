//! Rating a permission mode, so one session's can be compared with another's across providers.
//!
//! # Why this is its own file
//!
//! `mode_latitude` is what stops a model from opening a delegated session in a mode that skips the
//! user — `src-tauri`'s `handoff::mode_refusal` refuses anything rated above the caller, and
//! anything rated at the top outright. So the ratings are a security boundary, and the ways they
//! can be wrong are each a door left open:
//!
//! - **A top mode rated lower.** A model could ask for it by name. Cursor's Auto is the one most
//!   likely to slip, because the picker rates it `Elevated` and only this function says otherwise.
//! - **An unknown spelling rated at all.** `danger-full-access` is Codex's own word for its top
//!   sandbox, and `default` is what Claude's `init` calls `manual`. Neither is in a compiled table,
//!   and a rating guessed for either is a rating nobody read the meaning of.
//! - **The order reversed.** The comparison is the derived `Ord` on `ModeRisk`, so a reordering of
//!   its variants would invert every ceiling without failing to compile.
//!
//! `mode_within` is here for the same reason. It is the mode a delegated session gets when its
//! request named none and the provider's default is more than the caller may hand out, so a wrong
//! answer from it is the same door, opened by leaving an argument out instead of by naming one.

use pretty_assertions::assert_eq;

use wtm_agent::{claude, codex, compiled_modes, cursor, mode_latitude, mode_within};
use wtm_core::model::ModeRisk;

#[test]
fn every_providers_approve_everything_mode_is_rated_at_the_top() {
    for (provider, mode) in [
        (claude::ID, "bypassPermissions"),
        (codex::ID, "full-access"),
        (cursor::ID, "auto"),
    ] {
        assert_eq!(
            mode_latitude(provider, mode),
            Some(ModeRisk::Unsandboxed),
            "{provider} `{mode}`"
        );
    }
}

#[test]
fn cursors_auto_is_rated_at_the_top_although_its_picker_colours_it_elevated() {
    // The one place the two ratings differ, on purpose: see `mode_latitude`. If the picker's table
    // is ever changed to agree, this is the test that says the correction can go.
    let picker = compiled_modes(cursor::ID)
        .into_iter()
        .find(|mode| mode.id == "auto")
        .expect("Cursor offers Auto")
        .risk;
    assert_eq!(picker, ModeRisk::Elevated);
    assert_eq!(
        mode_latitude(cursor::ID, "auto"),
        Some(ModeRisk::Unsandboxed)
    );
    assert_eq!(mode_latitude(cursor::ID, "agent"), Some(ModeRisk::Elevated));
}

#[test]
fn every_compiled_mode_is_rated_and_nothing_else_is() {
    for provider in [claude::ID, codex::ID, cursor::ID] {
        let modes = compiled_modes(provider);
        assert!(!modes.is_empty(), "{provider} has a mode table");
        for mode in modes {
            assert!(
                mode_latitude(provider, &mode.id).is_some(),
                "{provider} `{}` is in the table and must be rated",
                mode.id
            );
        }
    }
    for (provider, mode) in [
        (codex::ID, "danger-full-access"),
        (claude::ID, "default"),
        (cursor::ID, "debug"),
        (claude::ID, "BYPASSPERMISSIONS"),
        ("no-such-agent", "auto"),
    ] {
        assert_eq!(mode_latitude(provider, mode), None, "{provider} `{mode}`");
    }
}

#[test]
fn the_tiers_order_from_asking_first_to_asking_nothing() {
    assert!(ModeRisk::Normal < ModeRisk::Elevated);
    assert!(ModeRisk::Elevated < ModeRisk::Unsandboxed);
    // And so a mode that asks first is below one that acts, across providers. Unwrapped first: an
    // `Option` compares `None` below everything, which would let a mode that lost its rating pass.
    let rated = |provider: &str, mode: &str| {
        mode_latitude(provider, mode).unwrap_or_else(|| panic!("{provider} `{mode}` is rated"))
    };
    assert!(rated(claude::ID, "plan") < rated(codex::ID, "auto"));
    assert!(rated(codex::ID, "read-only") < rated(claude::ID, "acceptEdits"));
    assert!(rated(cursor::ID, "ask") < rated(cursor::ID, "agent"));
}

#[test]
fn the_mode_wtm_picks_under_a_ceiling_is_the_one_that_asks_and_never_a_plan_mode() {
    // Pinned value for value because it is a decision rather than a derivation: within a tier the
    // modes do not compare, so nothing could compute this table, only change it.
    for (provider, asks_first, acts) in [
        (claude::ID, "manual", "auto"),
        (codex::ID, "read-only", "auto"),
        (cursor::ID, "ask", "agent"),
    ] {
        assert_eq!(
            mode_within(provider, ModeRisk::Normal),
            Some(asks_first),
            "{provider}"
        );
        assert_eq!(
            mode_within(provider, ModeRisk::Elevated),
            Some(acts),
            "{provider}"
        );
        assert_eq!(
            mode_within(provider, ModeRisk::Unsandboxed),
            Some(acts),
            "{provider} is never handed the top tier by wtm"
        );
    }
    assert_eq!(mode_within("no-such-agent", ModeRisk::Elevated), None);
}

#[test]
fn the_mode_wtm_picks_under_a_ceiling_is_rated_within_it_and_never_at_the_top() {
    // The property the table above has to keep if it is ever edited: a mode that is not compiled,
    // or is rated past the ceiling, would be the cap handing out the thing it caps.
    for provider in [claude::ID, codex::ID, cursor::ID] {
        for ceiling in [ModeRisk::Normal, ModeRisk::Elevated, ModeRisk::Unsandboxed] {
            let mode = mode_within(provider, ceiling)
                .unwrap_or_else(|| panic!("{provider} has a mode under {ceiling:?}"));
            let latitude = mode_latitude(provider, mode)
                .unwrap_or_else(|| panic!("{provider} `{mode}` is a compiled, rated mode"));
            assert!(
                latitude <= ceiling.min(ModeRisk::Elevated),
                "{provider} `{mode}` is rated {latitude:?}, past {ceiling:?}"
            );
            assert_ne!(mode, "plan", "{provider} under {ceiling:?}");
        }
    }
}

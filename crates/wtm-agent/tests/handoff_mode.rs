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

use pretty_assertions::assert_eq;

use wtm_agent::{claude, codex, compiled_modes, cursor, mode_latitude};
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

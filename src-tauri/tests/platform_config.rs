//! The two window configs must not drift.
//!
//! `tauri.linux.conf.json` is merged over `tauri.conf.json` with RFC 7396 JSON Merge
//! Patch, where a non-object value **replaces** rather than merges — so an overlay that
//! touches `app.windows` has to restate the whole array, every key of it. That
//! duplication is real and this test is its price: bump `minWidth` in one file and
//! forget the other, and this goes red.
//!
//! It is also why the overlay exists at all rather than the base config being made
//! conditional. With no `tauri.macos.conf.json`, targeting macOS reads a byte-identical
//! file to the one that shipped before Linux was a target, and no merge runs. "macOS
//! cannot regress" becomes a property of the build rather than something to re-verify.
//!
//! A lint, not a promise — the same shape as `repo_hygiene.rs`.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Keys Tauri applies only on macOS, verified against the vendored `tauri-runtime-wry`:
/// `titleBarStyle` / `hiddenTitle` / `trafficLightPosition` sit inside a
/// `cfg(target_os = "macos")` block, `shadow` is Windows-and-macOS, and `windowEffects`
/// reaches a `set_window_effects` with no Linux arm at all.
///
/// Omitted from the Linux file rather than repeated with a null value: a key that does
/// nothing is a lie about what runs.
const MACOS_ONLY: &[&str] = &[
    "titleBarStyle",
    "hiddenTitle",
    "trafficLightPosition",
    "shadow",
    "windowEffects",
    "acceptFirstMouse",
];

/// Keys meaningful on both platforms whose values must differ.
///
/// Only one so far. `transparent` is *not* macOS-only — `tauri-runtime-wry` applies it
/// under `cfg(any(not(macos), macos-private-api))` — so on Linux it really does request
/// an RGBA visual, which is right under a compositor and a black window without one.
/// macOS wants it true so the native sidebar vibrancy shows through.
const DIVERGES: &[&str] = &["transparent"];

fn window(config: &Value) -> &serde_json::Map<String, Value> {
    config["app"]["windows"][0].as_object().unwrap()
}

fn read(name: &str) -> Value {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn every_shared_window_key_agrees() {
    let base = read("tauri.conf.json");
    let linux = read("tauri.linux.conf.json");
    let (base, linux) = (window(&base), window(&linux));

    for (key, value) in base {
        if MACOS_ONLY.contains(&key.as_str()) || DIVERGES.contains(&key.as_str()) {
            continue;
        }
        assert_eq!(
            linux.get(key),
            Some(value),
            "`{key}` differs between tauri.conf.json and tauri.linux.conf.json; \
             the merge replaces the whole window object, so every shared key must be \
             restated identically"
        );
    }
}

#[test]
fn the_linux_overlay_invents_no_keys() {
    // A key present only in the overlay is either a typo or a Linux-only setting that
    // deserves to be noticed in review rather than discovered later.
    let base = read("tauri.conf.json");
    let linux = read("tauri.linux.conf.json");
    let (base, linux) = (window(&base), window(&linux));

    for key in linux.keys() {
        assert!(
            base.contains_key(key),
            "`{key}` exists only in tauri.linux.conf.json"
        );
    }
}

#[test]
fn macos_only_keys_are_absent_from_the_linux_overlay() {
    let linux = read("tauri.linux.conf.json");
    let linux = window(&linux);

    for key in MACOS_ONLY {
        assert!(
            !linux.contains_key(*key),
            "`{key}` does nothing on Linux and should not be restated there"
        );
    }
}

#[test]
fn the_window_is_transparent_on_macos_and_opaque_on_linux() {
    // The one divergence, asserted in both directions so neither side can quietly drift
    // to match the other. Pairs with `--under-window` in app.css: change one and the
    // other is wrong.
    let base = read("tauri.conf.json");
    let linux = read("tauri.linux.conf.json");

    assert_eq!(window(&base).get("transparent"), Some(&Value::Bool(true)));
    assert_eq!(window(&linux).get("transparent"), Some(&Value::Bool(false)));
}

//! What this crate promises about the update check and the upgrade it hands to Homebrew.
//!
//! The destination tests are the security argument for ARCHITECTURE §6a's one automatic request:
//! the host is a constant and the request is HTTPS-only. The parsing tests matter for a different
//! reason — an update prompt that misreads a version either nags on every launch or never appears,
//! and neither shows up as an error anyone would report. The helper-script tests exist because the
//! script runs after the app has gone, so nothing in the app can notice if it breaks.

// Tests assert on known-good input, where an `unwrap` that panics *is* the failure report.
#![allow(clippy::unwrap_used)]

use wtm_update::{
    CASK, HELPER_SCRIPT, HOST, Outcome, REPO, UpdateError, Version, check_argv, helper_argv,
    log_tail, outcome, parse_latest, parse_outdated, release_page,
};

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// What curl writes: the body, then the appended status marker.
fn reply(body: &str, status: u16) -> String {
    format!("{body}\nwtm-status:{status}")
}

#[test]
fn the_only_destination_is_the_github_releases_api_over_https() {
    let argv = check_argv();
    let url = argv.last().unwrap();

    assert_eq!(HOST, "api.github.com");
    assert_eq!(
        url,
        &format!("https://{HOST}/repos/{REPO}/releases/latest"),
        "the URL must be built from the two constants and nothing else"
    );
    // `--location` is there for a renamed repository; `--proto =https` is what keeps a redirect
    // from being followed to plain HTTP. Removing the second without the first is the regression.
    assert!(
        argv.windows(2).any(|w| w == ["--proto", "=https"]),
        "{argv:?}"
    );
    assert!(!argv.iter().any(|a| a.contains("http://")), "{argv:?}");
}

#[test]
fn the_check_sends_no_user_agent_or_version_of_its_own() {
    // The request is allowed to be automatic because it says nothing. A `User-Agent: wtm/1.6.0`
    // would be the first step towards counting installs, and it is one line in a diff.
    let argv = check_argv();
    let running = env!("CARGO_PKG_VERSION");
    assert!(
        !argv
            .iter()
            .any(|a| a.to_ascii_lowercase().contains("user-agent")),
        "{argv:?}"
    );
    assert!(!argv.iter().any(|a| a.contains(running)), "{argv:?}");
}

#[test]
fn a_newer_patch_minor_or_major_is_an_update_and_an_equal_or_older_one_is_not() {
    let current = v("1.6.0");
    for newer in ["1.6.1", "1.7.0", "2.0.0", "1.10.0"] {
        assert!(v(newer) > current, "{newer} must count as newer than 1.6.0");
    }
    for not_newer in ["1.6.0", "1.5.9", "0.99.99"] {
        assert!(
            v(not_newer) <= current,
            "{not_newer} must not count as newer than 1.6.0"
        );
    }
    // The case string comparison gets wrong, and the reason the fields are numbers.
    assert!(v("1.10.0") > v("1.9.0"));
}

#[test]
fn a_tag_that_is_not_major_minor_patch_is_rejected_rather_than_guessed() {
    assert_eq!(
        v("v1.7.0"),
        v("1.7.0"),
        "the tag's `v` prefix is not part of the version"
    );
    for bad in [
        "1.7",
        "1.7.0.1",
        "1.7.0-rc1",
        "v",
        "",
        "1..0",
        "+1.7.0",
        "one.two.three",
    ] {
        assert!(Version::parse(bad).is_none(), "`{bad}` must not parse");
    }
}

#[test]
fn the_latest_release_is_read_from_its_tag() {
    let body = r#"{"tag_name": "v1.7.0", "name": "v1.7.0", "draft": false}"#;
    assert_eq!(parse_latest(&reply(body, 200)).unwrap(), v("1.7.0"));
}

#[test]
fn a_rate_limited_reply_is_reported_as_such_not_as_malformed() {
    // The remedy differs — wait an hour, versus something is broken — so the message must too.
    let body = r#"{"message": "API rate limit exceeded for 203.0.113.9."}"#;
    assert_eq!(
        parse_latest(&reply(body, 403)),
        Err(UpdateError::RateLimited)
    );
    assert_eq!(parse_latest(&reply("", 429)), Err(UpdateError::RateLimited));
}

#[test]
fn a_refusal_carries_githubs_own_explanation() {
    let body = r#"{"message": "Not Found", "documentation_url": "https://docs.github.com"}"#;
    assert_eq!(
        parse_latest(&reply(body, 404)),
        Err(UpdateError::Refused {
            status: 404,
            message: "Not Found".to_owned()
        })
    );
}

#[test]
fn no_status_at_all_means_github_was_never_reached() {
    // curl writes `000` when it never got a status line, and nothing when it never ran the
    // write-out. Both are "unreachable", and neither is the JSON parser's problem.
    let unreachable =
        |r: Result<Version, UpdateError>| matches!(r, Err(UpdateError::Unreachable { .. }));
    assert!(unreachable(parse_latest(
        "curl: (6) Could not resolve host"
    )));
    assert!(unreachable(parse_latest(&reply("", 0))));
}

#[test]
fn a_reply_without_a_usable_tag_is_malformed() {
    let malformed =
        |r: Result<Version, UpdateError>| matches!(r, Err(UpdateError::Malformed { .. }));
    assert!(malformed(parse_latest(&reply("<html>", 200))));
    assert!(malformed(parse_latest(&reply(
        r#"{"name": "v1.7.0"}"#,
        200
    ))));
    assert!(malformed(parse_latest(&reply(
        r#"{"tag_name": "nightly"}"#,
        200
    ))));
}

#[test]
fn the_release_page_is_built_rather_than_taken_from_the_reply() {
    assert_eq!(
        release_page(v("1.7.0")),
        format!("https://github.com/{REPO}/releases/tag/v1.7.0")
    );
}

#[test]
fn homebrew_reports_the_version_it_would_install_or_nothing() {
    // The shape `brew outdated --cask --json=v2` writes, captured from Homebrew 7.
    let outdated = r#"{"formulae": [], "casks": [{"name": "wtm", "installed_versions": ["1.6.0"], "current_version": "1.7.0"}]}"#;
    assert_eq!(parse_outdated(outdated).unwrap(), Some(v("1.7.0")));

    let current = r#"{"formulae": [], "casks": []}"#;
    assert_eq!(parse_outdated(current).unwrap(), None);

    assert!(parse_outdated("Error: No available cask").is_err());
}

#[test]
fn the_helper_script_takes_its_paths_as_arguments_and_never_interpolates_them() {
    // `~/Applications` is the user's to name. A bundle path containing a quote, a `$` and a space
    // must reach `open` as one argument, which is only guaranteed if it never becomes script text.
    let bundle = r#"/Users/someone/Applications/it's "$HOME" & more/Worktree Manager.app"#;
    let argv = helper_argv(4242, "/opt/homebrew/bin/brew", bundle, "/tmp/update.log");

    assert_eq!(&argv[..4], ["/bin/sh", "-c", HELPER_SCRIPT, "wtm-update"]);
    assert_eq!(
        &argv[4..],
        [
            "4242",
            "/opt/homebrew/bin/brew",
            bundle,
            "/tmp/update.log",
            CASK
        ]
    );
    assert!(!HELPER_SCRIPT.contains(bundle));
    // Every use of a positional value is quoted, so a space cannot split it either.
    for name in ["$pid", "$brew", "$app", "$log", "$cask"] {
        assert!(
            HELPER_SCRIPT.contains(&format!("\"{name}\"")),
            "{name} must be used quoted somewhere in the script"
        );
    }
}

#[test]
fn the_helper_script_is_valid_shell() {
    // It runs after the app has quit, so a syntax error would be discovered as an app that closed
    // and never came back. `sh -n` parses without executing, which is the whole of what is wanted.
    //
    // `Command::new` is banned so that every spawn in the *app* goes through `wtm-exec`. This is a
    // test asking a shell to parse a string, where none of that wrapper's guarantees apply.
    #[allow(clippy::disallowed_methods)]
    let status = std::process::Command::new("/bin/sh")
        .args(["-n", "-c", HELPER_SCRIPT])
        .status()
        .unwrap();
    assert!(status.success(), "sh -n rejected the helper script");
}

#[test]
fn the_upgrade_names_the_tapped_cask_and_skips_a_second_brew_update() {
    // The bare token would match a same-named cask in any other tap, and would upgrade that.
    assert_eq!(CASK, "takumihendricksdev/tap/wtm");
    assert!(HELPER_SCRIPT.contains(r#"upgrade --cask "$cask""#));
    assert!(HELPER_SCRIPT.contains("HOMEBREW_NO_AUTO_UPDATE=1"));
}

#[test]
fn the_outcome_is_judged_by_the_running_version_not_the_log() {
    assert_eq!(
        outcome("1.7.0", v("1.7.0")),
        Some(Outcome::Updated {
            version: v("1.7.0")
        })
    );
    // Homebrew may have found something newer still between prepare and upgrade.
    assert_eq!(
        outcome("1.7.0\n", v("1.7.1")),
        Some(Outcome::Updated {
            version: v("1.7.1")
        })
    );
    assert_eq!(
        outcome("1.7.0", v("1.6.0")),
        Some(Outcome::Failed { wanted: v("1.7.0") })
    );
    assert_eq!(outcome("garbage", v("1.6.0")), None);
}

#[test]
fn the_log_tail_keeps_the_last_lines_that_say_something() {
    let log = "one\n\ntwo\nthree\n  \nfour\n";
    assert_eq!(log_tail(log, 2), "three\nfour");
    assert_eq!(log_tail(log, 10), "one\ntwo\nthree\nfour");
}

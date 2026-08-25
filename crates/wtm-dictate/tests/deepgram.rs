//! What this crate promises about the one network call the application makes.
//!
//! Two kinds of test live here and only one of them is about correctness. The parsing tests are
//! ordinary. The tests about the *destination* and about where the credential travels exist because
//! those two properties are the entire security argument for `SECURITY.md`'s rewritten claim — they
//! are cheap to state, cheap to check, and the sort of thing a well-meaning refactor removes
//! without noticing. A reviewer who reads only this file should be able to see that the host is not
//! configurable and that the key is never an argument.

// Tests assert on known-good input, where an `unwrap` that panics *is* the failure report.
#![allow(clippy::unwrap_used)]

use wtm_core::ports::dictate::{DictateError, Utterance};
use wtm_dictate::{HOST, Keystore, parse_response, recorder_argv, request_argv, request_config};

fn utterance() -> Utterance {
    Utterance {
        keyterms: vec!["SDK".to_owned(), "ChatGPT".to_owned()],
        ..Utterance::default()
    }
}

#[test]
fn the_only_destination_is_deepgram_over_https() {
    // The property, stated once where it can fail: there is no way for configuration to move this.
    // If a future change introduces a settable endpoint, `request_config` stops being derivable
    // from a constant and this test is where that shows up.
    let config = request_config(&utterance(), "k");
    let url = config
        .lines()
        .find_map(|l| l.strip_prefix("url = "))
        .expect("a url line");

    assert!(
        url.starts_with(&format!("\"https://{HOST}/")),
        "must be https and must be the one host: {url}"
    );
    assert_eq!(HOST, "api.deepgram.com");
    // No scheme that would send the audio in the clear, whatever else changes.
    assert!(!config.contains("http://"));
}

#[test]
fn the_credential_never_appears_in_argv() {
    // `argv` is world-readable through `ps`, so this is the difference between a key that is
    // private to the user and one that is not. The payload half of the pair carries it instead.
    let secret = "sk-do-not-log-me";
    let argv = request_argv("/tmp/audio.raw");

    assert!(
        !argv.iter().any(|a| a.contains(secret)),
        "the key must not be reachable from a process listing: {argv:?}"
    );
    // ...and the mechanism that replaces it is actually asked for.
    assert!(argv.windows(2).any(|w| w == ["--config", "-"]));
    assert!(request_config(&utterance(), secret).contains(secret));
}

#[test]
fn storing_a_key_keeps_it_out_of_argv_on_both_platforms() {
    // The same property for the write path, which is the one place a user hands wtm a secret. Both
    // stores accept it on stdin; neither is allowed to take it as an argument.
    let secret = "sk-do-not-log-me";
    for store in [Keystore::Keychain, Keystore::SecretTool] {
        let (argv, stdin) = store.write(secret);
        assert!(
            !argv.iter().any(|a| a.contains(secret)),
            "{store:?} put the key in argv: {argv:?}"
        );
        assert!(stdin.contains(secret), "{store:?} did not send the key");
    }
}

#[test]
fn a_secret_tool_payload_carries_no_trailing_newline() {
    // `secret-tool store` reads to EOF, so a newline would be stored *as part of the key* and every
    // request would then fail authentication for a reason nothing could surface. Worth its own test
    // because the bug is invisible until an account rejects a key that looks correct.
    let (_, stdin) = Keystore::SecretTool.write("sk-abc\n");
    assert_eq!(stdin, "sk-abc");
}

#[test]
fn keyterms_are_percent_encoded_so_one_cannot_add_query_parameters() {
    // Keyterms are user text. Without encoding, a term containing `&` would append parameters of
    // its own — including a second `model`, which is the interesting one, since the model is what
    // provides keyterm support in the first place.
    let hostile = Utterance {
        keyterms: vec!["a&model=nope".to_owned()],
        ..Utterance::default()
    };
    let url = wtm_dictate::request_url(&hostile);

    assert!(url.contains("keyterm=a%26model%3Dnope"), "{url}");
    assert_eq!(
        url.matches("model=").count(),
        1,
        "one model parameter: {url}"
    );
}

#[test]
fn an_empty_keyterm_is_dropped_rather_than_sent() {
    // The preference is a comma-separated string, so "SDK, , ChatGPT" and a trailing comma are both
    // ordinary user input rather than mistakes worth an error.
    let spaced = Utterance {
        keyterms: vec!["SDK".to_owned(), "  ".to_owned(), String::new()],
        ..Utterance::default()
    };
    assert_eq!(
        wtm_dictate::request_url(&spaced)
            .matches("keyterm=")
            .count(),
        1
    );
}

#[test]
fn the_recorder_writes_headerless_pcm_matching_what_the_url_claims() {
    // The two have to agree and are set in different places: SoX is told by flags, the service by
    // query parameters. A mismatch does not fail — it transcribes noise, at the wrong pitch, which
    // is a far worse failure than an error.
    let u = utterance();
    let argv = recorder_argv("/tmp/a.raw", &u);
    let pair = |flag: &str| {
        argv.iter()
            .position(|a| a == flag)
            .and_then(|i| argv.get(i + 1))
            .cloned()
    };

    assert_eq!(pair("-r").as_deref(), Some("16000"));
    assert_eq!(pair("-c").as_deref(), Some("1"));
    assert_eq!(pair("-b").as_deref(), Some("16"));
    // Raw, because stopping means killing the recorder and a killed WAV writer leaves a header
    // describing a length the file does not have.
    assert_eq!(pair("-t").as_deref(), Some("raw"));

    let url = wtm_dictate::request_url(&u);
    assert!(url.contains("encoding=linear16"));
    assert!(url.contains(&format!("sample_rate={}", u.sample_rate)));
    assert!(url.contains(&format!("channels={}", u.channels)));
}

#[test]
fn a_transcript_is_read_out_of_the_services_reply() {
    let body =
        r#"{"results":{"channels":[{"alternatives":[{"transcript":"Ship the SDK change."}]}]}}"#;
    let out = parse_response(&format!("{body}\nwtm-status:200")).unwrap();
    assert_eq!(out, "Ship the SDK change.");
}

#[test]
fn silence_is_reported_rather_than_inserted_as_nothing() {
    // A microphone muted at the OS level records perfect silence and the service accepts it. The
    // composer gaining nothing with no explanation is the failure this prevents.
    let body = r#"{"results":{"channels":[{"alternatives":[{"transcript":"   "}]}]}}"#;
    assert_eq!(
        parse_response(&format!("{body}\nwtm-status:200")),
        Err(DictateError::Silent)
    );
}

#[test]
fn a_rejected_key_is_its_own_error_because_its_remedy_is() {
    // Three failures, three remedies: fix the key in Settings, top up the account, check the
    // network. Collapsing them into one message is how a user retries forever.
    assert_eq!(
        parse_response("{}\nwtm-status:401"),
        Err(DictateError::Unauthorized)
    );

    let refused = parse_response(
        r#"{"err_msg":"project has no credit"}
wtm-status:402"#,
    );
    assert_eq!(
        refused,
        Err(DictateError::Refused {
            status: 402,
            message: "project has no credit".to_owned(),
        }),
        "the service's own words, which say more than the code does"
    );
}

#[test]
fn a_real_refusal_from_the_service_parses() {
    // Captured verbatim from `api.deepgram.com` on 2026-08-24, by sending a second of silence with
    // a deliberately invalid key. Kept as a fixture because every other test in this file asserts
    // against a shape *this crate invented*, and the one thing none of them can prove is that the
    // shape matches what the service actually sends — including that `--write-out` appends the
    // marker after the body rather than before it.
    let captured = "{\"err_code\":\"INVALID_AUTH\",\"err_msg\":\"Invalid credentials.\",\
                    \"request_id\":\"01a035d4-4c58-7ba1-b4e2-19ae59bfeb47\"}\nwtm-status:401";

    assert_eq!(parse_response(captured), Err(DictateError::Unauthorized));
}

#[test]
fn output_with_no_status_marker_is_treated_as_never_having_arrived() {
    // curl failing before it can write the marker — DNS, a refused connection, a proxy that says
    // no. Diagnosing that as a bad reply would blame the service for the network.
    let err = parse_response("curl: (6) Could not resolve host").unwrap_err();
    assert!(matches!(err, DictateError::Unreachable { .. }), "{err:?}");
}

#[test]
fn a_body_that_is_not_json_is_refused_with_the_status_that_came_with_it() {
    // A proxy's HTML error page is the realistic case, and it arrives with a 200 often enough to
    // matter.
    let err = parse_response("<html>blocked by policy</html>\nwtm-status:200").unwrap_err();
    assert!(
        matches!(err, DictateError::Refused { status: 200, .. }),
        "{err:?}"
    );
}

#[test]
fn a_quote_in_a_key_cannot_break_out_of_the_config_line() {
    // curl treats a backslash as an escape inside double quotes, so an unescaped quote in a key
    // would end the value and turn the rest into option syntax. Unlikely in a real key and cheap
    // to make impossible.
    let config = request_config(&Utterance::default(), "ab\"cd\\ef");
    let header = config
        .lines()
        .find(|l| l.starts_with("header = \"Authorization"))
        .expect("an authorization header");

    assert!(
        header.ends_with('"'),
        "the value must stay closed: {header}"
    );
    assert!(header.contains("ab\\\"cd\\\\ef"), "{header}");
}

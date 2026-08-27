//! What network access is allowed to mean.
//!
//! The webview has no direct network access. Rust may reach the fixed transcription host when the
//! user dictates, or a database target that an approved config declared and the user explicitly
//! connected. The parts of that boundary that can be mechanical belong here rather than in prose.
//!
//! Three properties, each with a different way of going wrong:
//!
//! 1. **The webview still cannot reach the network.** The CSP is what enforces that, and a feature
//!    that "just needed" `connect-src` widened is exactly how it would stop being true.
//! 2. **The destination is not configurable.** A settable endpoint is an exfiltration primitive,
//!    and the difference between "sends audio to Deepgram" and "sends audio anywhere a config file
//!    says" is invisible in a diff that adds one string field.
//! 3. **Nothing gained an HTTP client.** Database protocols may need TLS; that is not permission to
//!    add a general-purpose HTTP route around the webview's CSP.

use std::path::Path;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
}

#[test]
fn the_webview_is_still_denied_every_network_destination() {
    // The load-bearing half of the security argument for dictation: the egress moved to Rust
    // precisely so this line would not have to change. If a later feature widens `connect-src`,
    // the containment argument in `capabilities/default.json` stops being true and this fails.
    let conf = std::fs::read_to_string(repo_root().join("src-tauri/tauri.conf.json"))
        .expect("tauri.conf.json");
    let conf: serde_json::Value = serde_json::from_str(&conf).expect("valid JSON");
    let csp = conf["app"]["security"]["csp"]
        .as_str()
        .expect("a content security policy");

    let connect = csp
        .split(';')
        .map(str::trim)
        .find(|d| d.starts_with("connect-src"))
        .expect("an explicit connect-src, rather than falling back to default-src");

    for allowed in connect.trim_start_matches("connect-src").split_whitespace() {
        assert!(
            matches!(allowed, "'self'" | "ipc:" | "http://ipc.localhost"),
            "the webview may only address this app's own IPC, not `{allowed}`"
        );
    }
}

#[test]
fn the_transcription_host_is_compiled_in_rather_than_configured() {
    // Read out of the source rather than asserted against the constant, because the failure being
    // prevented is not a wrong value — it is the constant being *replaced* by a field somebody can
    // set. `HOST` staying equal to itself would not catch that; a `pub` binding turning into
    // configuration does.
    let source = std::fs::read_to_string(repo_root().join("crates/wtm-dictate/src/lib.rs"))
        .expect("wtm-dictate source");

    assert!(
        source.contains(r#"pub const HOST: &str = "api.deepgram.com";"#),
        "the destination must stay a constant"
    );
    assert_eq!(wtm_dictate::HOST, "api.deepgram.com");

    // The URL is built from that constant and nothing else. `https`, spelled once, is the other
    // half: a request that fell back to `http` would put the recording on the wire in the clear.
    let config = wtm_dictate::request_config(
        &wtm_core::ports::dictate::Utterance::default(),
        "unused-key",
    );
    assert!(config.contains("https://api.deepgram.com/"), "{config}");
    assert!(!config.contains("http://"), "{config}");
}

#[test]
fn no_crate_in_this_workspace_links_an_http_client() {
    // "No HTTP client crate is reachable in the dependency graph" is a sentence `SECURITY.md` still
    // makes, and it survived dictation only because the request goes through `curl`. That is a
    // choice worth defending mechanically. `wtm-db` links native TLS for PostgreSQL, but a protocol
    // driver is not a general-purpose HTTP route and does not weaken this property.
    //
    // Manifests rather than `Cargo.lock`, deliberately — ARCHITECTURE §6a explains why the lockfile
    // is the wrong artefact to grep: it is the union of every platform, and it lists `reqwest`
    // through Tauri's own mobile unions without any target this app builds for reaching it.
    const CLIENTS: &[&str] = &[
        "reqwest",
        "hyper",
        "ureq",
        "isahc",
        "surf",
        "attohttpc",
        "curl-sys",
    ];

    let mut offenders = Vec::new();
    let crates_dir = repo_root().join("crates");
    let mut manifests = vec![repo_root().join("src-tauri/Cargo.toml")];
    for entry in std::fs::read_dir(&crates_dir).expect("crates/") {
        let path = entry.expect("a directory entry").path().join("Cargo.toml");
        if path.is_file() {
            manifests.push(path);
        }
    }

    for manifest in manifests {
        let text = std::fs::read_to_string(&manifest).expect("a manifest");
        for line in text.lines() {
            // Only declarations, so the prose above a dependency explaining why it is *not* used
            // does not trip this.
            let Some(name) = line.split('=').next().map(str::trim) else {
                continue;
            };
            if CLIENTS.contains(&name) {
                offenders.push(format!("{}: {name}", manifest.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "database protocols and dictation need no linked HTTP client; found: {offenders:?}"
    );
}

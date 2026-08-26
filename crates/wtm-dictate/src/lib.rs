//! Dictation: recording speech and turning it into text.
//!
//! # The only part of this application that talks to the network
//!
//! wtm otherwise reaches nothing but local processes and the filesystem, and `SECURITY.md` used to
//! say so without qualification. This crate is the exception, added deliberately, and the shape of
//! it is most of the argument for why the exception is affordable.
//!
//! What makes dictation usable for writing prompts about software is punctuation inferred from
//! language rather than from pause length, and getting "SDK" and "`ChatGPT`" right instead of "S D K"
//! and "chat GPT". That is a hosted-model capability; nothing on-device is close for technical
//! vocabulary, and Claude Code's own `/voice` reaches for a cloud service for exactly this reason.
//!
//! # Why `curl` and not an HTTP crate
//!
//! Four reasons, in the order they mattered:
//!
//! 1. **The licence gate.** `rustls` needs a crypto backend, and both `ring` and `aws-lc-rs` carry
//!    an OpenSSL clause that `deny.toml`'s permissive-only list rejects. Passing that check by
//!    widening the licence policy would be paying for a dictation button with the supply-chain
//!    rule, which is the wrong trade.
//! 2. **`native-tls` moves the cost to the Linux build**, which would gain a system OpenSSL build
//!    dependency — on the one platform the README admits has never been run by a human.
//! 3. **It is the house style.** ARCHITECTURE §9 rejects `git2` because the porcelain CLI *is* the
//!    compatibility contract; the same argument applies here with more force, because `curl` uses
//!    the system trust store and honours the proxy configuration the user already has, where a
//!    linked TLS stack would quietly ignore both.
//! 4. **The claim stays true as written.** "No HTTP client crate is reachable in the dependency
//!    graph" is a property somebody can re-verify with `cargo tree`, and it survives.
//!
//! The cost is real and worth naming: `curl` is now a runtime prerequisite for dictation, and its
//! absence has to be reported as a missing tool rather than a failed request.
//!
//! # What this crate cannot do
//!
//! Spawn anything. Its `Cargo.toml` has no `wtm-exec`, which is the proof — the same division
//! `wtm-agent` uses. Everything here builds an argv, a stdin payload, or parses a reply, so all of
//! it is pure and testable without a process. The composition root runs the commands.

use wtm_core::ports::dictate::{DictateError, Utterance};

/// The only host this application ever contacts.
///
/// A constant, not configuration, and that is the security property rather than an implementation
/// detail: a settable endpoint would be an exfiltration primitive wearing a feature's clothes —
/// anyone who could write a config file could redirect every recording. The user chooses what to
/// say about the audio; they do not choose where it goes.
pub const HOST: &str = "api.deepgram.com";

/// The transcription endpoint, built from [`HOST`] so the two cannot drift apart.
const ENDPOINT: &str = "https://api.deepgram.com/v1/listen";

/// The model, which is the reason this service was chosen.
///
/// Nova-3 is the one with keyterm prompting — the mechanism that gets "SDK" and "`ChatGPT`" right —
/// and it punctuates from an utterance model rather than from silence length. Pinned rather than
/// left to the service's default, so an upstream default change cannot silently remove the
/// capability the `keyterm` parameters below depend on.
const MODEL: &str = "nova-3";

/// The recorder, and the reason it writes a headerless stream.
///
/// `SoX`, invoked as `rec`, which is what Claude Code's own dictation uses and therefore what a user
/// who has set up voice input anywhere else already has.
///
/// **Raw, not WAV.** Stopping a recording means killing the recorder, and a WAV whose writer was
/// killed has a header claiming a length its body does not have — the field is filled in on clean
/// exit. Raw PCM has no header to be wrong. The sample rate and channel count travel as query
/// parameters instead, where they are equally authoritative and cannot be truncated.
#[must_use]
pub fn recorder_argv(out_path: &str, utterance: &Utterance) -> Vec<String> {
    vec![
        "rec".to_owned(),
        // Quiet: SoX writes a progress meter to stderr otherwise, which is noise in a captured
        // output and looks like a failure when the recording is cancelled.
        "-q".to_owned(),
        "-c".to_owned(),
        utterance.channels.to_string(),
        "-r".to_owned(),
        utterance.sample_rate.to_string(),
        // 16-bit signed little-endian, which is what `linear16` means on the wire.
        "-b".to_owned(),
        "16".to_owned(),
        "-e".to_owned(),
        "signed-integer".to_owned(),
        "-t".to_owned(),
        "raw".to_owned(),
        out_path.to_owned(),
    ]
}

/// Percent-encode one query-parameter value.
///
/// Hand-rolled rather than pulled in, because the alternative is a dependency for thirty lines and
/// this crate's whole point is having none. The unreserved set is RFC 3986's, and everything else
/// is escaped — which is stricter than necessary and wrong in no direction.
///
/// This is also a security boundary, not merely a correctness one: keyterms are user text, and a
/// value that could carry a raw `&` would let a keyterm append query parameters of its own.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            other => {
                use std::fmt::Write;
                // `write!` rather than `push_str(&format!(..))`: the latter allocates a `String`
                // per escaped byte, and this runs over every character of every keyterm.
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// The full request URL for one utterance.
///
/// `smart_format` is what turns "one thousand two hundred" into "1200" and adds the punctuation
/// this feature exists for; `punctuate` is implied by it and sent anyway, because relying on one
/// flag to imply another is how a service's default change becomes a bug report about wtm.
#[must_use]
pub fn request_url(utterance: &Utterance) -> String {
    let mut url = format!(
        "{ENDPOINT}?model={MODEL}&smart_format=true&punctuate=true\
         &encoding=linear16&sample_rate={rate}&channels={channels}&language={language}",
        rate = utterance.sample_rate,
        channels = utterance.channels,
        language = encode(&utterance.language),
    );
    // Repeated rather than comma-joined: a keyterm may legitimately contain a comma, and joining
    // would make one unsplittable from two.
    for term in &utterance.keyterms {
        if term.trim().is_empty() {
            continue;
        }
        url.push_str("&keyterm=");
        url.push_str(&encode(term));
    }
    url
}

/// Escape a value for a `curl` configuration file's quoted form.
///
/// curl's parser treats a backslash as an escape inside double quotes, so both it and the quote
/// character have to be doubled back. Everything else is literal.
///
/// This is not a security boundary on its own, and that is the reason [`ensure_key_safe`] exists
/// beside it: a newline inside double quotes still ends the directive in both grammars this crate
/// writes into, so quoting is not enough and the unsafe character has to be refused rather than
/// escaped.
fn escape_config(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Refuse a key that cannot be carried into a line-oriented payload without changing its meaning.
///
/// The key is written into two grammars — the `curl` config on stdin and the `security -i`
/// keychain script — and both treat a raw newline as the end of the current directive regardless of
/// the quoting around it. So a key containing `\n` is not an escaping problem, it is an injection
/// primitive: `\nurl = "http://…"` redirects every recording, and `\ndelete-generic-password …`
/// runs an extra keychain verb. A real Deepgram key is base64-ish ASCII, so rejecting every control
/// character (C0, DEL, C1 — which includes `\n` and `\r`) is wrong in no direction.
///
/// Checked against the trimmed value, because that is what both payloads embed: [`request_config`]
/// and [`Keystore::write`] pass `key.trim()`, so an interior control character is the case that
/// survives trimming and the case this guards.
///
/// # Errors
///
/// [`DictateError::InvalidKey`] when the trimmed key holds a control character.
pub fn ensure_key_safe(key: &str) -> Result<(), DictateError> {
    if key.trim().chars().any(char::is_control) {
        return Err(DictateError::InvalidKey);
    }
    Ok(())
}

/// The `curl` configuration to hand over on stdin, carrying the URL and the credential.
///
/// # Why the key is here and not in argv
///
/// `argv` is world-readable through `ps`. A configuration file would be too, if it were a file —
/// so this is written to the child's stdin and never exists anywhere: not on disk, not in a
/// process listing, not in this application's own logs, where [`wtm_core::ports::Invocation`]'s
/// hand-written `Debug` redacts it.
///
/// The URL travels with it rather than in argv purely for cohesion: the request is one object, and
/// splitting it across two mechanisms invites one of them to be updated alone.
#[must_use]
pub fn request_config(utterance: &Utterance, key: &str) -> String {
    format!(
        "url = \"{url}\"\n\
         header = \"Authorization: Token {key}\"\n\
         header = \"Content-Type: audio/raw\"\n",
        url = escape_config(&request_url(utterance)),
        key = escape_config(key.trim()),
    )
}

/// The `curl` argv. Everything sensitive travels by [`request_config`] on stdin instead.
///
/// `--fail-with-body` rather than `--fail`, because the service explains its refusals in the body
/// and discarding that would turn "your key is for a different project" into "it did not work".
/// The status code is appended to stdout by `--write-out` so [`parse_response`] can tell an empty
/// transcript from a rejected request without the exit code having to carry that distinction.
#[must_use]
pub fn request_argv(audio_path: &str) -> Vec<String> {
    vec![
        "curl".to_owned(),
        "--silent".to_owned(),
        "--show-error".to_owned(),
        "--fail-with-body".to_owned(),
        // Options and credential arrive on stdin. See `request_config`.
        "--config".to_owned(),
        "-".to_owned(),
        "--data-binary".to_owned(),
        format!("@{audio_path}"),
        "--write-out".to_owned(),
        format!("\n{STATUS_MARKER}%{{http_code}}"),
    ]
}

/// Separates the response body from the status line `--write-out` appends.
///
/// A marker rather than "the last line", because a service answering with a trailing newline would
/// otherwise make the status line the *second* to last and the parse would silently read a blank.
const STATUS_MARKER: &str = "wtm-status:";

/// Pull the transcript out of what `curl` wrote.
///
/// Splits the appended status marker off first, so an HTTP failure is diagnosed from the code
/// rather than from a body that happens not to parse.
pub fn parse_response(stdout: &str) -> Result<String, DictateError> {
    let (body, status) = match stdout.rsplit_once(STATUS_MARKER) {
        Some((body, status)) => (body.trim_end_matches('\n'), status.trim()),
        // No marker at all means curl never got far enough to write one — a DNS failure, a refused
        // connection, a missing CA. Its own message on stderr is the useful thing, and the caller
        // has it.
        None => {
            return Err(DictateError::Unreachable {
                message: stdout.trim().to_owned(),
            });
        }
    };

    let Ok(status) = status.parse::<u16>() else {
        // A marker with no number is as useless as no marker: treating it as HTTP 0
        // made a parse failure look like the service refused with a code nobody sends.
        return Err(DictateError::Unreachable {
            message: format!("curl wrote a status marker that was not a number: {status}"),
        });
    };

    match status {
        200 => {}
        401 | 403 => return Err(DictateError::Unauthorized),
        other => {
            return Err(DictateError::Refused {
                status: other,
                message: service_message(body).unwrap_or_else(|| body.trim().to_owned()),
            });
        }
    }

    let parsed: serde_json::Value =
        serde_json::from_str(body).map_err(|e| DictateError::Refused {
            status,
            message: format!("the reply was not JSON: {e}"),
        })?;

    let transcript = parsed
        .pointer("/results/channels/0/alternatives/0/transcript")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();

    if transcript.is_empty() {
        // A muted microphone records perfect silence and the service accepts it happily. Reported
        // rather than inserted, because a composer that silently gained nothing looks broken in a
        // way no message explains.
        return Err(DictateError::Silent);
    }
    Ok(transcript.to_owned())
}

/// The service's own explanation of a refusal, when it gave one.
fn service_message(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    for key in ["err_msg", "message", "reason", "error"] {
        if let Some(text) = parsed.get(key).and_then(serde_json::Value::as_str)
            && !text.trim().is_empty()
        {
            return Some(text.trim().to_owned());
        }
    }
    None
}

/// Where the credential lives, and under what name.
///
/// One account and one service string, so the entry is findable by hand — a user who wants the key
/// gone should be able to delete it with the same tool wtm used to store it, without reading this
/// source to discover what it was called.
pub const KEYCHAIN_ACCOUNT: &str = "wtm";
pub const KEYCHAIN_SERVICE: &str = "wtm-dictation";

/// Which secret store this machine has.
///
/// Chosen by which program resolves on PATH rather than by a compile-time platform branch,
/// deliberately: `src-tauri/tests/platform_seams.rs` holds those to a declared allowlist, and this
/// distinction does not need to join it — a Linux machine with `security` on PATH is not a thing,
/// and the runtime probe is what the composition root already does for agent executables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keystore {
    /// macOS. `security(1)`.
    Keychain,
    /// Linux. `secret-tool(1)` from libsecret.
    SecretTool,
}

impl Keystore {
    /// The program this store needs, for a PATH probe and for a "please install it" message.
    #[must_use]
    pub fn program(self) -> &'static str {
        match self {
            Self::Keychain => "security",
            Self::SecretTool => "secret-tool",
        }
    }

    /// argv that prints the stored key on stdout, or fails if there is none.
    #[must_use]
    pub fn read_argv(self) -> Vec<String> {
        match self {
            Self::Keychain => vec![
                "security".to_owned(),
                "find-generic-password".to_owned(),
                "-a".to_owned(),
                KEYCHAIN_ACCOUNT.to_owned(),
                "-s".to_owned(),
                KEYCHAIN_SERVICE.to_owned(),
                // Print the password itself and nothing else.
                "-w".to_owned(),
            ],
            Self::SecretTool => vec![
                "secret-tool".to_owned(),
                "lookup".to_owned(),
                "account".to_owned(),
                KEYCHAIN_ACCOUNT.to_owned(),
                "service".to_owned(),
                KEYCHAIN_SERVICE.to_owned(),
            ],
        }
    }

    /// argv and stdin that store `key`, replacing whatever was there.
    ///
    /// Both halves matter: the key travels on stdin in either case, so it never reaches `argv`.
    /// `security` grows a `-i` mode that reads its *commands* from stdin for exactly this purpose,
    /// and `secret-tool store` reads the secret from stdin by design.
    #[must_use]
    pub fn write(self, key: &str) -> (Vec<String>, String) {
        match self {
            Self::Keychain => (
                vec!["security".to_owned(), "-i".to_owned()],
                format!(
                    "add-generic-password -U -a {KEYCHAIN_ACCOUNT} -s {KEYCHAIN_SERVICE} -w \"{}\"\n",
                    escape_config(key.trim())
                ),
            ),
            Self::SecretTool => (
                vec![
                    "secret-tool".to_owned(),
                    "store".to_owned(),
                    "--label=wtm dictation".to_owned(),
                    "account".to_owned(),
                    KEYCHAIN_ACCOUNT.to_owned(),
                    "service".to_owned(),
                    KEYCHAIN_SERVICE.to_owned(),
                ],
                // No trailing newline: `secret-tool` takes everything up to EOF as the secret, so
                // a newline here would be stored as part of the key and every request would fail
                // authentication for a reason nobody could see.
                key.trim().to_owned(),
            ),
        }
    }

    /// argv that removes the stored key.
    #[must_use]
    pub fn delete_argv(self) -> Vec<String> {
        match self {
            Self::Keychain => vec![
                "security".to_owned(),
                "delete-generic-password".to_owned(),
                "-a".to_owned(),
                KEYCHAIN_ACCOUNT.to_owned(),
                "-s".to_owned(),
                KEYCHAIN_SERVICE.to_owned(),
            ],
            Self::SecretTool => vec![
                "secret-tool".to_owned(),
                "clear".to_owned(),
                "account".to_owned(),
                KEYCHAIN_ACCOUNT.to_owned(),
                "service".to_owned(),
                KEYCHAIN_SERVICE.to_owned(),
            ],
        }
    }
}

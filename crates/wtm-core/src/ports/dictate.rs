//! Turning recorded speech into text.
//!
//! # The one port whose implementation leaves the machine
//!
//! Everything else behind these traits talks to a local process or the filesystem. This one talks
//! to a transcription service over the network, and that is a deliberate, narrow exception to a
//! property this project otherwise states absolutely — see `SECURITY.md` and ARCHITECTURE §6a,
//! both of which had to be rewritten rather than quietly qualified when it was added.
//!
//! The reason it is worth the exception: what makes dictation usable for this job is punctuation
//! inferred from language rather than from pause length, and recognition of terms like "SDK" or
//! "`ChatGPT`" that a general model renders as "S D K" and "chat GPT". Claude Code's own `/voice`
//! reaches for a cloud service for exactly this, and nothing on-device is close for technical
//! vocabulary.
//!
//! # What the trait deliberately does not carry
//!
//! No URL, and no host. A configurable endpoint would be an exfiltration primitive dressed as a
//! feature — the destination is a compile-time constant in the adapter, and a test asserts it. The
//! caller chooses *what to say about the audio*, never *where it goes*.
//!
//! No API key either. The key never crosses this boundary because it never crosses any boundary it
//! does not have to: the adapter fetches it from the OS keychain at the moment of the request. That
//! is the same reasoning `EnvKeys` records for environment values — the guarantee is in the shape
//! of the type rather than in a policy anybody has to remember.

use serde::{Deserialize, Serialize};

/// Raw audio, and what the transcriber should know about it.
///
/// `linear16` mono PCM with no container, which is what the recorder produces and what the service
/// accepts directly. Headerless on purpose: stopping a recording means killing the recorder, and a
/// truncated WAV has a header that disagrees with its body where raw PCM has nothing to disagree
/// with. See `wtm_exec`'s recorder for the rest of that argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Utterance {
    pub sample_rate: u32,
    pub channels: u16,
    /// BCP-47-ish language tag, in the service's spelling. A free string for the same reason a
    /// model id is: the vocabulary belongs to somebody else and grows without asking.
    pub language: String,
    /// Terms to bias recognition toward — the mechanism that gets "SDK" and "`ChatGPT`" right.
    ///
    /// Ordered and de-duplicated by the caller. Passed through verbatim; this crate does not
    /// invent vocabulary on the user's behalf.
    pub keyterms: Vec<String>,
}

impl Default for Utterance {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            channels: 1,
            language: "en".to_owned(),
            keyterms: Vec::new(),
        }
    }
}

/// Why a transcription did not happen.
///
/// Separate from [`crate::error::ExecError`] because the remedies are different and the UI says so:
/// a missing key is a settings problem, a refused key is an account problem, and no network is
/// neither. Collapsing them into one string was tried in the design and rejected — "dictation
/// failed" is the message that makes a user retry forever.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DictateError {
    /// Nothing in the keychain. The user has not finished setting dictation up.
    #[error("no transcription key is set")]
    NoKey,

    /// The key contains a character that cannot be carried safely.
    ///
    /// Both consumers of the key — the `curl` config and the keychain `security -i` script — are
    /// line-oriented parsers where a raw newline ends the current directive whatever the quoting
    /// around it. A key holding one is refused before either payload is built rather than escaped,
    /// because two different grammars made safe by one escaping pass is a standing invitation to a
    /// bug. See `wtm_dictate::ensure_key_safe`.
    #[error("the transcription key contains an unsupported character")]
    InvalidKey,

    /// The service rejected the credential.
    #[error("the transcription service rejected the key")]
    Unauthorized,

    /// The service was reached and refused, or answered something unparseable.
    #[error("the transcription service answered {status}: {message}")]
    Refused { status: u16, message: String },

    /// The service could not be reached at all.
    #[error("could not reach the transcription service: {message}")]
    Unreachable { message: String },

    /// Reached, accepted, and there were no words in it.
    ///
    /// Not an error in the ordinary sense, and it is one anyway: a mic that is muted at the OS
    /// level records perfect silence, and a composer that quietly inserted nothing would look
    /// broken in a way no message explained.
    #[error("nothing was said")]
    Silent,
}

// There is deliberately no `Transcriber` trait here.
//
// One was written and deleted. Every other file in this directory defines a port because the domain
// calls through it — a use-case needs to run a command or read a file, and the trait is what keeps
// `wtm-core` compiling for `wasm32`. Dictation is not like that: no use-case transcribes anything.
// The composition root records, sends and hands the text to the frontend, so a trait here would
// have had exactly one implementation, no domain caller, and no test that wanted to fake it.
//
// What earns its place is the vocabulary above, which both sides genuinely share. ARCHITECTURE §8a
// notes that dead code accumulates in this project and nothing warns about it; an unimplemented
// trait is the version of that which also looks like an architecture.

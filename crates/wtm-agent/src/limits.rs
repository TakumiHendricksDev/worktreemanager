//! Recognising "you are out of tokens" in a provider's own words.
//!
//! # Why this is string matching, which is not a thing this codebase likes
//!
//! Because there is nothing better to match on, and the alternative was matching nothing.
//!
//! Neither CLI has a documented machine-readable "limit exhausted" code. Both *do* emit structured
//! events near a limit — Claude a `rate_limit_event`, Codex an `account/rateLimits/updated` — and
//! both are mapped in the provider modules, but the only payloads anyone has captured are the
//! benign ones (`status: "allowed"`, `params: {}`), so what those look like when a limit actually
//! bites is a guess until somebody hits one and records it. What *is* known to happen is that the
//! turn fails with a sentence naming the limit, because that is the path every report of the
//! problem came in through.
//!
//! So: match the structured events where their shape can be read, and classify the failure text as
//! the reliable route. Two detectors for one condition, which is one more than ideal and one fewer
//! than the number of ways the providers signal it.
//!
//! # The cost of getting it wrong, in both directions
//!
//! A false negative is the status quo: the turn shows as failed, with the provider's own
//! explanation, and nothing offers to continue elsewhere. A false positive offers to move a
//! conversation to the other agent when it did not need moving — and since accepting is an
//! explicit click that opens a *new* pane and leaves the old one untouched, that is a wasted
//! click rather than lost work.
//!
//! That asymmetry is why the phrase list below is narrow rather than generous. Bare `"limit"` is
//! deliberately absent: it appears in ordinary tool output, in `ulimit` errors, and in the word
//! "unlimited".

/// What a provider's error text says about a limit, when it says anything.
pub(crate) struct LimitSignal {
    /// Unix seconds at which the provider says the limit lifts.
    pub resets_at: Option<u64>,
}

/// Phrases that mean a usage or rate limit, case-insensitively.
///
/// Each one is a whole phrase rather than a word, so the match needs the provider to have said
/// something specific. The `429` entries name what precedes the number rather than matching it
/// bare, which was a real false positive caught by this module's own test: `" 429"` matches
/// "compiled 429 modules successfully", and a build log line is exactly the kind of text that ends
/// up in a failure message. An HTTP 429 always arrives with one of these words next to it, and the
/// bodies that do not say the number say "too many requests" instead.
const PHRASES: &[&str] = &[
    "usage limit",
    "rate limit",
    "rate_limit",
    "out of tokens",
    "quota exceeded",
    "exceeded your quota",
    "too many requests",
    "insufficient_quota",
    "status 429",
    "status: 429",
    "http 429",
    "error 429",
    "code 429",
    "429 too many",
];

/// Recognise a limit in `message`, and the moment it lifts if that is stated.
pub(crate) fn classify(message: &str) -> Option<LimitSignal> {
    let haystack = message.to_lowercase();
    if !PHRASES.iter().any(|phrase| haystack.contains(phrase)) {
        return None;
    }
    Some(LimitSignal {
        resets_at: reset_epoch(message),
    })
}

/// The reset time out of Claude's `…usage limit reached|<unix seconds>` suffix.
///
/// An undocumented shape, and the only place either CLI states a reset time in a failure message.
/// Read defensively: a pipe with something else after it yields `None` rather than a wrong clock
/// time, because a banner promising 4pm when the truth is midnight is worse than one promising
/// nothing.
///
/// Bounded below by a plausible floor so a small trailing number — an error code, a count — cannot
/// be read as a timestamp. Anything before 2020 is not a reset time for a product that did not
/// exist then.
fn reset_epoch(message: &str) -> Option<u64> {
    const YEAR_2020: u64 = 1_577_836_800;

    let tail = message.rsplit('|').next()?;
    let seconds: u64 = tail.trim().parse().ok()?;
    (seconds > YEAR_2020).then_some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_naming_a_usage_limit_is_classified_as_a_limit() {
        assert!(classify("Claude AI usage limit reached").is_some());
        assert!(classify("You have hit your RATE LIMIT for this model").is_some());
        assert!(classify("stream error: request failed with status 429").is_some());
        assert!(classify("This session is out of tokens").is_some());
    }

    #[test]
    fn an_ordinary_failure_message_is_not_classified_as_a_limit() {
        // Each of these is a real-shaped failure that must keep presenting as a plain failure:
        // offering to move the conversation to another agent would not help any of them.
        assert!(classify("Failed to authenticate: OAuth session expired").is_none());
        assert!(classify("the turn failed and the CLI gave no reason").is_none());
        assert!(classify("Error: ENOENT: no such file or directory").is_none());
        // The trap the phrase list is narrow for. "limit" alone is a common English word and a
        // common shell error.
        assert!(classify("ulimit -n is too low for this build").is_none());
        assert!(classify("your plan has unlimited requests").is_none());
        assert!(classify("compiled 429 modules successfully").is_none());
    }

    #[test]
    fn claudes_pipe_separated_reset_epoch_is_extracted_from_the_message() {
        let signal = classify("Claude AI usage limit reached|1755590400").expect("a limit");
        assert_eq!(signal.resets_at, Some(1_755_590_400));
    }

    #[test]
    fn a_reset_suffix_that_is_not_a_plausible_timestamp_is_ignored_rather_than_shown() {
        // Better no clock time than a wrong one: this is rendered to the user as "resets around …".
        let small = classify("usage limit reached|7").expect("a limit");
        assert_eq!(small.resets_at, None);

        let words = classify("usage limit reached|try again later").expect("a limit");
        assert_eq!(words.resets_at, None);

        let none = classify("usage limit reached").expect("a limit");
        assert_eq!(none.resets_at, None);
    }
}

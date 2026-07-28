//! The filter vocabulary a `wtm.toml` may use.
//!
//! # Deliberately small
//!
//! Templates here are *configuration*, read from a file inside a repository, so the
//! engine is a sandbox: no includes, no arbitrary calls, no network. This list plus
//! minijinja's own string builtins is the whole vocabulary, and it exists because
//! each entry replaces a specific piece of shell that a project would otherwise have
//! to keep.
//!
//! # `slugify` is not implemented here
//!
//! It delegates to [`wtm_core::usecase::slugify`], which is bit-compatible with the
//! shell pipeline it replaces. Reimplementing it as a filter would mean two
//! definitions of what a branch is called — and they would drift on exactly the
//! inputs nobody tests (non-ASCII summaries, runs of punctuation).

use minijinja::{Error, ErrorKind, Value};
use regex::Regex;

use wtm_core::usecase::slugify;

/// Lowercase, collapse non-alphanumeric runs to `-`, trim hyphens.
///
/// See [`wtm_core::usecase::slug`] for the exact shell equivalence.
pub fn filter_slugify(value: &str) -> String {
    slugify(value)
}

/// Cut to at most `length` characters, appending `suffix` when truncation happened.
///
/// Character-based, not byte-based: cutting a multi-byte character in half would
/// produce invalid UTF-8 and a branch name git rejects. Note that `slugify` output
/// is pure ASCII, so this only matters when truncating raw input.
pub fn filter_truncate(value: &str, length: usize, suffix: Option<String>) -> String {
    let suffix = suffix.unwrap_or_default();
    if value.chars().count() <= length {
        return value.to_owned();
    }
    // Reserve room for the suffix, saturating so `truncate(2, '...')` cannot panic.
    let keep = length.saturating_sub(suffix.chars().count());
    let mut out: String = value.chars().take(keep).collect();
    out.push_str(&suffix);
    out
}

/// `value` unless it is empty, in which case `fallback`.
///
/// Distinct from jinja's `default`, which only substitutes for *undefined*. A form
/// field that exists but was left blank is defined and empty, and "blank means fall
/// back to the tracker's summary" is the single most common rule a config expresses.
pub fn filter_default_if_empty(value: &str, fallback: Option<&Value>) -> String {
    if value.is_empty() {
        fallback.map(value_to_string).unwrap_or_default()
    } else {
        value.to_owned()
    }
}

/// Regex replace, with `$1`-style capture references.
///
/// Replaces the `sed`/`tr` half of a shell pipeline. Every use is a template in a
/// config file, so a bad pattern must be a clear error rather than a panic — hence
/// the explicit compile step and message.
pub fn filter_re_replace(value: &str, pattern: &str, replacement: &str) -> Result<String, Error> {
    let regex = Regex::new(pattern).map_err(|e| {
        Error::new(
            ErrorKind::InvalidOperation,
            format!("invalid regex `{pattern}`: {e}"),
        )
    })?;
    Ok(regex.replace_all(value, replacement).into_owned())
}

/// Whether `value` matches `pattern`. Useful in `when` expressions.
pub fn filter_matches(value: &str, pattern: &str) -> Result<bool, Error> {
    let regex = Regex::new(pattern).map_err(|e| {
        Error::new(
            ErrorKind::InvalidOperation,
            format!("invalid regex `{pattern}`: {e}"),
        )
    })?;
    Ok(regex.is_match(value))
}

/// Drop `prefix` if present; otherwise return `value` unchanged.
pub fn filter_strip_prefix(value: &str, prefix: &str) -> String {
    value.strip_prefix(prefix).unwrap_or(value).to_owned()
}

/// Drop `suffix` if present; otherwise return `value` unchanged.
pub fn filter_strip_suffix(value: &str, suffix: &str) -> String {
    value.strip_suffix(suffix).unwrap_or(value).to_owned()
}

/// Everything after the first `separator`, or the whole string if absent.
///
/// This is the shell's `${var#*sep}`. With `/` it turns a branch name into the
/// directory name a project wants — `task/ACME-1-x` becomes `ACME-1-x`.
pub fn filter_after(value: &str, separator: &str) -> String {
    value
        .split_once(separator)
        .map_or_else(|| value.to_owned(), |(_, rest)| rest.to_owned())
}

/// Everything before the first `separator`, or the whole string if absent.
pub fn filter_before(value: &str, separator: &str) -> String {
    value
        .split_once(separator)
        .map_or_else(|| value.to_owned(), |(head, _)| head.to_owned())
}

/// Render a minijinja value as the string a template would produce.
///
/// `Value::to_string` on `Undefined`/`None` yields nothing useful for our purposes,
/// so those collapse to the empty string — consistent with how the rest of the
/// engine treats absence.
fn value_to_string(value: &Value) -> String {
    if value.is_undefined() || value.is_none() {
        String::new()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_values_untouched() {
        assert_eq!(filter_truncate("short", 40, None), "short");
        assert_eq!(filter_truncate("exact", 5, None), "exact");
    }

    #[test]
    fn truncate_with_an_empty_suffix_just_cuts() {
        // `truncate(40, '')` is how a config caps a directory name.
        assert_eq!(
            filter_truncate("abcdefghij", 4, Some(String::new())),
            "abcd"
        );
    }

    #[test]
    fn truncate_reserves_room_for_its_suffix() {
        assert_eq!(
            filter_truncate("abcdefghij", 5, Some("…".to_owned())),
            "abcd…"
        );
        assert_eq!(
            filter_truncate("abcdefghij", 6, Some("...".to_owned())),
            "abc..."
        );
    }

    #[test]
    fn truncate_does_not_panic_when_the_suffix_is_longer_than_the_limit() {
        assert_eq!(
            filter_truncate("abcdefghij", 2, Some("......".to_owned())),
            "......"
        );
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // Cutting mid-character would yield invalid UTF-8 and an unusable name.
        let value = "ααααα"; // 5 chars, 10 bytes
        assert_eq!(filter_truncate(value, 3, Some(String::new())), "ααα");
    }

    #[test]
    fn default_if_empty_substitutes_for_blank_not_just_missing() {
        // The distinction from jinja's `default`, and the reason this exists: a form
        // field left blank is defined and empty.
        assert_eq!(
            filter_default_if_empty("", Some(&Value::from("from-jira"))),
            "from-jira"
        );
        assert_eq!(
            filter_default_if_empty("typed", Some(&Value::from("from-jira"))),
            "typed"
        );
        assert_eq!(filter_default_if_empty("", None), "");
    }

    #[test]
    fn default_if_empty_treats_an_undefined_fallback_as_empty() {
        assert_eq!(filter_default_if_empty("", Some(&Value::UNDEFINED)), "");
    }

    #[test]
    fn re_replace_supports_capture_references() {
        // The bare-number auto-prefix rule, expressed as config.
        assert_eq!(
            filter_re_replace("1234", "^([0-9]+)$", "ACME-$1").unwrap(),
            "ACME-1234"
        );
        // Already prefixed: the pattern does not match, so nothing changes.
        assert_eq!(
            filter_re_replace("ACME-1234", "^([0-9]+)$", "ACME-$1").unwrap(),
            "ACME-1234"
        );
    }

    #[test]
    fn a_bad_regex_is_an_error_with_the_pattern_in_the_message() {
        let err = filter_re_replace("x", "([unclosed", "y").unwrap_err();
        assert!(err.to_string().contains("([unclosed"), "got {err}");
    }

    #[test]
    fn matches_answers_a_when_expression() {
        assert!(filter_matches("ACME-1234", "^[A-Z]+-[0-9]+$").unwrap());
        assert!(!filter_matches("nope", "^[A-Z]+-[0-9]+$").unwrap());
    }

    #[test]
    fn strip_prefix_and_suffix_are_no_ops_when_absent() {
        assert_eq!(filter_strip_prefix("HOST_PORT_WEB", "HOST_PORT_"), "WEB");
        assert_eq!(filter_strip_prefix("WEB", "HOST_PORT_"), "WEB");
        assert_eq!(filter_strip_suffix("a.toml", ".toml"), "a");
        assert_eq!(filter_strip_suffix("a", ".toml"), "a");
    }

    #[test]
    fn after_strips_only_up_to_the_first_separator() {
        // Matches the shell's ${var#*sep}: `a/b/c` keeps the inner slash.
        assert_eq!(filter_after("task/ACME-1234-slug", "/"), "ACME-1234-slug");
        assert_eq!(filter_after("a/b/c", "/"), "b/c");
        assert_eq!(filter_after("noslash", "/"), "noslash");
    }

    #[test]
    fn before_takes_the_first_segment() {
        assert_eq!(filter_before("task/ACME-1-x", "/"), "task");
        assert_eq!(filter_before("noslash", "/"), "noslash");
    }

    #[test]
    fn slugify_delegates_rather_than_reimplementing() {
        // Guards against a second definition of what a branch is called.
        assert_eq!(
            filter_slugify("Stretch: Action Plans and Action Plan Reports updates"),
            slugify("Stretch: Action Plans and Action Plan Reports updates")
        );
        assert_eq!(filter_slugify("Café"), "caf");
    }
}

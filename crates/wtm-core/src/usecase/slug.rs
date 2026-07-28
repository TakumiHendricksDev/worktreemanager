//! Slug generation.
//!
//! # Why this is domain code and not a template filter
//!
//! It lives here, in the crate with no dependencies, because it is the one piece of
//! naming logic that must be *bit-compatible with an existing shell function*. The
//! `slugify` template filter in `wtm-render` calls straight into this — one
//! implementation, one set of tests, no chance of the filter and the domain
//! disagreeing about what a branch is called.
//!
//! # The behaviour being reproduced
//!
//! ```sh
//! slugify() {
//!     echo "$1" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9' '-' | sed 's/^-*//;s/-*$//'
//! }
//! ```
//!
//! Read carefully, that pipeline says four things:
//!
//! 1. **ASCII-only case folding.** `tr '[:upper:]' '[:lower:]'` in the C locale maps
//!    `A-Z` and nothing else. Non-ASCII bytes pass through untouched.
//! 2. **Byte-wise, not character-wise.** `tr -c 'a-z0-9'` replaces every byte
//!    outside that set — so a multi-byte character becomes *several* replacements
//!    before squeezing, not one.
//! 3. **`-s` squeezes.** A run of replaced bytes collapses to a single `-`.
//! 4. **`echo` adds a newline**, which is itself outside the set, so every input
//!    ends with a replaced byte — invisible only because `sed` then strips trailing
//!    hyphens.
//!
//! The consequence worth knowing: **an all-non-ASCII summary slugifies to the empty
//! string.** That is faithful to the original, and it is why a project should set
//! `naming.branch_must_match` — an empty slug otherwise yields a branch like
//! `experiment/ACME-0000-`, which is not an error anywhere until it is a very
//! confusing one.

/// Lowercase, collapse every run of non-alphanumeric bytes to a single `-`, and
/// trim hyphens from both ends.
///
/// See the module documentation for the exact shell equivalence.
#[must_use]
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    // Set when we've seen non-alphanumeric input. Deferring the hyphen until the
    // next alphanumeric byte is what implements squeeze, leading-trim and
    // trailing-trim all at once: a pending hyphen at the end is simply never
    // written.
    let mut pending_separator = false;

    for byte in input.bytes() {
        let byte = byte.to_ascii_lowercase();
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            if pending_separator && !out.is_empty() {
                out.push('-');
            }
            pending_separator = false;
            out.push(char::from(byte));
        } else {
            pending_separator = true;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tracker summary with a leading label and a colon — the shape that first proved
    /// this must match `tr -cs 'a-z0-9' '-'` byte for byte rather than approximately: the
    /// colon and the following space collapse to a *single* dash, and a name that differs
    /// from the shell's by one dash is a different directory.
    #[test]
    fn reproduces_the_reference_shell_directory_name() {
        assert_eq!(
            slugify("Stretch: Action Plans and Action Plan Reports updates"),
            "stretch-action-plans-and-action-plan-reports-updates"
        );
    }

    /// A spread of real-world summary shapes, as a regression net. Each was chosen for a
    /// distinct hazard: acronyms, mixed case, punctuation, and trailing noise.
    #[test]
    fn reproduces_other_live_worktree_names() {
        for (summary, expected) in [
            (
                "Add system notification preferences",
                "add-system-notification-preferences",
            ),
            (
                "Improve logging in management commands",
                "improve-logging-in-management-commands",
            ),
            (
                "Migrate API keys org settings to the SPA pattern",
                "migrate-api-keys-org-settings-to-the-spa-pattern",
            ),
            (
                "Multiple addons targeting same field bug",
                "multiple-addons-targeting-same-field-bug",
            ),
            ("Fix upload bypass issues", "fix-upload-bypass-issues"),
            (
                "Add external ID field to user summary",
                "add-external-id-field-to-user-summary",
            ),
            ("Warm paper light mode", "warm-paper-light-mode"),
        ] {
            assert_eq!(slugify(summary), expected, "summary: {summary}");
        }
    }

    #[test]
    fn runs_of_separators_squeeze_to_one() {
        // `tr -s`. "Fix:  the   bug!!" has runs of length 3, 3 and 2.
        assert_eq!(slugify("Fix:  the   bug!!"), "fix-the-bug");
        assert_eq!(slugify("a___b---c...d"), "a-b-c-d");
    }

    #[test]
    fn hyphens_are_trimmed_from_both_ends() {
        // `sed 's/^-*//;s/-*$//'`
        assert_eq!(slugify("---leading"), "leading");
        assert_eq!(slugify("trailing---"), "trailing");
        assert_eq!(slugify("  spaced  "), "spaced");
        assert_eq!(slugify("[bracketed]"), "bracketed");
    }

    #[test]
    fn digits_survive_and_case_is_folded() {
        assert_eq!(slugify("ACME-1234"), "acme-1234");
        assert_eq!(slugify("V2 Rollout"), "v2-rollout");
    }

    #[test]
    fn existing_slugs_are_idempotent() {
        // Matters because a config may slugify a value that is already a slug.
        let slug = "extend-report-templates-and-exports";
        assert_eq!(slugify(slug), slug);
        assert_eq!(slugify(&slugify(slug)), slug);
    }

    #[test]
    fn empty_and_separator_only_input_yield_empty() {
        // Faithful to the shell, and the reason branch_must_match exists.
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("   "), "");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("!@#$%^&*()"), "");
    }

    #[test]
    fn non_ascii_is_replaced_bytewise_not_transliterated() {
        // `tr` is byte-wise: "é" is two bytes, both outside [a-z0-9], squeezed to
        // one hyphen — then trimmed here because it is trailing.
        assert_eq!(slugify("Café"), "caf");
        assert_eq!(slugify("Café Latte"), "caf-latte");
        // An entirely non-ASCII summary slugifies away completely. This is the
        // documented sharp edge, asserted so nobody "fixes" it by accident.
        assert_eq!(slugify("日本語"), "");
        assert_eq!(slugify("naïve approach"), "na-ve-approach");
    }

    #[test]
    fn newlines_and_tabs_are_separators() {
        // `echo` appends a newline, so trailing-newline handling has to be right.
        assert_eq!(slugify("line one\nline two"), "line-one-line-two");
        assert_eq!(slugify("trailing\n"), "trailing");
        assert_eq!(slugify("tab\tsep"), "tab-sep");
    }

    #[test]
    fn never_produces_a_leading_or_trailing_hyphen_or_a_double_hyphen() {
        // Property check over the awkward characters, since a malformed slug is
        // what turns into a malformed branch name.
        for input in [
            "",
            " ",
            "-",
            "a",
            "-a-",
            "a  b",
            "!!a!!b!!",
            "Café",
            "日本語",
            "ACME-0000",
            "a\n\nb",
            "\u{1F600} emoji",
        ] {
            let slug = slugify(input);
            assert!(
                !slug.starts_with('-'),
                "leading hyphen from {input:?}: {slug:?}"
            );
            assert!(
                !slug.ends_with('-'),
                "trailing hyphen from {input:?}: {slug:?}"
            );
            assert!(
                !slug.contains("--"),
                "double hyphen from {input:?}: {slug:?}"
            );
            assert!(
                slug.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "unexpected byte from {input:?}: {slug:?}"
            );
        }
    }
}

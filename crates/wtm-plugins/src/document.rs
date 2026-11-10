//! One rule for an HTML document the host shows in its fixed, network-less frame.
//!
//! Prototype reviews and plugin panels both render through that frame, and both must work with
//! nothing beside them: a reviewed design cannot freeze a live stylesheet, and a reviewed package
//! cannot fetch code the review never saw. The rule lives here, with no message of its own,
//! because the two callers owe their users different explanations — a mockup is asked to be
//! regenerated, a package to be fixed by its author.
use regex::Regex;
use std::sync::LazyLock;

static REFERENCES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:src|href|srcset)\s*=\s*["']([^"']*)["']"#)
        .expect("the attribute reference pattern compiles")
});
static CSS_URLS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)url\(([^)]*)\)").expect("the stylesheet URL pattern compiles")
});

/// Markup that would load or connect to something outside the document.
const LOADERS: &[&str] = &[
    "@import",
    "<iframe",
    "<object",
    "<embed",
    "<base",
    "fetch(",
    "xmlhttprequest",
    "websocket",
    "import(",
];

/// Whether `html` references only itself: fragment links and `data:` URLs.
pub fn self_contained(html: &str) -> bool {
    let external_attribute = REFERENCES.captures_iter(html).any(|capture| {
        let value = capture[1].trim();
        !value.is_empty() && !value.starts_with('#') && !value.starts_with("data:")
    });
    let external_css = CSS_URLS.captures_iter(html).any(|capture| {
        let value = capture[1].trim().trim_matches(['\'', '"']);
        !value.starts_with('#') && !value.starts_with("data:")
    });
    let lower = html.to_ascii_lowercase();
    !external_attribute && !external_css && !LOADERS.iter().any(|p| lower.contains(p))
}

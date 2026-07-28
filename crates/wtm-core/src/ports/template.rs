//! Template rendering.
//!
//! # Validation is separate from rendering, on purpose
//!
//! [`TemplateEngine::validate`] exists so a bad template is caught when config
//! loads — with a file, a line and a reason — rather than at create time. The
//! failure mode being prevented is specific and nasty: a template referencing a
//! token that cannot exist at that position renders to an empty string, and you
//! end up with a branch named `experiment/ACME-0000-`. That is not a crash, so
//! nothing catches it except a check.
//!
//! # The filter set is fixed
//!
//! Templates are configuration, and configuration comes from a file inside a
//! repository. The engine must therefore be sandboxed: no file includes, no
//! arbitrary function calls, no network. The filters below are the whole
//! vocabulary.

use std::collections::BTreeMap;

use crate::error::RenderError;
use crate::model::TokenScope;

/// The complete list of filters a config may use.
///
/// Documented here rather than only in the adapter because it is part of the
/// config contract: this list is what a `wtm.toml` author is allowed to rely on.
pub const FILTERS: &[&str] = &[
    // `slugify` must match the shell pipeline it replaces:
    // `tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9' '-' | sed 's/^-*//;s/-*$//'`
    "slugify",
    "lower",
    "upper",
    "trim",
    // `truncate(n, suffix)`
    "truncate",
    // `default_if_empty(other)` — treats "" as absent, unlike jinja's `default`,
    // which only substitutes for undefined.
    "default_if_empty",
    "default",
    // `re_replace(pattern, replacement)`
    "re_replace",
    "replace",
    "strip_prefix",
];

/// Values available to a template, already flattened into dotted keys
/// (`lookup.jira.summary`, `computed.slug`, `repo.root`).
///
/// Flattened rather than nested because the token-scope check works on prefixes,
/// and because a flat map is trivial to snapshot in a test.
pub type Context = BTreeMap<String, String>;

pub trait TemplateEngine: Send + Sync {
    /// Render `template`. `key` is used only for error messages.
    fn render(&self, key: &str, template: &str, ctx: &Context) -> Result<String, RenderError>;

    /// Evaluate `template` as a boolean, for `when` / `required_when`.
    ///
    /// Emptiness is falsy, so `when = "load_dump"` works without comparing against
    /// the string `"true"`.
    fn eval_bool(&self, key: &str, template: &str, ctx: &Context) -> Result<bool, RenderError>;

    /// Parse `template` and return every token it references, as dotted paths.
    ///
    /// The basis of the scope check: `wtm-config` compares these against the
    /// position's [`TokenScope`].
    fn referenced_tokens(&self, key: &str, template: &str) -> Result<Vec<String>, RenderError>;

    /// Check syntax and that every referenced token is legal in `scope`.
    fn validate(&self, key: &str, template: &str, scope: &TokenScope) -> Result<(), RenderError>;

    /// Apply a named filter from [`FILTERS`] to a value.
    ///
    /// Exposed as its own operation because lookup mappings apply filters to a
    /// JSON-extracted value without a surrounding template — this is what replaces
    /// the `| ascii_downcase` half of a `jq` pipeline.
    fn apply_filter(&self, filter: &str, value: &str) -> Result<String, RenderError>;
}

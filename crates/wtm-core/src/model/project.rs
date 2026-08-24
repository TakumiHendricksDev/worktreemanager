//! What a project's config declares.
//!
//! # Why these types derive `Deserialize` directly
//!
//! These are the domain types *and* the `wtm.toml` schema. There is no parallel
//! set of "raw config" structs that get mapped across, because the reason to
//! change is identical for both: a new config capability. A second hierarchy would
//! be pure duplication kept in sync by hand.
//!
//! `wtm-config` still owns the parts that are genuinely its own concern — merging
//! the four layers as `toml::Value`, reporting spans, and the trust store — and
//! deserializes into these types exactly once, at the end.
//!
//! # The DRY spine: [`CommandSpec`]
//!
//! A project config runs commands in five different places: to populate a select's
//! options, to look up issue metadata, to set a new worktree up, to tear one down,
//! and as an ad-hoc action. Those differ only in *when* they run and what is done
//! with the output — never in how a command is described. So they all embed one
//! [`CommandSpec`], which means a timeout, a `cwd`, an environment override or a
//! `when` guard added there is available everywhere at once.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Highest `schema_version` this build understands.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Stable identity for a registered project. Derived from its absolute root path.
///
/// `Default` exists only to satisfy `#[serde(skip_deserializing)]` on
/// [`Project::id`] — the id is never read from the file, it is assigned by
/// `wtm-config` from the resolved repo root.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(String);

impl ProjectId {
    #[must_use]
    pub fn from_root(root: &std::path::Path) -> Self {
        Self(root.to_string_lossy().into_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A fully-resolved project: the merge of all four config layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// Set by `wtm-config` from the repo root, not from the file.
    #[serde(skip_deserializing)]
    pub id: ProjectId,
    /// Absolute repo root. Set by `wtm-config`.
    #[serde(skip_deserializing)]
    pub root: PathBuf,

    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// The `[project]` table. Named `meta` in Rust because `project.project`
    /// reads badly; the TOML key is unchanged.
    #[serde(default, rename = "project")]
    pub meta: ProjectMeta,

    /// The New Worktree form, in display order.
    #[serde(default, rename = "field")]
    pub fields: Vec<FieldSpec>,

    #[serde(default, rename = "lookup")]
    pub lookups: Vec<LookupSpec>,

    /// Derived values, evaluated in declaration order. Each is visible to the next
    /// as `computed.<key>`, which is how a slug gets defined once and reused by
    /// both the branch and the directory template.
    #[serde(default, rename = "computed")]
    pub computed: Vec<ComputedSpec>,

    #[serde(default)]
    pub naming: NamingSpec,

    #[serde(default)]
    pub create: CreateSpec,

    /// Runs after `git worktree add`. Absent means "no setup needed", which is the
    /// correct behaviour for a repo with no tooling.
    #[serde(default)]
    pub setup: Option<SetupSpec>,

    #[serde(default)]
    pub remove: RemoveSpec,

    #[serde(default)]
    pub display: DisplaySpec,

    #[serde(default, rename = "action")]
    pub actions: Vec<ActionSpec>,

    /// Agent sessions this repository offers, keyed by catalogue id. See [`AgentSpec`].
    ///
    /// A map rather than an array of tables, deliberately unlike every other list in this struct —
    /// `AgentSpec`'s docs give the reason: arrays replace across config layers and tables merge, and
    /// agents are independent of each other in a way form fields are not.
    #[serde(default)]
    pub agent: BTreeMap<String, AgentSpec>,

    #[serde(default)]
    pub guards: GuardSpec,
}

fn default_schema_version() -> u32 {
    SUPPORTED_SCHEMA_VERSION
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMeta {
    /// Display name. Defaults to the repo directory name.
    #[serde(default)]
    pub name: Option<String>,
    /// Arbitrary constants, available to every template as `vars.<key>`. This is
    /// where a project's magic strings live once instead of being repeated in
    /// every template.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

// ─────────────────────────────── commands ───────────────────────────────

/// Where a command runs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CwdBase {
    /// The repo root of the *current* worktree context.
    #[default]
    RepoRoot,
    /// The main worktree, regardless of which worktree is selected.
    MainWorktree,
    /// The worktree the command is about. Invalid for commands that run before a
    /// worktree exists.
    Worktree,
    /// A path template, rendered with the current token scope.
    Custom(String),
}

/// What to do when a command fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Abort the surrounding operation.
    #[default]
    Fail,
    /// Record a warning and continue.
    Warn,
    /// Ignore entirely.
    Ignore,
    /// Keep whatever was created and hand the user remedies instead of rolling
    /// back. See [`super::plan::CreateOutcome::SetupFailed`].
    Keep,
}

/// Push extra argv entries when a condition holds.
///
/// This is how a boolean form field becomes a command-line flag without the Rust
/// knowing that `--no-db` exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalArgs {
    /// Expression over the current token scope.
    pub when: String,
    pub push: Vec<String>,
}

/// One command to run. The shared shape behind setup, actions, lookups,
/// select-options and removal steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// argv, each element a template. Never a shell string: no shell means no
    /// quoting bugs and no accidental expansion of a Jira summary containing a
    /// backtick.
    pub run: Vec<String>,

    #[serde(default)]
    pub cwd: CwdBase,

    /// Environment overrides, values templated. `PATH` is the common one — a
    /// bundled `.app` inherits a PATH without Homebrew, so configs set
    /// `PATH = "{{ env.LOGIN_PATH }}"`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Wall-clock limit. `None` means the adapter's default is used; captured
    /// (non-PTY) commands always end up with *some* limit, because a project
    /// script that prompts on stdin will otherwise hang forever.
    #[serde(default)]
    pub timeout_ms: Option<u64>,

    /// Run under a pseudo-terminal, streamed to the UI, with input routed back.
    /// Required for anything that might prompt, and for anything whose output the
    /// user should watch.
    #[serde(default)]
    pub pty: bool,

    /// Only run when this expression is true.
    #[serde(default)]
    pub when: Option<String>,

    #[serde(default)]
    pub on_failure: OnFailure,

    #[serde(default)]
    pub args_when: Vec<ConditionalArgs>,
}

impl CommandSpec {
    /// The program, for PATH-resolution preflight and for guard matching.
    #[must_use]
    pub fn program(&self) -> Option<&str> {
        self.run.first().map(String::as_str)
    }

    /// A single-line rendering of the argv, for display, logs and guard matching.
    /// Not shell-quoted, because it is never fed to a shell.
    #[must_use]
    pub fn display_argv(&self) -> String {
        self.run.join(" ")
    }
}

// ─────────────────────────────── form fields ───────────────────────────────

/// The closed set of field kinds.
///
/// Closed on purpose: the frontend's one form renderer switches on this, so adding
/// a kind is a compile error on the Rust side and a type error on the TypeScript
/// side until both handle it. A config cannot invent a widget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Multiline,
    Number,
    Bool,
    Select,
    Multiselect,
    /// A filesystem path, with a browse button.
    Path,
}

/// Where a select's options come from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OptionsSource {
    Static {
        values: Vec<String>,
    },
    /// Run a command and parse its output. This is the "pull the options from
    /// bash" capability: a base-branch list is `git for-each-ref`, an environment
    /// list is whatever the project already has a command for.
    Command {
        #[serde(flatten)]
        command: CommandSpec,
        #[serde(default)]
        parse: OptionsParse,
        /// Drop options matching this regex (e.g. `^origin/HEAD$`).
        #[serde(default)]
        exclude: Option<String>,
        /// Cache the result. Keeps a live-preview form from re-running the command
        /// on every keystroke.
        #[serde(default = "default_options_ttl")]
        cache_ttl_ms: u64,
    },
}

const fn default_options_ttl() -> u64 {
    15_000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionsParse {
    /// One option per non-empty output line.
    #[default]
    Lines,
    /// A JSON array of strings, or of `{value, label}` objects.
    Json,
    /// NUL-separated, for values that may contain newlines.
    Nul,
}

/// A field's default value, as written in TOML.
///
/// Untagged, so a config author writes what the field's type makes natural —
/// `default = false` for a bool, `default = "HEAD"` for a select, `default = 8000` for a
/// number — rather than having to quote everything. Forcing `default = "false"` on a
/// checkbox is the kind of papercut that catches every single person once.
///
/// Everything collapses to a string internally, because templates are string-oriented and
/// [`FieldValue`](super::FieldValue) already carries the typed form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldDefault {
    /// Listed before `Text` so `false` deserializes as a bool rather than being coerced.
    Bool(bool),
    Number(f64),
    Text(String),
}

impl FieldDefault {
    /// The value as a template string.
    #[must_use]
    pub fn as_string(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Bool(value) => value.to_string(),
            // Render 8 as "8", not "8.0" — these can end up in names and URLs.
            Self::Number(value) if value.fract() == 0.0 && value.is_finite() => {
                format!("{value:.0}")
            }
            Self::Number(value) => value.to_string(),
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0,
            Self::Text(text) => text == "true" || text == "1",
        }
    }
}

/// One field on the New Worktree form.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSpec {
    /// Template token name for this field's value.
    pub key: String,
    pub label: String,
    pub kind: FieldKind,

    #[serde(default)]
    pub required: bool,
    /// Required only when this expression is true — e.g. a title is mandatory only
    /// when no issue key was supplied.
    #[serde(default)]
    pub required_when: Option<String>,

    #[serde(default)]
    pub default: Option<FieldDefault>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub help: Option<String>,

    /// Template mapping the raw input to its effective value, applied before
    /// validation and before any lookup.
    ///
    /// This is what lets a config express "a bare number gets the project prefix"
    /// as data. The UI shows the normalized result next to the raw input, so a
    /// user typing `1234` can see it became `ACME-1234` before committing.
    #[serde(default)]
    pub normalize: Option<String>,

    /// Regex the normalized value must match.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Human-readable message for a `pattern` failure. Regexes make terrible
    /// error messages.
    #[serde(default)]
    pub pattern_message: Option<String>,

    /// For `select`/`multiselect`.
    #[serde(default)]
    pub options: Option<OptionsSource>,
    /// Allow a value that isn't in `options`.
    #[serde(default)]
    pub allow_custom: bool,
}

// ─────────────────────────────── lookups ───────────────────────────────

/// How to react when a lookup command fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupErrorPolicy {
    /// Abort. Correct when the lookup is the only source of a required value.
    Fail,
    /// Use fallbacks, record a warning, continue.
    ///
    /// The right default for a network call: a Jira outage should not stop you
    /// making a worktree.
    #[default]
    Warn,
}

/// Map one JSON path out of a lookup's output onto a token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupMapping {
    /// `JSONPath` expression, e.g. `$.fields.issuetype.name`.
    pub path: String,
    /// Filters applied in order — `lower`, `slugify`, and so on. Replaces the
    /// `| ascii_downcase` half of a `jq` pipeline.
    #[serde(default)]
    pub transform: Vec<String>,
    /// Literal substitutions applied after `transform`, for encoding a project's
    /// vocabulary differences as data instead of a `case` statement.
    #[serde(default)]
    pub rewrite: Vec<Rewrite>,
    /// Used when the path is absent, or when the lookup failed under
    /// [`LookupErrorPolicy::Warn`].
    #[serde(default)]
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rewrite {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupFormat {
    #[default]
    Json,
    /// Whole stdout as one string, trimmed.
    Text,
}

/// Enrich the form's values from an external source before naming happens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupSpec {
    /// Token namespace: results land at `lookup.<id>.<mapping-key>`. Namespaced so
    /// a lookup can never shadow a field key.
    pub id: String,

    #[serde(flatten)]
    pub command: CommandSpec,

    #[serde(default)]
    pub format: LookupFormat,
    #[serde(default)]
    pub on_error: LookupErrorPolicy,
    /// Cache TTL keyed on the *rendered* argv.
    #[serde(default = "default_lookup_ttl")]
    pub cache_ttl_ms: u64,

    #[serde(default)]
    pub map: BTreeMap<String, LookupMapping>,
}

const fn default_lookup_ttl() -> u64 {
    300_000
}

/// A value derived from fields and lookups, evaluated in declaration order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputedSpec {
    /// Available downstream as `computed.<key>`.
    pub key: String,
    pub template: String,
}

// ─────────────────────────────── naming ───────────────────────────────

/// What the rendered directory name is relative to.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirBase {
    /// Sibling of the repo root — the flat `../{name}` layout.
    #[default]
    RepoParent,
    /// Nested inside the repo. Must be gitignored; validation warns if it isn't.
    RepoRoot,
    /// An absolute path template, for a central worktree directory.
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamingSpec {
    /// Template for the branch name.
    pub branch: String,
    /// Template for the directory name (a single path component unless the
    /// template contains separators).
    pub directory: String,
    #[serde(default)]
    pub dir_base: DirBase,
    /// Regex the rendered branch must satisfy.
    ///
    /// This is the backstop against a template silently producing garbage — an
    /// empty slug turning `{type}/{key}-{slug}` into `experiment/ACME-0000-`.
    /// Cheap to configure, and it converts a corrupt branch name into a clear
    /// pre-mutation error.
    #[serde(default)]
    pub branch_must_match: Option<String>,
}

impl Default for NamingSpec {
    /// Enough to be useful with no configuration at all: name the branch and the
    /// directory after a slugified `name` field.
    fn default() -> Self {
        Self {
            branch: "{{ name | slugify }}".to_owned(),
            directory: "{{ name | slugify | truncate(40, '') }}".to_owned(),
            dir_base: DirBase::RepoParent,
            branch_must_match: None,
        }
    }
}

// ─────────────────────────────── create ───────────────────────────────

/// Whether a new branch tracks its base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackModeSpec {
    /// `git worktree add --no-track -b`.
    ///
    /// The right default when branching off a shared integration branch: with
    /// tracking, a reflexive `git push` targets that branch instead of your own.
    #[default]
    NoTrack,
    /// `git worktree add --track -b`.
    Track,
    /// `git worktree add --detach`.
    Detach,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExistingBranchBehavior {
    /// Show the matches and let the user choose. The GUI equivalent of a numbered
    /// stdin picker — and strictly better, because it is not blocking.
    #[default]
    Offer,
    /// Silently adopt a unique match.
    Prefer,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchScope {
    Local,
    Remote,
    #[default]
    LocalAndRemote,
}

/// Offer to reuse a branch that already exists instead of creating a new one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExistingBranchMatch {
    /// Glob over branch names, rendered first (so it can contain `{{ issue }}`).
    pub pattern: String,
    #[serde(default)]
    pub scope: BranchScope,
    #[serde(default)]
    pub behavior: ExistingBranchBehavior,
    /// Directory template used when a match is adopted, with `matched_branch` in
    /// scope. Defaults to the branch name minus its type prefix.
    #[serde(default)]
    pub directory: Option<String>,
    /// Create a local tracking branch when the match is remote-only.
    #[serde(default = "default_true")]
    pub adopt_remote_track: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSpec {
    /// Which field supplies the base ref.
    #[serde(default = "default_base_field")]
    pub base_field: String,
    /// Fetch before branching, when the base is remote-tracking. Failure is
    /// non-fatal: a stale base beats refusing to work offline.
    #[serde(default = "default_true")]
    pub fetch_base: bool,
    #[serde(default)]
    pub track: TrackModeSpec,
    #[serde(default, rename = "existing_branch_match")]
    pub existing_branch_match: Vec<ExistingBranchMatch>,
}

fn default_base_field() -> String {
    "base".to_owned()
}

impl Default for CreateSpec {
    fn default() -> Self {
        Self {
            base_field: default_base_field(),
            fetch_base: true,
            track: TrackModeSpec::default(),
            existing_branch_match: Vec::new(),
        }
    }
}

/// How many setups may run at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Concurrency {
    /// One per worktree; different worktrees proceed in parallel.
    #[default]
    OnePerWorktree,
    /// One at a time across the whole project.
    ///
    /// Needed when a project's setup allocates a shared resource by scanning the
    /// other worktrees — two concurrent scans can pick the same free port.
    OneGlobally,
    Unlimited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupSpec {
    #[serde(flatten)]
    pub command: CommandSpec,
    #[serde(default)]
    pub concurrency: Concurrency,
}

// ─────────────────────────────── remove ───────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveStrategy {
    /// The app runs `git worktree remove` itself.
    ///
    /// Preferred, because a project's own remove command tends to prompt with no
    /// non-interactive escape. Running git ourselves turns "answer y/n on stdin"
    /// into a checkbox.
    #[default]
    Native,
    /// Delegate to a configured command.
    Command,
}

// Four independent booleans, and clippy is right that this would usually be a
// smell. It isn't here: each one is a distinct `wtm.toml` key with its own
// default, and collapsing them into an enum would mean a config author could no
// longer set them independently. The schema is the constraint, not the struct.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveSpec {
    #[serde(default)]
    pub strategy: RemoveStrategy,
    /// Refuse when tracked files are modified.
    #[serde(default = "default_true")]
    pub require_clean: bool,
    /// Offer a force override in the dialog.
    #[serde(default = "default_true")]
    pub allow_force: bool,
    /// Offer to delete the branch too.
    #[serde(default = "default_true")]
    pub prompt_delete_branch: bool,
    /// Warn when the branch has commits not reachable from the base.
    #[serde(default = "default_true")]
    pub warn_if_unmerged: bool,
    /// Teardown steps, run before `git worktree remove` — stopping containers,
    /// fixing root-owned files a container created, and so on.
    #[serde(default, rename = "pre")]
    pub pre: Vec<CommandSpec>,
    /// Used when `strategy = "command"`.
    #[serde(default)]
    pub command: Option<CommandSpec>,
}

impl Default for RemoveSpec {
    fn default() -> Self {
        Self {
            strategy: RemoveStrategy::Native,
            require_clean: true,
            allow_force: true,
            prompt_delete_branch: true,
            warn_if_unmerged: true,
            pre: Vec::new(),
            command: None,
        }
    }
}

// ─────────────────────────────── display ───────────────────────────────

/// An external file whose contents become template tokens for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplaySource {
    /// Namespace: values land at `<id>.<KEY>`.
    pub id: String,
    pub kind: DisplaySourceKind,
    /// Path template.
    pub path: String,
    #[serde(default = "default_true")]
    pub optional: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplaySourceKind {
    /// `KEY=value` lines.
    Dotenv,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayBadge {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub when: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayLink {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub when: Option<String>,
    /// Openable in a browser.
    #[serde(default = "default_true")]
    pub open: bool,
}

/// Render every key sharing a prefix as a table.
///
/// Exists so a project with eleven port variables declares one table rather than
/// eleven rows. `defaults` carries the important subtlety: an *absent* variable
/// usually means a base value is in effect elsewhere (a compose file's
/// `${VAR:-base}`), so a missing key is not a missing port.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayTable {
    pub prefix: String,
    /// [`DisplaySource::id`] to read values from.
    pub from: String,
    /// [`DisplaySource::id`] supplying fallbacks for absent keys.
    #[serde(default)]
    pub defaults: Option<String>,
    #[serde(default)]
    pub label_transform: Vec<String>,
    #[serde(default)]
    pub link_template: Option<String>,
    /// Which keys get a link.
    #[serde(default)]
    pub link_for: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplaySpec {
    #[serde(default, rename = "source")]
    pub sources: Vec<DisplaySource>,
    /// Defaults to the directory name.
    #[serde(default)]
    pub title: Option<String>,
    /// Defaults to the branch.
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default, rename = "badge")]
    pub badges: Vec<DisplayBadge>,
    #[serde(default, rename = "link")]
    pub links: Vec<DisplayLink>,
    #[serde(default, rename = "port_table")]
    pub tables: Vec<DisplayTable>,
}

// ─────────────────────────────── actions & guards ───────────────────────────────

/// A command the user can run against a worktree on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSpec {
    pub id: String,
    pub label: String,
    #[serde(flatten)]
    pub command: CommandSpec,
}

/// How a repository configures one agent.
///
/// # Why keyed tables and not `[[agent]]`
///
/// Deliberately unlike `[[field]]`, and the reason is the merge semantics this config system already
/// has: **arrays replace across layers, tables merge per key**. With an array, a repo that wanted to
/// change Codex's default effort would have to restate Claude's whole block to avoid deleting it.
/// `[[field]]` accepts that on purpose — "a project defining `[[field]]` defines the whole form" — but
/// it is wrong here, because agents are independent of each other in a way form fields are not.
///
/// # What a repository cannot say
///
/// The program name. It is not a key here, and the omission is the point: the binary comes from the
/// compiled catalogue, so the word "Claude" in wtm's UI cannot be whatever a file in someone's branch
/// says it is. A trust prompt showing an approved-looking argv is a weak defence against a user who
/// has learned to approve them. The escape hatch for a wrapper script or an unusual install lives in
/// `~/.config/wtm/config.toml`, which is the same layer `exec.path` uses and one no repository can
/// write to.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// Offer this agent in this repository. Absent means yes.
    ///
    /// Present so a repo can turn one *off* without a guard, which is the softer statement:
    /// `[[guards.forbid]]` refuses with a reason and is for "this must not happen", where this is
    /// for "we do not use that here".
    #[serde(default)]
    pub enabled: Option<bool>,
    /// The model a new session starts on, in the provider's own spelling.
    ///
    /// A free string, validated against the capability query at session start rather than at config
    /// load: a config outlives the CLI version it was written against, so a model this build has
    /// never heard of must surface as a warning on the session, not as a refusal to load the file.
    #[serde(default)]
    pub model: Option<String>,
    /// The effort a new session starts on.
    ///
    /// Also a free string, and for a sharper reason: the ladder is **per model**, so no enum could be
    /// right for every one of them — `gpt-5.6-sol` offers `ultra` and `gpt-5.5` stops at `xhigh`.
    #[serde(default)]
    pub effort: Option<String>,
    /// The approval or permission mode, in the provider's own spelling.
    ///
    /// Not translated into a wtm-side enum: that would be a second name for the same thing, needing
    /// to be kept in step with two CLIs this app does not control.
    #[serde(default)]
    pub mode: Option<String>,
    /// Start sessions in the provider's high-speed mode, where it has one.
    ///
    /// A `bool` rather than a free string, unlike everything above it: this is not a value from a
    /// vocabulary the CLI owns and might extend, it is a switch that either is or is not thrown.
    ///
    /// Ignored by a provider whose capability does not advertise fast mode, rather than refused.
    /// A repository is allowed to state a preference for the agents it uses without having to know
    /// which of them can honour it — the same reasoning `enabled` uses for saying "we do not use
    /// that here" without a guard.
    #[serde(default)]
    pub fast: Option<bool>,
    /// Appended to the argv the catalogue builds.
    ///
    /// Templated, like every other argv in this file, and subject to `[[guards.forbid]]` at spawn
    /// time for the same reason the setup command is: this is the one place a repository can add
    /// arbitrary arguments to a process wtm starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
    /// Environment overlaid on the session's process.
    ///
    /// `PATH = "{{ env.LOGIN_PATH }}"` is the load-bearing case, the same one `[setup.env]` documents:
    /// a GUI launch does not inherit a shell's PATH, so a CLI in `~/.local/bin` is invisible without
    /// it once the app is installed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// MCP servers to hand the CLI, keyed by name.
    ///
    /// This is what makes session-to-session handoff work with no code: one entry pointing at
    /// `codex mcp-server` lets a Claude session open a Codex thread itself. Each server is an argv
    /// this repository names, so it goes through the trust prompt with everything else.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<String, McpServerSpec>,
}

/// One MCP server handed to an agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerSpec {
    /// The program. Templated, and part of what the trust prompt shows.
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl McpServerSpec {
    /// The argv this server would run, for the trust prompt and the guard check.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![self.command.clone()];
        argv.extend(self.args.iter().cloned());
        argv
    }
}

/// A command that must never run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForbidRule {
    /// Regex matched against the joined argv.
    pub argv_matches: String,
    /// An escape clause: if this also matches, the rule does **not** fire.
    ///
    /// Exists because Rust's `regex` crate has no look-around, by design — it
    /// guarantees linear-time matching, which is the right trade for patterns that come
    /// from a config file. But the most useful guards are of the form "forbid X unless
    /// Y", such as "forbid `git worktree list` unless `--porcelain` is present". Two
    /// patterns express that without needing `(?!…)`.
    #[serde(default)]
    pub unless_matches: Option<String>,
    /// Shown to the user when the rule fires. Write the *why* here — a guard with
    /// no reason gets deleted by the next person who trips it.
    pub reason: String,
}

/// The single place project-specific hazard knowledge lives.
///
/// Some scripts genuinely cannot be run from a GUI: they `exec` a login shell and
/// never return, or they prompt with a read loop that spins forever on EOF stdin.
/// That knowledge belongs to the project, not to this codebase, so it is data.
/// Rules are checked when config loads *and* again at spawn time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardSpec {
    #[serde(default)]
    pub forbid: Vec<ForbidRule>,
}

impl Project {
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.meta.name.as_deref().unwrap_or_else(|| {
            self.root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
        })
    }

    #[must_use]
    pub fn field(&self, key: &str) -> Option<&FieldSpec> {
        self.fields.iter().find(|f| f.key == key)
    }

    #[must_use]
    pub fn action(&self, id: &str) -> Option<&ActionSpec> {
        self.actions.iter().find(|a| a.id == id)
    }

    /// Every command this config could run, deduplicated, for the trust prompt.
    ///
    /// The user must be able to read exactly what a `wtm.toml` would execute
    /// before approving it, so this has to cover *all* five command sites — a
    /// prompt that missed one would be worse than no prompt at all.
    #[must_use]
    pub fn declared_commands(&self) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = Vec::new();
        let mut push = |c: &CommandSpec| {
            if !c.run.is_empty() && !out.contains(&c.run) {
                out.push(c.run.clone());
            }
        };

        for f in &self.fields {
            if let Some(OptionsSource::Command { command, .. }) = &f.options {
                push(command);
            }
        }
        for l in &self.lookups {
            push(&l.command);
        }
        if let Some(s) = &self.setup {
            push(&s.command);
        }
        for c in &self.remove.pre {
            push(c);
        }
        if let Some(c) = &self.remove.command {
            push(c);
        }
        for a in &self.actions {
            push(&a.command);
        }

        // Every MCP server an agent is handed. These are argv this repository names, and an MCP
        // server is a child process with the same reach as any other — one pointed at a script in the
        // repo would run on the first session. So they belong in the trust prompt, and the fact that
        // an agent's *own* binary does not appear here is deliberate: that comes from the compiled
        // catalogue, not from this file, which is what makes it not a thing a branch can choose.
        for spec in self.agent.values() {
            for server in spec.mcp.values() {
                let argv = server.argv();
                if !argv.is_empty() && !out.contains(&argv) {
                    out.push(argv);
                }
            }
        }
        out
    }

    /// The agent settings for `id`, merged with nothing — the file's own word, or defaults.
    #[must_use]
    pub fn agent_spec(&self, id: &str) -> AgentSpec {
        self.agent.get(id).cloned().unwrap_or_default()
    }

    /// Whether this repository offers an agent.
    ///
    /// Absent means yes. A repo that declares one agent's settings has not thereby refused the
    /// others — which is the whole reason these are keyed tables rather than an array.
    #[must_use]
    pub fn offers_agent(&self, id: &str) -> bool {
        self.agent
            .get(id)
            .is_none_or(|spec| spec.enabled != Some(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(run: &[&str]) -> CommandSpec {
        CommandSpec {
            run: run.iter().map(|s| (*s).to_owned()).collect(),
            cwd: CwdBase::default(),
            env: BTreeMap::new(),
            timeout_ms: None,
            pty: false,
            when: None,
            on_failure: OnFailure::default(),
            args_when: Vec::new(),
        }
    }

    fn empty_project() -> Project {
        Project {
            id: ProjectId("/x".to_owned()),
            root: PathBuf::from("/x"),
            schema_version: 1,
            meta: ProjectMeta::default(),
            fields: Vec::new(),
            lookups: Vec::new(),
            computed: Vec::new(),
            naming: NamingSpec::default(),
            create: CreateSpec::default(),
            setup: None,
            remove: RemoveSpec::default(),
            display: DisplaySpec::default(),
            actions: Vec::new(),
            agent: BTreeMap::new(),
            guards: GuardSpec::default(),
        }
    }

    #[test]
    fn an_mcp_server_an_agent_is_handed_reaches_the_trust_prompt() {
        // An MCP server is a child process with the same reach as any other, and one pointed at a
        // script in the repo would run on the first session. So it belongs in the list the trust
        // prompt shows — the same list `[setup]` and `[[action]]` are in.
        let mut p = empty_project();
        let mut spec = AgentSpec::default();
        spec.mcp.insert(
            "codex".to_owned(),
            McpServerSpec {
                command: "codex".to_owned(),
                args: vec!["mcp-server".to_owned()],
                env: BTreeMap::new(),
            },
        );
        p.agent.insert("claude".to_owned(), spec);

        assert!(
            p.declared_commands()
                .contains(&vec!["codex".to_owned(), "mcp-server".to_owned()]),
            "an MCP server's argv must be shown before it is approved"
        );
    }

    #[test]
    fn an_agents_own_binary_is_not_a_declared_command() {
        // Deliberately absent, and the omission is the security property: a provider's program comes
        // from the compiled catalogue, not from this file, so a repository cannot name it — which is
        // what stops the word "Claude" in wtm's UI being whatever a branch says it is.
        let mut p = empty_project();
        p.agent.insert(
            "claude".to_owned(),
            AgentSpec {
                model: Some("opus".to_owned()),
                ..AgentSpec::default()
            },
        );
        assert!(p.declared_commands().is_empty());
    }

    #[test]
    fn declaring_one_agent_does_not_refuse_the_others() {
        // The whole reason these are keyed tables rather than an array. With `[[agent]]` a repo that
        // configured Codex would have replaced the list and silently lost Claude.
        let mut p = empty_project();
        p.agent.insert(
            "codex".to_owned(),
            AgentSpec {
                effort: Some("ultra".to_owned()),
                ..AgentSpec::default()
            },
        );

        assert!(p.offers_agent("codex"));
        assert!(
            p.offers_agent("claude"),
            "configuring one must not refuse another"
        );
        assert!(p.offers_agent("some-future-agent"));
    }

    #[test]
    fn a_repository_can_turn_an_agent_off_without_a_guard() {
        // The softer statement of the two. A guard refuses with a reason and is for "this must not
        // happen"; this is for "we do not use that here".
        let mut p = empty_project();
        p.agent.insert(
            "codex".to_owned(),
            AgentSpec {
                enabled: Some(false),
                ..AgentSpec::default()
            },
        );
        assert!(!p.offers_agent("codex"));
        assert!(p.offers_agent("claude"));
    }

    #[test]
    fn declared_commands_covers_every_command_site() {
        let mut p = empty_project();
        p.fields.push(FieldSpec {
            key: "base".to_owned(),
            label: "Base".to_owned(),
            kind: FieldKind::Select,
            required: false,
            required_when: None,
            default: None,
            placeholder: None,
            help: None,
            normalize: None,
            pattern: None,
            pattern_message: None,
            options: Some(OptionsSource::Command {
                command: cmd(&["git", "for-each-ref"]),
                parse: OptionsParse::Lines,
                exclude: None,
                cache_ttl_ms: 0,
            }),
            allow_custom: false,
        });
        p.lookups.push(LookupSpec {
            id: "jira".to_owned(),
            command: cmd(&["acli", "jira"]),
            format: LookupFormat::Json,
            on_error: LookupErrorPolicy::Warn,
            cache_ttl_ms: 0,
            map: BTreeMap::new(),
        });
        p.setup = Some(SetupSpec {
            command: cmd(&["./bin/setup.sh"]),
            concurrency: Concurrency::default(),
        });
        p.remove.pre.push(cmd(&["docker", "compose", "down"]));
        p.remove.command = Some(cmd(&["./bin/teardown.sh"]));
        p.actions.push(ActionSpec {
            id: "shell".to_owned(),
            label: "Shell".to_owned(),
            command: cmd(&["zsh", "-l"]),
        });

        let found = p.declared_commands();
        assert_eq!(
            found.len(),
            6,
            "every command site must be surfaced: {found:?}"
        );
        assert!(found.contains(&vec!["git".to_owned(), "for-each-ref".to_owned()]));
        assert!(found.contains(&vec!["zsh".to_owned(), "-l".to_owned()]));
    }

    #[test]
    fn declared_commands_deduplicates() {
        let mut p = empty_project();
        p.remove.pre.push(cmd(&["docker", "compose", "down"]));
        p.actions.push(ActionSpec {
            id: "down".to_owned(),
            label: "Down".to_owned(),
            command: cmd(&["docker", "compose", "down"]),
        });
        assert_eq!(p.declared_commands().len(), 1);
    }

    #[test]
    fn display_name_falls_back_to_the_directory() {
        let p = empty_project();
        assert_eq!(p.display_name(), "x");
    }

    #[test]
    fn default_naming_works_with_no_config() {
        let n = NamingSpec::default();
        assert!(n.branch.contains("slugify"));
        assert_eq!(n.dir_base, DirBase::RepoParent);
    }
}

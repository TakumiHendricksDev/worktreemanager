//! The IPC data contract.
//!
//! # Why these types exist rather than sending domain types
//!
//! The frontend needs *rendered* information: a display title, resolved links, a port
//! table with fallbacks applied. Domain types carry the raw facts. Doing that rendering
//! in Rust — where the template engine, the config and the `.env` files already are —
//! keeps the frontend free of business logic, and means the sidebar and the detail pane
//! cannot disagree about what a worktree is called.
//!
//! These are also the only structs whose field names are an external contract, since
//! `src/lib/ipc/types.ts` mirrors them by hand. `contract_shape` in the tests snapshots
//! the serialized keys so a rename cannot silently break the UI.

use std::collections::BTreeMap;

use serde::Serialize;
use wtm_core::error::WtmError;
use wtm_core::model::{
    FieldKind, PlanPreview, PreflightItem, PreflightSeverity, Project, Worktree,
};

/// A registered repository, as the sidebar's project switcher needs it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub root: String,
    /// `false` when the config failed to load or is awaiting trust; the UI shows the
    /// project but disables creation and surfaces `problem`.
    pub usable: bool,
    /// Human-readable reason the project is not usable.
    pub problem: Option<String>,
    /// Set when the config declares unapproved commands, so the UI can raise the trust
    /// prompt rather than only reporting an error.
    pub trust: Option<TrustPromptView>,
}

/// The new project list, plus which entry the operation actually landed on.
///
/// The id is here because **the frontend cannot compute it**. Registration accepts any path
/// inside a repository and resolves it to the toplevel, so what the user typed and what got
/// registered are routinely different strings: `~/Sites/foo` becomes `/home/you/Sites/foo`,
/// and a subdirectory becomes its repo root. Only git knows which repository a path lands in.
///
/// Returning just the list forced the caller to guess by matching the typed path against every
/// root, which failed silently for every tilde path — that is, for the exact form the Add
/// dialog's own placeholder suggests — and could match the wrong project outright, since a
/// prefix test without a separator boundary lets `/x/foo/src` match a project at `/x/f`.
///
/// Unregistering returns the same shape for the same reason: it also accepts any path inside
/// the repository, so the argument is not necessarily the id of what was removed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredView {
    /// The affected project's id — its resolved absolute root.
    pub id: String,
    pub projects: Vec<ProjectView>,
}

/// Everything the trust prompt needs to let a person make an informed decision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustPromptView {
    pub path: String,
    /// Every argv the config would run, verbatim. The whole point of the prompt.
    pub commands: Vec<Vec<String>>,
    pub content_hash: String,
}

/// One badge in a worktree's detail header.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BadgeView {
    pub label: String,
    pub value: String,
}

/// A link the user can open.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkView {
    pub label: String,
    pub url: String,
}

/// The environment keys a worktree exposes, as a list of **names only**.
///
/// # Why there is no value here, and no "is this a secret" flag
///
/// There used to be. `EnvEntryView` carried an `Option<String>` value plus a `sensitive`
/// bool, filled in by a three-signal classifier: a substring table of key names
/// (`secret`, `token`, `password`, …), a check for `scheme://user:pw@host` in the value,
/// and a pass that treated any value matching a known secret as a secret too.
///
/// It worked, and it was still the wrong shape. Guessing has two failure modes and both are
/// bad: under-match and a credential is published; over-match and a port number needs a
/// click. Every project's `.env` gets a vote on which way it fails, so the table could only
/// ever grow, and each addition was a judgement call defended by a comment. The `MinIO` case
/// that forced the third signal — where the access key, secret key, username and password
/// were all the *same string*, so masking the obviously-named ones published it anyway — was
/// the tell: the classifier was chasing a property of the data it could not see.
///
/// So nothing is classified. No value crosses this boundary at all; the payload is key names,
/// and `reveal_env_value` fetches exactly one on request. That is not a stricter policy on
/// top of the same type — the type can no longer *hold* a value, so there is nothing for a
/// future edit to accidentally start sending. A screenshot, a screen-share or a poke at the
/// webview's memory has nothing to find, and the guarantee needs no test to stay true.
pub type EnvKeys = Vec<String>;

/// One row of a prefix-grouped table, such as a project's host ports.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRowView {
    pub label: String,
    pub value: String,
    /// True when the value came from a defaults source rather than the worktree's own
    /// file. Surfaced because "absent means the base value is in effect" is exactly the
    /// kind of thing that is invisible and then confusing.
    pub inherited: bool,
    pub url: Option<String>,
}

/// A worktree, ready to render.
// Four independent booleans. Clippy's suggestion — fold them into enums — would be right
// for a domain type, but this is the wire format: each flag is a separate thing the sidebar
// renders, the frontend reads them by name, and an enum would trade four obvious fields for
// a tagged union that TypeScript has to unpack. The JSON contract is the constraint.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeView {
    pub id: String,
    /// The rendered display title. Defaults to the directory name.
    pub title: String,
    /// Rendered subtitle. Defaults to the branch, or `(detached)`.
    pub subtitle: String,
    pub path: String,
    pub dirname: String,
    /// `None` for a detached worktree. Never inferred from the directory name.
    pub branch: Option<String>,
    pub head: Option<String>,
    pub is_main: bool,
    pub is_bare: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,

    pub dirty: bool,
    pub untracked: usize,
    pub staged: usize,
    pub ahead: u32,
    pub behind: u32,

    /// An issue key extracted from the branch, then the directory, if either has one.
    pub issue_key: Option<String>,

    /// Starred by the user, which floats it to the top of the sidebar.
    ///
    /// The only field here that comes from the *app* config rather than from git or the
    /// project config. It rides along on this view so the sidebar's ordering survives a
    /// cold start from the frontend's cache, with no second round-trip.
    pub favorite: bool,

    pub badges: Vec<BadgeView>,
    pub links: Vec<LinkView>,
    pub table: Vec<TableRowView>,
    /// Key names from the project's declared display source, for the Env tab. Names only —
    /// see [`EnvKeys`].
    pub env: EnvKeys,
}

/// One field of the New Worktree form.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldView {
    pub key: String,
    pub label: String,
    /// Serialized `FieldKind`, e.g. `text`, `select`, `bool`.
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<String>,
    pub placeholder: Option<String>,
    pub help: Option<String>,
    pub allow_custom: bool,
    /// True when options come from a command, so the UI knows to fetch them.
    pub has_dynamic_options: bool,
    /// Static options, when the config listed them inline.
    pub options: Vec<String>,
    pub pattern: Option<String>,
    pub pattern_message: Option<String>,
}

/// The whole form, plus the labels the dialog needs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormView {
    pub project_id: String,
    pub fields: Vec<FieldView>,
    /// Action ids and labels available on an existing worktree.
    pub actions: Vec<ActionView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionView {
    pub id: String,
    pub label: String,
    pub pty: bool,
}

/// One external tool the worktree can be opened in.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenerView {
    pub id: String,
    pub label: String,
    pub available: bool,
    /// Why it is unavailable, for a tooltip. `None` when it is available.
    ///
    /// Carried rather than left to the frontend because the useful version of this
    /// sentence names the program that was searched for, which only the catalogue knows.
    pub detail: Option<String>,
}

impl From<&crate::openers::Availability> for OpenerView {
    fn from(availability: &crate::openers::Availability) -> Self {
        Self {
            id: availability.id.to_owned(),
            label: availability.label.to_owned(),
            available: availability.available(),
            detail: availability.detail.clone(),
        }
    }
}

/// The whole opener catalogue plus which one the primary button should run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenersView {
    pub openers: Vec<OpenerView>,
    /// `None` only if the catalogue were empty, which it never is — the platform file
    /// manager has no prerequisites.
    pub preferred: Option<String>,
}

/// A preflight finding, ready to render as a checklist row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightView {
    pub id: String,
    pub severity: PreflightSeverity,
    pub message: String,
    pub overridable: bool,
    pub hint: Option<String>,
}

/// The review screen: what will happen, before anything has.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewView {
    pub branch: Option<String>,
    pub directory: String,
    pub base_ref: String,
    pub base_commit: Option<String>,
    pub will_fetch: bool,
    /// The literal `git worktree add …` argv. Shown because "trust me" is not a review.
    pub git_argv: Vec<String>,
    pub setup_argv: Option<Vec<String>>,
    /// Where setup runs. Surfaced deliberately: it is often the repo root rather than
    /// the new worktree, which surprises people.
    pub setup_cwd: Option<String>,
    pub preflight: Vec<PreflightView>,
    pub warnings: Vec<String>,
    pub lookups: BTreeMap<String, String>,
    pub computed: BTreeMap<String, String>,
    /// Existing branches the user could adopt instead — the GUI form of the shell's
    /// numbered stdin picker.
    pub branch_choices: Vec<BranchChoiceView>,
    /// Field keys that only feed the branch and directory templates, so the form can mark
    /// them inert once an existing branch is adopted.
    pub naming_fields: Vec<String>,
    /// Normalized field values, so the form can show `1234` → `ACME-1234`.
    pub normalized: BTreeMap<String, String>,
    pub can_create: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchChoiceView {
    pub branch: String,
    pub remote_only: bool,
    pub directory: String,
}

/// Diagnostics for the panel that answers "why can't it find `just`?".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorView {
    /// The PATH used for every spawn.
    pub resolved_path: String,
    /// How that PATH was obtained: a config override, a login shell, or inheritance.
    pub path_source: String,
    pub config_dir: String,
    pub tools: Vec<ToolView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolView {
    pub name: String,
    pub path: Option<String>,
}

/// One user-defined colour palette, as Settings needs it.
///
/// Validated here rather than in the frontend, and rather than in `wtm-config`. Not in
/// `wtm-config` because that crate's contract is to round-trip the file: one unusable
/// palette must not stop the rest of the config loading. Not in the frontend because then
/// every rule would live in TypeScript with nothing checking it, and "is this four hex
/// strings" is not a judgement call.
///
/// A broken palette is still returned, carrying `error`, so Settings can show it greyed
/// out with the reason. Dropping it silently would leave someone staring at a config file
/// that looks right and a picker that does not list it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteView {
    pub id: String,
    pub name: String,
    pub hue: f64,
    pub chroma: f64,
    /// The accent ramp at 300, 400, 500, 600. Empty when `error` is set.
    pub brand: Vec<String>,
    pub error: Option<String>,
}

/// The error shape the frontend receives.
///
/// A flat `{ kind, message, detail }` rather than the full nested enum: the UI needs to
/// distinguish a few cases (untrusted, validation, preflight) and otherwise show a
/// message, and a deeply-tagged union would push Rust's error taxonomy into TypeScript
/// for no benefit. `detail` carries the structured payload for the cases that need it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorView {
    pub kind: String,
    pub message: String,
    pub detail: Option<serde_json::Value>,
}

impl ErrorView {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_owned(),
            message: message.into(),
            detail: None,
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl From<WtmError> for ErrorView {
    fn from(err: WtmError) -> Self {
        let message = err.to_string();
        let kind = match &err {
            WtmError::Config(_) => "config",
            WtmError::Git(_) => "git",
            WtmError::Exec(_) => "exec",
            WtmError::Render(_) => "render",
            WtmError::Validation(_) => "validation",
            WtmError::Preflight(_) => "preflight",
            WtmError::Cancelled => "cancelled",
            WtmError::UnknownProject(_) => "unknownProject",
            WtmError::UnknownWorktree(_) => "unknownWorktree",
            WtmError::UnknownEnvKey(_) => "unknownEnvKey",
        };
        // Serialize the original error as the detail payload, so a validation error can
        // still be attributed to individual fields without a second round-trip.
        let detail = serde_json::to_value(&err).ok();
        Self {
            kind: kind.to_owned(),
            message,
            detail,
        }
    }
}

impl From<wtm_core::error::ConfigError> for ErrorView {
    fn from(err: wtm_core::error::ConfigError) -> Self {
        WtmError::Config(err).into()
    }
}

impl From<wtm_core::error::GitError> for ErrorView {
    fn from(err: wtm_core::error::GitError) -> Self {
        WtmError::Git(err).into()
    }
}

/// Extract an issue key such as `ACME-1234` from a branch or directory name.
///
/// Tried against the branch first, then the directory, because they can legitimately
/// disagree — in the reference repo a directory named `ACME-4567-…` sits on a branch
/// named `experiment/ACME-0000-…`, and the branch is the truth.
#[must_use]
pub fn extract_issue_key(worktree: &Worktree) -> Option<String> {
    worktree
        .branch()
        .and_then(|b| find_issue_key(b.as_str()))
        .or_else(|| find_issue_key(worktree.dirname()))
}

/// Find the first `ABC-123`-shaped token.
///
/// Hand-rolled rather than a regex: it runs for every worktree on every refresh, and the
/// grammar is two characters wide.
fn find_issue_key(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        // A key starts at a boundary, so the preceding byte must not be alphanumeric.
        let at_boundary = index == 0 || !bytes[index - 1].is_ascii_alphanumeric();
        if !at_boundary || !bytes[index].is_ascii_uppercase() {
            index += 1;
            continue;
        }

        let start = index;
        let mut cursor = index;
        while cursor < bytes.len() && bytes[cursor].is_ascii_uppercase() {
            cursor += 1;
        }
        // Need at least one letter, a hyphen, then at least one digit.
        if cursor > start && cursor < bytes.len() && bytes[cursor] == b'-' {
            let digits_start = cursor + 1;
            let mut digits_end = digits_start;
            while digits_end < bytes.len() && bytes[digits_end].is_ascii_digit() {
                digits_end += 1;
            }
            if digits_end > digits_start {
                return Some(text[start..digits_end].to_owned());
            }
        }
        index = cursor.max(index + 1);
    }

    None
}

/// Build a [`ProjectView`] for a project whose config loaded cleanly.
#[must_use]
pub fn project_view(project: &Project) -> ProjectView {
    ProjectView {
        id: project.id.as_str().to_owned(),
        name: project.display_name().to_owned(),
        root: project.root.to_string_lossy().into_owned(),
        usable: true,
        problem: None,
        trust: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use wtm_core::model::{BranchRef, Checkout, CommitId, WorktreeId};

    use super::*;

    fn worktree(dirname: &str, branch: Option<&str>) -> Worktree {
        let path = PathBuf::from("/x").join(dirname);
        Worktree {
            id: WorktreeId::from_path(&path),
            path,
            head: Some(CommitId::new("abc123")),
            checkout: branch.map_or(Checkout::Detached, |b| Checkout::Branch {
                branch: BranchRef::new(b),
            }),
            is_main: false,
            is_bare: false,
            locked: None,
            prunable: None,
        }
    }

    #[test]
    fn extracts_an_issue_key_from_a_branch() {
        let wt = worktree("ACME-1234-slug", Some("task/ACME-1234-slug"));
        assert_eq!(extract_issue_key(&wt).as_deref(), Some("ACME-1234"));
    }

    /// The branch wins, because in the real repo they disagree.
    #[test]
    fn the_branch_takes_precedence_over_the_directory_name() {
        let wt = worktree(
            "ACME-4567-move-account-settings",
            Some("experiment/ACME-0000-migrate-api-key-settings"),
        );
        assert_eq!(
            extract_issue_key(&wt).as_deref(),
            Some("ACME-0000"),
            "the branch is the truth; the directory name is only a label"
        );
    }

    #[test]
    fn falls_back_to_the_directory_when_detached() {
        let wt = worktree("ACME-1234-slug", None);
        assert_eq!(extract_issue_key(&wt).as_deref(), Some("ACME-1234"));
    }

    #[test]
    fn no_key_when_neither_has_one() {
        let wt = worktree("some-feature", Some("feature/some-feature"));
        assert_eq!(extract_issue_key(&wt), None);
    }

    #[test]
    fn issue_key_grammar_edge_cases() {
        assert_eq!(find_issue_key("ACME-1234").as_deref(), Some("ACME-1234"));
        assert_eq!(find_issue_key("task/ACME-1-x").as_deref(), Some("ACME-1"));
        assert_eq!(find_issue_key("FNS-42-thing").as_deref(), Some("FNS-42"));
        // Needs digits after the hyphen.
        assert_eq!(find_issue_key("ABC-"), None);
        assert_eq!(find_issue_key("ABC-def"), None);
        // Needs uppercase letters.
        assert_eq!(find_issue_key("abc-123"), None);
        // Must start at a boundary, so an embedded run does not match.
        assert_eq!(find_issue_key("xACME-1"), None);
        assert_eq!(find_issue_key(""), None);
        assert_eq!(find_issue_key("-----"), None);
        // A lowercase prefix followed by a real key still finds the key.
        assert_eq!(find_issue_key("wip/ACME-9-x").as_deref(), Some("ACME-9"));
    }

    #[test]
    fn issue_key_scanning_terminates_on_pathological_input() {
        // Guards the manual index arithmetic against an infinite loop.
        for input in ["A", "A-", "AAAA", "-A-1", "A1-", "AB-CD-12"] {
            let _ = find_issue_key(input);
        }
        // `AB-CD-12` finds `CD-12`, not nothing: `AB-` fails (no digits follow), the scan
        // advances, and `CD` starts at a boundary because `-` is not alphanumeric. That is
        // the right answer — `CD-12` is a well-formed key — and it is asserted so the
        // "keep scanning after a near miss" behaviour cannot silently regress into
        // stopping at the first partial match.
        assert_eq!(find_issue_key("AB-CD-12").as_deref(), Some("CD-12"));
        assert_eq!(find_issue_key("-A-1").as_deref(), Some("A-1"));
    }

    /// The serialized key names are an external contract with `types.ts`.
    #[test]
    fn contract_shape_is_camel_case() {
        let view = WorktreeView {
            id: "/x/a".to_owned(),
            title: "a".to_owned(),
            subtitle: "main".to_owned(),
            path: "/x/a".to_owned(),
            dirname: "a".to_owned(),
            branch: Some("main".to_owned()),
            head: Some("abc".to_owned()),
            is_main: true,
            is_bare: false,
            locked: None,
            prunable: None,
            dirty: false,
            untracked: 0,
            staged: 0,
            ahead: 0,
            behind: 0,
            issue_key: None,
            favorite: false,
            badges: vec![],
            links: vec![],
            table: vec![],
            env: Vec::new(),
        };
        let json = serde_json::to_value(&view).unwrap();
        let object = json.as_object().unwrap();

        // Snake_case leaking through would silently break every binding in the UI.
        for expected in [
            "isMain", "isBare", "issueKey", "dirname", "subtitle", "favorite",
        ] {
            assert!(
                object.contains_key(expected),
                "missing `{expected}` in {object:?}"
            );
        }
        assert!(!object.contains_key("is_main"), "must not emit snake_case");
    }

    #[test]
    fn a_progress_event_carries_camel_case_fields() {
        // `ProgressEvent` is serialized at the domain boundary and consumed as `types.ts`.
        // `rename_all = "snake_case"` on the tag used to leave `duration_ms` on the wire
        // while the frontend read `durationMs`, so a finished command never showed its duration.
        use wtm_core::ports::progress::ProgressEvent;

        let json = serde_json::to_value(ProgressEvent::CommandFinished {
            argv: vec!["git".to_owned()],
            code: 0,
            duration_ms: 12,
        })
        .unwrap();
        assert_eq!(json["kind"], "command_finished");
        assert_eq!(json["durationMs"], 12);
        assert!(json.get("duration_ms").is_none(), "{json}");

        let warning = serde_json::to_value(ProgressEvent::Warning(
            wtm_core::model::PlanWarning::new("id", "msg"),
        ))
        .unwrap();
        assert_eq!(warning["kind"], "warning");
        assert_eq!(warning["id"], "id");
        assert_eq!(warning["message"], "msg");
    }

    /// The capability, which is the newest thing on this boundary and the one with two nested
    /// struct lists inside it.
    ///
    /// Worth its own test rather than a line in the one above, because `AgentMode` and `AgentModel`
    /// are domain types serialized *directly* — `CapabilityView` holds them rather than converting
    /// them — so their `rename_all` is a property of `wtm-core` that this boundary depends on and
    /// cannot see. A reviewer removing the attribute over there would break the picker over here
    /// with nothing in between to notice.
    #[test]
    fn a_capability_carries_camel_case_modes_and_models() {
        let view = CapabilityView::from(wtm_agent::claude_capability());
        let json = serde_json::to_value(&view).unwrap();

        assert!(json.get("modelsAreLive").is_some(), "{json:?}");
        assert!(
            json.get("models_are_live").is_none(),
            "must not emit snake_case"
        );
        // `flags` is gone: `ultracode` became a rung on the effort ladder, and with it went the
        // last provider switch that was neither model nor effort.
        assert!(json.get("flags").is_none(), "flags should be retired");
        // Fast mode is the switch that came *back*, and it is a named field rather than a revived
        // `flags` bag — so the picker's control is typed on both sides of this boundary.
        assert_eq!(json.get("supportsFast"), Some(&serde_json::json!(true)));
        assert!(
            json.get("supports_fast").is_none(),
            "must not emit snake_case"
        );

        let model = &json["models"][0];
        assert!(model.get("isDefault").is_some(), "{model:?}");
        assert!(model.get("defaultEffort").is_some(), "{model:?}");
        assert!(model.get("impliedMode").is_some(), "{model:?}");

        let mode = &json["modes"][0];
        assert!(mode.get("isDefault").is_some(), "{mode:?}");
        // The risk tier the composer colours its mode control from. `snake_case` *values*, unlike
        // the keys around them — matching how every other enum on this boundary is tagged.
        assert_eq!(mode["risk"], "normal");
    }

    /// The whole key set, not a sample, and that is the point.
    ///
    /// No field on [`TerminalSessionView`] is multi-word, so `rename_all` is invisible today
    /// and the usual "does it emit camelCase" check would prove nothing. Asserting the exact
    /// set is what makes the hand-written mirror in `src/lib/ipc/types.ts` checkable: adding
    /// an `is_shell` here would break the frontend silently, and this fails instead.
    #[test]
    fn the_terminal_session_contract_has_exactly_three_keys() {
        let view = TerminalSessionView {
            session: "f0e1".to_owned(),
            worktree: "/x/a".to_owned(),
            project: "/x".to_owned(),
        };
        let json = serde_json::to_value(&view).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();

        assert_eq!(
            keys,
            ["project", "session", "worktree"],
            "the TypeScript mirror in `src/lib/ipc/types.ts` must be updated to match"
        );
    }

    /// The exact key sets for the two agent views, for the same reason as the one above.
    ///
    /// Both are hand-mirrored in `src/lib/ipc/types.ts`, and both have a field the other does
    /// not — `provider` here, `blurb` there — so a copy-paste between them is a real hazard that
    /// nothing else would notice.
    #[test]
    fn the_agent_contracts_have_exactly_the_keys_the_typescript_mirror_declares() {
        let keys_of = |value: &serde_json::Value| {
            let mut keys: Vec<String> = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::clone)
                .collect();
            keys.sort_unstable();
            keys
        };

        let option = AgentOptionView {
            id: "codex".to_owned(),
            label: "Codex".to_owned(),
            blurb: "OpenAI Codex".to_owned(),
            available: false,
            offered: true,
            detail: Some("not on wtm's PATH".to_owned()),
        };
        assert_eq!(
            keys_of(&serde_json::to_value(&option).unwrap()),
            ["available", "blurb", "detail", "id", "label", "offered"]
        );

        let session = AgentSessionView {
            session: "f0e1".to_owned(),
            worktree: "/x/a".to_owned(),
            project: "/x".to_owned(),
            provider: "codex".to_owned(),
            provider_session: "019fd37c".to_owned(),
        };
        assert_eq!(
            keys_of(&serde_json::to_value(&session).unwrap()),
            [
                "project",
                "provider",
                "providerSession",
                "session",
                "worktree"
            ]
        );
    }

    /// The normalized event stream is a discriminated union tagged `kind`.
    ///
    /// The frontend switches on that tag exhaustively, so the tag's spelling *is* the contract.
    /// Serializing one variant of each shape catches the two ways it could silently change:
    /// a `rename_all` slipping to `snake_case` on a payload field, and the `Raw` variant's `event`
    /// field colliding with the tag again if someone renames it back to `kind`.
    #[test]
    fn an_agent_event_is_tagged_by_kind_with_camel_case_payloads() {
        use wtm_core::model::AgentEvent;

        let ready = serde_json::to_value(AgentEvent::SessionReady {
            provider_session_id: "t1".to_owned(),
            model: None,
            effort: None,
            mode: None,
            tools: Vec::new(),
        })
        .unwrap();
        assert_eq!(ready["kind"], "session_ready");
        assert!(
            ready.get("providerSessionId").is_some(),
            "payload fields must be camelCase, got {ready:?}"
        );

        // A second shape: a variant whose payload is a list of structs, which is where a missing
        // `rename_all` on the *inner* type would hide. `AgentSkill`'s fields are single words so
        // camelCase and snake_case coincide today — this asserts the keys the frontend reads, so
        // that renaming one to something with two words fails here rather than in the composer.
        let skills = serde_json::to_value(AgentEvent::SkillsListed {
            skills: vec![wtm_core::model::AgentSkill {
                name: "review".to_owned(),
                description: Some("Review a diff".to_owned()),
                scope: Some("repo".to_owned()),
            }],
        })
        .unwrap();
        assert_eq!(skills["kind"], "skills_listed");
        assert_eq!(skills["skills"][0]["name"], "review");
        assert_eq!(skills["skills"][0]["description"], "Review a diff");
        assert_eq!(skills["skills"][0]["scope"], "repo");

        // A two-word payload field, which is where a missing `rename_all` on a *variant* actually
        // shows: this one is read by the limit banner, and `resets_at` reaching the frontend under
        // that spelling would leave the banner permanently unable to say when the limit lifts —
        // silently, because the field is optional and `undefined` is a legitimate value for it.
        let limit = serde_json::to_value(AgentEvent::LimitReached {
            message: "usage limit reached".to_owned(),
            resets_at: Some(1_755_590_400),
        })
        .unwrap();
        assert_eq!(limit["kind"], "limit_reached");
        assert_eq!(limit["resetsAt"], 1_755_590_400_u64);
        assert!(
            limit.get("resets_at").is_none(),
            "the snake_case spelling must not reach the frontend, got {limit:?}"
        );

        let raw = serde_json::to_value(AgentEvent::Raw {
            provider: "codex".to_owned(),
            event: "item/mcpToolCall/progress".to_owned(),
            payload: serde_json::json!({ "progress": 0.5 }),
        })
        .unwrap();
        assert_eq!(raw["kind"], "raw");
        // `event`, not `kind` — the tag already owns that name, and a variant field of it is a
        // compile error rather than a subtle bug. Asserted so the rename is not undone.
        assert_eq!(raw["event"], "item/mcpToolCall/progress");
    }

    #[test]
    fn an_untrusted_config_error_keeps_its_structured_detail() {
        // The UI needs the command list to render the trust prompt, not just a sentence.
        let err = wtm_core::error::ConfigError::Untrusted {
            path: PathBuf::from("/r/wtm.toml"),
            commands: vec![vec!["./bin/setup.sh".to_owned()]],
            content_hash: "deadbeef".to_owned(),
        };
        let view = ErrorView::from(err);
        assert_eq!(view.kind, "config");
        let detail = view
            .detail
            .expect("detail must survive for the trust prompt");
        assert!(
            detail.to_string().contains("./bin/setup.sh"),
            "got {detail}"
        );
    }

    #[test]
    fn project_view_falls_back_to_the_directory_name() {
        let project = Project {
            id: wtm_core::model::ProjectId::from_root(Path::new("/Users/dev/code/webapp")),
            root: PathBuf::from("/Users/dev/code/webapp"),
            schema_version: 1,
            meta: wtm_core::model::ProjectMeta::default(),
            fields: vec![],
            lookups: vec![],
            computed: vec![],
            naming: wtm_core::model::NamingSpec::default(),
            create: wtm_core::model::CreateSpec::default(),
            setup: None,
            remove: wtm_core::model::RemoveSpec::default(),
            display: wtm_core::model::DisplaySpec::default(),
            actions: vec![],
            agent: std::collections::BTreeMap::new(),
            guards: wtm_core::model::GuardSpec::default(),
        };
        let view = project_view(&project);
        assert_eq!(view.name, "webapp");
        assert!(view.usable);
    }
}

/// The result of re-running setup against an existing worktree.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupResultView {
    pub session: String,
    pub success: bool,
    pub summary: String,
}

/// One live shell in the terminal dock.
///
/// Called `TerminalSession` on the TypeScript side rather than `Terminal`, unlike every other
/// `XView` → `X` pair in this file: `Terminal.svelte` imports `Terminal` from `@xterm/xterm`,
/// and a contract type that shadows the terminal emulator is a fifteen-minute mystery waiting
/// to happen.
///
/// One row per *shell*, not per worktree — a worktree may have several, so `session` is the
/// identifying field and `worktree` says where it is, exactly as [`AgentSessionView`] does. No
/// `argv`: a dock shell is always the login shell, so the field would be a constant the frontend
/// never reads.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionView {
    pub session: String,
    pub worktree: String,
    pub project: String,
}

/// One agent wtm can start, and whether this machine can.
///
/// Reports the unavailable ones too, with the reason — the same choice `OpenersView` makes, and
/// for the same reason: a greyed row saying *"no `codex` on wtm's PATH"* doubles as the diagnosis
/// of this app's most likely production failure, where omitting the row silently is a mystery.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOptionView {
    pub id: String,
    pub label: String,
    pub blurb: String,
    pub available: bool,
    /// Whether the *repository* offers it, which is a different refusal from not being installed.
    ///
    /// Kept apart from [`Self::available`] rather than folded into it, because a greyed row has to
    /// say **which** of the two it is: "install `codex`" and "this repo's `wtm.toml` turned it off"
    /// have different fixes, and one of them is not the user's machine.
    ///
    /// True when no project is in scope, so the startup call before anything is selected still
    /// reports the whole catalogue.
    pub offered: bool,
    /// Why it cannot be used, for a tooltip. `None` when it can.
    pub detail: Option<String>,
}

/// A live agent session, for adopting after a webview reload.
///
/// Keyed by `session` rather than by worktree, unlike [`TerminalSessionView`], because a worktree
/// may have several — which is the feature.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionView {
    pub session: String,
    pub worktree: String,
    pub project: String,
    pub provider: String,
    /// Empty until the provider's handshake has named the conversation.
    pub provider_session: String,
}

/// A stored plan.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BriefView {
    pub id: String,
    pub title: String,
    pub provider: String,
    pub created: String,
    /// The whole document. Small enough to send with the listing — a plan is prose, not a transcript,
    /// and a second round trip per plan to render a list would be the wrong trade.
    pub markdown: String,
}

/// One background agent, as its CLI reports it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskView {
    pub id: String,
    pub name: String,
    /// The CLI's own word — `done`, `failed`, `blocked`, `running`. Not normalized into a wtm enum:
    /// this is another program's vocabulary and it may add to it.
    pub state: String,
    pub session: Option<String>,
}

/// A conversation that can be picked up again.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumableView {
    pub provider: String,
    /// The id that provider knows the conversation by. Handed straight back to resume it.
    pub provider_session: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub updated: Option<String>,
}

/// What an agent can do on this machine.
///
/// A view rather than the domain type crossing directly, for the same reason every other type here
/// is one: `serde(rename_all = "camelCase")` is a property of the boundary, and the domain should not
/// have to carry an opinion about JavaScript's naming conventions to be serialized.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityView {
    pub models: Vec<wtm_core::model::AgentModel>,
    pub modes: Vec<wtm_core::model::AgentMode>,
    /// True when the models came from asking the CLI rather than from a compiled table.
    ///
    /// Surfaced so the UI can say "as reported by codex" against "as of this wtm build" — which is
    /// the difference between a stale list being the CLI's fault and being ours.
    pub models_are_live: bool,
    /// True where the provider has a high-speed mode, so the picker can offer the control.
    pub supports_fast: bool,
}

impl From<wtm_core::model::AgentCapability> for CapabilityView {
    fn from(value: wtm_core::model::AgentCapability) -> Self {
        Self {
            models: value.models,
            modes: value.modes,
            models_are_live: value.models_are_live,
            supports_fast: value.supports_fast,
        }
    }
}

/// Render a preflight item for the checklist.
#[must_use]
pub fn preflight_view(item: &PreflightItem) -> PreflightView {
    PreflightView {
        id: item.id.clone(),
        severity: item.severity,
        message: item.message.clone(),
        overridable: item.overridable,
        hint: item.hint.clone(),
    }
}

/// Render the review screen.
///
/// `normalized` is included so the form can show what a value *became* — the reason a user
/// typing `1234` can see it turn into `ACME-1234` before committing.
#[must_use]
pub fn preview_view(preview: &PlanPreview, values: &wtm_core::model::FormValues) -> PreviewView {
    PreviewView {
        branch: preview
            .plan
            .branch_plan
            .branch()
            .map(|b| b.as_str().to_owned()),
        directory: preview.plan.directory.to_string_lossy().into_owned(),
        base_ref: preview.plan.base_ref.clone(),
        base_commit: preview
            .plan
            .base_commit
            .as_ref()
            .map(|c| c.short().to_owned()),
        will_fetch: preview.plan.will_fetch,
        git_argv: preview.plan.git_argv.clone(),
        setup_argv: preview.plan.setup_argv.clone(),
        setup_cwd: preview
            .plan
            .setup_cwd
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        preflight: preview.preflight.iter().map(preflight_view).collect(),
        warnings: preview.warnings.iter().map(|w| w.message.clone()).collect(),
        lookups: preview.lookups.clone(),
        computed: preview.computed.clone(),
        branch_choices: preview
            .branch_choices
            .iter()
            .map(|choice| BranchChoiceView {
                branch: choice.branch.as_str().to_owned(),
                remote_only: choice.remote_only,
                directory: choice.directory.to_string_lossy().into_owned(),
            })
            .collect(),
        naming_fields: preview.naming_fields.clone(),
        normalized: values
            .normalized
            .iter()
            .map(|(key, value)| (key.clone(), value.as_template_string()))
            .collect(),
        can_create: preview.is_clear(),
    }
}

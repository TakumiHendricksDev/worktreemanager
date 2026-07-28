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

/// One entry of a worktree's environment.
///
/// `value` is `None` for anything [`is_sensitive_key`] flags. That is deliberate and it is
/// the whole point of this type: a secret is not withheld at render time, it is **never
/// sent over IPC at all** until the user explicitly asks for that one key. So a screenshot,
/// a screen-share, or a poke around the webview's memory cannot expose it, because it was
/// never there.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvEntryView {
    pub key: String,
    /// `None` when the key looks sensitive; fetch it with `reveal_env_value`.
    pub value: Option<String>,
    pub sensitive: bool,
}

/// Substrings that mark an environment key as secret-bearing.
///
/// Matched case-insensitively against the key name, never the value. Names are the only
/// signal available — inspecting values would mean heuristics over the very data we are
/// trying not to handle.
///
/// Tuned to over-match rather than under-match: a masked port number costs one click, a
/// leaked Stripe key costs an incident. `SSH_AUTH_SOCK` is included because although the
/// path itself is not a credential, it addresses the agent that holds your signing keys.
const SENSITIVE_MARKERS: &[&str] = &[
    "secret",
    "token",
    "password",
    "passwd",
    "credential",
    "private",
    "api_key",
    "apikey",
    "access_key",
    "auth",
    "signature",
    "salt",
    "dsn",
    "session",
    "cookie",
    "license",
    "sentry",
];

/// Keys that contain one of the markers but are not actually sensitive.
///
/// Deliberately almost empty. `AWS_ACCESS_KEY_ID` was here once, on the reasonable-sounding
/// argument that it is the public half of a key pair — until a leak test against a real
/// `.env` showed that a local `MinIO` setup uses *the same string* for the access key, the
/// secret key, the `MinIO` user and the `MinIO` password. The exception was therefore publishing
/// the secret verbatim. The lesson: do not exempt a key because of what its name implies
/// about its value.
const SENSITIVE_EXCEPTIONS: &[&str] = &["SECRET_KEY_FALLBACKS"];

/// Minimum length for a value to be worth cross-checking.
///
/// Below this, `contains` matches are coincidence — a port number or a single word will
/// appear inside unrelated text and mask half the panel for no reason.
const MIN_CROSS_CHECK_LEN: usize = 8;

/// Whether an environment key's *name* suggests it holds a credential.
///
/// One signal, not the whole answer — see [`classify_env`]. A name-only check cannot catch
/// `DATABASE_URL=postgres://user:hunter2@host`, where the key reads as harmless and the
/// credential is inside the value.
#[must_use]
pub fn is_sensitive_key(key: &str) -> bool {
    if SENSITIVE_EXCEPTIONS
        .iter()
        .any(|exception| exception.eq_ignore_ascii_case(key))
    {
        return false;
    }
    let lower = key.to_ascii_lowercase();
    // A bare `KEY` or `*_KEY` is a credential often enough to be worth masking, but
    // `HOST_PORT_*` style names must not trip it, so require it to be a whole word.
    if lower == "key" || lower.ends_with("_key") || lower.starts_with("key_") {
        return true;
    }
    SENSITIVE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Whether a value looks like a URL with embedded credentials.
///
/// `scheme://user:password@host` is a structural signal rather than a guess, which is what
/// makes it worth checking: it catches a connection string regardless of what the key is
/// called.
#[must_use]
pub fn value_embeds_credentials(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    {
        return false;
    }
    // Userinfo is everything before the first `@`, and must itself contain a `:` for there
    // to be a password rather than just a username.
    let Some((userinfo, _)) = rest.split_once('@') else {
        return false;
    };
    !userinfo.is_empty() && userinfo.contains(':') && !userinfo.contains('/')
}

/// Classify a whole environment map, deciding what may cross the IPC boundary.
///
/// Three signals, applied together, because any one alone leaks:
///
/// 1. **The key name** — `STRIPE_API_KEY`.
/// 2. **A credential embedded in the value** — `DATABASE_URL=postgres://u:pw@host`, whose
///    key name is entirely innocent.
/// 3. **The value matching another entry's secret** — the case a real `.env` exposed: a dev
///    `MinIO` setup where the access key, secret key, username and password are all the same
///    string, so masking only the obviously-named ones publishes the secret anyway.
///
/// Signal 3 is a consistency check within the file rather than a heuristic about the world,
/// which is why it is the strongest of the three.
#[must_use]
pub fn classify_env(values: &BTreeMap<String, String>) -> Vec<EnvEntryView> {
    // Pass one: names and value structure.
    let mut sensitive: BTreeMap<&String, bool> = values
        .iter()
        .map(|(key, value)| {
            (
                key,
                is_sensitive_key(key) || value_embeds_credentials(value),
            )
        })
        .collect();

    // Pass two: any value already known to be secret becomes a needle. An entry whose value
    // *contains* one is publishing it, whatever the key is called.
    let needles: Vec<&str> = values
        .iter()
        .filter(|(key, value)| {
            sensitive.get(key).copied().unwrap_or(false) && value.len() >= MIN_CROSS_CHECK_LEN
        })
        .map(|(_, value)| value.as_str())
        .collect();

    for (key, value) in values {
        if sensitive.get(key).copied().unwrap_or(false) {
            continue;
        }
        if needles.iter().any(|needle| value.contains(needle)) {
            sensitive.insert(key, true);
        }
    }

    values
        .iter()
        .map(|(key, value)| {
            let is_secret = sensitive.get(key).copied().unwrap_or(false);
            EnvEntryView {
                key: key.clone(),
                value: if is_secret { None } else { Some(value.clone()) },
                sensitive: is_secret,
            }
        })
        .collect()
}

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
    /// The project's declared display source, for the Env tab. Sensitive values are
    /// withheld — see [`EnvEntryView`].
    pub env: Vec<EnvEntryView>,
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
            guards: wtm_core::model::GuardSpec::default(),
        };
        let view = project_view(&project);
        assert_eq!(view.name, "webapp");
        assert!(view.usable);
    }
}

#[cfg(test)]
mod sensitivity_tests {
    use super::*;

    /// Every secret-bearing key actually present in the reference project's `.env`.
    ///
    /// Taken from the real file, because a heuristic tested only against invented names is
    /// a heuristic tested against the author's imagination.
    #[test]
    fn masks_every_real_credential_in_the_reference_env() {
        for key in [
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "EMAIL_HOST_PASSWORD",
            "GEOCODIO_API_KEY",
            "GOOGLE_API_KEY",
            "MINIO_ROOT_PASSWORD",
            "OPENAI_API_KEY",
            "PGPASSWORD",
            "POSTGRES_PASSWORD",
            "SECRET_KEY",
            "STRIPE_API_KEY",
            "STRIPE_WEBHOOK_SECRET",
            "TRANSACT_API_KEY",
            "SSH_AUTH_SOCK",
        ] {
            assert!(is_sensitive_key(key), "{key} must be masked");
        }
    }

    #[test]
    fn leaves_ordinary_configuration_visible() {
        // Masking these would make the panel useless, which is how a masking feature ends
        // up switched off.
        for key in [
            "HOST_PORT_WEB",
            "HOST_PORT_DB",
            "HOST_PORT_MAILPIT_UI",
            "COMPOSE_PROJECT_NAME",
            "DOMAIN",
            "POSTGRES_DB",
            "POSTGRES_USER",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "AWS_S3_ENDPOINT_URL",
            "AWS_STORAGE_BUCKET_NAME",
            "AWS_S3_PREFIX",
            "DEBUG",
            "PYTHONPATH",
        ] {
            assert!(!is_sensitive_key(key), "{key} should stay visible");
        }
    }

    /// A credential inside a URL, under a key name that gives nothing away.
    #[test]
    fn detects_credentials_embedded_in_a_connection_string() {
        assert!(value_embeds_credentials(
            "postgres://user:hunter2@localhost:5432/db"
        ));
        assert!(value_embeds_credentials("redis://:pw@127.0.0.1:6379"));
        assert!(value_embeds_credentials("amqps://u:p@broker.example"));

        // Must not fire on an ordinary URL, or every endpoint gets masked.
        assert!(!value_embeds_credentials("http://127.0.0.1:8007"));
        assert!(!value_embeds_credentials("https://example.com/path"));
        assert!(!value_embeds_credentials("postgres://localhost:5432/db"));
        // A colon in the path is not userinfo.
        assert!(!value_embeds_credentials("http://host/a:b"));
        assert!(!value_embeds_credentials("not a url at all"));
        assert!(!value_embeds_credentials(""));
    }

    /// The regression this whole mechanism exists for.
    ///
    /// A real `.env` used one string for the `MinIO` user, the `MinIO` password, the `AWS` access
    /// key and the `AWS` secret. Masking only the obviously-named keys published it anyway.
    #[test]
    fn masks_a_secret_duplicated_under_an_innocent_key_name() {
        let shared = "supersecretvalue123";
        let values: BTreeMap<String, String> = [
            ("AWS_SECRET_ACCESS_KEY", shared),
            ("AWS_ACCESS_KEY_ID", shared),
            ("MINIO_ROOT_USER", shared),
            ("MINIO_ROOT_PASSWORD", shared),
            ("HOST_PORT_WEB", "8007"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();

        let entries = classify_env(&values);
        let by_key = |key: &str| entries.iter().find(|e| e.key == key).expect("entry");

        for key in [
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ACCESS_KEY_ID",
            "MINIO_ROOT_USER",
            "MINIO_ROOT_PASSWORD",
        ] {
            let entry = by_key(key);
            assert!(
                entry.sensitive,
                "{key} shares a secret value and must be masked"
            );
            assert!(entry.value.is_none(), "{key} still carried its value");
        }

        // And the port stays useful.
        let port = by_key("HOST_PORT_WEB");
        assert!(!port.sensitive);
        assert_eq!(port.value.as_deref(), Some("8007"));
    }

    /// The other real case: a harmless key name holding a connection string.
    #[test]
    fn masks_a_database_url_and_anything_repeating_its_password() {
        let values: BTreeMap<String, String> = [
            ("DATABASE_URL", "postgres://app:tr0ub4dor&3@db:5432/app"),
            ("PGPASSWORD", "tr0ub4dor&3"),
            ("POSTGRES_DB", "app"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();

        let entries = classify_env(&values);
        let by_key = |key: &str| entries.iter().find(|e| e.key == key).expect("entry");

        assert!(
            by_key("DATABASE_URL").sensitive,
            "a URL with userinfo must be masked"
        );
        assert!(by_key("PGPASSWORD").sensitive);
        assert!(
            !by_key("POSTGRES_DB").sensitive,
            "the database name is not a secret"
        );
    }

    #[test]
    fn a_short_shared_value_does_not_mask_the_whole_panel() {
        // Cross-checking on very short values would match coincidentally and mask
        // everything, which is how a masking feature gets switched off.
        let values: BTreeMap<String, String> = [("API_KEY", "abc"), ("HOST_PORT_WEB", "8007")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();

        let entries = classify_env(&values);
        let port = entries
            .iter()
            .find(|e| e.key == "HOST_PORT_WEB")
            .expect("entry");
        assert!(!port.sensitive);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(is_sensitive_key("stripe_api_key"));
        assert!(is_sensitive_key("Secret_Key"));
        assert!(is_sensitive_key("DATABASE_PASSWORD"));
    }

    #[test]
    fn a_bare_key_suffix_is_masked_but_a_port_name_is_not() {
        assert!(is_sensitive_key("KEY"));
        assert!(is_sensitive_key("SIGNING_KEY"));
        assert!(is_sensitive_key("KEY_MATERIAL"));
        // The trap this guards: `*_KEY` matching must not be a bare substring test, or
        // every `HOST_PORT_*` name with `key` in it would be masked.
        assert!(!is_sensitive_key("KEYBOARD_LAYOUT"));
        assert!(!is_sensitive_key("MONKEY_PATCH"));
    }

    /// A sensitive value must not be serializable by accident.
    #[test]
    fn a_sensitive_entry_serializes_with_a_null_value() {
        let entry = EnvEntryView {
            key: "STRIPE_API_KEY".to_owned(),
            value: None,
            sensitive: true,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"value\":null"), "got {json}");
        assert!(json.contains("\"sensitive\":true"));
        // The obvious catastrophe: the secret appearing in the payload anyway.
        assert!(!json.contains("sk_"), "got {json}");
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
        normalized: values
            .normalized
            .iter()
            .map(|(key, value)| (key.clone(), value.as_template_string()))
            .collect(),
        can_create: preview.is_clear(),
    }
}

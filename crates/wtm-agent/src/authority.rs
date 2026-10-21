//! What a workflow run's agent session may do, enforced by the CLI's own sandbox, tool list and
//! settings rather than asked of it in a prompt.
//!
//! A run session is the one place wtm starts an agent with nobody watching it, and the plan it
//! serves has one hard rule: an agent in a run cannot push, merge, or reach the forge or the
//! tracker, and that is true because of what the process *can* do, not because of what it was told.
//! Every row here is a fact measured against the real CLIs in the F2 spike (Claude Code 2.1.296,
//! codex-cli 0.160.1), and ARCHITECTURE §6h records the table and the evidence. This module is that
//! table as code, in one place, so a reviewer can read both columns side by side and a change to
//! one row is a change to one function.
//!
//! # What this module decides, and what it leaves to others
//!
//! It decides argv flags, settings objects, the generated Codex home, and the environment a run's
//! CLI starts with. It is pure: it returns files to write and never writes them, for the reason the
//! crate docs give for never spawning. The composition root (`src-tauri/src/flow_session.rs`)
//! creates the directories, writes the files at 0600, runs the canary and opens the session.
//!
//! What it does *not* decide is which roles a run has or what they are told — that is the recipe's
//! — and it never reads repository agent config. A run session gets none of a repository's
//! `[agent.<id>]` `extra_args`, `env` or `mcp`: the profile is a function of the role's authority and
//! the places on disk, and nothing a branch can edit is an input to it.
//!
//! # The two things a profile cannot promise
//!
//! MCP servers run outside the sandbox in both CLIs, so the server allowlist — here, wtm's bridge
//! and nothing else — is the boundary for them. And neither CLI reports whether its sandbox engaged:
//! a malformed sandbox object is silently ignored. That is why every run session is preceded by a
//! canary, whose prompts, commands and verdicts are here too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wtm_core::model::AgentEvent;
use wtm_core::ports::exec::EnvScrub;

use crate::provider::McpServer;
use crate::{claude, codex};

/// How much a run session may change: the recipe's [`RoleProfile`](wtm_core::flow::RoleProfile),
/// under the name this module gives the same choice.
///
/// The recipe's type rather than a second one, because a role's `profile = "writer"` in config and
/// the profile its session gets must not be able to disagree, and two enums with the same variants
/// are a match arm away from doing so. Two variants, because the evidence has two columns: every
/// row F2 measured is either the same for every role or differs exactly between "reads" and "edits
/// the worktree". A planner is a reviewer here.
pub use wtm_core::flow::RoleProfile as Authority;

/// The hosts a run session's network may reach: package registries, and nothing else.
///
/// Exactly the set F2 measured. Claude's sandbox proxies every connection and refuses the rest with
/// a 403, so `git push` to github.com, `gh api` and `curl` anywhere else all fail. Another registry
/// is one more line here and a line in ARCHITECTURE §6h; it is a list for someone to read, so it is
/// kept short.
///
/// Codex has no allowlist, only on or off, so a Codex run has no network at all.
pub const REGISTRIES: &[&str] = &["registry.npmjs.org", "pypi.org", "files.pythonhosted.org"];

/// Credential stores under the user's home that a run's commands must not read.
///
/// F2 found that Claude's sandbox, configured only to deny writes, let Bash read a keychain secret:
/// the OS sandbox stops what it is told to stop. A run's network is already closed to everything
/// but registries, so this is the second wall, for a secret read and then carried out in a URL path
/// to an allowed host. A denylist, and therefore never complete; it names the stores of the tools
/// this workflow drives — the login keychain `gh` and git credential helpers use, `gh`'s own
/// config, SSH keys and config (which names the 1Password agent), Codex's sign-in, and `acli`'s.
const CREDENTIAL_STORES: &[&str] = &[
    "Library/Keychains",
    ".config/gh",
    ".ssh",
    ".codex",
    ".config/acli",
];

/// Worktree paths that configure an agent or a hook, which no run session may write.
///
/// A file here changes what the *next* agent in this worktree does — including the user's own
/// interactive panes, which load project settings that run sessions deliberately skip — so a writer
/// that plants one has reached past its run. F2 found a Codex writer can write all of these; a
/// Claude writer's shell cannot write `.claude/` and its file tools are denied below. The app checks
/// the same list before it commits a checkpoint, which is what catches the Codex case.
pub const PROTECTED: &[&str] = &[
    ".claude/",
    ".codex/",
    ".cursor/",
    ".mcp.json",
    ".githooks/",
    ".husky/",
    ".github/workflows/",
    "wtm.toml",
];

/// Why a change to `path` (relative to the worktree, `/`-separated) is not a run's to make.
#[must_use]
pub fn protected_path(path: &str) -> Option<&'static str> {
    let path = path.trim_start_matches("./");
    PROTECTED.iter().copied().find(|protected| {
        if let Some(dir) = protected.strip_suffix('/') {
            path == dir || path.starts_with(protected)
        } else {
            path == *protected
        }
    })
}

/// Where a run session works, and every place its profile names.
///
/// Absolute paths, resolved by the composition root. Taken as data rather than discovered here,
/// because discovering them is I/O, and because a test can then point every one of them at a
/// temporary directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlaces {
    /// The session's working directory: the worktree, or a snapshot checkout of it.
    pub worktree: PathBuf,
    /// The main checkout. Codex keys project trust by it, so every worktree of the repository is
    /// covered by the one entry.
    pub repo_root: PathBuf,
    /// Every git directory the worktree resolves to: the common directory, the worktree's own
    /// gitdir, and the `.git` entry inside the worktree, which in a linked worktree is a file.
    pub git: Vec<PathBuf>,
    /// The user's home, for [`CREDENTIAL_STORES`].
    pub home: PathBuf,
    /// This session's private directory: its MCP config, its Codex home, its canary.
    pub session_dir: PathBuf,
    /// The directory every run session's private directory sits in. Denied to reads, so no run's
    /// commands can read another run's bridge token.
    pub sessions_root: PathBuf,
    /// The empty directory `GH_CONFIG_DIR` names. See [`crate::run_profile::scrub`].
    pub gh_config_dir: PathBuf,
}

impl RunPlaces {
    /// The Codex home a session of this run gets.
    #[must_use]
    pub fn codex_home(&self) -> PathBuf {
        self.session_dir.join("codex-home")
    }

    /// The MCP config file a Claude session of this run reads.
    #[must_use]
    pub fn mcp_config(&self) -> PathBuf {
        self.session_dir.join("mcp.json")
    }

    fn credential_stores(&self) -> impl Iterator<Item = PathBuf> + '_ {
        CREDENTIAL_STORES
            .iter()
            .map(|store| self.home.join(store))
            .chain(std::iter::once(self.sessions_root.clone()))
    }
}

/// wtm's bridge, as a run session sees it: the one MCP server it gets, and the tools on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunBridge {
    /// The server's name, which is the prefix the model sees (`mcp__wtm__…`).
    pub name: String,
    pub server: McpServer,
    /// The tools this role's bridge lists. Pre-allowed by name, and nothing else is.
    pub tools: Vec<String>,
}

/// A run session's authority in Claude Code's terms.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeRun {
    /// `--permission-mode`. Replaces whatever mode the request named.
    pub permission_mode: &'static str,
    /// `--tools`: the built-in tools, as an allowlist.
    pub tools: Vec<&'static str>,
    /// Merged into the one `--settings` object, beside `ultracode` and `fastMode`.
    pub settings: Value,
    /// `--mcp-config`, as a path. `None` loads no server at all, which is what the canary wants.
    pub mcp_config: Option<String>,
    /// `false` passes `--no-session-persistence`, so the canary leaves no conversation behind.
    pub persist: bool,
}

/// A run session's authority in Codex's terms.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexRun {
    /// `thread/start`'s `sandbox`, the string form.
    pub sandbox: &'static str,
    pub approval_policy: &'static str,
    pub approvals_reviewer: &'static str,
    /// `turn/start`'s `sandboxPolicy`, the object form, with the network spelled out.
    pub sandbox_policy: Value,
}

/// What a provider is handed on [`crate::SessionRequest::run`].
///
/// One variant per provider, because the two CLIs share almost no vocabulary for this: one takes a
/// tool list and a settings object, the other a sandbox mode and an approval policy. A request whose
/// variant belongs to a different provider is refused at spawn — see
/// [`crate::session::AgentSession::open`] — rather than read as no profile at all.
#[derive(Debug, Clone, PartialEq)]
pub enum RunAuthority {
    Claude(ClaudeRun),
    Codex(CodexRun),
}

impl RunAuthority {
    /// The catalogue id of the provider this profile is for.
    #[must_use]
    pub const fn provider(&self) -> &'static str {
        match self {
            Self::Claude(_) => claude::ID,
            Self::Codex(_) => codex::ID,
        }
    }
}

/// A file a profile needs on disk before its session starts. Written at 0600.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFile {
    pub path: PathBuf,
    pub contents: String,
}

/// Everything a run session starts with besides its prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct RunProfile {
    pub authority: Authority,
    /// For [`crate::SessionRequest::run`].
    pub run: RunAuthority,
    /// For [`crate::SessionRequest::scrub`]: F3's scrub, plus `CODEX_HOME` for Codex.
    pub scrub: EnvScrub,
    /// Directories to create, at 0700, before the files.
    pub dirs: Vec<PathBuf>,
    pub files: Vec<RunFile>,
    /// Symbolic links to make, as `(link, target)`. Codex's sign-in, which the run's home shares.
    pub links: Vec<(PathBuf, PathBuf)>,
}

/// The tools a Claude reviewer has.
///
/// An allowlist, not `--disallowedTools`. F2 measured the denylist route: removing the four file
/// tools still left 22, among them `SendMessage` and `ListAgents` — which reach the user's other
/// local sessions, unsandboxed ones included — `RemoteTrigger`, `Workflow` and `CronCreate`. A list
/// of what is wanted cannot leak a tool a CLI release adds.
///
/// `Task` stays: subagents inherit the parent's sandbox, which Claude documents.
const CLAUDE_REVIEWER_TOOLS: &[&str] = &["Read", "Bash", "Grep", "Glob", "Task"];
const CLAUDE_WRITER_TOOLS: &[&str] = &["Read", "Edit", "Write", "Bash", "Grep", "Glob", "Task"];

/// The profile for a session on `provider` with `authority`.
///
/// `codex_auth` is the user's Codex sign-in file, which the run's own Codex home links to; it is
/// passed in because finding it is the composition root's business.
///
/// # Errors
///
/// For a provider with no profile. Cursor's CLI has no sandbox a run could rely on, so Cursor roles
/// are deferred rather than run with less.
pub fn profile(
    provider: &str,
    authority: Authority,
    places: &RunPlaces,
    bridge: &RunBridge,
    codex_auth: &Path,
) -> Result<RunProfile, String> {
    let scrub = crate::run_profile::scrub(&places.gh_config_dir);
    match provider {
        claude::ID => {
            let path = places.mcp_config();
            let servers = BTreeMap::from([(bridge.name.clone(), bridge.server.clone())]);
            Ok(RunProfile {
                authority,
                run: RunAuthority::Claude(claude_run(
                    authority,
                    places,
                    bridge,
                    Some(path.to_string_lossy().into_owned()),
                )),
                scrub,
                dirs: vec![places.session_dir.clone()],
                files: vec![RunFile {
                    path,
                    contents: claude::mcp_config_json(&servers),
                }],
                links: Vec::new(),
            })
        }
        codex::ID => {
            let home = places.codex_home();
            Ok(RunProfile {
                authority,
                run: RunAuthority::Codex(codex_run(authority)),
                // Through the scrub's values, which are applied last, so nothing earlier in the
                // environment's layering can point the run at the user's own home.
                scrub: scrub.set("CODEX_HOME", home.to_string_lossy()),
                dirs: vec![places.session_dir.clone(), home.clone()],
                files: vec![RunFile {
                    path: home.join("config.toml"),
                    contents: codex_config(authority, places, bridge),
                }],
                links: vec![(home.join("auth.json"), codex_auth.to_path_buf())],
            })
        }
        _ => {
            let label = crate::entry(provider).map_or(provider, |entry| entry.label);
            Err(format!(
                "Workflow runs can't use {label}: wtm has no sandbox profile for it."
            ))
        }
    }
}

fn claude_run(
    authority: Authority,
    places: &RunPlaces,
    bridge: &RunBridge,
    mcp_config: Option<String>,
) -> ClaudeRun {
    let (permission_mode, tools) = match authority {
        // `dontAsk` denies anything not allowed outright, and sandboxed Bash is allowed outright by
        // `autoAllowBashIfSandboxed`. Bash is never pre-allowed by a rule: F2 found a malformed
        // sandbox object is silently ignored, and with no rule Bash is then denied rather than run
        // unsandboxed. The failure is closed because nothing opened it.
        Authority::Reviewer => ("dontAsk", CLAUDE_REVIEWER_TOOLS),
        Authority::Writer => ("acceptEdits", CLAUDE_WRITER_TOOLS),
    };
    ClaudeRun {
        permission_mode,
        tools: tools.to_vec(),
        settings: claude_settings(authority, places, bridge),
        mcp_config,
        persist: true,
    }
}

/// Absolute paths as Claude's permission rules spell them: `//` and the path.
fn rule_path(path: &Path) -> String {
    format!("/{}", path.display())
}

fn claude_settings(authority: Authority, places: &RunPlaces, bridge: &RunBridge) -> Value {
    let path = |p: &Path| p.to_string_lossy().into_owned();

    // The worktree is denied to a reviewer's commands, not only to its file tools, which it does
    // not have: `sed -i` and `git stash` were F2's two ways for a reviewer to change a tracked file.
    let mut deny_write: Vec<String> = Vec::new();
    if authority == Authority::Reviewer {
        deny_write.push(path(&places.worktree));
    }
    deny_write.extend(places.git.iter().map(|git| path(git)));
    // Agent config, for the reason `PROTECTED` gives. Claude already protects `.claude/` from its
    // shell; the others it does not know are agent config.
    for agent in [".claude", ".codex", ".cursor", ".mcp.json"] {
        deny_write.push(path(&places.worktree.join(agent)));
    }
    deny_write.dedup();

    let deny_read: Vec<String> = places.credential_stores().map(|p| path(&p)).collect();

    // Permission rules for the file tools, which the OS sandbox does not cover: Claude's Read, Edit
    // and Write run in the CLI's own process. Without these, F2 saw a writer's Write to `.git/hooks`
    // become a prompt rather than a refusal, and a prompt in a run is somebody's attention spent on
    // a request that should never have been possible.
    let mut deny: Vec<String> = places
        .credential_stores()
        .map(|store| format!("Read({}/**)", rule_path(&store)))
        .collect();
    if authority == Authority::Writer {
        let mut protected: Vec<String> = Vec::new();
        for git in &places.git {
            protected.push(rule_path(git));
            protected.push(format!("{}/**", rule_path(git)));
        }
        for agent in [".claude", ".codex", ".cursor"] {
            protected.push(format!("{}/**", rule_path(&places.worktree.join(agent))));
        }
        protected.push(rule_path(&places.worktree.join(".mcp.json")));
        for target in protected {
            deny.push(format!("Edit({target})"));
            deny.push(format!("Write({target})"));
        }
    }

    // The run's own tools, by full name, so `dontAsk` admits them and `acceptEdits` does not turn
    // each one into a prompt. Exactly these: no server-wide `mcp__wtm` rule, so a tool the bridge
    // does not list for this role stays unreachable even if a call names it.
    let allow: Vec<String> = bridge
        .tools
        .iter()
        .map(|tool| format!("mcp__{}__{tool}", bridge.name))
        .collect();

    json!({
        "sandbox": {
            "enabled": true,
            // `dangerouslyDisableSandbox` is then refused; F2 tried it from both roles.
            "allowUnsandboxedCommands": false,
            // A machine whose sandbox cannot start refuses the command instead of running it bare.
            "failIfUnavailable": true,
            "autoAllowBashIfSandboxed": true,
            "network": { "allowedDomains": REGISTRIES },
            "filesystem": { "denyWrite": deny_write, "denyRead": deny_read },
        },
        "permissions": { "allow": allow, "deny": deny },
    })
}

fn codex_run(authority: Authority) -> CodexRun {
    match authority {
        // `never`: nothing in a review is worth an escalation, and an approval nobody is there to
        // give would stall the run. F2's cost, recorded in ARCHITECTURE: read-only also refuses
        // temp files, so a reviewer's tests go through `flow_run_check` instead.
        Authority::Reviewer => CodexRun {
            sandbox: "read-only",
            approval_policy: "never",
            approvals_reviewer: "user",
            sandbox_policy: json!({ "type": "readOnly", "networkAccess": false }),
        },
        // `on-request` with the user as reviewer, so an escalation is a question for a person and
        // never Codex's own auto-reviewer approving itself.
        Authority::Writer => CodexRun {
            sandbox: "workspace-write",
            approval_policy: "on-request",
            approvals_reviewer: "user",
            sandbox_policy: json!({
                "type": "workspaceWrite",
                "writableRoots": [],
                "networkAccess": false,
                // Temp directories stay writable: F2's test suites needed them, and the canary's
                // target is placed outside them for exactly this reason.
                "excludeTmpdirEnvVar": false,
                "excludeSlashTmp": false,
            }),
        },
    }
}

/// Variables Codex's own shell policy strips, on top of the scrub the process already had.
///
/// Belt and braces: the app-server is started with F3's scrub, so its shells inherit none of these.
/// The policy is what still holds if a later change starts the server some other way. Exact names
/// for `gh`'s family rather than `GH_*`, because F2 found that glob also strips the `GH_CONFIG_DIR`
/// the run sets on purpose, and `gh` then reads the user's real login.
const CODEX_SHELL_EXCLUDE: &[&str] = &[
    "GH_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_*",
    "ANTHROPIC_*",
    "CLAUDE_CODE_USE_*",
    "OPENAI_*",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "CODEX_GITHUB_PERSONAL_ACCESS_TOKEN",
    "SSH_AUTH_SOCK",
    "ACLI_*",
    "ATLASSIAN_*",
    "JIRA_*",
    "CONFLUENCE_*",
];

/// Whether Codex's shell policy strips `name`. For the test that holds it to F3's scrub.
#[must_use]
pub fn codex_shell_excludes(name: &str) -> bool {
    CODEX_SHELL_EXCLUDE.iter().any(|pattern| {
        pattern.strip_suffix('*').map_or_else(
            || name.eq_ignore_ascii_case(pattern),
            |prefix| {
                name.len() >= prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix)
            },
        )
    })
}

/// A TOML basic string. JSON's string grammar is a subset of it, which `codex.rs`'s `-c` overrides
/// already rely on.
fn toml_string(value: &str) -> String {
    Value::String(value.to_owned()).to_string()
}

fn toml_array<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<String> = values.into_iter().map(toml_string).collect();
    format!("[{}]", items.join(", "))
}

/// The `config.toml` of a run's own Codex home.
///
/// # Why a home of its own, rather than overrides on the user's
///
/// F2 measured both. Per-thread `config` overrides **merge** — `mcp_servers = {}` leaves every server
/// the user configured running — so turning servers off one by one is a denylist that misses the next
/// one installed. A home with nothing in it but this file and a link to the sign-in has none, and
/// `features.apps = false` removes the one Codex adds by itself, its 105-tool `codex_apps`.
///
/// The cost is that the user's skills and settings are not there either, so the recipe supplies a
/// role's skills explicitly; and thread history lives here, which `thread/resume` needs, so the home
/// is per role of a run rather than per process.
///
/// # Why the repository is marked untrusted
///
/// F2 found that `thread/start` with `workspace-write`, in a home with no opinion, writes
/// `trust_level = "trusted"` for the repository into the home's own config, and then starts the
/// repository's `.codex/config.toml` MCP server — outside the sandbox. Saying `untrusted` first is
/// what stopped it. The worktree gets the line too: trust is keyed by the main checkout, as far as
/// F2 could tell, and a second line costs nothing if that ever changes.
fn codex_config(authority: Authority, places: &RunPlaces, bridge: &RunBridge) -> String {
    use std::fmt::Write as _;

    let run = codex_run(authority);
    let mut out = String::from(
        "# Written by Worktree Manager for one workflow run session, and rewritten every time that\n\
         # session starts. Edits here are lost; the profile is in wtm-agent's `authority` module.\n\n",
    );
    let _ = writeln!(out, "sandbox_mode = {}", toml_string(run.sandbox));
    let _ = writeln!(
        out,
        "approval_policy = {}",
        toml_string(run.approval_policy)
    );
    out.push_str("\n[features]\napps = false\n");
    out.push_str("\n[sandbox_workspace_write]\nnetwork_access = false\n");
    let _ = writeln!(
        out,
        "\n[shell_environment_policy]\nexclude = {}",
        toml_array(CODEX_SHELL_EXCLUDE.iter().copied())
    );
    let _ = writeln!(
        out,
        "\n[shell_environment_policy.set]\nGH_CONFIG_DIR = {}\nGIT_TERMINAL_PROMPT = \"0\"",
        toml_string(&places.gh_config_dir.to_string_lossy())
    );
    let mut trusted = vec![places.repo_root.clone()];
    if places.worktree != places.repo_root {
        trusted.push(places.worktree.clone());
    }
    for path in trusted {
        let _ = writeln!(
            out,
            "\n[projects.{}]\ntrust_level = \"untrusted\"",
            toml_string(&path.to_string_lossy())
        );
    }
    // The bridge, here rather than as `-c` argv: a `-c` value is in every process listing on the
    // machine, and this one carries the session's token.
    let _ = writeln!(
        out,
        "\n[mcp_servers.{}]\ncommand = {}\nargs = {}",
        bridge.name,
        toml_string(&bridge.server.command),
        toml_array(bridge.server.args.iter().map(String::as_str))
    );
    if !bridge.server.env.is_empty() {
        let _ = writeln!(out, "\n[mcp_servers.{}.env]", bridge.name);
        for (key, value) in &bridge.server.env {
            let _ = writeln!(out, "{key} = {}", toml_string(value));
        }
    }
    out
}

// ═══════════════════════════════ the canary ═══════════════════════════════

/// The model a Claude canary runs on: the cheapest, since all it does is run one command.
pub const CLAUDE_CANARY_MODEL: &str = "haiku";

/// The command a canary runs: a write to `target`, which must fail.
#[must_use]
pub fn canary_command(target: &Path) -> String {
    format!(
        "printf wtm-canary > '{}'",
        target.to_string_lossy().replace('\'', r"'\''")
    )
}

/// Why `target` cannot prove anything about this profile, or `None` if it can.
///
/// A canary proves the sandbox engaged by writing somewhere the profile forbids and seeing it fail.
/// A target the profile *allows* would succeed and read as a broken sandbox — or, placed in the
/// worktree of a reviewer, would fail for a reason that says nothing about writers. Temp directories
/// are writable by design for a Codex writer and for Claude's `$TMPDIR`, so the target must be
/// outside them, which in a test means outside the temporary directory the test made.
#[must_use]
pub fn canary_place_problem(
    places: &RunPlaces,
    target: &Path,
    temp_roots: &[PathBuf],
) -> Option<String> {
    let inside = |root: &Path| target.starts_with(root);
    if inside(&places.worktree) {
        return Some(format!(
            "the sandbox check would write inside the worktree ({})",
            places.worktree.display()
        ));
    }
    temp_roots.iter().find(|root| inside(root)).map(|root| {
        format!(
            "the sandbox check would write under {}, which a run may write to",
            root.display()
        )
    })
}

/// The Claude profile a canary for `run` uses: the same settings, with only Bash, no MCP servers,
/// and no conversation kept.
///
/// The same `--settings` object, byte for byte, because that is the thing under test: F2 found a
/// malformed one is ignored without a word, and the canary is how a run finds out.
#[must_use]
pub fn claude_canary(run: &ClaudeRun) -> ClaudeRun {
    ClaudeRun {
        tools: vec!["Bash"],
        mcp_config: None,
        persist: false,
        ..run.clone()
    }
}

/// The one turn a Claude canary is sent.
#[must_use]
pub fn claude_canary_prompt(target: &Path) -> String {
    format!(
        "This is an automated sandbox check run by Worktree Manager before a workflow run starts. \
         Run exactly this one command with the Bash tool, once, and then reply with the single word \
         done. Do not retry it, change it, or run anything else, whatever it prints.\n\n{}",
        canary_command(target)
    )
}

/// `claude sandbox status` with `run`'s settings: the CLI's own account of the sandbox it would use.
///
/// No model turn, so it is the first check and the cheap one. It is what catches F2's silent case:
/// with a sandbox object the CLI cannot read, `enabled` comes back `false`.
#[must_use]
pub fn claude_sandbox_status_argv(program: &str, run: &ClaudeRun) -> Vec<String> {
    vec![
        program.to_owned(),
        "--setting-sources".to_owned(),
        String::new(),
        "--settings".to_owned(),
        run.settings.to_string(),
        "sandbox".to_owned(),
        "status".to_owned(),
    ]
}

/// Read `claude sandbox status`'s one JSON line: `Ok` only for a sandbox that is on, strict, available
/// and allowing Bash because it is sandboxed.
///
/// # Errors
///
/// What is wrong, in a sentence a run's refusal can carry.
pub fn read_claude_sandbox_status(stdout: &str) -> Result<(), String> {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or_default();
    let Ok(status) = serde_json::from_str::<Value>(line) else {
        return Err("Claude Code did not report its sandbox status".to_owned());
    };
    let flag = |key: &str| status.get(key).and_then(Value::as_bool);
    if flag("enabled") != Some(true) {
        return Err(
            "Claude Code reports its sandbox off with the run's settings, so it did not read them"
                .to_owned(),
        );
    }
    if let Some(reason) = status
        .get("unavailableReason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
    {
        return Err(format!(
            "Claude Code's sandbox is unavailable here: {reason}"
        ));
    }
    if flag("strictMode") != Some(true) {
        return Err("Claude Code's sandbox would let a command ask to run outside it".to_owned());
    }
    if flag("autoAllowBashIfSandboxed") != Some(true) {
        return Err("Claude Code would not run sandboxed commands without asking".to_owned());
    }
    Ok(())
}

/// Judge a Claude canary from what its session emitted, and whether `target` exists afterwards.
///
/// Fails closed. A write that landed is the obvious failure, but so is a command that never ran —
/// the model may refuse, or answer without trying — because then nothing was proved. And so is a
/// permission prompt: under these profiles sandboxed Bash runs without asking, so a prompt for it
/// means the sandbox is not what is letting it through.
///
/// # What counts as having run
///
/// The shell's own refusal naming the target. Claude reports Bash as a tool call whose input this
/// crate does not see, only its title and output, so the command line itself is not evidence; the
/// output is. A refused write prints `operation not permitted: <path>`, and the path ends in a fresh
/// nonce, so output that names it can only have come from running this command. A write that
/// succeeded prints nothing and is caught by `target_exists` first; a Bash call the permission mode
/// denied prints a denial that does not name the target, and fails here as never run.
///
/// # Errors
///
/// Why the run may not start.
pub fn judge_claude_canary(
    events: &[AgentEvent],
    target: &Path,
    target_exists: bool,
) -> Result<(), String> {
    if target_exists {
        return Err(
            "the sandbox check wrote outside the worktree, so the sandbox is not on".to_owned(),
        );
    }
    let prompted = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::ApprovalRequested {
                request: wtm_core::model::ApprovalRequest::Command { .. },
                ..
            }
        )
    });
    if prompted {
        return Err(
            "Claude Code asked to run the sandbox check instead of running it sandboxed".to_owned(),
        );
    }
    let nonce = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ran = !nonce.is_empty()
        && events.iter().any(|event| match event {
            AgentEvent::ToolFinished {
                output: Some(output),
                ..
            }
            | AgentEvent::CommandOutput { chunk: output, .. } => output.contains(&nonce),
            _ => false,
        });
    if !ran {
        return Err("the sandbox check never ran its command, so nothing was proved".to_owned());
    }
    Ok(())
}

/// Judge a Codex canary from `command/exec`'s reply, and whether `target` exists afterwards.
///
/// # Errors
///
/// Why the run may not start.
pub fn judge_codex_canary(reply: &Value, target_exists: bool) -> Result<(), String> {
    if target_exists {
        return Err(
            "the sandbox check wrote outside the worktree, so the sandbox is not on".to_owned(),
        );
    }
    if let Some(error) = reply.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message");
        return Err(format!("Codex would not run the sandbox check: {message}"));
    }
    match reply.pointer("/result/exitCode").and_then(Value::as_i64) {
        Some(0) => Err("the sandbox check's write succeeded, so the sandbox is not on".to_owned()),
        Some(_) => Ok(()),
        None => Err("Codex did not say how the sandbox check ended".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn places() -> RunPlaces {
        RunPlaces {
            worktree: PathBuf::from("/w/repo-wt"),
            repo_root: PathBuf::from("/w/repo"),
            git: vec![
                PathBuf::from("/w/repo/.git"),
                PathBuf::from("/w/repo/.git/worktrees/repo-wt"),
                PathBuf::from("/w/repo-wt/.git"),
            ],
            home: PathBuf::from("/Users/someone"),
            session_dir: PathBuf::from("/data/run-sessions/flow-7/conventions"),
            sessions_root: PathBuf::from("/data/run-sessions"),
            gh_config_dir: PathBuf::from("/data/run-gh-config"),
        }
    }

    fn bridge(tools: &[&str]) -> RunBridge {
        RunBridge {
            name: "wtm".to_owned(),
            server: McpServer {
                command: "/Applications/wtm.app/Contents/MacOS/wtm".to_owned(),
                args: vec!["--mcp-bridge".to_owned()],
                env: BTreeMap::from([("WTM_HANDOFF_TOKEN".to_owned(), "t0k".to_owned())]),
            },
            tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
        }
    }

    #[test]
    fn a_protected_path_is_named_by_the_entry_that_covers_it() {
        assert_eq!(protected_path(".claude/settings.json"), Some(".claude/"));
        assert_eq!(protected_path(".claude"), Some(".claude/"));
        assert_eq!(protected_path(".mcp.json"), Some(".mcp.json"));
        assert_eq!(protected_path("./.githooks/pre-commit"), Some(".githooks/"));
        assert_eq!(
            protected_path(".github/workflows/ci.yml"),
            Some(".github/workflows/")
        );
        // Neighbours are not protected: only exact files and what is under the directories.
        assert_eq!(protected_path(".mcp.json.bak"), None);
        assert_eq!(protected_path(".github/CODEOWNERS"), None);
        assert_eq!(protected_path("src/.claude.rs"), None);
    }

    #[test]
    fn every_variable_the_run_scrub_removes_is_also_stripped_by_the_codex_shell_policy() {
        let scrub = crate::run_profile::scrub(Path::new("/empty"));
        for name in [
            "GH_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_TOKEN",
            "GITHUB_PAT",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "CODEX_GITHUB_PERSONAL_ACCESS_TOKEN",
            "SSH_AUTH_SOCK",
            "ACLI_CONFIG_DIR",
            "ATLASSIAN_API_TOKEN",
            "JIRA_API_TOKEN",
            "CONFLUENCE_API_TOKEN",
        ] {
            assert!(scrub.removes(name), "the scrub should remove {name}");
            assert!(
                codex_shell_excludes(name),
                "the Codex policy should strip {name}"
            );
        }
        // The one the scrub sets on purpose, which a `GH_*` glob would have stripped.
        assert!(!codex_shell_excludes("GH_CONFIG_DIR"));
        assert!(!codex_shell_excludes("CODEX_HOME"));
    }

    #[test]
    fn a_reviewers_codex_config_parses_and_says_what_the_profile_says() {
        let config = codex_config(Authority::Reviewer, &places(), &bridge(&["flow_context"]));
        let parsed: toml::Value = toml::from_str(&config).expect("the generated config is TOML");

        assert_eq!(parsed["sandbox_mode"].as_str(), Some("read-only"));
        assert_eq!(parsed["approval_policy"].as_str(), Some("never"));
        assert_eq!(parsed["features"]["apps"].as_bool(), Some(false));
        assert_eq!(
            parsed["sandbox_workspace_write"]["network_access"].as_bool(),
            Some(false)
        );
        assert_eq!(
            parsed["shell_environment_policy"]["set"]["GH_CONFIG_DIR"].as_str(),
            Some("/data/run-gh-config")
        );
        for path in ["/w/repo", "/w/repo-wt"] {
            assert_eq!(
                parsed["projects"][path]["trust_level"].as_str(),
                Some("untrusted"),
                "{path}"
            );
        }
        let server = &parsed["mcp_servers"]["wtm"];
        assert_eq!(
            server["command"].as_str(),
            Some("/Applications/wtm.app/Contents/MacOS/wtm")
        );
        assert_eq!(server["env"]["WTM_HANDOFF_TOKEN"].as_str(), Some("t0k"));
        // Only the bridge: the home has no other server for a run to load.
        assert_eq!(
            parsed["mcp_servers"].as_table().map(toml::Table::len),
            Some(1)
        );
    }

    #[test]
    fn a_writers_codex_config_asks_rather_than_never_and_still_has_no_network() {
        let config = codex_config(Authority::Writer, &places(), &bridge(&[]));
        let parsed: toml::Value = toml::from_str(&config).expect("the generated config is TOML");
        assert_eq!(parsed["sandbox_mode"].as_str(), Some("workspace-write"));
        assert_eq!(parsed["approval_policy"].as_str(), Some("on-request"));
        assert_eq!(
            parsed["sandbox_workspace_write"]["network_access"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn a_path_with_quotes_survives_into_the_codex_config_intact() {
        let mut odd = places();
        odd.repo_root = PathBuf::from(r#"/w/a "quoted" \ repo"#);
        let config = codex_config(Authority::Reviewer, &odd, &bridge(&[]));
        let parsed: toml::Value = toml::from_str(&config).expect("the generated config is TOML");
        assert_eq!(
            parsed["projects"][r#"/w/a "quoted" \ repo"#]["trust_level"].as_str(),
            Some("untrusted")
        );
    }

    #[test]
    fn a_reviewers_claude_sandbox_denies_writes_to_the_worktree_and_every_git_directory() {
        let run = claude_run(Authority::Reviewer, &places(), &bridge(&[]), None);
        let deny_write = &run.settings["sandbox"]["filesystem"]["denyWrite"];
        for path in [
            "/w/repo-wt",
            "/w/repo/.git",
            "/w/repo/.git/worktrees/repo-wt",
            "/w/repo-wt/.git",
        ] {
            assert!(
                deny_write.as_array().unwrap().iter().any(|p| p == path),
                "{path} should be denied: {deny_write}"
            );
        }
    }

    #[test]
    fn a_writers_claude_sandbox_may_write_the_worktree_but_not_its_git_or_agent_config() {
        let run = claude_run(Authority::Writer, &places(), &bridge(&[]), None);
        let deny_write: Vec<&str> = run.settings["sandbox"]["filesystem"]["denyWrite"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(!deny_write.contains(&"/w/repo-wt"));
        assert!(deny_write.contains(&"/w/repo-wt/.git"));
        assert!(deny_write.contains(&"/w/repo-wt/.mcp.json"));

        let deny: Vec<&str> = run.settings["permissions"]["deny"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for rule in [
            "Write(//w/repo/.git/**)",
            "Edit(//w/repo-wt/.git)",
            "Write(//w/repo-wt/.claude/**)",
            "Edit(//w/repo-wt/.mcp.json)",
        ] {
            assert!(deny.contains(&rule), "{rule} should be denied: {deny:?}");
        }
    }

    #[test]
    fn both_claude_roles_are_denied_reads_of_every_credential_store_and_every_runs_token() {
        for authority in [Authority::Reviewer, Authority::Writer] {
            let run = claude_run(authority, &places(), &bridge(&[]), None);
            let deny_read = &run.settings["sandbox"]["filesystem"]["denyRead"];
            assert_eq!(
                deny_read,
                &json!([
                    "/Users/someone/Library/Keychains",
                    "/Users/someone/.config/gh",
                    "/Users/someone/.ssh",
                    "/Users/someone/.codex",
                    "/Users/someone/.config/acli",
                    "/data/run-sessions",
                ]),
                "{authority:?}"
            );
            let deny = run.settings["permissions"]["deny"].as_array().unwrap();
            assert!(
                deny.iter()
                    .any(|rule| rule == "Read(//Users/someone/.config/gh/**)"),
                "{authority:?}: the Read tool is not sandboxed, so it needs its own rule"
            );
        }
    }

    #[test]
    fn only_the_roles_own_bridge_tools_are_allowed_and_bash_never_is() {
        let run = claude_run(
            Authority::Reviewer,
            &places(),
            &bridge(&["flow_context", "flow_submit"]),
            None,
        );
        assert_eq!(
            run.settings["permissions"]["allow"],
            json!(["mcp__wtm__flow_context", "mcp__wtm__flow_submit"])
        );
    }

    #[test]
    fn a_claude_canary_keeps_the_settings_under_test_and_drops_everything_else() {
        let run = claude_run(
            Authority::Writer,
            &places(),
            &bridge(&["flow_checkpoint"]),
            Some("/data/mcp.json".to_owned()),
        );
        let canary = claude_canary(&run);
        assert_eq!(canary.settings, run.settings);
        assert_eq!(canary.permission_mode, run.permission_mode);
        assert_eq!(canary.tools, ["Bash"]);
        assert_eq!(canary.mcp_config, None);
        assert!(!canary.persist);
    }

    #[test]
    fn a_canary_target_the_profile_may_write_is_refused_before_it_proves_the_wrong_thing() {
        let places = places();
        let temp = [PathBuf::from("/tmp"), PathBuf::from("/var/folders/x/T")];
        assert_eq!(
            canary_place_problem(&places, &places.session_dir.join("canary-1"), &temp),
            None
        );
        assert!(
            canary_place_problem(&places, Path::new("/w/repo-wt/canary"), &temp)
                .is_some_and(|why| why.contains("inside the worktree"))
        );
        assert!(
            canary_place_problem(&places, Path::new("/var/folders/x/T/canary"), &temp)
                .is_some_and(|why| why.contains("/var/folders/x/T"))
        );
    }

    #[test]
    fn the_canary_command_quotes_a_target_with_a_quote_in_it() {
        assert_eq!(
            canary_command(Path::new("/d/it's/canary")),
            r"printf wtm-canary > '/d/it'\''s/canary'"
        );
    }

    /// `claude sandbox status` on Claude Code 2.1.296, with a run's settings and with the malformed
    /// object F2 found the CLI ignores in silence.
    const STATUS_ON: &str = r#"{"statusVersion":3,"available":false,"installed":false,"policyLocked":false,"reasons":["Windows sandbox install is only available on native Windows."],"supported":true,"enabled":true,"enabledSource":"policy","unavailableReason":null,"strictMode":true,"strictModeSource":"policy","filesystemPolicy":"strict","autoAllowBashIfSandboxed":true,"autoAllowBashIfSandboxedSource":"policy"}"#;
    const STATUS_IGNORED: &str = r#"{"statusVersion":3,"available":false,"installed":false,"policyLocked":false,"reasons":["Windows sandbox install is only available on native Windows."],"supported":true,"enabled":false,"enabledSource":"off","unavailableReason":null,"strictMode":false,"strictModeSource":"off","filesystemPolicy":"strict","autoAllowBashIfSandboxed":true,"autoAllowBashIfSandboxedSource":"default"}"#;

    #[test]
    fn a_sandbox_status_that_is_on_and_strict_passes_and_an_ignored_object_does_not() {
        assert_eq!(read_claude_sandbox_status(STATUS_ON), Ok(()));
        assert!(
            read_claude_sandbox_status(STATUS_IGNORED)
                .is_err_and(|why| why.contains("did not read them"))
        );
        let lax = STATUS_ON.replace(r#""strictMode":true"#, r#""strictMode":false"#);
        assert!(read_claude_sandbox_status(&lax).is_err());
        let away = STATUS_ON.replace(
            r#""unavailableReason":null"#,
            r#""unavailableReason":"sandbox-exec missing""#,
        );
        assert!(read_claude_sandbox_status(&away).is_err_and(|why| why.contains("sandbox-exec")));
        assert!(read_claude_sandbox_status("Usage: claude …").is_err());
    }

    /// A Bash call's end, as Claude Code 2.1.296 reported the canary's under the reviewer profile.
    fn finished(output: &str) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: "toolu_1".to_owned(),
            ok: false,
            output: Some(output.to_owned()),
        }
    }

    const REFUSED: &str =
        "Exit code 1\n(eval):1: operation not permitted: /data/run-sessions/s/canary-5f0d6e19";

    #[test]
    fn a_claude_canary_passes_only_when_the_shell_refused_this_write_and_left_nothing_behind() {
        let target = Path::new("/data/run-sessions/s/canary-5f0d6e19");
        assert_eq!(
            judge_claude_canary(&[finished(REFUSED)], target, false),
            Ok(())
        );
        assert!(judge_claude_canary(&[finished(REFUSED)], target, true).is_err());
        // A different write's refusal is not this one's.
        let elsewhere = Path::new("/data/run-sessions/s/canary-00000000");
        assert!(
            judge_claude_canary(&[finished(REFUSED)], elsewhere, false)
                .is_err_and(|why| why.contains("never ran"))
        );
        // The mode denying Bash outright is not the sandbox refusing the write.
        let denied = finished("Permission to use Bash has been denied.");
        assert!(judge_claude_canary(&[denied], target, false).is_err());
        assert!(judge_claude_canary(&[], target, false).is_err());
    }

    #[test]
    fn a_claude_canary_that_asked_for_permission_fails_even_though_nothing_was_written() {
        let target = Path::new("/data/run-sessions/s/canary-5f0d6e19");
        let asked = AgentEvent::ApprovalRequested {
            id: "a1".to_owned(),
            blocking: true,
            request: wtm_core::model::ApprovalRequest::Command {
                command: canary_command(target),
                cwd: None,
                reason: None,
            },
        };
        assert!(
            judge_claude_canary(&[finished(REFUSED), asked], target, false)
                .is_err_and(|why| why.contains("asked"))
        );
    }

    #[test]
    fn a_codex_canary_passes_only_on_a_failed_write_that_left_nothing_behind() {
        let refused = json!({ "id": 3, "result": { "exitCode": 1, "stdout": "", "stderr": "Operation not permitted" } });
        let wrote = json!({ "id": 3, "result": { "exitCode": 0, "stdout": "", "stderr": "" } });
        let error = json!({ "id": 3, "error": { "code": -32600, "message": "unknown method" } });
        assert_eq!(judge_codex_canary(&refused, false), Ok(()));
        assert!(judge_codex_canary(&refused, true).is_err());
        assert!(judge_codex_canary(&wrote, false).is_err());
        assert!(judge_codex_canary(&error, false).is_err_and(|why| why.contains("unknown method")));
        assert!(judge_codex_canary(&json!({ "id": 3 }), false).is_err());
    }
}

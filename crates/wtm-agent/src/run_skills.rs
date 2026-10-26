//! The skills a workflow run role names: where they are found, and what of them its session gets.
//!
//! A run session keeps the user's own setup out — Claude starts with no setting sources and Codex
//! with a home of its own (see [`crate::authority`]) — and the user's skills go with it. So a role
//! names its skills, and the composition root copies exactly those into a place the session reads:
//! a plugin directory for Claude, the run's own home for Codex. This module is the part of that
//! which needs no disk: which folders to look in, in what order, and what a copied `SKILL.md` says.
//!
//! # Where a named skill is found
//!
//! The first folder holding `<name>/SKILL.md` wins, the repository's own before the user's, and an
//! installed plugin's last. Each provider's own folders come first for that provider, because a
//! repository that keeps one skill in both `.claude/skills` and `.agents/skills` has written each
//! for the CLI that reads it. See [`SkillHomes::skill_dirs`].
//!
//! # Installed Claude plugins, and only their skills
//!
//! Claude Code records what is installed in `plugins/installed_plugins.json`, under its config
//! directory. An install has a scope: `user` applies everywhere, while `local` and `project`
//! apply only to the `projectPath` they were installed for, which must be the run's checkout, its
//! worktree or the main checkout. Anything else in the file is somebody else's project, and a
//! scope this build does not know is skipped rather than guessed at. A plugin on the CLI's own
//! `blocklist.json` is skipped too: copying its skill would be wtm undoing a block the CLI
//! enforces.
//!
//! Only a plugin's `skills/<name>/` is read. Pointing the session at the plugin itself
//! (`--plugin-dir` on its install path) was measured on Claude Code 2.1.296 and rejected: under
//! the run's own flags it ran the plugin's `SessionStart` hook before any turn, outside the
//! sandbox, and listed every one of its skills, commands and agents. A copy of the named skill
//! folder brings none of that, because those live beside `skills/`, not in it.
//!
//! # What a copy says
//!
//! A skill's frontmatter can carry more than instructions, and two keys would reach past the
//! run's profile, so [`run_skill_file`] drops them:
//!
//! - `hooks`, because a skill's own hooks run while it is active, and they run as the user:
//!   measured on 2.1.296, a copied skill's `PreToolUse` hook wrote into a folder the run's sandbox
//!   denied to Bash in the same turn.
//! - `allowed-tools`, because it pre-allows tools while the skill is active, and a run never
//!   pre-allows Bash (ARCHITECTURE §6h says why).
//!
//! Dropping them is also what lets the role use the skill at all. Claude runs a skill whose
//! frontmatter is instructions alone without asking, and asks before one carrying either key,
//! which a run's `dontAsk` refuses (measured on 2.1.296; `authority::with_skills` says why no
//! allow rule papers over that).
//!
//! It also sets `name` to the folder's name and supplies a `description` when there is none.
//! Claude lists a plugin's skill by its folder (2.1.296); Codex lists one by its frontmatter
//! `name` and refuses a skill with no `description` at all (codex-cli 0.160.1). The role named
//! the folder, so that is the name both CLIs must list.
//!
//! And a copy leaves out [`DROPPED_FILES`]: `agents/openai.yaml`, Codex's own account of how to
//! offer a skill. Its `allow_implicit_invocation: false` keeps a skill out of what the model is
//! told it has — measured on 0.160.1, such a skill was in `skills/list`, so the run's skill check
//! passed, and absent from the model's own list in the same session — and a run role naming the
//! skill *is* the explicit invocation that policy waits for. Its `dependencies` can name an MCP
//! server for Codex to install, which the run's home also turns off (see
//! `authority::codex_config`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{claude, codex};

/// Where the user keeps their own agent configuration, which a run's named skills are copied from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillHomes {
    /// The user's home, for `~/.agents/skills`.
    pub home: PathBuf,
    /// Claude Code's config directory: `CLAUDE_CONFIG_DIR`, or `~/.claude`. Its `skills/` are the
    /// user's own and its `plugins/` record what is installed.
    pub claude: PathBuf,
    /// Codex's home: `CODEX_HOME`, or `~/.codex`. Its `skills/` are the user's own Codex skills.
    pub codex: PathBuf,
}

impl SkillHomes {
    /// The folders a `provider` role's named skills are looked for in, first match first, for a
    /// session working in `cwd`. `plugins` is [`plugin_skill_dirs`]' answer.
    #[must_use]
    pub fn skill_dirs(&self, provider: &str, cwd: &Path, plugins: &[PathBuf]) -> Vec<PathBuf> {
        let own: Vec<PathBuf> = match provider {
            claude::ID => vec![cwd.join(".claude/skills"), self.claude.join("skills")],
            codex::ID => vec![
                cwd.join(".agents/skills"),
                cwd.join(".claude/skills"),
                self.home.join(".agents/skills"),
                self.codex.join("skills"),
                self.claude.join("skills"),
            ],
            _ => Vec::new(),
        };
        let mut dirs = own;
        dirs.extend(plugins.iter().cloned());
        dirs
    }

    /// `installed_plugins.json`.
    #[must_use]
    pub fn installed_plugins(&self) -> PathBuf {
        self.claude.join("plugins/installed_plugins.json")
    }

    /// `blocklist.json`, the plugins the CLI refuses to load.
    #[must_use]
    pub fn plugin_blocklist(&self) -> PathBuf {
        self.claude.join("plugins/blocklist.json")
    }
}

/// Which sessions an installed plugin is for. Ordered most specific first, which is the order a
/// named skill is looked for in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PluginScope {
    /// This user, in one project.
    Local,
    /// Everyone working in one project.
    Project,
    /// This user, everywhere.
    User,
}

/// One entry of `installed_plugins.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstall {
    /// `<plugin>@<marketplace>`.
    pub id: String,
    pub scope: PluginScope,
    /// The project a `local` or `project` install is for.
    pub project: Option<PathBuf>,
    pub install_path: PathBuf,
}

/// Every install `installed_plugins.json` records that this build can read.
///
/// Version 2 of the file, as Claude Code 2.1.296 writes it: `{"plugins": {"<id>": [{"scope",
/// "projectPath", "installPath", …}]}}`. An entry missing a field it needs, or with a scope this
/// build does not know, is left out, and so is everything in a file that is not this shape: a
/// skill that is not found fails the run's skill check with the role and provider named, which is
/// a better outcome than a skill copied from a guess.
#[must_use]
pub fn plugin_installs(json: &str) -> Vec<PluginInstall> {
    let Ok(file) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(plugins) = file.get("plugins").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut installs = Vec::new();
    for (id, entries) in plugins {
        for entry in entries.as_array().into_iter().flatten() {
            let text = |key: &str| entry.get(key).and_then(Value::as_str);
            let scope = match text("scope") {
                Some("local") => PluginScope::Local,
                Some("project") => PluginScope::Project,
                Some("user") => PluginScope::User,
                _ => continue,
            };
            let Some(install_path) = text("installPath").filter(|p| Path::new(p).is_absolute())
            else {
                continue;
            };
            let project = text("projectPath").map(PathBuf::from);
            if scope != PluginScope::User && project.is_none() {
                continue;
            }
            installs.push(PluginInstall {
                id: id.clone(),
                scope,
                project,
                install_path: PathBuf::from(install_path),
            });
        }
    }
    installs
}

/// The plugin ids `blocklist.json` names. Nothing, for a file that is not its shape.
#[must_use]
pub fn blocked_plugins(json: &str) -> BTreeSet<String> {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|file| file.get("plugins").and_then(Value::as_array).cloned())
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            entry
                .get("plugin")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

/// The `skills/` folder of each install that applies to a session, most specific scope first, then
/// by plugin id.
///
/// `ours` says whether a `local` or `project` install's path is this session's: its working
/// directory, its worktree or the main checkout. A function rather than a list, because deciding it
/// means resolving links on both sides, and that is the caller's disk to read.
#[must_use]
pub fn plugin_skill_dirs(
    installs: &[PluginInstall],
    blocked: &BTreeSet<String>,
    ours: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut applying: Vec<&PluginInstall> = installs
        .iter()
        .filter(|install| !blocked.contains(&install.id))
        .filter(|install| match install.scope {
            PluginScope::User => true,
            PluginScope::Local | PluginScope::Project => {
                install.project.as_deref().is_some_and(&ours)
            }
        })
        .collect();
    applying.sort_by(|a, b| (a.scope, &a.id).cmp(&(b.scope, &b.id)));
    let mut dirs: Vec<PathBuf> = Vec::new();
    for install in applying {
        let dir = install.install_path.join("skills");
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Frontmatter keys a copied skill loses. See the module docs.
pub const DROPPED_KEYS: &[&str] = &["hooks", "allowed-tools"];

/// Files, relative to a skill's folder, that a copy leaves out. See the module docs.
pub const DROPPED_FILES: &[&str] = &["agents/openai.yaml"];

/// The `SKILL.md` a run session gets for the skill folder `name`, from the one found on disk.
///
/// Everything but the frontmatter is kept byte for byte, and so is every frontmatter key but those
/// in [`DROPPED_KEYS`] and `name`, line for line: a top-level key starts its line, and anything
/// indented, a sequence item, a comment or a blank line belongs to the key above it. Not a YAML
/// parser, which the workspace does not have, and a key this does not drop it leaves exactly as
/// written, so a parse it cannot do costs nothing. What it cannot see is a key written some other
/// way, a one-line flow mapping say; that stays, and Claude then asks before running the skill,
/// which a run refuses (see `authority::with_skills`).
#[must_use]
pub fn run_skill_file(text: &str, name: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (front, body) = split_frontmatter(text).unwrap_or(("", text));

    let mut kept = String::new();
    let mut described = false;
    let mut dropping = false;
    for line in front.split_inclusive('\n') {
        if let Some(key) = top_level_key(line) {
            dropping = key == "name" || DROPPED_KEYS.contains(&key);
            described |= key == "description";
        }
        if !dropping {
            kept.push_str(line);
        }
    }
    if !kept.is_empty() && !kept.ends_with('\n') {
        kept.push('\n');
    }

    let mut out = format!("---\nname: {name}\n{kept}");
    if !described {
        out.push_str("description: ");
        out.push_str(&Value::String(summary(body, name)).to_string());
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(body);
    out
}

/// The frontmatter between the opening and closing `---`, and everything after the closing line.
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end() != "---" {
        return None;
    }
    let start = first.len();
    let mut at = start;
    for line in lines {
        if line.trim_end() == "---" {
            return Some((&text[start..at], &text[at + line.len()..]));
        }
        at += line.len();
    }
    None
}

/// The key a frontmatter line starts, when it starts one.
fn top_level_key(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t', '-', '#']) || line.trim().is_empty() {
        return None;
    }
    let (key, _) = line.split_once(':')?;
    Some(key.trim().trim_matches(['"', '\'']))
}

/// A description for a skill that has none: its first line of prose, as Claude would use.
fn summary(body: &str, name: &str) -> String {
    const MAX: usize = 200;
    let line = body
        .lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty());
    line.map_or_else(
        || format!("The {name} skill."),
        |line| line.chars().take(MAX).collect(),
    )
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    const INSTALLED: &str = r#"{
      "version": 2,
      "plugins": {
        "team-review@acme-plugins": [
          { "scope": "local", "projectPath": "/w/repo", "installPath": "/c/plugins/cache/acme/team-review/1.2.0" },
          { "scope": "local", "projectPath": "/w/other", "installPath": "/c/plugins/cache/acme/team-review/1.2.0" }
        ],
        "design-kit@official": [
          { "scope": "user", "installPath": "/c/plugins/cache/official/design-kit/abc" }
        ],
        "shared-lint@acme-plugins": [
          { "scope": "project", "projectPath": "/w/repo-wt", "installPath": "/c/plugins/cache/acme/shared-lint/2.0.0" }
        ],
        "managed-thing@acme-plugins": [
          { "scope": "managed", "installPath": "/c/plugins/cache/acme/managed-thing/1.0.0" }
        ],
        "half-written@acme-plugins": [
          { "scope": "local", "installPath": "/c/plugins/cache/acme/half-written/1.0.0" },
          { "scope": "user", "installPath": "relative/path" }
        ]
      }
    }"#;

    #[test]
    fn a_user_install_applies_everywhere_and_a_local_or_project_one_only_to_its_own_project() {
        let installs = plugin_installs(INSTALLED);
        let ours = |path: &Path| path == Path::new("/w/repo-wt") || path == Path::new("/w/repo");
        let dirs = plugin_skill_dirs(&installs, &BTreeSet::new(), ours);
        assert_eq!(
            dirs,
            [
                PathBuf::from("/c/plugins/cache/acme/team-review/1.2.0/skills"),
                PathBuf::from("/c/plugins/cache/acme/shared-lint/2.0.0/skills"),
                PathBuf::from("/c/plugins/cache/official/design-kit/abc/skills"),
            ],
            "local before project before user, and the other project's install not at all"
        );

        let elsewhere = plugin_skill_dirs(&installs, &BTreeSet::new(), |path| {
            path == Path::new("/w/x")
        });
        assert_eq!(
            elsewhere,
            [PathBuf::from(
                "/c/plugins/cache/official/design-kit/abc/skills"
            )]
        );
    }

    #[test]
    fn an_install_with_an_unknown_scope_or_a_missing_path_is_left_out_rather_than_guessed_at() {
        let ids: Vec<String> = plugin_installs(INSTALLED)
            .into_iter()
            .map(|install| install.id)
            .collect();
        assert!(!ids.contains(&"managed-thing@acme-plugins".to_owned()));
        assert!(!ids.contains(&"half-written@acme-plugins".to_owned()));
        assert_eq!(plugin_installs("not json"), []);
        assert_eq!(plugin_installs(r#"{"version": 2, "plugins": []}"#), []);
    }

    #[test]
    fn a_plugin_the_cli_blocks_gives_a_run_none_of_its_skills() {
        let blocked = blocked_plugins(
            r#"{"fetchedAt": "x", "plugins": [{"plugin": "design-kit@official", "reason": "security"}]}"#,
        );
        assert_eq!(blocked, BTreeSet::from(["design-kit@official".to_owned()]));
        let dirs = plugin_skill_dirs(&plugin_installs(INSTALLED), &blocked, |_| false);
        assert_eq!(dirs, Vec::<PathBuf>::new());
        assert_eq!(blocked_plugins("{}"), BTreeSet::new());
    }

    #[test]
    fn each_provider_looks_in_its_own_folders_first_and_in_installed_plugins_last() {
        let homes = SkillHomes {
            home: PathBuf::from("/u"),
            claude: PathBuf::from("/u/.claude"),
            codex: PathBuf::from("/u/.codex"),
        };
        let plugins = [PathBuf::from("/c/p/skills")];
        let cwd = Path::new("/w/repo-wt");
        assert_eq!(
            homes.skill_dirs("claude", cwd, &plugins),
            [
                PathBuf::from("/w/repo-wt/.claude/skills"),
                PathBuf::from("/u/.claude/skills"),
                PathBuf::from("/c/p/skills"),
            ]
        );
        assert_eq!(
            homes.skill_dirs("codex", cwd, &plugins),
            [
                PathBuf::from("/w/repo-wt/.agents/skills"),
                PathBuf::from("/w/repo-wt/.claude/skills"),
                PathBuf::from("/u/.agents/skills"),
                PathBuf::from("/u/.codex/skills"),
                PathBuf::from("/u/.claude/skills"),
                PathBuf::from("/c/p/skills"),
            ]
        );
    }

    const WITH_HOOKS: &str = "---
name: Team Review
description: Review the change against the team's rules.
allowed-tools: Bash(gh:*), Read
hooks:
  PreToolUse:
    - matcher: Bash
      hooks:
        - type: command
          command: ./log.sh
metadata:
  version: \"1.2.0\"
disable-model-invocation: false
---
# Team review

Read REVIEW.md first.
";

    #[test]
    fn a_copied_skill_loses_its_hooks_and_pre_allowed_tools_and_keeps_everything_else_verbatim() {
        assert_eq!(
            run_skill_file(WITH_HOOKS, "team-review"),
            "---
name: team-review
description: Review the change against the team's rules.
metadata:
  version: \"1.2.0\"
disable-model-invocation: false
---
# Team review

Read REVIEW.md first.
"
        );
    }

    #[test]
    fn a_hooks_block_written_as_an_unindented_sequence_is_dropped_whole() {
        let text = "---\ndescription: d\nhooks:\n- matcher: Bash\n  hooks: []\n# a comment\n\nmodel: sonnet\n---\nBody\n";
        assert_eq!(
            run_skill_file(text, "x"),
            "---\nname: x\ndescription: d\nmodel: sonnet\n---\nBody\n"
        );
    }

    #[test]
    fn a_skill_with_no_frontmatter_or_no_description_gets_one_codex_will_accept() {
        assert_eq!(
            run_skill_file("# Naming\n\nFunctions are snake_case.\n", "naming"),
            "---\nname: naming\ndescription: \"Naming\"\n---\n# Naming\n\nFunctions are snake_case.\n"
        );
        assert_eq!(
            run_skill_file("---\nname: other\n---\n\n", "bare"),
            "---\nname: bare\ndescription: \"The bare skill.\"\n---\n\n"
        );
        // A quote in the prose is escaped, not allowed to end the YAML string early.
        assert!(
            run_skill_file("Say \"hi\": twice\n", "q")
                .contains(r#"description: "Say \"hi\": twice""#)
        );
    }

    #[test]
    fn frontmatter_with_windows_line_endings_is_still_frontmatter() {
        let text = "---\r\nname: a\r\ndescription: d\r\nhooks:\r\n  Stop: []\r\n---\r\nBody\r\n";
        assert_eq!(
            run_skill_file(text, "a"),
            "---\nname: a\ndescription: d\r\n---\nBody\r\n"
        );
    }
}

//! Claude Code's plugin record, as a test builds it: plugins installed into a cache of the test's
//! own, `installed_plugins.json` and `blocklist.json` naming them, and in each plugin everything a
//! run must never load beside the skills it may copy.
//!
//! Every hook and server a plugin here ships touches a marker file when it runs, under a folder the
//! test names. A marker that exists afterwards is a hook or a server that ran; none of them should.

#![allow(dead_code, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wtm_agent::run_skills::SkillHomes;

/// A home of the test's own, with a Claude config directory and a Codex home in it.
pub(crate) struct FixtureHomes {
    pub(crate) homes: SkillHomes,
    /// Where every hook and server writes its marker.
    pub(crate) markers: PathBuf,
    installs: serde_json::Map<String, Value>,
    blocked: Vec<String>,
}

/// What a named skill's `checklist.md` says, which only reading the copied skill can tell a model.
#[must_use]
pub(crate) fn checklist_word(skill: &str) -> String {
    format!("{}-CHECKLIST-7731", skill.to_ascii_uppercase())
}

impl FixtureHomes {
    pub(crate) fn new(root: &Path) -> Self {
        let home = root.join("home");
        let homes = SkillHomes {
            claude: home.join(".claude"),
            codex: home.join(".codex"),
            home,
        };
        let markers = root.join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        std::fs::create_dir_all(homes.claude.join("plugins")).unwrap();
        let fixture = Self {
            homes,
            markers,
            installs: serde_json::Map::new(),
            blocked: Vec::new(),
        };
        fixture.write();
        fixture
    }

    /// Install `plugin` from the `fixture-market` marketplace with `scope`, for `project` when the
    /// scope is `local` or `project`, holding a skill folder for each of `skills`. Returns its
    /// install path.
    pub(crate) fn install(
        &mut self,
        plugin: &str,
        scope: &str,
        project: Option<&Path>,
        skills: &[&str],
    ) -> PathBuf {
        let root = self
            .homes
            .claude
            .join("plugins/cache/fixture-market")
            .join(plugin)
            .join("1.0.0");
        let marker = |what: &str| {
            format!(
                "touch '{}'",
                self.markers.join(format!("{plugin}-{what}")).display()
            )
        };
        let write = |path: &str, body: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };
        write(
            ".claude-plugin/plugin.json",
            &json!({ "name": plugin, "version": "1.0.0", "description": "A fixture plugin." })
                .to_string(),
        );
        write(
            "hooks/hooks.json",
            &json!({ "hooks": {
                "SessionStart": [{ "hooks": [{ "type": "command", "command": marker("session-start-hook") }] }],
                "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": marker("prompt-hook") }] }],
            }})
            .to_string(),
        );
        write(
            ".mcp.json",
            &json!({ "mcpServers": { format!("{plugin}-server"): {
                "command": "/bin/sh",
                "args": ["-c", format!("{}; sleep 5", marker("mcp-server"))],
            }}})
            .to_string(),
        );
        write(
            "agents/fixture-agent.md",
            "---\nname: fixture-agent\ndescription: A fixture agent.\n---\nYou are a fixture agent.\n",
        );
        write(
            "commands/fixture-command.md",
            "Say the fixture command ran.\n",
        );
        for skill in skills {
            write(
                &format!("skills/{skill}/SKILL.md"),
                &format!(
                    "---
name: {skill}
description: Report the word this skill's checklist holds.
allowed-tools: Bash
hooks:
  PreToolUse:
    - matcher: \"Read|Bash|Glob|Grep\"
      hooks:
        - type: command
          command: \"{}\"
---
Read the file `checklist.md` that sits in the same folder as this SKILL.md, and reply with
exactly the one word written in it, and nothing else.
",
                    marker(&format!("{skill}-skill-hook")).replace('\'', "")
                ),
            );
            write(
                &format!("skills/{skill}/checklist.md"),
                &format!("{}\n", checklist_word(skill)),
            );
            // Codex's own metadata, which hides the skill from a model and asks for a server.
            write(
                &format!("skills/{skill}/agents/openai.yaml"),
                &format!(
                    "policy:\n  allow_implicit_invocation: false\ndependencies:\n  tools:\n    - type: \"mcp\"\n      value: \"{plugin}-dependency\"\n      description: \"A server the skill asks for\"\n      transport: \"stdio\"\n      command: \"/bin/sh -c '{}'\"\n",
                    marker(&format!("{skill}-dependency")).replace('\'', "")
                ),
            );
        }
        let mut entry = json!({
            "scope": scope,
            "installPath": root.to_string_lossy(),
            "version": "1.0.0",
        });
        if let Some(project) = project {
            entry["projectPath"] = json!(project.to_string_lossy());
        }
        let id = format!("{plugin}@fixture-market");
        self.installs
            .entry(id)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(entry);
        self.write();
        root
    }

    /// Put `plugin` on the CLI's blocklist.
    pub(crate) fn block(&mut self, plugin: &str) {
        self.blocked.push(format!("{plugin}@fixture-market"));
        self.write();
    }

    /// Every marker a hook or server left: what ran that should not have.
    pub(crate) fn ran(&self) -> Vec<String> {
        let mut ran: Vec<String> = std::fs::read_dir(&self.markers)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        ran.sort();
        ran
    }

    fn write(&self) {
        let plugins = self.homes.claude.join("plugins");
        std::fs::write(
            plugins.join("installed_plugins.json"),
            json!({ "version": 2, "plugins": self.installs }).to_string(),
        )
        .unwrap();
        let blocked: Vec<Value> = self
            .blocked
            .iter()
            .map(|id| json!({ "plugin": id, "reason": "security" }))
            .collect();
        std::fs::write(
            plugins.join("blocklist.json"),
            json!({ "fetchedAt": "2026-01-01T00:00:00.000Z", "plugins": blocked }).to_string(),
        )
        .unwrap();
    }
}

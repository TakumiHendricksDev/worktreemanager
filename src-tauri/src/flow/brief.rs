//! What a review role is told: its brief, which `flow_context` hands it, and the few turns the
//! engine sends.
//!
//! # The brief is facts, and the turn is short
//!
//! The first turn only says to call `flow_context`. Everything the role works from — its
//! objective, what it reads, its skills, and the exact schema its result is checked against — is
//! in the brief, which `flow_context` returns fenced as material rather than as the app speaking.
//! The objective and instructions come from your own config, but a later backlog item puts an
//! issue's text in the same place, and the fence is cheaper to keep than to add back.
//!
//! # Inputs that are not here yet
//!
//! A role may ask for the issue, a plan, a design, your rules, earlier findings or answers. A review
//! of your own work in this build has none of those to give, and the brief says so by name rather
//! than leaving a role to wonder whether it missed them.

use std::fmt::Write as _;

use wtm_core::flow::submission::{self, kind_name};
use wtm_core::flow::{OutputKind, RoleInput, RoleProvider, RoleSpec, Snapshot, Workspace};

/// What a brief is written from.
#[derive(Debug, Clone)]
pub struct BriefFacts<'a> {
    pub role: &'a str,
    pub spec: &'a RoleSpec,
    pub recipe: &'a str,
    pub worktree_path: &'a str,
    pub branch: Option<&'a str>,
    pub base: &'a str,
    pub snapshot: &'a Snapshot,
    /// The files the change touches, when git could list them all.
    pub changed: Option<&'a [String]>,
    /// For a fallback: what the role it stands in for got to.
    pub handover: Option<&'a str>,
}

/// How many changed files a brief lists before it says how to list the rest.
const LISTED_FILES: usize = 60;

/// The name a role invokes `skill` by, in its provider's spelling.
///
/// A Claude run session gets its skills from the plugin `flow_session` writes, so they are listed
/// and invoked under the plugin's name. Codex lists a repository's skills under their own names.
#[must_use]
pub fn invocable(provider: RoleProvider, skill: &str) -> String {
    let skill = skill.trim_start_matches('/');
    match provider {
        RoleProvider::Claude => format!("{}:{skill}", wtm_agent::authority::RUN_SKILL_PLUGIN),
        RoleProvider::Codex => skill.to_owned(),
    }
}

/// The brief `flow_context` hands a review role.
#[must_use]
pub fn brief(facts: &BriefFacts<'_>) -> String {
    let spec = facts.spec;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Role `{}` in a review run of recipe `{}`.\n\nObjective: {}",
        facts.role,
        facts.recipe,
        spec.objective.trim()
    );
    if let Some(instructions) = spec.instructions.as_deref().map(str::trim)
        && !instructions.is_empty()
    {
        let _ = writeln!(out, "\n{instructions}");
    }

    if let Some(handover) = facts.handover {
        let _ = writeln!(out, "\nYou run in place of another role.\n{handover}");
    }

    let short = |sha: &str| sha.chars().take(10).collect::<String>();
    let snapshot = facts.snapshot;
    let _ = writeln!(out, "\nWhat you are reviewing:");
    let branch = facts
        .branch
        .map_or_else(String::new, |branch| format!(" (branch `{branch}`)"));
    match spec.workspace() {
        Workspace::Snapshot => {
            let _ = writeln!(
                out,
                "- Your working directory is a checkout of a snapshot of the worktree at `{}`{branch}, \
                 uncommitted and untracked work included. It is read-only, and the person keeps \
                 editing the real worktree while you read this one, so review this directory.",
                facts.worktree_path
            );
        }
        Workspace::Worktree => {
            let _ = writeln!(
                out,
                "- Your working directory is the worktree itself{branch}. It may change while you \
                 work; the run checks it again when you finish."
            );
        }
    }
    let _ = writeln!(
        out,
        "- The change is everything from the merge base with `{}` ({}) to the snapshot (tree {}). \
         `git diff {}` in your working directory shows all of it.",
        facts.base,
        short(&snapshot.base_sha),
        short(&snapshot.tree_sha),
        snapshot.base_sha
    );
    match facts.changed {
        Some([]) => {
            let _ = writeln!(out, "- No files differ from the merge base.");
        }
        Some(paths) => {
            let _ = writeln!(out, "- Files changed ({}):", paths.len());
            for path in paths.iter().take(LISTED_FILES) {
                let _ = writeln!(out, "  - {path}");
            }
            if paths.len() > LISTED_FILES {
                let _ = writeln!(
                    out,
                    "  - …and {} more; `git diff --stat {}` lists them.",
                    paths.len() - LISTED_FILES,
                    snapshot.base_sha
                );
            }
        }
        None => {}
    }

    if !spec.skills.is_empty() {
        let names: Vec<String> = spec
            .skills
            .iter()
            .map(|skill| format!("`{}`", invocable(spec.provider, skill)))
            .collect();
        let how = match spec.provider {
            RoleProvider::Claude => "Invoke them with the Skill tool",
            RoleProvider::Codex => "They are in your skills list; read and follow them",
        };
        let _ = writeln!(
            out,
            "\nSkills for this role: {}. {how}. Where a skill says how to report, report through \
             `flow_submit` instead, in the shape below.",
            names.join(", ")
        );
    }

    let missing: Vec<&str> = spec
        .inputs
        .iter()
        .filter_map(|input| match input {
            RoleInput::Issue => Some("the issue"),
            RoleInput::Plan => Some("an approved plan"),
            RoleInput::Design => Some("a design"),
            RoleInput::Rules => Some("your review rules"),
            RoleInput::Findings => Some("earlier findings"),
            RoleInput::Answers => Some("answers to questions"),
            RoleInput::Diff | RoleInput::Instructions => None,
        })
        .collect();
    if spec.inputs.contains(&RoleInput::Instructions) {
        let _ = writeln!(
            out,
            "\nThe repository's own instruction files at the root of your working directory \
             (such as CLAUDE.md, AGENTS.md or a review guide) are its rules; read them."
        );
    }
    if !missing.is_empty() {
        let _ = writeln!(
            out,
            "\nThis role asks for {}, and this review has none to give. Work from the change itself.",
            missing.join(", ")
        );
    }

    let kind = spec.output;
    let _ = writeln!(
        out,
        "\nHand in your result by calling `flow_submit` with kind `{}` and a payload matching this \
         JSON Schema. It is checked; a payload that does not match comes back with every problem \
         listed, to fix and submit again. Your result is what you submit, not what you write in \
         chat.\n{}",
        kind_name(kind),
        submission::schema(kind)
    );
    if kind == OutputKind::Findings {
        let _ = writeln!(
            out,
            "\nReport each problem once. A confirmed bug is one you demonstrated, with the \
             `verification` that showed it; anything you suspect but did not confirm is a \
             suggestion. A conflict between two sources of guidance is its own finding: do not \
             pick a side. An empty `findings` list is a valid result."
        );
    }
    let _ = write!(
        out,
        "\nWhen you have submitted, end your turn. Do not try to change files, commit or push: \
         this session cannot, and nothing you do here is published."
    );
    out
}

/// The first turn a role session is sent.
#[must_use]
pub fn opening_turn(role: &str) -> String {
    format!(
        "Start the `{role}` review: call `flow_context` for your brief, do the review it describes, \
         and hand in your result with `flow_submit`."
    )
}

/// The turn sent to a role that ended its turn without handing in a result.
#[must_use]
pub fn nudge(kind: OutputKind) -> String {
    format!(
        "Your turn ended without a result. The run reads your result only from `flow_submit`, with \
         kind `{}`. Call it now with what you found — an empty list is a valid result if you found \
         nothing — and then end your turn.",
        kind_name(kind)
    )
}

/// What a fallback is told about the role it runs in place of, and what the record keeps.
///
/// Text, never the session: a fallback on another provider cannot resume the first one's
/// conversation, and handing over less than everything it said is the point — the fallback reviews
/// the change itself rather than inheriting another model's conclusions.
#[must_use]
pub fn handover(from: &str, why: &str, progress: &[String]) -> String {
    let mut out = format!("`{from}` stopped before handing in a result: {why}.");
    if progress.is_empty() {
        out.push_str(" It reported no progress first.");
    } else {
        out.push_str(" What it reported doing first:");
        for line in progress {
            let _ = write!(out, "\n- {line}");
        }
    }
    out.push_str("\nNothing it found reached the run. Review the change in full yourself.");
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wtm_core::flow::BlobRef;

    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            head_sha: "1".repeat(40),
            tree_sha: "2".repeat(40),
            base_sha: "3".repeat(40),
            patch: BlobRef {
                path: "p".to_owned(),
                sha256: "s".to_owned(),
                bytes: 1,
            },
        }
    }

    fn role(value: serde_json::Value) -> RoleSpec {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_claude_role_is_told_its_skills_by_the_name_its_session_lists_them_under() {
        let spec = role(json!({
            "objective": "Check conventions",
            "provider": "claude",
            "skills": ["/team-conventions"],
            "inputs": ["diff", "rules"],
        }));
        let changed = ["src/a.rs".to_owned()];
        let text = brief(&BriefFacts {
            role: "conventions",
            spec: &spec,
            recipe: "standard",
            worktree_path: "/w/acme-wt",
            branch: Some("fix/x"),
            base: "origin/main",
            snapshot: &snapshot(),
            changed: Some(&changed),
            handover: None,
        });
        assert!(text.contains("`wtm:team-conventions`"), "{text}");
        assert!(text.contains("Skill tool"));
        assert!(
            text.contains("your review rules"),
            "a missing input is named"
        );
        assert!(text.contains(&format!("git diff {}", "3".repeat(40))));
        assert!(text.contains("  - src/a.rs"));
        assert!(
            text.contains(r#""confirmed_bug""#),
            "the schema itself is in the brief"
        );
    }

    #[test]
    fn a_codex_skill_keeps_its_own_name() {
        assert_eq!(invocable(RoleProvider::Codex, "/review"), "review");
        assert_eq!(invocable(RoleProvider::Claude, "review"), "wtm:review");
    }

    #[test]
    fn a_handover_says_what_the_first_role_did_and_that_none_of_it_counts() {
        let text = handover(
            "correctness",
            "Codex reached its usage limit",
            &["reading export.py".to_owned()],
        );
        assert!(text.contains("`correctness` stopped"));
        assert!(text.contains("- reading export.py"));
        assert!(text.contains("Nothing it found reached the run"));
    }
}

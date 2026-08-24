//! Reading a provider's skill directories, so `/` is useful before the session says anything.
//!
//! # Why this exists when the CLIs report their own
//!
//! Because of *when* they report it. Claude Code emits nothing at all until it receives a turn —
//! its `system`/`init` line, which carries `slash_commands` and `skills`, arrives as the first line
//! *after* a user message. So on a fresh pane the composer's `/` menu had no session list to merge
//! and fell back entirely to the compiled catalogue of built-ins, which is exactly when somebody
//! wants `/ship` or `/team-review`: before they have typed anything. In a repository with
//! fifty-five skills in `.claude/skills/`, none of them appeared.
//!
//! # Why duplicating another program's discovery rules is acceptable here and would not be
//!
//! `claude.rs` used to say that scanning these directories would be "this app re-implementing
//! another program's discovery rules and going stale the first time they changed", and that is a
//! good argument against making this the **source of truth**. It is not one against a *seed*.
//!
//! Everything read here is merged *under* whatever the provider later reports, in `commandsFor`, so
//! the CLI corrects it the moment it speaks. The failure mode of going stale is therefore an entry
//! that is briefly missing or briefly extra in a menu — not a wrong answer anybody acts on, and not
//! a state that survives the first turn. That asymmetry is the whole justification, and if this ever
//! becomes authoritative for anything, it stops holding.
//!
//! Descriptions are the bonus. Both CLIs report their skills as bare `string[]`, so the frontmatter
//! read here is the *only* place a description for a project skill can come from at all.

use std::path::{Path, PathBuf};

use wtm_core::model::AgentSkill;

/// How far up from the worktree to look for a skills directory.
///
/// Claude's own rule is "up to the repository root", which this cannot evaluate without asking git —
/// and this crate has no `Git` port, deliberately. A small fixed depth covers the shape that
/// actually occurs (a worktree, or a package inside one) without walking to `/` and reading whatever
/// a stranger left in a parent directory.
const ASCEND: usize = 4;

/// A skill directory and what to call what it finds.
struct Root {
    path: PathBuf,
    scope: &'static str,
}

/// Skills discoverable for a Claude session working in `cwd`.
///
/// `.claude/skills/<name>/SKILL.md` and `.claude/commands/<name>.md`, from the worktree upwards,
/// plus the same two under `$HOME`. Command files are the older form of the same thing and are
/// dispatched identically, which is why they are one list here.
#[must_use]
pub fn claude(cwd: &Path, home: Option<&Path>) -> Vec<AgentSkill> {
    let mut roots = Vec::new();
    for base in ascend(cwd) {
        roots.push(Root {
            path: base.join(".claude/skills"),
            scope: "project",
        });
        roots.push(Root {
            path: base.join(".claude/commands"),
            scope: "project",
        });
    }
    if let Some(home) = home {
        roots.push(Root {
            path: home.join(".claude/skills"),
            scope: "personal",
        });
        roots.push(Root {
            path: home.join(".claude/commands"),
            scope: "personal",
        });
    }
    collect(&roots)
}

/// Skills discoverable for a Codex session working in `cwd`.
///
/// Codex looks in `.agents/skills` rather than `.claude/skills`, in the working directory or the
/// repository root, plus `$HOME/.agents/skills`. Its own `skills/list` covers the bundled and admin
/// scopes, which is why they are not repeated here.
#[must_use]
pub fn codex(cwd: &Path, home: Option<&Path>) -> Vec<AgentSkill> {
    let mut roots: Vec<Root> = ascend(cwd)
        .into_iter()
        .map(|base| Root {
            path: base.join(".agents/skills"),
            scope: "project",
        })
        .collect();
    if let Some(home) = home {
        roots.push(Root {
            path: home.join(".agents/skills"),
            scope: "personal",
        });
    }
    collect(&roots)
}

/// `cwd` and its parents, nearest first, bounded by [`ASCEND`].
fn ascend(cwd: &Path) -> Vec<PathBuf> {
    cwd.ancestors()
        .take(ASCEND)
        .map(Path::to_path_buf)
        .collect()
}

/// Every skill under these roots, nearest scope winning a name collision.
fn collect(roots: &[Root]) -> Vec<AgentSkill> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();

    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root.path) else {
            continue;
        };
        // `read_dir` yields in whatever order the filesystem likes, which would make the composer's
        // list reshuffle between launches for no reason anybody could explain.
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();

        for path in paths {
            let Some(skill) = read_skill(&path, root.scope) else {
                continue;
            };
            // First root wins: the worktree's own skill shadows one of the same name in a parent or
            // in `$HOME`, which is the precedence both CLIs document.
            if seen.insert(skill.name.clone()) {
                out.push(skill);
            }
        }
    }
    out
}

/// One directory holding a `SKILL.md`, or one `.md` command file.
fn read_skill(path: &Path, scope: &str) -> Option<AgentSkill> {
    let (name, file) = if path.is_dir() {
        (
            path.file_name()?.to_str()?.to_owned(),
            path.join("SKILL.md"),
        )
    } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
        (path.file_stem()?.to_str()?.to_owned(), path.to_path_buf())
    } else {
        return None;
    };

    // Only the head: a skill body runs to hundreds of lines and none of it is frontmatter. Reading
    // whole files for a menu that opens on a keystroke is the cost this bound exists to refuse.
    let text = read_head(&file)?;
    let front = frontmatter(&text);

    // `user-invocable: false` means the model may use it and a person may not, so it is not a
    // command and does not belong in a menu of things you can type.
    if front_value(front, "user-invocable").as_deref() == Some("false") {
        return None;
    }

    Some(AgentSkill {
        name: front_value(front, "name").unwrap_or(name),
        description: front_value(front, "description"),
        scope: Some(scope.to_owned()),
    })
}

/// How much of a skill file is read looking for its frontmatter.
const HEAD_BYTES: usize = 4_096;

fn read_head(file: &Path) -> Option<String> {
    use std::io::Read;

    let mut buffer = vec![0u8; HEAD_BYTES];
    let mut handle = std::fs::File::open(file).ok()?;
    let read = handle.read(&mut buffer).ok()?;
    buffer.truncate(read);
    // Lossy rather than a failure: a description with a stray byte in it is still a better label
    // than no label, and this is a menu rather than a parser anybody depends on.
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// The YAML frontmatter block, if the file opens with one.
fn frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---") else {
        return "";
    };
    let rest = rest.trim_start_matches(['\r', '\n']);
    match rest.find("\n---") {
        Some(end) => &rest[..end],
        // Truncated by `HEAD_BYTES` before the closing fence. Everything read so far is still
        // frontmatter, and `name` and `description` are conventionally at the top of it.
        None => rest,
    }
}

/// A scalar `key: value` from the frontmatter.
///
/// Deliberately not a YAML parser. Three of these fields are read, all of them scalars, and pulling
/// in a parser to find them would be a dependency in the crate whose `Cargo.toml` is a proof about
/// what it can do. A multi-line or quoted value degrades to a slightly odd label in a menu.
fn front_value(front: &str, key: &str) -> Option<String> {
    for line in front.lines() {
        let Some((found, value)) = line.split_once(':') else {
            continue;
        };
        if found.trim() != key {
            continue;
        }
        let value = value.trim().trim_matches(['"', '\'']).trim();
        if value.is_empty() {
            return None;
        }
        return Some(value.to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create dirs");
        std::fs::write(path, body).expect("write");
    }

    #[test]
    fn a_projects_skills_are_found_with_the_descriptions_only_their_frontmatter_carries() {
        // The reason this module exists: both CLIs report skills as bare names, so a description
        // for a project skill can come from nowhere else.
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            &dir.path().join(".claude/skills/ship/SKILL.md"),
            "---\nname: ship\ndescription: Ship the branch\n---\n\nBody.\n",
        );
        write(
            &dir.path().join(".claude/commands/deploy.md"),
            "---\ndescription: Deploy it\n---\n\nBody.\n",
        );

        let found = claude(dir.path(), None);
        assert_eq!(
            found,
            vec![
                AgentSkill {
                    name: "ship".to_owned(),
                    description: Some("Ship the branch".to_owned()),
                    scope: Some("project".to_owned()),
                },
                AgentSkill {
                    name: "deploy".to_owned(),
                    description: Some("Deploy it".to_owned()),
                    scope: Some("project".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn a_skill_the_model_may_use_but_a_person_may_not_stays_out_of_the_menu() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            &dir.path().join(".claude/skills/internal/SKILL.md"),
            "---\nname: internal\ndescription: Not for typing\nuser-invocable: false\n---\n",
        );
        assert!(claude(dir.path(), None).is_empty());
    }

    #[test]
    fn a_worktrees_own_skill_shadows_one_of_the_same_name_in_the_home_directory() {
        let project = tempfile::tempdir().expect("temp dir");
        let home = tempfile::tempdir().expect("temp dir");
        write(
            &project.path().join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: This repo's review\n---\n",
        );
        write(
            &home.path().join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: The personal one\n---\n",
        );

        let found = claude(project.path(), Some(home.path()));
        assert_eq!(found.len(), 1, "one entry per name");
        assert_eq!(found[0].description.as_deref(), Some("This repo's review"));
        assert_eq!(found[0].scope.as_deref(), Some("project"));
    }

    #[test]
    fn a_directory_with_no_skills_in_it_is_not_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(claude(dir.path(), None).is_empty());
        assert!(codex(dir.path(), None).is_empty());
    }

    #[test]
    fn a_skill_with_no_frontmatter_still_appears_under_its_directory_name() {
        // Its name is the thing you type, and that comes from the path. A missing description is a
        // blank second column, which is what the menu showed for every skill before this existed.
        let dir = tempfile::tempdir().expect("temp dir");
        write(&dir.path().join(".claude/skills/bare/SKILL.md"), "Body.\n");

        let found = claude(dir.path(), None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "bare");
        assert_eq!(found[0].description, None);
    }

    #[test]
    fn codex_reads_its_own_directory_and_not_claudes() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            &dir.path().join(".claude/skills/ship/SKILL.md"),
            "---\nname: ship\n---\n",
        );
        write(
            &dir.path().join(".agents/skills/land/SKILL.md"),
            "---\nname: land\ndescription: Land it\n---\n",
        );

        let found = codex(dir.path(), None);
        assert_eq!(found.len(), 1, "only `.agents/skills`");
        assert_eq!(found[0].name, "land");
    }
}

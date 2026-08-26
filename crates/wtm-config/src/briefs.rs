//! Plans, stored as documents.
//!
//! # Why `Brief` and not `Plan`
//!
//! `wtm-core`'s `model::plan` has owned that word since v0.1 — `CreatePlan`, `PlanPreview`,
//! `BranchPlan`, `PlanWarning` — for the create pipeline's preview. A second `Plan` would make every
//! import a decision. `Brief` is what these actually are: a document written to be handed to someone
//! else, which is the whole point of keeping them.
//!
//! An agent's *live* step list is a different thing again and is not stored at all: it changes during
//! a turn and is a progress widget, so it renders and is gone. A `Brief` is promoted when a plan
//! stops moving — when its approval is allowed, or when the user asks.
//!
//! # Why not in the worktree
//!
//! wtm writes nothing into a repository, and that property is worth more than the convenience of a
//! plan sitting next to the code. A file appearing in `git status` that the user did not create is
//! the kind of thing that makes a tool untrustworthy. `.git/` was the other candidate and is worse:
//! that directory is git's, and another tool's documents do not belong in it.
//!
//! # What this file is honest about
//!
//! A plan is agent-authored prose about a codebase, so this store is a **secret-bearing file the
//! user did not create**. That is a real cost of persisting them, and it is why the directory is
//! `0700`, the files are `0600`, and the README's environment-values section names this location.
//! The alternative — keeping briefs in memory so they die with the app — was weighed and rejected:
//! a plan the app forgets on restart cannot be the thing you hand to a reviewer tomorrow, which is
//! the feature.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wtm_core::error::ConfigError;

/// Everything about a brief except its text.
///
/// A sidecar rather than front matter, so the `.md` is a plain markdown file anything can open and
/// the metadata can gain fields without touching a document the user may have edited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefMeta {
    /// A short label for a list. The plan's first heading, or its first line.
    pub title: String,
    /// Which provider wrote it, by catalogue id.
    pub provider: String,
    /// The worktree it was written in — its absolute path.
    pub worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The provider's own session id, so "open the session that wrote this" is possible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session: Option<String>,
    /// Where the provider wrote its own copy, when it wrote one.
    ///
    /// Claude Code writes a plan to `~/.claude/plans/*.md` itself. Recorded rather than followed:
    /// wtm keeps its own copy because the CLI's is outside wtm's control and may be pruned, but the
    /// path is worth having when the two need reconciling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_path: Option<String>,
    pub created: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// One stored plan: its metadata and its markdown.
#[derive(Debug, Clone, PartialEq)]
pub struct Brief {
    pub id: String,
    pub meta: BriefMeta,
    pub markdown: String,
}

/// Where briefs live for one project.
///
/// Keyed by a hash of the project root rather than by the path itself: a path contains separators and
/// spaces and can exceed a filename limit, and the alternative — sanitising it — produces two
/// projects that collide the day two roots differ only by a character that got replaced.
#[must_use]
pub fn project_dir(root: &Path, project_root: &str) -> PathBuf {
    root.join("plans").join(digest(project_root))
}

/// A short, stable, filesystem-safe key for a string.
///
/// Not `sha2`: this is a directory name rather than a security boundary, and the trust store's use of
/// a real hash is what makes that distinction worth keeping visible. FNV-1a in six lines, hex, which
/// is collision-safe enough for the number of repositories one person registers.
fn digest(value: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Store a brief, returning its id.
///
/// The id is derived from the creation timestamp so a directory listing is chronological, which is
/// the order a plan list wants and means no index file has to be kept in step.
///
/// # Errors
///
/// If the directory cannot be created or either file cannot be written.
pub fn save(dir: &Path, meta: &BriefMeta, markdown: &str) -> Result<String, ConfigError> {
    let io = |path: &Path, e: &std::io::Error| ConfigError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    };

    std::fs::create_dir_all(dir).map_err(|e| io(dir, &e))?;
    restrict(dir, 0o700);

    // Colons are legal on the platforms this app targets but are a path separator in some tooling and
    // in Finder's display, so the timestamp is flattened. A counter suffix would need a directory
    // read to pick; a nanosecond-precision stamp does not collide in practice. A second-level
    // collision still happens if two saves share a clock tick, so the exclusive create below
    // retries with `-2`, `-3`, … rather than overwriting.
    let base = meta
        .created
        .replace([':', '.', '+'], "-")
        .trim_end_matches('Z')
        .to_owned();

    let mut id = base.clone();
    let mut suffix = 2_u32;
    loop {
        let body = dir.join(format!("{id}.md"));
        match create_restricted(&body) {
            Ok(mut file) => {
                file.write_all(markdown.as_bytes())
                    .map_err(|e| io(&body, &e))?;
                let sidecar = dir.join(format!("{id}.toml"));
                let text = toml::to_string_pretty(meta).map_err(|e| ConfigError::Io {
                    path: sidecar.clone(),
                    message: format!("serialize the brief metadata: {e}"),
                })?;
                match create_restricted(&sidecar) {
                    Ok(mut file) => {
                        file.write_all(text.as_bytes())
                            .map_err(|e| io(&sidecar, &e))?;
                        return Ok(id);
                    }
                    Err(error) => {
                        let _ = std::fs::remove_file(&body);
                        return Err(io(&sidecar, &error));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                id = format!("{base}-{suffix}");
                suffix = suffix.saturating_add(1);
                if suffix > 1_000 {
                    return Err(ConfigError::Io {
                        path: dir.to_path_buf(),
                        message: "could not mint a unique brief id".to_owned(),
                    });
                }
            }
            Err(error) => return Err(io(&body, &error)),
        }
    }
}

/// Every brief in a directory, newest first.
///
/// A missing directory is an empty list rather than an error: a project with no plans has never had
/// this directory created, which is the ordinary case.
#[must_use]
pub fn list(dir: &Path) -> Vec<Brief> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut out: Vec<Brief> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .filter_map(|entry| {
            let sidecar = entry.path();
            let id = sidecar.file_stem()?.to_str()?.to_owned();
            let meta: BriefMeta = toml::from_str(&std::fs::read_to_string(&sidecar).ok()?).ok()?;
            // A sidecar with no body is a half-written brief — from a crash between the two writes.
            // Skipped rather than shown, because a plan with no text is not a plan.
            let markdown = std::fs::read_to_string(sidecar.with_extension("md")).ok()?;
            Some(Brief { id, meta, markdown })
        })
        .collect();

    // The id is a timestamp, so a plain reverse sort is chronological — which is why no index file
    // has to be kept in step with the directory.
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

/// Whether `id` is a name [`save`] could have minted, and nothing else.
///
/// `save` derives the id from a flattened RFC 3339 timestamp — ASCII digits, `-`, and the `T`
/// separator — so a real id matches `[0-9A-Za-z-]+`. That matters because the id crosses IPC from
/// the webview (`remove_brief`, and any read), and `Path::join` follows `..`: an unchecked id is a
/// path-traversal primitive, so `../../…/secret` would read or **delete** a `.md`/`.toml` outside
/// the plans directory. Refusing anything outside the minted shape closes that without a
/// canonicalisation dance, and rejects the empty string, which would otherwise target the
/// directory itself. This is the same containment `wtm_exec::path::app_bundle_in` does for a value
/// that is only ever a literal.
#[must_use]
fn is_minted_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Read one brief by id.
#[must_use]
pub fn read(dir: &Path, id: &str) -> Option<Brief> {
    if !is_minted_id(id) {
        return None;
    }
    let sidecar = dir.join(format!("{id}.toml"));
    let meta: BriefMeta = toml::from_str(&std::fs::read_to_string(&sidecar).ok()?).ok()?;
    let markdown = std::fs::read_to_string(dir.join(format!("{id}.md"))).ok()?;
    Some(Brief {
        id: id.to_owned(),
        meta,
        markdown,
    })
}

/// Delete a brief.
///
/// Infallible, and deliberately so: a missing file is already the desired state, so there is nothing
/// a caller could do about a failure except ignore it. The first version returned a `Result` on the
/// grounds that a future non-filesystem store might need one — which is a signature written for a
/// store that does not exist, and clippy was right to say so.
pub fn remove(dir: &Path, id: &str) {
    // A traversing id would delete files outside the plans directory. See `is_minted_id`.
    if !is_minted_id(id) {
        return;
    }
    let _ = std::fs::remove_file(dir.join(format!("{id}.md")));
    let _ = std::fs::remove_file(dir.join(format!("{id}.toml")));
}

/// A title for a plan, from its first heading or its first non-empty line.
///
/// Truncated, because a heading can be a sentence and this goes in a list.
#[must_use]
pub fn title_of(markdown: &str) -> String {
    let line = markdown
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Untitled plan");
    let stripped = line.trim_start_matches('#').trim();
    let mut title: String = stripped.chars().take(80).collect();
    if stripped.chars().count() > 80 {
        title.push('…');
    }
    if title.is_empty() {
        "Untitled plan".to_owned()
    } else {
        title
    }
}

/// Create a new file that is already `0600`, so a crash between `write` and `chmod` cannot
/// leave a world-readable brief. `create_new` is also what makes a colliding id fail instead
/// of silently overwriting another plan.
fn create_restricted(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Tighten a path's permissions, best-effort.
///
/// A plan is agent-authored prose about a codebase, so this store holds material the user did not
/// write and may not expect to exist. `0700`/`0600` is the same posture the trust store has, and a
/// failure is logged rather than fatal: on a filesystem that cannot express a mode, refusing to save
/// the plan would be worse than saving it with the default.
fn restrict(path: &Path, mode: u32) {
    // `cfg(unix)` rather than a per-OS seam, so `platform_seams.rs` does not count it — and both
    // platforms this app builds for are unix, so no arm is deleted on either.
    //
    // Naming that attribute in full here is what *made* this a counted seam: the scan is by line
    // text and does not care that the line is a comment, so the note explaining the avoidance
    // tripped the check it was describing.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
            tracing::debug!(path = %path.display(), %error, "could not restrict a brief's mode");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(title: &str, created: &str) -> BriefMeta {
        BriefMeta {
            title: title.to_owned(),
            provider: "claude".to_owned(),
            worktree: "/w/a".to_owned(),
            model: Some("opus".to_owned()),
            provider_session: Some("abc".to_owned()),
            provider_path: None,
            created: created.to_owned(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn a_brief_survives_a_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let id = save(
            dir.path(),
            &meta("Do the thing", "2026-08-05T12:00:00Z"),
            "# Do the thing\n",
        )
        .unwrap();

        let read = read(dir.path(), &id).expect("a brief");
        assert_eq!(read.markdown, "# Do the thing\n");
        assert_eq!(read.meta.provider, "claude");
        assert_eq!(read.meta.provider_session.as_deref(), Some("abc"));
    }

    #[test]
    fn the_list_is_newest_first_without_an_index_file() {
        // The id is the creation timestamp, so a reverse sort is chronological — which is why nothing
        // has to keep an index in step with the directory.
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &meta("first", "2026-08-05T10:00:00Z"), "a").unwrap();
        save(dir.path(), &meta("second", "2026-08-05T11:00:00Z"), "b").unwrap();
        save(dir.path(), &meta("third", "2026-08-05T12:00:00Z"), "c").unwrap();

        let titles: Vec<String> = list(dir.path()).into_iter().map(|b| b.meta.title).collect();
        assert_eq!(titles, ["third", "second", "first"]);
    }

    #[test]
    fn a_sidecar_with_no_body_is_skipped_rather_than_shown() {
        // The shape a crash between the two writes leaves behind. A plan with no text is not a plan.
        let dir = tempfile::tempdir().unwrap();
        let id = save(dir.path(), &meta("orphan", "2026-08-05T12:00:00Z"), "x").unwrap();
        std::fs::remove_file(dir.path().join(format!("{id}.md"))).unwrap();
        assert!(list(dir.path()).is_empty());
    }

    #[test]
    fn a_missing_directory_is_an_empty_list_rather_than_an_error() {
        // A project with no plans has never had the directory created, which is the ordinary case.
        assert!(list(Path::new("/nonexistent/wtm/plans/deadbeef")).is_empty());
    }

    #[test]
    fn two_projects_get_two_directories() {
        // Hashed rather than sanitised: sanitising two roots that differ only by a replaced character
        // would collide, and the collision would silently mix one project's plans into another's.
        let root = Path::new("/cfg");
        assert_ne!(
            project_dir(root, "/Users/dev/code/one"),
            project_dir(root, "/Users/dev/code/two")
        );
        // Stable across calls, or a restart would lose every plan.
        assert_eq!(
            project_dir(root, "/Users/dev/code/one"),
            project_dir(root, "/Users/dev/code/one")
        );
    }

    #[test]
    fn a_title_comes_from_the_first_heading() {
        assert_eq!(title_of("# Add a comment\n\nbody"), "Add a comment");
        assert_eq!(title_of("\n\n  ## Nested heading\n"), "Nested heading");
        // No heading: the first line that says anything.
        assert_eq!(title_of("just prose\nmore"), "just prose");
        assert_eq!(title_of(""), "Untitled plan");
        // A heading with nothing after the hashes is not a title.
        assert_eq!(title_of("#\n"), "Untitled plan");
    }

    #[test]
    fn a_long_heading_is_truncated_for_a_list() {
        let long = format!("# {}", "x".repeat(200));
        let title = title_of(&long);
        assert!(title.chars().count() <= 81, "got {}", title.chars().count());
        assert!(title.ends_with('…'));
    }

    #[test]
    fn a_brief_id_that_escapes_the_plans_directory_is_refused() {
        // The id crosses IPC from the webview and is interpolated into a path; `Path::join` follows
        // `..`, so an unchecked id reads or deletes files outside the plans directory. Here `plans`
        // sits beside a `secret.md`, and `../secret` resolves to it.
        let root = tempfile::tempdir().unwrap();
        let plans = root.path().join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        let secret = root.path().join("secret.md");
        std::fs::write(&secret, "do not delete me").unwrap();

        remove(&plans, "../secret");
        assert!(
            secret.exists(),
            "traversal id must not delete an outside file"
        );
        assert!(
            read(&plans, "../secret").is_none(),
            "traversal id must not read out"
        );

        // Separator- and dot-bearing ids are refused whatever they point at.
        for hostile in ["../secret", "a/b", "a.b", "", "..", "a\\b"] {
            assert!(!is_minted_id(hostile), "should refuse {hostile:?}");
        }
        // A real minted id is still accepted, so the guard does not break ordinary use.
        let id = save(&plans, &meta("ok", "2026-08-05T12:00:00Z"), "body").unwrap();
        assert!(is_minted_id(&id));
        assert!(read(&plans, &id).is_some());
    }

    #[test]
    fn a_second_save_at_the_same_timestamp_gets_a_suffix_instead_of_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let first = save(dir.path(), &meta("one", "2026-08-05T12:00:00Z"), "first").unwrap();
        let second = save(dir.path(), &meta("two", "2026-08-05T12:00:00Z"), "second").unwrap();
        assert_eq!(first, "2026-08-05T12-00-00");
        assert_eq!(second, "2026-08-05T12-00-00-2");
        assert_eq!(read(dir.path(), &first).unwrap().markdown, "first");
        assert_eq!(read(dir.path(), &second).unwrap().markdown, "second");
        assert!(is_minted_id(&second));
    }

    #[cfg(unix)]
    #[test]
    fn a_brief_is_not_world_readable() {
        // It is agent-authored prose about a codebase — material the user did not write and may not
        // expect to exist. Same posture as the trust store.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let id = save(
            dir.path(),
            &meta("secret", "2026-08-05T12:00:00Z"),
            "creds inside",
        )
        .unwrap();
        let mode = std::fs::metadata(dir.path().join(format!("{id}.md")))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "group and other must have no access");
    }
}

//! Review knowledge for Settings › Knowledge and the Save as rule dialog: your rules, and the
//! repository's instruction sources they sit beside.
//!
//! # What is here and what is not
//!
//! The rules themselves are `wtm_core::flow::rule`, their retrieval `wtm_core::flow::knowledge`,
//! and their storage `wtm-appdb`. This module is the composition root's part: the IPC commands, the
//! views they answer with, and reading the instruction source files, which is I/O. The review
//! engine calls [`instruction_set`] and the store's `record_applications` when it starts a role;
//! nothing here starts one.
//!
//! # Instruction sources are read, never written
//!
//! There is no command here, or anywhere, that writes one. A file is read only if git lists it in
//! the worktree and it resolves inside the worktree, so a committed symlink cannot hand a review
//! the contents of a file elsewhere on the machine, such as a key under `~/.ssh`, to send to a
//! provider.

use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use wtm_appdb::AppDb;
use wtm_core::flow::instructions::{self, MAX_DOCUMENT_BYTES, MAX_TOTAL_BYTES};
use wtm_core::flow::{
    DEFAULT_INSTRUCTION_SOURCES, Finding, FindingId, FlowId, InstructionDocument, InstructionSet,
    NewRule, Rule, RuleChange, RuleDraft, RuleId, RuleUsage, SkippedDocument, UnixMs, WorkflowSpec,
};
use wtm_core::model::{Project, ProjectId};
use wtm_core::ports::git::Git;
use wtm_core::ports::{FlowStore, RuleStore, RuleStoreError, RunStore};

use crate::app::App;
use crate::commands::{AppState, Reply, blocking};
use crate::view::ErrorView;

/// A rule as Settings shows it: the rule, how much reviews have used it, and the work item it
/// came out of.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleView {
    #[serde(flatten)]
    pub rule: Rule,
    pub usage: Option<UsageView>,
    /// The work item of the flow the rule came from, such as `APP-8701` or `PR #4031`.
    pub source_subject: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageView {
    pub reviews: u32,
    pub last_at: UnixMs,
    pub last_subject: Option<String>,
}

/// Which instruction sources a project's reviews read, without their text.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionSourcesView {
    pub patterns: Vec<String>,
    /// False when the patterns are the default ones because the project names none.
    pub configured: bool,
    pub documents: Vec<DocumentView>,
    /// Patterns that matched no file.
    pub unmatched: Vec<String>,
    pub skipped: Vec<SkippedDocument>,
    /// The worktree listing was cut short, so a source may exist and not be listed.
    pub incomplete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentView {
    pub path: String,
    pub pattern: String,
    pub truncated: bool,
}

/// A command's view of the store, with every error made into something the window can show.
fn store(app: &App) -> Result<Arc<AppDb>, ErrorView> {
    app.appdb.get()
}

fn refused(error: RuleStoreError) -> ErrorView {
    // A refusal's detail is the rule error itself, such as `{"kind": "bad_glob", ...}`, which is
    // what the editor reads to put the message under the right field. Serialized whole it would be
    // two `kind` keys, the store's and the rule's, and the second would silently win.
    let message = error.to_string();
    match error {
        RuleStoreError::Refused(rule) => {
            let view = ErrorView::new("validation", message);
            match serde_json::to_value(&rule) {
                Ok(detail) => view.with_detail(detail),
                Err(_) => view,
            }
        }
        other => {
            let view = ErrorView::new("rules", message);
            match serde_json::to_value(&other) {
                Ok(detail) => view.with_detail(detail),
                Err(_) => view,
            }
        }
    }
}

fn now(app: &App) -> UnixMs {
    app.clock.now_unix_ms()
}

/// The project, and the rule if it is one of the project's.
fn owned_rule(db: &AppDb, project: &ProjectId, id: RuleId) -> Result<Rule, ErrorView> {
    match db.rule(id).map_err(refused)? {
        Some(rule) if &rule.project == project => Ok(rule),
        // The same answer for another project's rule as for none, so a stale window cannot edit a
        // rule it was never showing.
        _ => Err(refused(RuleStoreError::UnknownRule(id))),
    }
}

/// What a flow's work item is called, for "from a finding on APP-8701".
fn subject(db: &AppDb, flow: FlowId) -> Option<String> {
    let flow = db.flow(flow).ok()??;
    let item = db.work_item(flow.work_item).ok()??;
    Some(match (&item.issue, &item.pull_request) {
        (Some(issue), _) => issue.key.clone(),
        (None, Some(pr)) => format!("PR #{}", pr.number),
        (None, None) => item.title,
    })
}

fn views(db: &AppDb, project: &ProjectId) -> Result<Vec<RuleView>, ErrorView> {
    let usage = db.usage(project).map_err(refused)?;
    let rules = db.rules(project).map_err(refused)?;
    Ok(rules
        .into_iter()
        .map(|rule| view_of(db, rule, &usage))
        .collect())
}

fn view_of(db: &AppDb, rule: Rule, usage: &[RuleUsage]) -> RuleView {
    let usage = usage
        .iter()
        .find(|usage| usage.rule == rule.id)
        .map(|usage| UsageView {
            reviews: usage.reviews,
            last_at: usage.last_at,
            last_subject: subject(db, usage.last_flow),
        });
    let source_subject = rule.source.flow().and_then(|flow| subject(db, flow));
    RuleView {
        rule,
        usage,
        source_subject,
    }
}

fn single(db: &AppDb, rule: Rule) -> Result<RuleView, ErrorView> {
    let usage = db.usage(&rule.project).map_err(refused)?;
    Ok(view_of(db, rule, &usage))
}

/// The instruction sources `project` names, or the default ones.
#[must_use]
pub fn patterns(project: &Project) -> (Vec<String>, bool) {
    match project
        .workflow
        .as_ref()
        .and_then(|workflow| workflow.instruction_sources.as_ref())
    {
        Some(configured) => (configured.clone(), true),
        None => (
            project
                .workflow
                .as_ref()
                .map_or_else(default_patterns, WorkflowSpec::instruction_sources),
            false,
        ),
    }
}

fn default_patterns() -> Vec<String> {
    DEFAULT_INSTRUCTION_SOURCES
        .iter()
        .map(|source| (*source).to_owned())
        .collect()
}

/// Read the instruction sources `patterns` names in the checkout at `root`, which for a review is
/// the snapshot checkout the role reads.
///
/// Returns the set and whether git's listing was cut short. A file that cannot be given to a role
/// is listed in `skipped` with the reason, never dropped silently.
pub fn instruction_set(
    git: &dyn Git,
    root: &Path,
    patterns: &[String],
) -> Result<(InstructionSet, bool), ErrorView> {
    let listing = git.files(root)?;
    let sources = instructions::resolve(patterns, &listing.paths);
    let canonical_root = root
        .canonicalize()
        .map_err(|error| ErrorView::new("io", format!("{}: {error}", root.display())))?;

    let mut set = InstructionSet {
        sources: sources.clone(),
        ..InstructionSet::default()
    };
    let mut budget = MAX_TOTAL_BYTES;
    for source in &sources {
        for path in &source.paths {
            match read_document(&canonical_root, path, budget) {
                Ok((text, truncated)) => {
                    budget -= text.len();
                    set.documents.push(InstructionDocument {
                        path: path.clone(),
                        pattern: source.pattern.clone(),
                        text,
                        truncated,
                    });
                }
                Err(reason) => set.skipped.push(SkippedDocument {
                    path: path.clone(),
                    reason,
                }),
            }
        }
    }
    Ok((set, listing.truncated))
}

/// One source file's text, cut to what is left of the budget, or why it is not read.
fn read_document(root: &Path, path: &str, budget: usize) -> Result<(String, bool), String> {
    if budget == 0 {
        return Err("the other instruction sources already fill a role's allowance".to_owned());
    }
    let resolved = root
        .join(path)
        .canonicalize()
        .map_err(|error| format!("cannot be read: {error}"))?;
    if !resolved.starts_with(root) {
        return Err("it links outside the repository".to_owned());
    }
    let metadata = std::fs::metadata(&resolved).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("it is not a file".to_owned());
    }
    // Refused before reading rather than read and cut, so an enormous generated file someone
    // globbed by accident is not loaded into memory to keep its first 64 KiB.
    let ceiling = u64::try_from(MAX_DOCUMENT_BYTES * 16).unwrap_or(u64::MAX);
    if metadata.len() > ceiling {
        return Err(format!(
            "it is {} KiB, far past the {} KiB a role is given",
            metadata.len() / 1024,
            MAX_DOCUMENT_BYTES / 1024
        ));
    }
    let bytes = std::fs::read(&resolved).map_err(|error| error.to_string())?;
    let mut text = String::from_utf8(bytes).map_err(|_| "it is not UTF-8 text".to_owned())?;
    let truncated = instructions::truncate(&mut text, MAX_DOCUMENT_BYTES.min(budget));
    Ok((text, truncated))
}

#[tauri::command]
pub async fn list_rules(app: AppState<'_>, project_id: String) -> Reply<Vec<RuleView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        views(&*store(&app)?, &project.id)
    })
    .await
}

#[tauri::command]
pub async fn instruction_sources(
    app: AppState<'_>,
    project_id: String,
) -> Reply<InstructionSourcesView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let (patterns, configured) = patterns(&project);
        let (set, incomplete) = instruction_set(app.git.as_ref(), &project.root, &patterns)?;
        Ok(InstructionSourcesView {
            unmatched: set
                .sources
                .iter()
                .filter(|source| source.paths.is_empty())
                .map(|source| source.pattern.clone())
                .collect(),
            documents: set
                .documents
                .into_iter()
                .map(|document| DocumentView {
                    path: document.path,
                    pattern: document.pattern,
                    truncated: document.truncated,
                })
                .collect(),
            skipped: set.skipped,
            patterns,
            configured,
            incomplete,
        })
    })
    .await
}

#[tauri::command]
pub async fn create_rule(
    app: AppState<'_>,
    project_id: String,
    draft: RuleDraft,
) -> Reply<RuleView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let new = NewRule::written(project.id, draft).map_err(|e| refused(e.into()))?;
        let db = store(&app)?;
        let rule = db.create_rule(&new, now(&app)).map_err(refused)?;
        single(&db, rule)
    })
    .await
}

/// Save a finding as a rule.
///
/// Takes the finding's id and reads it from the store, so the dismissal `NewRule::from_finding`
/// refuses is the store's and not the window's copy, and the rule's provenance names a finding that
/// exists. A finding of another project's review is refused as if it did not exist.
#[tauri::command]
pub async fn save_finding_as_rule(
    app: AppState<'_>,
    project_id: String,
    finding_id: FindingId,
    draft: RuleDraft,
) -> Reply<RuleView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let db = store(&app)?;
        let finding = finding_of(&db, &project.id, finding_id)?;
        let new =
            NewRule::from_finding(project.id, &finding, draft).map_err(|e| refused(e.into()))?;
        let rule = db.create_rule(&new, now(&app)).map_err(refused)?;
        single(&db, rule)
    })
    .await
}

/// A finding of one of `project`'s reviews, from the store.
fn finding_of(db: &AppDb, project: &ProjectId, id: FindingId) -> Result<Finding, ErrorView> {
    let missing = || ErrorView::new("rules", format!("there is no finding {id} in this project"));
    let record = db
        .finding(id)
        .map_err(|e| ErrorView::new("rules", e.to_string()))?
        .ok_or_else(missing)?;
    match db.review(record.finding.flow) {
        Ok(Some(review)) if &review.project == project => Ok(record.finding),
        _ => Err(missing()),
    }
}

#[tauri::command]
pub async fn change_rule(
    app: AppState<'_>,
    project_id: String,
    rule_id: RuleId,
    change: RuleChange,
) -> Reply<RuleView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let db = store(&app)?;
        owned_rule(&db, &project.id, rule_id)?;
        let rule = db
            .change_rule(rule_id, &change, now(&app))
            .map_err(refused)?;
        single(&db, rule)
    })
    .await
}

/// Replace a rule with a new one. Answers with the new rule.
#[tauri::command]
pub async fn replace_rule(
    app: AppState<'_>,
    project_id: String,
    rule_id: RuleId,
    draft: RuleDraft,
) -> Reply<RuleView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let db = store(&app)?;
        owned_rule(&db, &project.id, rule_id)?;
        let (_, created) = db
            .replace_rule(rule_id, &draft, now(&app))
            .map_err(refused)?;
        single(&db, created)
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use wtm_config::AppPaths;
    use wtm_core::flow::{FindingId, RuleScope};
    use wtm_testkit::GitFixture;

    #[test]
    fn a_rule_view_is_camel_case_and_flattens_the_rule() {
        let rule = Rule::create(
            RuleId(14),
            &NewRule::written(
                ProjectId::from_root(Path::new("/x/webapp")),
                RuleDraft {
                    kind: wtm_core::flow::RuleKind::Required,
                    text: "Use AppDialog".to_owned(),
                    rationale: String::new(),
                    scope: RuleScope::default(),
                    examples: Vec::new(),
                    references: Vec::new(),
                },
            )
            .unwrap(),
            100,
        );
        let json = serde_json::to_value(RuleView {
            rule,
            usage: Some(UsageView {
                reviews: 6,
                last_at: 200,
                last_subject: Some("APP-8701".to_owned()),
            }),
            source_subject: None,
        })
        .unwrap();

        // Snake_case leaking through would silently break every binding in the Knowledge tab, which
        // `src/lib/ipc/types.ts` mirrors by hand.
        for key in [
            "id",
            "project",
            "origin",
            "source",
            "status",
            "approval",
            "supersededBy",
            "revisions",
            "updatedAt",
            "usage",
            "sourceSubject",
        ] {
            assert!(json.get(key).is_some(), "missing {key}: {json}");
        }
        assert_eq!(json["usage"]["lastSubject"], "APP-8701");
        assert_eq!(json["source"]["kind"], "written");
        assert_eq!(
            json["revisions"][0]["draft"]["scope"]["paths"],
            serde_json::json!([])
        );
    }

    #[test]
    fn a_refused_glob_reaches_the_window_as_the_rule_errors_own_kind() {
        // The editor puts the message under Files only when the detail says `bad_glob`.
        let error = refused(RuleStoreError::Refused(
            wtm_core::flow::RuleError::BadGlob {
                glob: "../x".to_owned(),
                message: "a path glob cannot climb out of the repository with `..`".to_owned(),
            },
        ));
        assert_eq!(error.kind, "validation");
        assert_eq!(error.detail.unwrap()["kind"], "bad_glob");
    }

    #[test]
    fn the_window_spells_changes_and_findings_the_way_the_domain_reads_them() {
        // `types.ts` writes these by hand, so a rename on either side has to fail somewhere.
        let change: RuleChange = serde_json::from_value(serde_json::json!({
            "kind": "approve_revised", "draft": {
                "kind": "guidance", "text": "t", "rationale": "", "examples": [
                    { "stance": "prefer", "text": "src/a.vue" }
                ], "references": [], "scope": { "areas": [], "paths": [] }
            }
        }))
        .unwrap();
        assert!(matches!(change, RuleChange::ApproveRevised { .. }));

        let finding: Finding = serde_json::from_value(serde_json::json!({
            "id": 3, "flow": 1, "class": { "kind": "suggestion" }, "severity": "notable",
            "location": { "path": "src/a.vue", "startLine": 12, "endLine": 12 },
            "title": "t", "rationale": "r", "requirement": null, "treeSha": "9f", "dismissed": null
        }))
        .unwrap();
        assert_eq!(finding.id, FindingId(3));
    }

    /// A repository with `files` committed, and an app whose git is the real one.
    fn repository(files: &[(&str, &str)]) -> (GitFixture, tempfile::TempDir, App) {
        let fixture = GitFixture::new();
        for (path, text) in files {
            fixture.commit(path, text, "instructions");
        }
        let config = tempfile::tempdir().unwrap();
        let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();
        (fixture, config, app)
    }

    #[test]
    fn the_default_sources_are_read_from_the_files_git_lists() {
        let (repo, _config, app) = repository(&[
            ("CLAUDE.md", "Use bun.\n"),
            ("docs/review/focus.md", "Watch the export paths.\n"),
            ("src/main.ts", "export {};\n"),
        ]);

        let (set, incomplete) =
            instruction_set(app.git.as_ref(), repo.root(), &default_patterns()).unwrap();

        assert!(!incomplete);
        let read: Vec<(&str, &str)> = set
            .documents
            .iter()
            .map(|document| (document.path.as_str(), document.text.as_str()))
            .collect();
        assert_eq!(
            read,
            [
                ("CLAUDE.md", "Use bun.\n"),
                ("docs/review/focus.md", "Watch the export paths.\n")
            ]
        );
        assert!(set.skipped.is_empty());
    }

    #[test]
    fn a_source_that_links_outside_the_repository_is_skipped_and_says_why() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("id_ed25519");
        std::fs::write(&secret, "PRIVATE KEY").unwrap();
        let (repo, _config, app) = repository(&[("CLAUDE.md", "Hello.\n")]);
        std::os::unix::fs::symlink(&secret, repo.root().join("REVIEW.md")).unwrap();

        let (set, _) = instruction_set(app.git.as_ref(), repo.root(), &default_patterns()).unwrap();

        assert!(
            set.documents.iter().all(|d| !d.text.contains("PRIVATE")),
            "{set:?}"
        );
        assert_eq!(set.skipped.len(), 1, "{set:?}");
        assert_eq!(set.skipped[0].path, "REVIEW.md");
        assert!(set.skipped[0].reason.contains("outside"), "{set:?}");
    }

    #[test]
    fn a_long_source_is_cut_and_marked_rather_than_dropped() {
        let long = "x".repeat(MAX_DOCUMENT_BYTES + 10);
        let (repo, _config, app) = repository(&[("REVIEW.md", long.as_str())]);

        let (set, _) =
            instruction_set(app.git.as_ref(), repo.root(), &["REVIEW.md".to_owned()]).unwrap();

        assert_eq!(set.documents[0].text.len(), MAX_DOCUMENT_BYTES);
        assert!(set.documents[0].truncated);
    }

    #[test]
    fn a_project_without_a_workflow_reads_the_default_sources() {
        let project = project_of(serde_json::json!({}));
        assert_eq!(patterns(&project), (default_patterns(), false));

        let configured = project_of(serde_json::json!({
            "workflow": { "instruction_sources": ["GUIDE.md"] }
        }));
        assert_eq!(patterns(&configured), (vec!["GUIDE.md".to_owned()], true));
    }

    fn project_of(value: serde_json::Value) -> Project {
        serde_json::from_value(value).unwrap()
    }
}

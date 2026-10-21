//! The rule store against a real SQLite file.
//!
//! These protect three promises review knowledge rests on. Revisions are append-only, so a finding
//! that cites R-14 v1 can always read what v1 said. An agent's proposal never applies until you
//! approve it, and nothing turns a dismissed finding into a rule. And a review records exactly the
//! revisions it was given, so a later finding's citation can be checked against that record; the
//! plan's acceptance scenario for review knowledge is the last test here.
//!
//! Every test opens a fresh database in a temporary directory, as the flow store's tests do.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rusqlite::Connection;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    Dismissal, ExampleStance, Finding, FindingClass, FindingId, FlowId, FlowKind, Location,
    MatchReason, NewRule, NewWorkItem, ReviewInvocation, Rule, RuleApproval, RuleChange, RuleDraft,
    RuleError, RuleExample, RuleId, RuleKind, RuleOrigin, RuleQuery, RuleScope, RuleSource,
    RuleStatus, Severity, knowledge,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, RuleStore, RuleStoreError};

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    db: AppDb,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flow.db");
    let db = AppDb::open(&path).unwrap();
    Fixture {
        _dir: dir,
        path,
        db,
    }
}

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
}

fn other_project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/other"))
}

/// A review flow on a fresh ad hoc work item in `project`.
fn review_flow(db: &AppDb, project: ProjectId, title: &str) -> FlowId {
    let item = db
        .create_work_item(
            &NewWorkItem {
                project,
                title: title.to_owned(),
                issue: None,
                pull_request: None,
            },
            10,
        )
        .unwrap();
    db.start_flow(item.id, FlowKind::ReviewMine, 10).unwrap().id
}

fn dialog_draft() -> RuleDraft {
    RuleDraft {
        kind: RuleKind::Required,
        text: "Use AppDialog for confirm and cancel flows".to_owned(),
        rationale: "AppDialog carries the focus trap and Escape handling.".to_owned(),
        scope: RuleScope {
            areas: vec!["frontend".to_owned()],
            paths: vec!["src/**/*.vue".to_owned()],
        },
        examples: vec![RuleExample {
            stance: ExampleStance::Prefer,
            text: "src/components/AppDialog.vue".to_owned(),
        }],
        references: Vec::new(),
    }
}

fn revised(text: &str) -> RuleChange {
    RuleChange::Revise {
        draft: RuleDraft {
            text: text.to_owned(),
            ..dialog_draft()
        },
    }
}

fn written(db: &AppDb) -> Rule {
    db.create_rule(&NewRule::written(project(), dialog_draft()).unwrap(), 100)
        .unwrap()
}

fn finding(flow: FlowId, dismissed: bool) -> Finding {
    Finding {
        id: FindingId(3),
        flow,
        class: FindingClass::Suggestion,
        severity: Severity::Notable,
        location: Some(Location {
            path: "src/exports/ExportModal.vue".to_owned(),
            start_line: 12,
            end_line: 12,
        }),
        title: "Use AppDialog rather than BaseModal".to_owned(),
        rationale: "BaseModal skips the focus trap.".to_owned(),
        requirement: None,
        tree_sha: "9f00".to_owned(),
        dismissed: dismissed.then(|| Dismissal {
            reason: "A deliberate exception.".to_owned(),
            at: 50,
        }),
    }
}

#[test]
fn a_rule_you_write_is_saved_active_with_its_first_revision() {
    let f = fixture();
    let rule = written(&f.db);

    assert_eq!(rule.origin, RuleOrigin::Human);
    assert!(rule.applies());
    assert_eq!(f.db.rule(rule.id).unwrap(), Some(rule.clone()));
    assert_eq!(f.db.rules(&project()).unwrap(), [rule]);
    assert!(f.db.rules(&other_project()).unwrap().is_empty());
}

#[test]
fn an_edit_appends_a_revision_and_the_cited_one_survives_a_reopen() {
    let f = fixture();
    let rule = written(&f.db);

    f.db.change_rule(
        rule.id,
        &revised("Use AppDialog for every confirmation"),
        200,
    )
    .unwrap();
    drop(f.db);
    let reopened = AppDb::open(&f.path).unwrap();

    let stored = reopened.rule(rule.id).unwrap().unwrap();
    assert_eq!(stored.revisions.len(), 2);
    assert_eq!(
        stored.revision(1).unwrap().draft.text,
        "Use AppDialog for confirm and cancel flows"
    );
    assert_eq!(stored.current().unwrap().at, 200);
    assert_eq!(stored.updated_at, 200);
}

#[test]
fn a_revision_cannot_be_rewritten_or_deleted_even_with_raw_sql() {
    let f = fixture();
    let rule = written(&f.db);
    let conn = Connection::open(&f.path).unwrap();

    for sql in [
        "UPDATE rule_revisions SET draft = '{}' WHERE rule_id = ?1",
        "DELETE FROM rule_revisions WHERE rule_id = ?1",
        "DELETE FROM rules WHERE id = ?1",
    ] {
        let error = conn.execute(sql, [rule.id.0]).unwrap_err().to_string();
        assert!(
            error.contains("append-only") || error.contains("never deleted"),
            "{sql}: {error}"
        );
    }
}

#[test]
fn a_revision_cannot_skip_a_number() {
    let f = fixture();
    let rule = written(&f.db);
    let conn = Connection::open(&f.path).unwrap();

    let error = conn
        .execute(
            "INSERT INTO rule_revisions (rule_id, number, draft, at) VALUES (?1, 3, '{}', 1)",
            [rule.id.0],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("numbered in order"), "{error}");
}

#[test]
fn an_agents_proposal_is_stored_pending_and_applies_only_after_approval() {
    let f = fixture();
    let flow = review_flow(&f.db, project(), "Bulk export");
    let proposal = NewRule::proposal(project(), flow, "correctness", dialog_draft(), None).unwrap();

    let pending = f.db.create_rule(&proposal, 100).unwrap();
    assert_eq!(pending.approval, RuleApproval::Pending);
    assert!(!f.db.rule(pending.id).unwrap().unwrap().applies());

    let approved =
        f.db.change_rule(pending.id, &RuleChange::Approve, 200)
            .unwrap();
    assert!(approved.applies());
    assert!(f.db.rule(pending.id).unwrap().unwrap().applies());
}

#[test]
fn the_database_refuses_a_rule_of_yours_that_is_not_approved() {
    // The second line of defence: even a store bug cannot write one of your rules as pending,
    // which would hide it from reviews, or an agent's as approved without the domain deciding.
    let f = fixture();
    let conn = Connection::open(&f.path).unwrap();
    let error = conn
        .execute(
            "INSERT INTO rules (project, origin, status, approval, source, created_at, updated_at)
             VALUES ('/work/webapp', 'human', 'active', 'pending', '{\"kind\":\"written\"}', 1, 1)",
            [],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("CHECK"), "{error}");
}

#[test]
fn a_rejected_proposal_stays_rejected_and_writes_nothing_more() {
    let f = fixture();
    let flow = review_flow(&f.db, project(), "Bulk export");
    let proposal = NewRule::proposal(project(), flow, "correctness", dialog_draft(), None).unwrap();
    let pending = f.db.create_rule(&proposal, 100).unwrap();
    let rejected =
        f.db.change_rule(pending.id, &RuleChange::Reject, 200)
            .unwrap();

    let error =
        f.db.change_rule(pending.id, &RuleChange::Approve, 300)
            .unwrap_err();
    assert!(matches!(
        error,
        RuleStoreError::Refused(RuleError::NotPending {
            approval: RuleApproval::Rejected
        })
    ));
    assert_eq!(f.db.rule(pending.id).unwrap(), Some(rejected));
}

#[test]
fn a_dismissed_finding_never_reaches_the_store() {
    let f = fixture();
    let flow = review_flow(&f.db, project(), "Bulk export");
    let dismissed = finding(flow, true);

    assert_eq!(
        NewRule::from_finding(project(), &dismissed, dialog_draft()),
        Err(RuleError::DismissedFinding)
    );
    assert!(f.db.rules(&project()).unwrap().is_empty());
}

#[test]
fn a_rule_saved_from_a_finding_remembers_the_flow_and_the_finding() {
    let f = fixture();
    let flow = review_flow(&f.db, project(), "Bulk export");
    let new = NewRule::from_finding(project(), &finding(flow, false), dialog_draft()).unwrap();

    let rule = f.db.create_rule(&new, 100).unwrap();

    assert!(rule.applies());
    assert_eq!(rule.source.flow(), Some(flow));
    assert_eq!(f.db.rule(rule.id).unwrap().unwrap().source, rule.source);
}

#[test]
fn a_rule_from_a_flow_that_does_not_exist_is_refused() {
    let f = fixture();
    let new =
        NewRule::from_finding(project(), &finding(FlowId(404), false), dialog_draft()).unwrap();
    assert!(matches!(
        f.db.create_rule(&new, 100),
        Err(RuleStoreError::UnknownFlow(FlowId(404)))
    ));
}

#[test]
fn turning_a_rule_off_and_on_is_kept_across_a_reopen() {
    let f = fixture();
    let rule = written(&f.db);
    f.db.change_rule(rule.id, &RuleChange::Disable, 200)
        .unwrap();
    drop(f.db);

    let reopened = AppDb::open(&f.path).unwrap();
    assert_eq!(
        reopened.rule(rule.id).unwrap().unwrap().status,
        RuleStatus::Disabled
    );
    let enabled = reopened
        .change_rule(rule.id, &RuleChange::Enable, 300)
        .unwrap();
    assert!(enabled.applies());
}

#[test]
fn replacing_a_rule_supersedes_it_and_links_the_two_in_one_write() {
    let f = fixture();
    let old = written(&f.db);
    let replacement = RuleDraft {
        text: "Use AppDialog or AppSheet for confirmations".to_owned(),
        ..dialog_draft()
    };

    let (superseded, new) = f.db.replace_rule(old.id, &replacement, 200).unwrap();

    assert_eq!(superseded.status, RuleStatus::Superseded);
    assert_eq!(superseded.superseded_by, Some(new.id));
    assert_eq!(new.source, RuleSource::Replacement { replaces: old.id });
    assert!(new.applies());
    assert_eq!(f.db.rule(old.id).unwrap(), Some(superseded));
    assert!(matches!(
        f.db.change_rule(old.id, &RuleChange::Enable, 300),
        Err(RuleStoreError::Refused(RuleError::Superseded { by })) if by == new.id
    ));
}

#[test]
fn a_refused_replacement_leaves_no_new_rule_behind() {
    let f = fixture();
    let flow = review_flow(&f.db, project(), "Bulk export");
    let proposal = NewRule::proposal(project(), flow, "correctness", dialog_draft(), None).unwrap();
    let pending = f.db.create_rule(&proposal, 100).unwrap();

    let error =
        f.db.replace_rule(pending.id, &dialog_draft(), 200)
            .unwrap_err();

    assert!(matches!(
        error,
        RuleStoreError::Refused(RuleError::NotApproved { .. })
    ));
    assert_eq!(f.db.rules(&project()).unwrap(), [pending]);
}

#[test]
fn an_invocation_records_the_revisions_it_read_once() {
    let f = fixture();
    let rule = written(&f.db);
    let flow = review_flow(&f.db, project(), "Bulk export");
    let rules = f.db.rules(&project()).unwrap();
    let selection = knowledge::select(
        &rules,
        &project(),
        &RuleQuery::new(["src/App.vue".to_owned()], []),
    );
    let invocation = ReviewInvocation {
        flow,
        role: "conventions".to_owned(),
        attempt: 1,
    };

    f.db.record_applications(&invocation, &selection.applied, 300)
        .unwrap();

    let recorded = f.db.applications(flow).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].rule, rule.id);
    assert_eq!(recorded[0].revision, 1);
    assert!(matches!(recorded[0].reason, MatchReason::Path { .. }));

    assert!(matches!(
        f.db.record_applications(&invocation, &selection.applied, 400),
        Err(RuleStoreError::AlreadyRecorded { attempt: 1, .. })
    ));
}

#[test]
fn a_revision_that_does_not_exist_or_another_projects_rule_is_never_recorded() {
    let f = fixture();
    let rule = written(&f.db);
    let elsewhere =
        f.db.create_rule(
            &NewRule::written(other_project(), dialog_draft()).unwrap(),
            100,
        )
        .unwrap();
    let flow = review_flow(&f.db, project(), "Bulk export");
    let selection = |rule: RuleId, revision: u32| {
        vec![wtm_core::flow::SelectedRule {
            rule,
            revision,
            kind: RuleKind::Required,
            reason: MatchReason::Project,
        }]
    };
    let invocation = |attempt| ReviewInvocation {
        flow,
        role: "conventions".to_owned(),
        attempt,
    };

    assert!(matches!(
        f.db.record_applications(&invocation(1), &selection(rule.id, 9), 300),
        Err(RuleStoreError::UnknownRevision { revision: 9, .. })
    ));
    assert!(matches!(
        f.db.record_applications(&invocation(2), &selection(elsewhere.id, 1), 300),
        Err(RuleStoreError::OtherProject { .. })
    ));
    assert!(f.db.applications(flow).unwrap().is_empty());
}

#[test]
fn usage_counts_distinct_reviews_and_names_the_latest() {
    let f = fixture();
    let rule = written(&f.db);
    let first = review_flow(&f.db, project(), "Bulk export");
    let second = review_flow(&f.db, project(), "Date picker");
    let rules = f.db.rules(&project()).unwrap();
    let applied = knowledge::select(
        &rules,
        &project(),
        &RuleQuery::new(["src/App.vue".to_owned()], []),
    )
    .applied;
    for (flow, role, at) in [
        (first, "conventions", 300),
        (first, "ui", 310),
        (second, "conventions", 400),
    ] {
        let invocation = ReviewInvocation {
            flow,
            role: role.to_owned(),
            attempt: 1,
        };
        f.db.record_applications(&invocation, &applied, at).unwrap();
    }

    let usage = f.db.usage(&project()).unwrap();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].rule, rule.id);
    assert_eq!(usage[0].reviews, 2);
    assert_eq!(usage[0].last_flow, second);
    assert_eq!(usage[0].last_at, 400);
    assert!(f.db.usage(&other_project()).unwrap().is_empty());
}

/// The plan's acceptance scenario for review knowledge: a component-selection rule saved from one
/// review's finding is retrieved by a later review of a matching file, recorded as applied, and a
/// finding in that review cites the revision the record says it was given.
#[test]
fn a_saved_component_rule_is_retrieved_and_cited_in_a_later_review() {
    let f = fixture();
    let earlier = review_flow(&f.db, project(), "Bulk export");
    let from_finding = finding(earlier, false);
    let draft = RuleDraft {
        kind: RuleKind::Required,
        scope: RuleScope {
            areas: vec!["frontend".to_owned()],
            paths: vec!["src/**/*.vue".to_owned()],
        },
        ..RuleDraft::from_finding(&from_finding).unwrap()
    };
    let saved =
        f.db.create_rule(
            &NewRule::from_finding(project(), &from_finding, draft).unwrap(),
            100,
        )
        .unwrap();

    // A later review, of a different component.
    let later = review_flow(&f.db, project(), "Date picker");
    let conventions: wtm_core::flow::RoleSpec = serde_json::from_value(serde_json::json!({
        "objective": "Check the change against the project's conventions",
        "provider": "claude",
        "inputs": ["diff", "instructions", "rules"],
    }))
    .unwrap();
    let changed = [
        "src/pickers/DatePicker.vue".to_owned(),
        "api/dates.py".to_owned(),
    ];
    let query = RuleQuery::for_role(&conventions, &changed).unwrap();
    let rules = f.db.rules(&project()).unwrap();
    let selection = knowledge::select(&rules, &project(), &query);
    let invocation = ReviewInvocation {
        flow: later,
        role: "conventions".to_owned(),
        attempt: 1,
    };
    f.db.record_applications(&invocation, &selection.applied, 500)
        .unwrap();

    // The brief the role reads names the rule by id and revision.
    let brief = knowledge::brief(None, Some((&selection, &rules)));
    assert!(
        brief.contains(&format!("<rule id=\"R-{}\" revision=\"1\"", saved.id)),
        "{brief}"
    );

    // The role reports a violation citing that revision, and the record backs the citation.
    let citation = FindingClass::RuleViolation {
        rule: saved.id,
        revision: 1,
    };
    let recorded = f.db.applications(later).unwrap();
    assert!(
        recorded.iter().any(|applied| citation
            == FindingClass::RuleViolation {
                rule: applied.rule,
                revision: applied.revision,
            }),
        "{recorded:?}"
    );
    assert_eq!(
        recorded[0].reason,
        MatchReason::Path {
            glob: "src/**/*.vue".to_owned(),
            file: "src/pickers/DatePicker.vue".to_owned(),
        }
    );
    assert_eq!(f.db.usage(&project()).unwrap()[0].last_flow, later);
}

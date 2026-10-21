//! [`RuleStore`] over SQLite.
//!
//! # The rules decide, then one transaction writes
//!
//! Every write loads the rule inside an `IMMEDIATE` transaction, asks the domain for the rule as
//! it should be afterwards, and writes the difference: the row's status, approval and replacement,
//! plus any revisions the change appended. A refused change returns before anything is written,
//! and the dropped transaction rolls back nothing. Migration 2's checks and triggers are the second
//! line: a pending rule of yours, a renumbered revision or an edited application is refused by the
//! database even if a bug here tried to write one.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use wtm_core::flow::{
    FlowId, MatchReason, NewRule, RecordedApplication, ReviewInvocation, Rule, RuleChange,
    RuleDraft, RuleId, RuleRevision, RuleSource, RuleUsage, SelectedRule, UnixMs,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStoreError, RuleStore, RuleStoreError};

use crate::AppDb;
use crate::store;

impl RuleStore for AppDb {
    fn create_rule(&self, rule: &NewRule, at: UnixMs) -> Result<Rule, RuleStoreError> {
        let mut conn = self.conn.lock();
        let tx = immediate(&mut conn)?;
        let created = insert_rule(&tx, rule, at)?;
        tx.commit().map_err(storage)?;
        Ok(created)
    }

    fn rule(&self, id: RuleId) -> Result<Option<Rule>, RuleStoreError> {
        let conn = self.conn.lock();
        load_rule(&conn, id)
    }

    fn rules(&self, project: &ProjectId) -> Result<Vec<Rule>, RuleStoreError> {
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare(&format!(
                "SELECT {RULE_COLUMNS} FROM rules WHERE project = ?1 ORDER BY id"
            ))
            .map_err(storage)?;
        let rows = statement
            .query_map(params![project.as_str()], RuleRow::read)
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        rows.into_iter()
            .map(|row| {
                let revisions = load_revisions(&conn, RuleId(row.id))?;
                row.into_rule(revisions)
            })
            .collect()
    }

    fn change_rule(
        &self,
        id: RuleId,
        change: &RuleChange,
        at: UnixMs,
    ) -> Result<Rule, RuleStoreError> {
        let mut conn = self.conn.lock();
        let tx = immediate(&mut conn)?;
        let before = load_rule(&tx, id)?.ok_or(RuleStoreError::UnknownRule(id))?;
        let after = before.change(change, at)?;
        write_changes(&tx, &before, &after)?;
        tx.commit().map_err(storage)?;
        Ok(after)
    }

    fn replace_rule(
        &self,
        id: RuleId,
        draft: &RuleDraft,
        at: UnixMs,
    ) -> Result<(Rule, Rule), RuleStoreError> {
        let mut conn = self.conn.lock();
        let tx = immediate(&mut conn)?;
        let before = load_rule(&tx, id)?.ok_or(RuleStoreError::UnknownRule(id))?;
        let replacement = NewRule::replacement(before.project.clone(), id, draft.clone())?;
        let created = insert_rule(&tx, &replacement, at)?;
        let superseded = before.supersede(created.id, at)?;
        write_changes(&tx, &before, &superseded)?;
        tx.commit().map_err(storage)?;
        Ok((superseded, created))
    }

    fn record_applications(
        &self,
        invocation: &ReviewInvocation,
        rules: &[SelectedRule],
        at: UnixMs,
    ) -> Result<(), RuleStoreError> {
        let mut conn = self.conn.lock();
        let tx = immediate(&mut conn)?;
        let flow_project = flow_project(&tx, invocation.flow)?;

        let recorded = tx
            .query_row(
                "SELECT 1 FROM rule_applications
                 WHERE flow_id = ?1 AND role = ?2 AND attempt = ?3 LIMIT 1",
                params![invocation.flow.0, invocation.role, invocation.attempt],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?;
        if recorded.is_some() {
            return Err(RuleStoreError::AlreadyRecorded {
                flow: invocation.flow,
                role: invocation.role.clone(),
                attempt: invocation.attempt,
            });
        }

        for (position, selected) in rules.iter().enumerate() {
            let rule_project: Option<String> = tx
                .query_row(
                    "SELECT r.project FROM rule_revisions v JOIN rules r ON r.id = v.rule_id
                     WHERE v.rule_id = ?1 AND v.number = ?2",
                    params![selected.rule.0, selected.revision],
                    |row| row.get(0),
                )
                .optional()
                .map_err(storage)?;
            match rule_project {
                None => {
                    return Err(RuleStoreError::UnknownRevision {
                        rule: selected.rule,
                        revision: selected.revision,
                    });
                }
                Some(project) if project != flow_project => {
                    return Err(RuleStoreError::OtherProject {
                        rule: selected.rule,
                        flow: invocation.flow,
                    });
                }
                Some(_) => {}
            }
            tx.execute(
                "INSERT INTO rule_applications
                     (flow_id, role, attempt, position, rule_id, revision, reason, applied_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    invocation.flow.0,
                    invocation.role,
                    invocation.attempt,
                    position,
                    selected.rule.0,
                    selected.revision,
                    json(&selected.reason)?,
                    at
                ],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }

    fn applications(&self, flow: FlowId) -> Result<Vec<RecordedApplication>, RuleStoreError> {
        let conn = self.conn.lock();
        flow_project(&conn, flow)?;
        let mut statement = conn
            .prepare(
                "SELECT role, attempt, rule_id, revision, reason, applied_at FROM rule_applications
                 WHERE flow_id = ?1 ORDER BY applied_at, role, attempt, position",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map(params![flow.0], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, UnixMs>(5)?,
                ))
            })
            .map_err(storage)?;
        rows.map(|row| {
            let (role, attempt, rule_id, revision, reason, at) = row.map_err(storage)?;
            Ok(RecordedApplication {
                invocation: ReviewInvocation {
                    flow,
                    role,
                    attempt,
                },
                rule: RuleId(rule_id),
                revision,
                reason: from_json::<MatchReason>(&reason)?,
                at,
            })
        })
        .collect()
    }

    fn usage(&self, project: &ProjectId) -> Result<Vec<RuleUsage>, RuleStoreError> {
        let conn = self.conn.lock();
        // The last flow is a subquery rather than SQLite's bare-column-beside-MAX rule, which
        // picks arbitrarily between two applications recorded in the same millisecond.
        let mut statement = conn
            .prepare(
                "SELECT a.rule_id, COUNT(DISTINCT a.flow_id), MAX(a.applied_at),
                        (SELECT b.flow_id FROM rule_applications b WHERE b.rule_id = a.rule_id
                         ORDER BY b.applied_at DESC, b.flow_id DESC LIMIT 1)
                 FROM rule_applications a JOIN rules r ON r.id = a.rule_id
                 WHERE r.project = ?1
                 GROUP BY a.rule_id ORDER BY a.rule_id",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map(params![project.as_str()], |row| {
                Ok(RuleUsage {
                    rule: RuleId(row.get(0)?),
                    reviews: row.get(1)?,
                    last_at: row.get(2)?,
                    last_flow: FlowId(row.get(3)?),
                })
            })
            .map_err(storage)?;
        rows.map(|row| row.map_err(storage)).collect()
    }
}

fn immediate(conn: &mut Connection) -> Result<Transaction<'_>, RuleStoreError> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)
}

/// Insert `rule` with its first revision, as [`Rule::create`] makes it.
fn insert_rule(tx: &Transaction<'_>, rule: &NewRule, at: UnixMs) -> Result<Rule, RuleStoreError> {
    let source_flow = rule.source().flow();
    if let Some(flow) = source_flow {
        flow_project(tx, flow)?;
    }
    if let RuleSource::Replacement { replaces } = rule.source()
        && load_rule(tx, *replaces)?.is_none()
    {
        return Err(RuleStoreError::UnknownRule(*replaces));
    }

    // The status and approval are the domain's: the row is written from `Rule::create`'s answer,
    // with a placeholder id that the insert replaces.
    let shape = Rule::create(RuleId(0), rule, at);
    tx.execute(
        "INSERT INTO rules (project, origin, status, approval, source, source_flow, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            shape.project.as_str(),
            tag(&shape.origin)?,
            tag(&shape.status)?,
            tag(&shape.approval)?,
            json(&shape.source)?,
            source_flow.map(|flow| flow.0),
            at
        ],
    )
    .map_err(storage)?;
    let created = Rule {
        id: RuleId(tx.last_insert_rowid()),
        ..shape
    };
    for revision in &created.revisions {
        insert_revision(tx, created.id, revision)?;
    }
    Ok(created)
}

/// Write what changed between `before` and `after`: the row, and any appended revisions.
fn write_changes(tx: &Transaction<'_>, before: &Rule, after: &Rule) -> Result<(), RuleStoreError> {
    if after == before {
        return Ok(());
    }
    tx.execute(
        "UPDATE rules SET status = ?1, approval = ?2, superseded_by = ?3, updated_at = ?4
         WHERE id = ?5",
        params![
            tag(&after.status)?,
            tag(&after.approval)?,
            after.superseded_by.map(|rule| rule.0),
            after.updated_at,
            after.id.0
        ],
    )
    .map_err(storage)?;
    for revision in after.revisions.iter().skip(before.revisions.len()) {
        insert_revision(tx, after.id, revision)?;
    }
    Ok(())
}

fn insert_revision(
    tx: &Transaction<'_>,
    rule: RuleId,
    revision: &RuleRevision,
) -> Result<(), RuleStoreError> {
    tx.execute(
        "INSERT INTO rule_revisions (rule_id, number, draft, at) VALUES (?1, ?2, ?3, ?4)",
        params![rule.0, revision.number, json(&revision.draft)?, revision.at],
    )
    .map_err(storage)?;
    Ok(())
}

/// The project of the work item `flow` belongs to, or [`RuleStoreError::UnknownFlow`].
fn flow_project(conn: &Connection, flow: FlowId) -> Result<String, RuleStoreError> {
    conn.query_row(
        "SELECT w.project FROM flows f JOIN work_items w ON w.id = f.work_item_id WHERE f.id = ?1",
        params![flow.0],
        |row| row.get(0),
    )
    .optional()
    .map_err(storage)?
    .ok_or(RuleStoreError::UnknownFlow(flow))
}

const RULE_COLUMNS: &str =
    "id, project, origin, status, approval, source, superseded_by, updated_at";

fn load_rule(conn: &Connection, id: RuleId) -> Result<Option<Rule>, RuleStoreError> {
    let Some(row) = conn
        .query_row(
            &format!("SELECT {RULE_COLUMNS} FROM rules WHERE id = ?1"),
            params![id.0],
            RuleRow::read,
        )
        .optional()
        .map_err(storage)?
    else {
        return Ok(None);
    };
    let revisions = load_revisions(conn, id)?;
    row.into_rule(revisions).map(Some)
}

fn load_revisions(conn: &Connection, rule: RuleId) -> Result<Vec<RuleRevision>, RuleStoreError> {
    let mut statement = conn
        .prepare("SELECT number, draft, at FROM rule_revisions WHERE rule_id = ?1 ORDER BY number")
        .map_err(storage)?;
    let rows = statement
        .query_map(params![rule.0], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, UnixMs>(2)?,
            ))
        })
        .map_err(storage)?;
    rows.map(|row| {
        let (number, draft, at) = row.map_err(storage)?;
        Ok(RuleRevision {
            number,
            draft: from_json(&draft)?,
            at,
        })
    })
    .collect()
}

/// A `rules` row before its text columns are parsed, for the reason `FlowRow` gives.
struct RuleRow {
    id: i64,
    project: String,
    origin: String,
    status: String,
    approval: String,
    source: String,
    superseded_by: Option<i64>,
    updated_at: UnixMs,
}

impl RuleRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            project: row.get(1)?,
            origin: row.get(2)?,
            status: row.get(3)?,
            approval: row.get(4)?,
            source: row.get(5)?,
            superseded_by: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    fn into_rule(self, revisions: Vec<RuleRevision>) -> Result<Rule, RuleStoreError> {
        if revisions.is_empty() {
            return Err(RuleStoreError::Storage(format!(
                "rule {} has no revisions",
                self.id
            )));
        }
        Ok(Rule {
            id: RuleId(self.id),
            project: ProjectId::from_root(std::path::Path::new(&self.project)),
            origin: untag(self.origin)?,
            source: from_json(&self.source)?,
            status: untag(self.status)?,
            approval: untag(self.approval)?,
            superseded_by: self.superseded_by.map(RuleId),
            revisions,
            updated_at: self.updated_at,
        })
    }
}

// The flow store's codecs, so a name is spelt the same way in both stores' tables. They fail only
// with `Storage`, which is carried across as it is.

fn tag<T: Serialize>(value: &T) -> Result<String, RuleStoreError> {
    store::tag(value).map_err(from_flow)
}

fn untag<T: DeserializeOwned>(name: String) -> Result<T, RuleStoreError> {
    store::untag(name).map_err(from_flow)
}

fn json<T: Serialize>(value: &T) -> Result<String, RuleStoreError> {
    store::json(value).map_err(from_flow)
}

fn from_json<T: DeserializeOwned>(text: &str) -> Result<T, RuleStoreError> {
    store::from_json(text).map_err(from_flow)
}

fn from_flow(error: FlowStoreError) -> RuleStoreError {
    RuleStoreError::Storage(match error {
        FlowStoreError::Storage(message) => message,
        other => other.to_string(),
    })
}

fn storage(error: impl std::fmt::Display) -> RuleStoreError {
    RuleStoreError::Storage(error.to_string())
}

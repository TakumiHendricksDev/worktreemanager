//! [`DesignStore`] over SQLite: an Implement run's design record, its revisions, your comments on
//! them and your decisions (migration 15).
//!
//! A revision is numbered here, inside the transaction that writes it, as a plan is: two
//! publications racing each other get 1 and 2 rather than two 1s. A comment and a decision name a
//! revision by its number within the run, and are refused when the run has no such revision, so a
//! comment cannot end up attached to nothing.

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use wtm_core::flow::{
    ApprovalId, Approved, Design, DesignComment, DesignCommentId, DesignDecision, DesignDecisionId,
    DesignRevision, DesignRevisionId, DesignVerdict, FlowId, InvocationId, NewApproval, NewDesign,
    NewDesignRevision, StepId, UnixMs,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{DesignStore, FlowStoreError};

use crate::AppDb;
use crate::outbox::approved;
use crate::runs::{ensure_flow, query};
use crate::store::{from_json, json, storage, tag, untag};

const DESIGN_COLUMNS: &str = "flow_id, project, worktree, issue_key, role, origin, request, \
     prototype, created_at, updated_at";

fn read_design(row: &Row<'_>) -> rusqlite::Result<(Design, String)> {
    let project: String = row.get(1)?;
    Ok((
        Design {
            flow: FlowId(row.get(0)?),
            project: ProjectId::from_root(std::path::Path::new(&project)),
            worktree: row.get(2)?,
            key: row.get(3)?,
            role: row.get(4)?,
            origin: wtm_core::flow::DesignOrigin::Plan,
            request: row.get(6)?,
            prototype: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        },
        row.get(5)?,
    ))
}

fn finish_design((mut design, origin): (Design, String)) -> Result<Design, FlowStoreError> {
    design.origin = untag(origin)?;
    Ok(design)
}

fn design_of(conn: &Connection, flow: FlowId) -> Result<Option<Design>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {DESIGN_COLUMNS} FROM designs WHERE flow_id = ?1"),
        params![flow.0],
        read_design,
    )
    .optional()
    .map_err(storage)?
    .map(finish_design)
    .transpose()
}

const REVISION_COLUMNS: &str = "id, flow_id, revision, prototype, review, fingerprint, brief, \
     feedback, reuse, sources, plan_revision, step_id, invocation_id, submitted_at";

struct RevisionRow {
    id: i64,
    flow: i64,
    revision: u32,
    prototype: String,
    review: String,
    fingerprint: String,
    brief: String,
    feedback: Option<String>,
    reuse: String,
    sources: String,
    plan_revision: u32,
    step: Option<i64>,
    invocation: Option<i64>,
    submitted_at: UnixMs,
}

impl RevisionRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            revision: row.get(2)?,
            prototype: row.get(3)?,
            review: row.get(4)?,
            fingerprint: row.get(5)?,
            brief: row.get(6)?,
            feedback: row.get(7)?,
            reuse: row.get(8)?,
            sources: row.get(9)?,
            plan_revision: row.get(10)?,
            step: row.get(11)?,
            invocation: row.get(12)?,
            submitted_at: row.get(13)?,
        })
    }

    fn into_revision(self) -> Result<DesignRevision, FlowStoreError> {
        Ok(DesignRevision {
            id: DesignRevisionId(self.id),
            flow: FlowId(self.flow),
            revision: self.revision,
            prototype: self.prototype,
            review: self.review,
            fingerprint: self.fingerprint,
            brief: self.brief,
            feedback: self.feedback,
            reuse: self.reuse,
            sources: from_json(&self.sources)?,
            plan_revision: self.plan_revision,
            step: self.step.map(StepId),
            invocation: self.invocation.map(InvocationId),
            submitted_at: self.submitted_at,
        })
    }
}

fn revision_where(
    conn: &Connection,
    id: DesignRevisionId,
) -> Result<Option<DesignRevision>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {REVISION_COLUMNS} FROM design_revisions WHERE id = ?1"),
        params![id.0],
        RevisionRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(RevisionRow::into_revision)
    .transpose()
}

/// The id of revision `revision` of `flow`, or the refusal that names what does not exist.
fn revision_id(conn: &Connection, flow: FlowId, revision: u32) -> Result<i64, FlowStoreError> {
    conn.query_row(
        "SELECT id FROM design_revisions WHERE flow_id = ?1 AND revision = ?2",
        params![flow.0, revision],
        |row| row.get(0),
    )
    .optional()
    .map_err(storage)?
    .ok_or(FlowStoreError::NoSuchRevision { flow, revision })
}

const COMMENT_COLUMNS: &str = "c.id, c.flow_id, r.revision, c.text, c.said_at";

fn read_comment(row: &Row<'_>) -> rusqlite::Result<DesignComment> {
    Ok(DesignComment {
        id: DesignCommentId(row.get(0)?),
        flow: FlowId(row.get(1)?),
        revision: row.get(2)?,
        text: row.get(3)?,
        at: row.get(4)?,
    })
}

const DECISION_COLUMNS: &str =
    "d.id, d.flow_id, r.revision, d.verdict, d.approval_id, d.note, d.decided_at";

struct DecisionRow {
    id: i64,
    flow: i64,
    revision: u32,
    verdict: String,
    approval: Option<i64>,
    note: Option<String>,
    at: UnixMs,
}

impl DecisionRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            revision: row.get(2)?,
            verdict: row.get(3)?,
            approval: row.get(4)?,
            note: row.get(5)?,
            at: row.get(6)?,
        })
    }

    fn into_decision(self) -> Result<DesignDecision, FlowStoreError> {
        let verdict = match (self.verdict.as_str(), self.approval, self.note) {
            ("approved", Some(approval), _) => DesignVerdict::Approved {
                approval: ApprovalId(approval),
            },
            ("changes_requested", _, Some(note)) => DesignVerdict::ChangesRequested { note },
            (other, ..) => {
                return Err(FlowStoreError::Storage(format!(
                    "design decision {} has an unreadable verdict `{other}`",
                    self.id
                )));
            }
        };
        Ok(DesignDecision {
            id: DesignDecisionId(self.id),
            flow: FlowId(self.flow),
            revision: self.revision,
            verdict,
            at: self.at,
        })
    }
}

fn decision_where(
    conn: &Connection,
    id: DesignDecisionId,
) -> Result<Option<DesignDecision>, FlowStoreError> {
    conn.query_row(
        &format!(
            "SELECT {DECISION_COLUMNS} FROM design_decisions d
             JOIN design_revisions r ON r.id = d.revision_id WHERE d.id = ?1"
        ),
        params![id.0],
        DecisionRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(DecisionRow::into_decision)
    .transpose()
}

impl DesignStore for AppDb {
    fn begin_design(&self, design: &NewDesign, at: UnixMs) -> Result<Design, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, design.flow)?;
        // Kept, not replaced: the role and worktree a run designs with are the ones its
        // conversation is in, and a request for changes must find the same.
        conn.execute(
            "INSERT INTO designs
                 (flow_id, project, worktree, issue_key, role, origin, request, prototype,
                  created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?8)
             ON CONFLICT (flow_id) DO NOTHING",
            params![
                design.flow.0,
                design.project.as_str(),
                design.worktree,
                design.key,
                design.role,
                tag(&design.origin)?,
                design.request,
                at
            ],
        )
        .map_err(storage)?;
        design_of(&conn, design.flow)?.ok_or(FlowStoreError::UnknownDesign(design.flow))
    }

    fn design(&self, flow: FlowId) -> Result<Option<Design>, FlowStoreError> {
        design_of(&self.conn.lock(), flow)
    }

    fn designs(&self) -> Result<Vec<Design>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!("SELECT {DESIGN_COLUMNS} FROM designs ORDER BY flow_id"),
            [],
            read_design,
        )?
        .into_iter()
        .map(finish_design)
        .collect()
    }

    fn set_design_prototype(
        &self,
        flow: FlowId,
        prototype: &str,
        at: UnixMs,
    ) -> Result<Design, FlowStoreError> {
        let conn = self.conn.lock();
        let Some(design) = design_of(&conn, flow)? else {
            return Err(FlowStoreError::UnknownDesign(flow));
        };
        match design.prototype.as_deref() {
            Some(kept) if kept == prototype => return Ok(design),
            // A second prototype would start a second history beside the first, and the revisions
            // numbered in this run would no longer be artifacts of one design.
            Some(kept) => {
                return Err(FlowStoreError::Storage(format!(
                    "run {flow} already designs in prototype {kept}"
                )));
            }
            None => {}
        }
        conn.execute(
            "UPDATE designs SET prototype = ?2, updated_at = ?3 WHERE flow_id = ?1",
            params![flow.0, prototype, at],
        )
        .map_err(storage)?;
        design_of(&conn, flow)?.ok_or(FlowStoreError::UnknownDesign(flow))
    }

    fn record_design_revision(
        &self,
        revision: &NewDesignRevision,
        at: UnixMs,
    ) -> Result<DesignRevision, FlowStoreError> {
        let mut conn = self.conn.lock();
        if design_of(&conn, revision.flow)?.is_none() {
            return Err(FlowStoreError::UnknownDesign(revision.flow));
        }
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let next: u32 = tx
            .query_row(
                "SELECT COALESCE(MAX(revision), 0) + 1 FROM design_revisions WHERE flow_id = ?1",
                params![revision.flow.0],
                |row| row.get(0),
            )
            .map_err(storage)?;
        tx.execute(
            "INSERT INTO design_revisions
                 (flow_id, revision, prototype, review, fingerprint, brief, feedback, reuse,
                  sources, plan_revision, step_id, invocation_id, submitted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                revision.flow.0,
                next,
                revision.prototype,
                revision.review,
                revision.fingerprint,
                revision.brief,
                revision.feedback,
                revision.reuse,
                json(&revision.sources)?,
                revision.plan_revision,
                revision.step.map(|s| s.0),
                revision.invocation.map(|i| i.0),
                at
            ],
        )
        .map_err(storage)?;
        let id = DesignRevisionId(tx.last_insert_rowid());
        let recorded = revision_where(&tx, id)?.ok_or(FlowStoreError::UnknownDesignRevision(id))?;
        tx.commit().map_err(storage)?;
        Ok(recorded)
    }

    fn design_revisions(&self, flow: FlowId) -> Result<Vec<DesignRevision>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!(
                "SELECT {REVISION_COLUMNS} FROM design_revisions WHERE flow_id = ?1
                 ORDER BY revision"
            ),
            params![flow.0],
            RevisionRow::read,
        )?
        .into_iter()
        .map(RevisionRow::into_revision)
        .collect()
    }

    fn comment_on_design(
        &self,
        flow: FlowId,
        revision: u32,
        text: &str,
        at: UnixMs,
    ) -> Result<DesignComment, FlowStoreError> {
        let conn = self.conn.lock();
        let on = revision_id(&conn, flow, revision)?;
        conn.execute(
            "INSERT INTO design_comments (flow_id, revision_id, text, said_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![flow.0, on, text, at],
        )
        .map_err(storage)?;
        let id = conn.last_insert_rowid();
        conn.query_row(
            &format!(
                "SELECT {COMMENT_COLUMNS} FROM design_comments c
                 JOIN design_revisions r ON r.id = c.revision_id WHERE c.id = ?1"
            ),
            params![id],
            read_comment,
        )
        .map_err(storage)
    }

    fn design_comments(&self, flow: FlowId) -> Result<Vec<DesignComment>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!(
                "SELECT {COMMENT_COLUMNS} FROM design_comments c
                 JOIN design_revisions r ON r.id = c.revision_id
                 WHERE c.flow_id = ?1 ORDER BY c.id"
            ),
            params![flow.0],
            read_comment,
        )
    }

    fn approve_design(
        &self,
        approval: &NewApproval,
        revision: DesignRevisionId,
        at: UnixMs,
    ) -> Result<(Approved, DesignDecision), FlowStoreError> {
        let mut conn = self.conn.lock();
        let record = revision_where(&conn, revision)?
            .ok_or(FlowStoreError::UnknownDesignRevision(revision))?;
        if record.flow != approval.flow {
            return Err(FlowStoreError::UnknownDesignRevision(revision));
        }
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        tx.execute(
            "INSERT INTO approvals (flow_id, intent, binding, subject, granted_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                approval.flow.0,
                tag(&approval.intent)?,
                json(&approval.binding)?,
                json(&approval.subject)?,
                at
            ],
        )
        .map_err(storage)?;
        let approval_id = ApprovalId(tx.last_insert_rowid());
        tx.execute(
            "INSERT INTO design_decisions
                 (flow_id, revision_id, verdict, approval_id, note, decided_at)
             VALUES (?1, ?2, 'approved', ?3, NULL, ?4)",
            params![record.flow.0, revision.0, approval_id.0, at],
        )
        .map_err(storage)?;
        let decision = DesignDecisionId(tx.last_insert_rowid());
        tx.commit().map_err(storage)?;
        let approved =
            approved(&conn, approval_id)?.ok_or(FlowStoreError::UnknownApproval(approval_id))?;
        let decision = decision_where(&conn, decision)?
            .ok_or_else(|| FlowStoreError::Storage("the decision was not kept".to_owned()))?;
        Ok((approved, decision))
    }

    fn request_design_changes(
        &self,
        revision: DesignRevisionId,
        note: &str,
        at: UnixMs,
    ) -> Result<DesignDecision, FlowStoreError> {
        let conn = self.conn.lock();
        let record = revision_where(&conn, revision)?
            .ok_or(FlowStoreError::UnknownDesignRevision(revision))?;
        conn.execute(
            "INSERT INTO design_decisions
                 (flow_id, revision_id, verdict, approval_id, note, decided_at)
             VALUES (?1, ?2, 'changes_requested', NULL, ?3, ?4)",
            params![record.flow.0, revision.0, note, at],
        )
        .map_err(storage)?;
        let id = DesignDecisionId(conn.last_insert_rowid());
        decision_where(&conn, id)?
            .ok_or_else(|| FlowStoreError::Storage("the decision was not kept".to_owned()))
    }

    fn design_decisions(&self, flow: FlowId) -> Result<Vec<DesignDecision>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!(
                "SELECT {DECISION_COLUMNS} FROM design_decisions d
                 JOIN design_revisions r ON r.id = d.revision_id
                 WHERE d.flow_id = ?1 ORDER BY d.id"
            ),
            params![flow.0],
            DecisionRow::read,
        )?
        .into_iter()
        .map(DecisionRow::into_decision)
        .collect()
    }
}

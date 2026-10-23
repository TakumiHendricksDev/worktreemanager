//! [`PlanStore`] over SQLite: an Implement run's planning record, its plans, your decisions about
//! them, and the discussions of its questions (migration 13).
//!
//! A plan's revision is numbered here, inside the transaction that writes it, so two submissions
//! racing each other get 1 and 2 rather than two 1s; the `UNIQUE (flow_id, revision)` would refuse
//! the second anyway, and numbering it here means it is never refused.

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use wtm_core::flow::{
    ApprovalId, Approved, FlowId, InvocationId, NewApproval, NewPlan, NewPlanning, PlanContext,
    PlanDecision, PlanDecisionId, PlanId, PlanRecord, PlanVerdict, Planning, QuestionId,
    QuestionMessage, QuestionMessageId, Speaker, StepId, UnixMs,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStoreError, PlanStore};

use crate::AppDb;
use crate::outbox::approved;
use crate::runs::{ensure_flow, query};
use crate::store::{from_json, json, storage, tag, untag};

const PLANNING_COLUMNS: &str =
    "flow_id, project, worktree, issue_key, tracker, role, context, created_at, updated_at";

fn read_planning(row: &Row<'_>) -> rusqlite::Result<(Planning, Option<String>, String)> {
    let project: String = row.get(1)?;
    let tracker: String = row.get(4)?;
    Ok((
        Planning {
            flow: FlowId(row.get(0)?),
            project: ProjectId::from_root(std::path::Path::new(&project)),
            worktree: row.get(2)?,
            key: row.get(3)?,
            tracker: wtm_core::flow::TrackerKind::Jira,
            role: row.get(5)?,
            context: None,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        },
        row.get(6)?,
        tracker,
    ))
}

fn finish_planning(
    (mut planning, context, tracker): (Planning, Option<String>, String),
) -> Result<Planning, FlowStoreError> {
    planning.tracker = untag(tracker)?;
    planning.context = context.as_deref().map(from_json).transpose()?;
    Ok(planning)
}

fn planning_of(conn: &Connection, flow: FlowId) -> Result<Option<Planning>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {PLANNING_COLUMNS} FROM plannings WHERE flow_id = ?1"),
        params![flow.0],
        read_planning,
    )
    .optional()
    .map_err(storage)?
    .map(finish_planning)
    .transpose()
}

const PLAN_COLUMNS: &str =
    "id, flow_id, revision, step_id, invocation_id, plan, issue, issue_revision, submitted_at";

struct PlanRow {
    id: i64,
    flow: i64,
    revision: u32,
    step: Option<i64>,
    invocation: Option<i64>,
    plan: String,
    issue: String,
    issue_revision: String,
    submitted_at: UnixMs,
}

impl PlanRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            revision: row.get(2)?,
            step: row.get(3)?,
            invocation: row.get(4)?,
            plan: row.get(5)?,
            issue: row.get(6)?,
            issue_revision: row.get(7)?,
            submitted_at: row.get(8)?,
        })
    }

    fn into_record(self) -> Result<PlanRecord, FlowStoreError> {
        Ok(PlanRecord {
            id: PlanId(self.id),
            flow: FlowId(self.flow),
            revision: self.revision,
            step: self.step.map(StepId),
            invocation: self.invocation.map(InvocationId),
            plan: from_json(&self.plan)?,
            issue: from_json(&self.issue)?,
            issue_revision: self.issue_revision,
            submitted_at: self.submitted_at,
        })
    }
}

fn plan_where(conn: &Connection, id: PlanId) -> Result<Option<PlanRecord>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {PLAN_COLUMNS} FROM plans WHERE id = ?1"),
        params![id.0],
        PlanRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(PlanRow::into_record)
    .transpose()
}

const DECISION_COLUMNS: &str = "d.id, d.flow_id, d.plan_id, p.revision, d.verdict, d.approval_id, d.skip_design, d.note, \
     d.decided_at";

struct DecisionRow {
    id: i64,
    flow: i64,
    plan: i64,
    revision: u32,
    verdict: String,
    approval: Option<i64>,
    skip_design: bool,
    note: Option<String>,
    at: UnixMs,
}

impl DecisionRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            plan: row.get(2)?,
            revision: row.get(3)?,
            verdict: row.get(4)?,
            approval: row.get(5)?,
            skip_design: row.get(6)?,
            note: row.get(7)?,
            at: row.get(8)?,
        })
    }

    fn into_decision(self) -> Result<PlanDecision, FlowStoreError> {
        let verdict = match (self.verdict.as_str(), self.approval, self.note) {
            ("approved", Some(approval), _) => PlanVerdict::Approved {
                approval: ApprovalId(approval),
                skip_design: self.skip_design,
            },
            ("changes_requested", _, Some(note)) => PlanVerdict::ChangesRequested { note },
            (other, ..) => {
                return Err(FlowStoreError::Storage(format!(
                    "plan decision {} has an unreadable verdict `{other}`",
                    self.id
                )));
            }
        };
        Ok(PlanDecision {
            id: PlanDecisionId(self.id),
            flow: FlowId(self.flow),
            plan: PlanId(self.plan),
            revision: self.revision,
            verdict,
            at: self.at,
        })
    }
}

fn decision_where(
    conn: &Connection,
    id: PlanDecisionId,
) -> Result<Option<PlanDecision>, FlowStoreError> {
    conn.query_row(
        &format!(
            "SELECT {DECISION_COLUMNS} FROM plan_decisions d JOIN plans p ON p.id = d.plan_id
             WHERE d.id = ?1"
        ),
        params![id.0],
        DecisionRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(DecisionRow::into_decision)
    .transpose()
}

fn read_message(row: &Row<'_>) -> rusqlite::Result<(i64, i64, String, String, UnixMs)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn into_message(
    (id, question, speaker, text, at): (i64, i64, String, String, UnixMs),
) -> Result<QuestionMessage, FlowStoreError> {
    Ok(QuestionMessage {
        id: QuestionMessageId(id),
        question: QuestionId(question),
        from: untag(speaker)?,
        text,
        at,
    })
}

impl PlanStore for AppDb {
    fn begin_planning(
        &self,
        planning: &NewPlanning,
        at: UnixMs,
    ) -> Result<Planning, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, planning.flow)?;
        // Kept, not replaced: the role and worktree a run plans with are the ones its questions
        // were asked from, and a resume must find the same.
        conn.execute(
            "INSERT INTO plannings
                 (flow_id, project, worktree, issue_key, tracker, role, context, created_at,
                  updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?7)
             ON CONFLICT (flow_id) DO NOTHING",
            params![
                planning.flow.0,
                planning.project.as_str(),
                planning.worktree,
                planning.key,
                tag(&planning.tracker)?,
                planning.role,
                at
            ],
        )
        .map_err(storage)?;
        planning_of(&conn, planning.flow)?.ok_or(FlowStoreError::UnknownPlanning(planning.flow))
    }

    fn planning(&self, flow: FlowId) -> Result<Option<Planning>, FlowStoreError> {
        planning_of(&self.conn.lock(), flow)
    }

    fn plannings(&self) -> Result<Vec<Planning>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!("SELECT {PLANNING_COLUMNS} FROM plannings ORDER BY flow_id"),
            [],
            read_planning,
        )?
        .into_iter()
        .map(finish_planning)
        .collect()
    }

    fn set_plan_context(
        &self,
        flow: FlowId,
        context: &PlanContext,
        at: UnixMs,
    ) -> Result<Planning, FlowStoreError> {
        let conn = self.conn.lock();
        let changed = conn
            .execute(
                "UPDATE plannings SET context = ?2, updated_at = ?3 WHERE flow_id = ?1",
                params![flow.0, json(context)?, at],
            )
            .map_err(storage)?;
        if changed == 0 {
            return Err(FlowStoreError::UnknownPlanning(flow));
        }
        planning_of(&conn, flow)?.ok_or(FlowStoreError::UnknownPlanning(flow))
    }

    fn record_plan(&self, plan: &NewPlan, at: UnixMs) -> Result<PlanRecord, FlowStoreError> {
        let mut conn = self.conn.lock();
        ensure_flow(&conn, plan.flow)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let next: u32 = tx
            .query_row(
                "SELECT COALESCE(MAX(revision), 0) + 1 FROM plans WHERE flow_id = ?1",
                params![plan.flow.0],
                |row| row.get(0),
            )
            .map_err(storage)?;
        tx.execute(
            "INSERT INTO plans
                 (flow_id, revision, step_id, invocation_id, plan, issue, issue_revision,
                  submitted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                plan.flow.0,
                next,
                plan.step.map(|s| s.0),
                plan.invocation.map(|i| i.0),
                json(&plan.plan)?,
                json(&plan.issue)?,
                plan.issue.revision(),
                at
            ],
        )
        .map_err(storage)?;
        let id = PlanId(tx.last_insert_rowid());
        let recorded = plan_where(&tx, id)?.ok_or(FlowStoreError::UnknownPlan(id))?;
        tx.commit().map_err(storage)?;
        Ok(recorded)
    }

    fn plans(&self, flow: FlowId) -> Result<Vec<PlanRecord>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!("SELECT {PLAN_COLUMNS} FROM plans WHERE flow_id = ?1 ORDER BY revision"),
            params![flow.0],
            PlanRow::read,
        )?
        .into_iter()
        .map(PlanRow::into_record)
        .collect()
    }

    fn approve_plan(
        &self,
        approval: &NewApproval,
        plan: PlanId,
        skip_design: bool,
        at: UnixMs,
    ) -> Result<(Approved, PlanDecision), FlowStoreError> {
        let mut conn = self.conn.lock();
        let record = plan_where(&conn, plan)?.ok_or(FlowStoreError::UnknownPlan(plan))?;
        if record.flow != approval.flow {
            return Err(FlowStoreError::UnknownPlan(plan));
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
            "INSERT INTO plan_decisions
                 (flow_id, plan_id, verdict, approval_id, skip_design, note, decided_at)
             VALUES (?1, ?2, 'approved', ?3, ?4, NULL, ?5)",
            params![record.flow.0, plan.0, approval_id.0, skip_design, at],
        )
        .map_err(storage)?;
        let decision = PlanDecisionId(tx.last_insert_rowid());
        tx.commit().map_err(storage)?;
        let approved =
            approved(&conn, approval_id)?.ok_or(FlowStoreError::UnknownApproval(approval_id))?;
        let decision = decision_where(&conn, decision)?
            .ok_or_else(|| FlowStoreError::Storage("the decision was not kept".to_owned()))?;
        Ok((approved, decision))
    }

    fn request_plan_changes(
        &self,
        plan: PlanId,
        note: &str,
        at: UnixMs,
    ) -> Result<PlanDecision, FlowStoreError> {
        let conn = self.conn.lock();
        let record = plan_where(&conn, plan)?.ok_or(FlowStoreError::UnknownPlan(plan))?;
        conn.execute(
            "INSERT INTO plan_decisions
                 (flow_id, plan_id, verdict, approval_id, skip_design, note, decided_at)
             VALUES (?1, ?2, 'changes_requested', NULL, 0, ?3, ?4)",
            params![record.flow.0, plan.0, note, at],
        )
        .map_err(storage)?;
        let id = PlanDecisionId(conn.last_insert_rowid());
        decision_where(&conn, id)?
            .ok_or_else(|| FlowStoreError::Storage("the decision was not kept".to_owned()))
    }

    fn plan_decisions(&self, flow: FlowId) -> Result<Vec<PlanDecision>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!(
                "SELECT {DECISION_COLUMNS} FROM plan_decisions d JOIN plans p ON p.id = d.plan_id
                 WHERE d.flow_id = ?1 ORDER BY d.id"
            ),
            params![flow.0],
            DecisionRow::read,
        )?
        .into_iter()
        .map(DecisionRow::into_decision)
        .collect()
    }

    fn discuss(
        &self,
        question: QuestionId,
        from: Speaker,
        text: &str,
        at: UnixMs,
    ) -> Result<QuestionMessage, FlowStoreError> {
        let conn = self.conn.lock();
        let known = conn
            .query_row(
                "SELECT 1 FROM questions WHERE id = ?1",
                params![question.0],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?;
        known.ok_or(FlowStoreError::UnknownQuestion(question))?;
        conn.execute(
            "INSERT INTO question_messages (question_id, speaker, text, said_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![question.0, tag(&from)?, text, at],
        )
        .map_err(storage)?;
        let id = conn.last_insert_rowid();
        conn.query_row(
            "SELECT id, question_id, speaker, text, said_at FROM question_messages WHERE id = ?1",
            params![id],
            read_message,
        )
        .map_err(storage)
        .and_then(into_message)
    }

    fn discussion(&self, question: QuestionId) -> Result<Vec<QuestionMessage>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            "SELECT id, question_id, speaker, text, said_at FROM question_messages
             WHERE question_id = ?1 ORDER BY id",
            params![question.0],
            read_message,
        )?
        .into_iter()
        .map(into_message)
        .collect()
    }
}

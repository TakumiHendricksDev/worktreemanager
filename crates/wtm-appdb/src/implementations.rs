//! [`ImplementStore`] over SQLite: an Implement run's implementation, its rounds, the commits the
//! app made for it, and what its questions are about (migration 16).
//!
//! A round is numbered here, inside the transaction that writes it, as a plan revision is, so two
//! rounds racing each other get 1 and 2. A round's task — what it set out to fix — is written with
//! it and never again; the database refuses a change to it, which is what lets the bounds count it.

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use wtm_core::flow::{
    CheckpointBy, Field, FixBounds, FlowId, ImplementChange, ImplementCheckpoint,
    ImplementQuestion, Implementation, NewImplementation, NewRound, QuestionId, Round, UnixMs,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStoreError, ImplementStore};

use crate::AppDb;
use crate::runs::{ensure_flow, query};
use crate::store::{from_json, json, storage, tag, untag};

const COLUMNS: &str = "flow_id, project, worktree, issue_key, role, recipe, base_sha, \
     plan_revision, scope, fix_rounds, fix_attempts, review_flow, outcome, remaining, stopped, \
     started_at, updated_at";

struct ImplementationRow {
    flow: i64,
    project: String,
    worktree: String,
    key: String,
    role: String,
    recipe: String,
    base_sha: String,
    plan_revision: u32,
    scope: String,
    fix_rounds: u32,
    fix_attempts: u32,
    review_flow: Option<i64>,
    outcome: Option<String>,
    remaining: String,
    stopped: Option<String>,
    started_at: UnixMs,
    updated_at: UnixMs,
}

impl ImplementationRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            flow: row.get(0)?,
            project: row.get(1)?,
            worktree: row.get(2)?,
            key: row.get(3)?,
            role: row.get(4)?,
            recipe: row.get(5)?,
            base_sha: row.get(6)?,
            plan_revision: row.get(7)?,
            scope: row.get(8)?,
            fix_rounds: row.get(9)?,
            fix_attempts: row.get(10)?,
            review_flow: row.get(11)?,
            outcome: row.get(12)?,
            remaining: row.get(13)?,
            stopped: row.get(14)?,
            started_at: row.get(15)?,
            updated_at: row.get(16)?,
        })
    }

    fn into_implementation(self) -> Result<Implementation, FlowStoreError> {
        Ok(Implementation {
            flow: FlowId(self.flow),
            project: ProjectId::from_root(std::path::Path::new(&self.project)),
            worktree: self.worktree,
            key: self.key,
            role: self.role,
            recipe: self.recipe,
            base_sha: self.base_sha,
            plan_revision: self.plan_revision,
            scope: from_json(&self.scope)?,
            bounds: FixBounds {
                rounds: self.fix_rounds,
                attempts: self.fix_attempts,
            },
            review_flow: self.review_flow.map(FlowId),
            outcome: self.outcome.map(untag).transpose()?,
            remaining: from_json(&self.remaining)?,
            stopped: self.stopped.as_deref().map(from_json).transpose()?,
            started_at: self.started_at,
            updated_at: self.updated_at,
        })
    }
}

fn implementation_where(
    conn: &Connection,
    clause: &str,
    id: i64,
) -> Result<Option<Implementation>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM implementations WHERE {clause} = ?1"),
        params![id],
        ImplementationRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(ImplementationRow::into_implementation)
    .transpose()
}

const ROUND_COLUMNS: &str = "flow_id, round, kind, plan_revision, findings, checks, head_sha, \
     changed, started_at, ended_at";

struct RoundRow {
    flow: i64,
    round: u32,
    kind: String,
    plan_revision: u32,
    findings: String,
    checks: String,
    head_sha: Option<String>,
    changed: String,
    started_at: UnixMs,
    ended_at: Option<UnixMs>,
}

impl RoundRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            flow: row.get(0)?,
            round: row.get(1)?,
            kind: row.get(2)?,
            plan_revision: row.get(3)?,
            findings: row.get(4)?,
            checks: row.get(5)?,
            head_sha: row.get(6)?,
            changed: row.get(7)?,
            started_at: row.get(8)?,
            ended_at: row.get(9)?,
        })
    }

    fn into_round(self) -> Result<Round, FlowStoreError> {
        Ok(Round {
            flow: FlowId(self.flow),
            round: self.round,
            kind: untag(self.kind)?,
            plan_revision: self.plan_revision,
            findings: from_json(&self.findings)?,
            checks: from_json(&self.checks)?,
            head_sha: self.head_sha,
            changed: from_json(&self.changed)?,
            started_at: self.started_at,
            ended_at: self.ended_at,
        })
    }
}

fn round_of(conn: &Connection, flow: FlowId, round: u32) -> Result<Option<Round>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {ROUND_COLUMNS} FROM implement_rounds WHERE flow_id = ?1 AND round = ?2"),
        params![flow.0, round],
        RoundRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(RoundRow::into_round)
    .transpose()
}

fn read_checkpoint(row: &Row<'_>) -> rusqlite::Result<(ImplementCheckpoint, String, String)> {
    Ok((
        ImplementCheckpoint {
            flow: FlowId(row.get(0)?),
            round: row.get(1)?,
            commit: row.get(2)?,
            message: row.get(3)?,
            paths: Vec::new(),
            by: CheckpointBy::App,
            made_at: row.get(6)?,
        },
        row.get(4)?,
        row.get(5)?,
    ))
}

fn finish_checkpoint(
    (mut checkpoint, paths, by): (ImplementCheckpoint, String, String),
) -> Result<ImplementCheckpoint, FlowStoreError> {
    checkpoint.paths = from_json(&paths)?;
    checkpoint.by = untag(by)?;
    Ok(checkpoint)
}

fn read_question(row: &Row<'_>) -> rusqlite::Result<(ImplementQuestion, String, String, String)> {
    Ok((
        ImplementQuestion {
            question: QuestionId(row.get(0)?),
            flow: FlowId(row.get(1)?),
            kind: wtm_core::flow::ImplementAsk::Writer,
            paths: Vec::new(),
            findings: Vec::new(),
            plan_revision: row.get(5)?,
        },
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

impl ImplementStore for AppDb {
    fn begin_implementation(
        &self,
        implementation: &NewImplementation,
        at: UnixMs,
    ) -> Result<Implementation, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, implementation.flow)?;
        // Kept, not replaced: the writer and worktree a run implements with are the ones its
        // conversation and its commits are in.
        conn.execute(
            "INSERT INTO implementations
                 (flow_id, project, worktree, issue_key, role, recipe, base_sha, plan_revision,
                  scope, fix_rounds, fix_attempts, started_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
             ON CONFLICT (flow_id) DO NOTHING",
            params![
                implementation.flow.0,
                implementation.project.as_str(),
                implementation.worktree,
                implementation.key,
                implementation.role,
                implementation.recipe,
                implementation.base_sha,
                implementation.plan_revision,
                json(&implementation.scope)?,
                implementation.bounds.rounds,
                implementation.bounds.attempts,
                at
            ],
        )
        .map_err(storage)?;
        implementation_where(&conn, "flow_id", implementation.flow.0)?
            .ok_or(FlowStoreError::UnknownImplementation(implementation.flow))
    }

    fn implementation(&self, flow: FlowId) -> Result<Option<Implementation>, FlowStoreError> {
        implementation_where(&self.conn.lock(), "flow_id", flow.0)
    }

    fn implementations(&self) -> Result<Vec<Implementation>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!("SELECT {COLUMNS} FROM implementations ORDER BY flow_id"),
            [],
            ImplementationRow::read,
        )?
        .into_iter()
        .map(ImplementationRow::into_implementation)
        .collect()
    }

    fn implementation_reviewed_by(
        &self,
        review: FlowId,
    ) -> Result<Option<Implementation>, FlowStoreError> {
        implementation_where(&self.conn.lock(), "review_flow", review.0)
    }

    fn change_implementation(
        &self,
        flow: FlowId,
        change: &ImplementChange,
        at: UnixMs,
    ) -> Result<Implementation, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let current = implementation_where(&tx, "flow_id", flow.0)?
            .ok_or(FlowStoreError::UnknownImplementation(flow))?;
        if current.outcome == Some(wtm_core::flow::ImplementOutcome::Cancelled)
            && *change != ImplementChange::default()
        {
            return Err(FlowStoreError::ImplementationEnded(flow));
        }
        if let Some((revision, scope)) = &change.plan {
            tx.execute(
                "UPDATE implementations SET plan_revision = ?2, scope = ?3 WHERE flow_id = ?1",
                params![flow.0, revision, json(scope)?],
            )
            .map_err(storage)?;
        }
        if let Some(review) = change.review_flow {
            tx.execute(
                "UPDATE implementations SET review_flow = ?2 WHERE flow_id = ?1",
                params![flow.0, review.0],
            )
            .map_err(storage)?;
        }
        if let Some(remaining) = &change.remaining {
            tx.execute(
                "UPDATE implementations SET remaining = ?2 WHERE flow_id = ?1",
                params![flow.0, json(remaining)?],
            )
            .map_err(storage)?;
        }
        if change.stopped != Field::Keep {
            let stopped = match &change.stopped {
                Field::Set(reason) => Some(json(reason)?),
                _ => None,
            };
            tx.execute(
                "UPDATE implementations SET stopped = ?2 WHERE flow_id = ?1",
                params![flow.0, stopped],
            )
            .map_err(storage)?;
        }
        tx.execute(
            "UPDATE implementations SET updated_at = ?2 WHERE flow_id = ?1",
            params![flow.0, at],
        )
        .map_err(storage)?;
        // Last: once it says stopped, the row takes no other change.
        if change.outcome != Field::Keep {
            let outcome = match &change.outcome {
                Field::Set(outcome) => Some(tag(outcome)?),
                _ => None,
            };
            tx.execute(
                "UPDATE implementations SET outcome = ?2 WHERE flow_id = ?1",
                params![flow.0, outcome],
            )
            .map_err(storage)?;
        }
        let changed = implementation_where(&tx, "flow_id", flow.0)?
            .ok_or(FlowStoreError::UnknownImplementation(flow))?;
        tx.commit().map_err(storage)?;
        Ok(changed)
    }

    fn start_round(&self, round: &NewRound, at: UnixMs) -> Result<Round, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        if implementation_where(&tx, "flow_id", round.flow.0)?.is_none() {
            return Err(FlowStoreError::UnknownImplementation(round.flow));
        }
        let next: u32 = tx
            .query_row(
                "SELECT COALESCE(MAX(round), 0) + 1 FROM implement_rounds WHERE flow_id = ?1",
                params![round.flow.0],
                |row| row.get(0),
            )
            .map_err(storage)?;
        tx.execute(
            "INSERT INTO implement_rounds
                 (flow_id, round, kind, plan_revision, findings, checks, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                round.flow.0,
                next,
                tag(&round.kind)?,
                round.plan_revision,
                json(&round.findings)?,
                json(&round.checks)?,
                at
            ],
        )
        .map_err(storage)?;
        let started = round_of(&tx, round.flow, next)?
            .ok_or(FlowStoreError::UnknownImplementation(round.flow))?;
        tx.commit().map_err(storage)?;
        Ok(started)
    }

    fn end_round(
        &self,
        flow: FlowId,
        round: u32,
        head_sha: Option<&str>,
        changed: &[String],
        at: UnixMs,
    ) -> Result<Round, FlowStoreError> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE implement_rounds SET head_sha = ?3, changed = ?4, ended_at = ?5
             WHERE flow_id = ?1 AND round = ?2",
            params![flow.0, round, head_sha, json(&changed)?, at],
        )
        .map_err(storage)?;
        round_of(&conn, flow, round)?.ok_or(FlowStoreError::UnknownImplementation(flow))
    }

    fn rounds(&self, flow: FlowId) -> Result<Vec<Round>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!(
                "SELECT {ROUND_COLUMNS} FROM implement_rounds WHERE flow_id = ?1 ORDER BY round"
            ),
            params![flow.0],
            RoundRow::read,
        )?
        .into_iter()
        .map(RoundRow::into_round)
        .collect()
    }

    fn record_implement_checkpoint(
        &self,
        flow: FlowId,
        round: u32,
        commit: &str,
        message: &str,
        paths: &[String],
        by: CheckpointBy,
        at: UnixMs,
    ) -> Result<ImplementCheckpoint, FlowStoreError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO implement_checkpoints
                 (flow_id, round, commit_sha, message, paths, made_by, made_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![flow.0, round, commit, message, json(&paths)?, tag(&by)?, at],
        )
        .map_err(storage)?;
        Ok(ImplementCheckpoint {
            flow,
            round,
            commit: commit.to_owned(),
            message: message.to_owned(),
            paths: paths.to_vec(),
            by,
            made_at: at,
        })
    }

    fn implement_checkpoints(
        &self,
        flow: FlowId,
    ) -> Result<Vec<ImplementCheckpoint>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            "SELECT flow_id, round, commit_sha, message, paths, made_by, made_at
             FROM implement_checkpoints WHERE flow_id = ?1 ORDER BY id",
            params![flow.0],
            read_checkpoint,
        )?
        .into_iter()
        .map(finish_checkpoint)
        .collect()
    }

    fn link_implement_question(&self, link: &ImplementQuestion) -> Result<(), FlowStoreError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO implement_questions
                 (question_id, flow_id, kind, paths, findings, plan_revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (question_id) DO NOTHING",
            params![
                link.question.0,
                link.flow.0,
                tag(&link.kind)?,
                json(&link.paths)?,
                json(&link.findings)?,
                link.plan_revision
            ],
        )
        .map_err(storage)?;
        Ok(())
    }

    fn implement_questions(&self, flow: FlowId) -> Result<Vec<ImplementQuestion>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            "SELECT question_id, flow_id, kind, paths, findings, plan_revision
             FROM implement_questions WHERE flow_id = ?1 ORDER BY question_id",
            params![flow.0],
            read_question,
        )?
        .into_iter()
        .map(|(mut link, kind, paths, findings)| {
            link.kind = untag(kind)?;
            link.paths = from_json(&paths)?;
            link.findings = from_json(&findings)?;
            Ok(link)
        })
        .collect()
    }
}

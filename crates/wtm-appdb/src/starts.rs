//! [`StartStore`] over SQLite: Start Work's jobs (migration 11).
//!
//! One row per work item, which is the whole point: a second Start of the same issue meets the
//! first one's row in the same transaction that would have written its own. A row whose run has
//! ended is rewritten for the new run rather than kept beside it, because the job is "what is being
//! done to give this issue its worktree now"; what it used before is in the work item's links.

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use wtm_core::flow::{
    FlowId, NewStartJob, StartJob, StartJobChange, StartJobError, UnixMs, WorkItemId,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStoreError, StartStore};

use crate::AppDb;
use crate::runs::{ensure_flow, query};
use crate::store::{from_json, json, storage, tag, untag};

const COLUMNS: &str = "work_item_id, flow_id, project, issue_key, candidate, worktree, branch, \
     state, detail, setup_session, created_at, updated_at";

struct JobRow {
    work_item: i64,
    flow: i64,
    project: String,
    key: String,
    candidate: String,
    worktree: String,
    branch: Option<String>,
    state: String,
    detail: Option<String>,
    setup_session: Option<String>,
    created_at: UnixMs,
    updated_at: UnixMs,
}

impl JobRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            work_item: row.get(0)?,
            flow: row.get(1)?,
            project: row.get(2)?,
            key: row.get(3)?,
            candidate: row.get(4)?,
            worktree: row.get(5)?,
            branch: row.get(6)?,
            state: row.get(7)?,
            detail: row.get(8)?,
            setup_session: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    }

    fn into_job(self) -> Result<StartJob, FlowStoreError> {
        Ok(StartJob {
            work_item: WorkItemId(self.work_item),
            flow: FlowId(self.flow),
            project: ProjectId::from_root(std::path::Path::new(&self.project)),
            key: self.key,
            candidate: from_json(&self.candidate)?,
            worktree: self.worktree,
            branch: self.branch,
            state: untag(self.state)?,
            detail: self.detail,
            setup_session: self.setup_session,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn job_where(
    conn: &Connection,
    clause: &str,
    param: i64,
) -> Result<Option<StartJob>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM start_jobs WHERE {clause}"),
        params![param],
        JobRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(JobRow::into_job)
    .transpose()
}

impl StartStore for AppDb {
    fn record_start_job(&self, job: &NewStartJob, at: UnixMs) -> Result<StartJob, FlowStoreError> {
        let mut conn = self.conn.lock();
        ensure_flow(&conn, job.flow)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        // A job whose run is still open is the start under way. Read inside the write transaction,
        // so a second Start cannot slip between the read and the write.
        let open: Option<i64> = tx
            .query_row(
                "SELECT s.flow_id FROM start_jobs s JOIN flows f ON f.id = s.flow_id
                 WHERE s.work_item_id = ?1 AND f.outcome IS NULL",
                params![job.work_item.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        if let Some(flow) = open {
            return Err(FlowStoreError::StartUnderway {
                item: job.work_item,
                flow: FlowId(flow),
            });
        }
        tx.execute(
            "INSERT INTO start_jobs
                 (work_item_id, flow_id, project, issue_key, candidate, worktree, branch, state,
                  detail, setup_session, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, ?9, ?9)
             ON CONFLICT (work_item_id) DO UPDATE SET
                 flow_id = excluded.flow_id, project = excluded.project,
                 issue_key = excluded.issue_key, candidate = excluded.candidate,
                 worktree = excluded.worktree, branch = excluded.branch, state = excluded.state,
                 detail = NULL, setup_session = NULL,
                 created_at = excluded.created_at, updated_at = excluded.updated_at",
            params![
                job.work_item.0,
                job.flow.0,
                job.project.as_str(),
                job.key,
                json(&job.candidate)?,
                job.worktree,
                job.branch,
                tag(&job.state)?,
                at
            ],
        )
        .map_err(storage)?;
        let recorded = job_where(&tx, "work_item_id = ?1", job.work_item.0)?
            .ok_or(FlowStoreError::UnknownStartJob(job.work_item))?;
        tx.commit().map_err(storage)?;
        Ok(recorded)
    }

    fn start_job(&self, item: WorkItemId) -> Result<Option<StartJob>, FlowStoreError> {
        job_where(&self.conn.lock(), "work_item_id = ?1", item.0)
    }

    fn start_job_of(&self, flow: FlowId) -> Result<Option<StartJob>, FlowStoreError> {
        job_where(&self.conn.lock(), "flow_id = ?1", flow.0)
    }

    fn change_start_job(
        &self,
        item: WorkItemId,
        change: &StartJobChange,
        at: UnixMs,
    ) -> Result<StartJob, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let mut job = job_where(&tx, "work_item_id = ?1", item.0)?
            .ok_or(FlowStoreError::UnknownStartJob(item))?;
        // Staying in the same state is how a job notes its terminal or a detail on the way.
        if job.state != change.state && !job.state.can_move_to(change.state) {
            return Err(StartJobError::NoSuchStep {
                from: job.state,
                to: change.state,
            }
            .into());
        }
        job.state = change.state;
        change.detail.apply(&mut job.detail);
        change.setup_session.apply(&mut job.setup_session);
        if let Some(worktree) = &change.worktree {
            job.worktree.clone_from(worktree);
        }
        tx.execute(
            "UPDATE start_jobs SET state = ?2, detail = ?3, setup_session = ?4, worktree = ?5,
                 updated_at = ?6
             WHERE work_item_id = ?1",
            params![
                item.0,
                tag(&job.state)?,
                job.detail,
                job.setup_session,
                job.worktree,
                at
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        job.updated_at = at;
        Ok(job)
    }

    fn start_jobs(&self, project: Option<&ProjectId>) -> Result<Vec<StartJob>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!(
                "SELECT {COLUMNS} FROM start_jobs
                 WHERE (?1 IS NULL OR project = ?1)
                 ORDER BY created_at, work_item_id"
            ),
            params![project.map(ProjectId::as_str)],
            JobRow::read,
        )?
        .into_iter()
        .map(JobRow::into_job)
        .collect()
    }
}

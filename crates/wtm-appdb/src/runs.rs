//! [`RunStore`] over SQLite: a review run's snapshot, steps and invocations.
//!
//! Enums are stored as their serde names, the same rule `store.rs` follows, so `sqlite3 flow.db`
//! reads the way the IPC wire and the event JSON do. A step's result is its `Submission` as JSON,
//! exactly what `flow_submit` accepted, so a later reader sees what the role handed in rather than
//! a projection of it.

use rusqlite::{Connection, OptionalExtension, Row, params};
use wtm_core::flow::{
    BlobRef, CheckRun, FindingId, FindingRecord, FlowId, Invocation, InvocationChange,
    InvocationId, NewCheckRun, NewInvocation, NewReviewRun, NewStep, Reconciled, ReviewRun,
    Snapshot, SnapshotId, Step, StepChange, StepId, UnixMs, WorkItemId,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStoreError, RunStore};

use crate::AppDb;
use crate::findings;
use crate::store::{from_json, is_unique_violation, json, storage, tag, untag};

const REVIEW_COLUMNS: &str =
    "flow_id, project, worktree, worktree_path, recipe, base, snapshot_id, created_at";

const STEP_COLUMNS: &str = "id, flow_id, stage, role, provider, model, profile, workspace, \
     timeout_ms, retries, status, failures, stands_in_for, handover, detail, resets_at, result, \
     created_at, updated_at, skill_problem";

const INVOCATION_COLUMNS: &str = "id, flow_id, step_id, attempt, role, provider, model, profile, \
     snapshot_id, cwd, session, provider_session, status, detail, progress, started_at, ended_at, \
     instruction_sources";

impl RunStore for AppDb {
    fn work_item_for_worktree(
        &self,
        project: &ProjectId,
        worktree_path: &str,
    ) -> Result<Option<WorkItemId>, FlowStoreError> {
        let conn = self.conn.lock();
        // The newest link wins: a worktree path can be reused after a removal, and the item that
        // linked it last is the one it now belongs to.
        conn.query_row(
            "SELECT l.work_item_id FROM work_item_links l
             JOIN work_items w ON w.id = l.work_item_id
             WHERE w.project = ?1 AND l.kind = 'worktree'
               AND json_extract(l.link, '$.path') = ?2
             ORDER BY l.id DESC LIMIT 1",
            params![project.as_str(), worktree_path],
            |row| row.get(0).map(WorkItemId),
        )
        .optional()
        .map_err(storage)
    }

    fn create_review(
        &self,
        review: &NewReviewRun,
        at: UnixMs,
    ) -> Result<ReviewRun, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, review.flow)?;
        conn.execute(
            "INSERT INTO review_runs (flow_id, project, worktree, worktree_path, recipe, base,
                                      created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                review.flow.0,
                review.project.as_str(),
                review.worktree,
                review.worktree_path,
                review.recipe,
                review.base,
                at
            ],
        )
        .map_err(storage)?;
        Ok(ReviewRun {
            flow: review.flow,
            project: review.project.clone(),
            worktree: review.worktree.clone(),
            worktree_path: review.worktree_path.clone(),
            recipe: review.recipe.clone(),
            base: review.base.clone(),
            snapshot: None,
            created_at: at,
        })
    }

    fn review(&self, flow: FlowId) -> Result<Option<ReviewRun>, FlowStoreError> {
        let conn = self.conn.lock();
        conn.query_row(
            &format!("SELECT {REVIEW_COLUMNS} FROM review_runs WHERE flow_id = ?1"),
            params![flow.0],
            read_review,
        )
        .optional()
        .map_err(storage)
    }

    fn reviews_of(
        &self,
        project: &ProjectId,
        worktree: &str,
    ) -> Result<Vec<ReviewRun>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!(
                "SELECT {REVIEW_COLUMNS} FROM review_runs
                 WHERE project = ?1 AND worktree = ?2 ORDER BY flow_id DESC"
            ),
            params![project.as_str(), worktree],
            read_review,
        )
    }

    fn open_reviews(&self) -> Result<Vec<ReviewRun>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!(
                "SELECT {REVIEW_COLUMNS} FROM review_runs
                 WHERE flow_id IN (SELECT id FROM flows WHERE outcome IS NULL)
                 ORDER BY flow_id"
            ),
            [],
            read_review,
        )
    }

    fn record_snapshot(
        &self,
        flow: FlowId,
        snapshot: &Snapshot,
        at: UnixMs,
    ) -> Result<SnapshotId, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(storage)?;
        ensure_flow(&tx, flow)?;
        tx.execute(
            "INSERT INTO snapshots (flow_id, head_sha, tree_sha, base_sha, patch_path, patch_sha256,
                                    patch_bytes, taken_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                flow.0,
                snapshot.head_sha,
                snapshot.tree_sha,
                snapshot.base_sha,
                snapshot.patch.path,
                snapshot.patch.sha256,
                snapshot.patch.bytes,
                at
            ],
        )
        .map_err(storage)?;
        let id = SnapshotId(tx.last_insert_rowid());
        tx.execute(
            "UPDATE review_runs SET snapshot_id = ?1 WHERE flow_id = ?2",
            params![id.0, flow.0],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(id)
    }

    fn snapshot(&self, id: SnapshotId) -> Result<Option<Snapshot>, FlowStoreError> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT head_sha, tree_sha, base_sha, patch_path, patch_sha256, patch_bytes
             FROM snapshots WHERE id = ?1",
            params![id.0],
            |row| {
                Ok(Snapshot {
                    head_sha: row.get(0)?,
                    tree_sha: row.get(1)?,
                    base_sha: row.get(2)?,
                    patch: BlobRef {
                        path: row.get(3)?,
                        sha256: row.get(4)?,
                        bytes: row.get(5)?,
                    },
                })
            },
        )
        .optional()
        .map_err(storage)
    }

    fn add_steps(
        &self,
        flow: FlowId,
        steps: &[NewStep],
        at: UnixMs,
    ) -> Result<Vec<Step>, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(storage)?;
        ensure_flow(&tx, flow)?;
        let mut ids = Vec::with_capacity(steps.len());
        for step in steps {
            let inserted = tx.execute(
                "INSERT INTO steps (flow_id, stage, role, provider, model, profile, workspace,
                                    timeout_ms, retries, status, stands_in_for, handover, detail,
                                    created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
                params![
                    flow.0,
                    step.stage,
                    step.role,
                    tag(&step.provider)?,
                    step.model,
                    tag(&step.profile)?,
                    tag(&step.workspace)?,
                    step.timeout_ms,
                    step.retries,
                    tag(&step.status)?,
                    step.stands_in_for,
                    step.handover,
                    step.detail,
                    at
                ],
            );
            match inserted {
                Ok(_) => ids.push(tx.last_insert_rowid()),
                Err(e) if is_unique_violation(&e) => {
                    return Err(FlowStoreError::Storage(format!(
                        "run {flow} already has a step for role `{}`",
                        step.role
                    )));
                }
                Err(e) => return Err(storage(e)),
            }
        }
        let added = ids
            .into_iter()
            .map(|id| load_step(&tx, StepId(id)))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().map_err(storage)?;
        Ok(added)
    }

    fn steps(&self, flow: FlowId) -> Result<Vec<Step>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!("SELECT {STEP_COLUMNS} FROM steps WHERE flow_id = ?1 ORDER BY stage, id"),
            params![flow.0],
            StepRow::read,
        )?
        .into_iter()
        .map(StepRow::into_step)
        .collect()
    }

    fn update_step(
        &self,
        step: StepId,
        change: &StepChange,
        at: UnixMs,
    ) -> Result<Step, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(storage)?;
        let mut current = load_step(&tx, step)?;
        if let Some(status) = change.status {
            current.status = status;
        }
        if let Some(failures) = change.failures {
            current.failures = failures;
        }
        change.detail.apply(&mut current.detail);
        change.resets_at.apply(&mut current.resets_at);
        change.skill_problem.apply(&mut current.skill_problem);
        if let Some(result) = &change.result {
            current.result = Some(result.clone());
        }
        let result = current.result.as_ref().map(json).transpose()?;
        let skill_problem = current.skill_problem.as_ref().map(json).transpose()?;
        tx.execute(
            "UPDATE steps SET status = ?1, failures = ?2, detail = ?3, resets_at = ?4, result = ?5,
                              updated_at = ?6, skill_problem = ?7
             WHERE id = ?8",
            params![
                tag(&current.status)?,
                current.failures,
                current.detail,
                current.resets_at,
                result,
                at,
                skill_problem,
                step.0
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        current.updated_at = at;
        Ok(current)
    }

    fn start_invocation(
        &self,
        invocation: &NewInvocation,
        at: UnixMs,
    ) -> Result<Invocation, FlowStoreError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO invocations (flow_id, step_id, attempt, role, provider, model, profile,
                                      snapshot_id, cwd, status, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'opening', ?10)",
            params![
                invocation.flow.0,
                invocation.step.0,
                invocation.attempt,
                invocation.role,
                tag(&invocation.provider)?,
                invocation.model,
                tag(&invocation.profile)?,
                invocation.snapshot.map(|s| s.0),
                invocation.cwd,
                at
            ],
        )
        .map_err(storage)?;
        load_invocation(&conn, InvocationId(conn.last_insert_rowid()))
    }

    fn update_invocation(
        &self,
        invocation: InvocationId,
        change: &InvocationChange,
        _at: UnixMs,
    ) -> Result<Invocation, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(storage)?;
        let mut current = load_invocation(&tx, invocation)?;
        if let Some(status) = change.status {
            current.status = status;
        }
        if let Some(session) = &change.session {
            current.session = Some(session.clone());
        }
        if let Some(provider_session) = &change.provider_session {
            current.provider_session = Some(provider_session.clone());
        }
        if let Some(detail) = &change.detail {
            current.detail = Some(detail.clone());
        }
        if let Some(progress) = &change.progress {
            current.progress = Some(progress.clone());
        }
        if let Some(sources) = &change.instruction_sources {
            current.instruction_sources.clone_from(sources);
        }
        if let Some(ended_at) = change.ended_at {
            current.ended_at = Some(ended_at);
        }
        tx.execute(
            "UPDATE invocations SET status = ?1, session = ?2, provider_session = ?3, detail = ?4,
                                    progress = ?5, ended_at = ?6, instruction_sources = ?7
             WHERE id = ?8",
            params![
                tag(&current.status)?,
                current.session,
                current.provider_session,
                current.detail,
                current.progress,
                current.ended_at,
                json(&current.instruction_sources)?,
                invocation.0
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(current)
    }

    fn invocations(&self, flow: FlowId) -> Result<Vec<Invocation>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!("SELECT {INVOCATION_COLUMNS} FROM invocations WHERE flow_id = ?1 ORDER BY id"),
            params![flow.0],
            InvocationRow::read,
        )?
        .into_iter()
        .map(InvocationRow::into_invocation)
        .collect()
    }

    fn findings(&self, flow: FlowId) -> Result<Vec<FindingRecord>, FlowStoreError> {
        findings::findings(&self.conn.lock(), flow)
    }

    fn finding(&self, id: FindingId) -> Result<Option<FindingRecord>, FlowStoreError> {
        findings::finding(&self.conn.lock(), id)
    }

    fn save_consolidation(
        &self,
        flow: FlowId,
        change: &Reconciled,
        at: UnixMs,
    ) -> Result<Vec<FindingRecord>, FlowStoreError> {
        let mut conn = self.conn.lock();
        ensure_flow(&conn, flow)?;
        findings::save_consolidation(&mut conn, flow, change, at)
    }

    fn dismiss_finding(
        &self,
        id: FindingId,
        reason: Option<&str>,
        at: UnixMs,
    ) -> Result<FindingRecord, FlowStoreError> {
        findings::dismiss(&self.conn.lock(), id, reason, at)
    }

    fn record_check(&self, run: &NewCheckRun, at: UnixMs) -> Result<CheckRun, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, run.flow)?;
        findings::record_check(&conn, run, at)
    }

    fn checks(&self, flow: FlowId) -> Result<Vec<CheckRun>, FlowStoreError> {
        findings::checks(&self.conn.lock(), flow)
    }
}

pub(crate) fn query<T>(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, FlowStoreError> {
    let mut statement = conn.prepare(sql).map_err(storage)?;
    statement
        .query_map(params, read)
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)
}

pub(crate) fn ensure_flow(conn: &Connection, flow: FlowId) -> Result<(), FlowStoreError> {
    let exists = conn
        .query_row("SELECT 1 FROM flows WHERE id = ?1", params![flow.0], |_| {
            Ok(())
        })
        .optional()
        .map_err(storage)?;
    exists.ok_or(FlowStoreError::UnknownFlow(flow))
}

fn read_review(row: &Row<'_>) -> rusqlite::Result<ReviewRun> {
    let project: String = row.get(1)?;
    Ok(ReviewRun {
        flow: FlowId(row.get(0)?),
        project: ProjectId::from_root(std::path::Path::new(&project)),
        worktree: row.get(2)?,
        worktree_path: row.get(3)?,
        recipe: row.get(4)?,
        base: row.get(5)?,
        snapshot: row.get::<_, Option<i64>>(6)?.map(SnapshotId),
        created_at: row.get(7)?,
    })
}

fn load_step(conn: &Connection, id: StepId) -> Result<Step, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {STEP_COLUMNS} FROM steps WHERE id = ?1"),
        params![id.0],
        StepRow::read,
    )
    .optional()
    .map_err(storage)?
    .ok_or_else(|| FlowStoreError::Storage(format!("no step {id}")))?
    .into_step()
}

fn load_invocation(conn: &Connection, id: InvocationId) -> Result<Invocation, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {INVOCATION_COLUMNS} FROM invocations WHERE id = ?1"),
        params![id.0],
        InvocationRow::read,
    )
    .optional()
    .map_err(storage)?
    .ok_or_else(|| FlowStoreError::Storage(format!("no invocation {id}")))?
    .into_invocation()
}

/// A `steps` row as SQLite hands it back, before its names and JSON are read.
struct StepRow {
    id: i64,
    flow_id: i64,
    stage: u32,
    role: String,
    provider: String,
    model: Option<String>,
    profile: String,
    workspace: String,
    timeout_ms: u64,
    retries: u32,
    status: String,
    failures: u32,
    stands_in_for: Option<String>,
    handover: Option<String>,
    detail: Option<String>,
    resets_at: Option<u64>,
    result: Option<String>,
    created_at: UnixMs,
    updated_at: UnixMs,
    skill_problem: Option<String>,
}

impl StepRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow_id: row.get(1)?,
            stage: row.get(2)?,
            role: row.get(3)?,
            provider: row.get(4)?,
            model: row.get(5)?,
            profile: row.get(6)?,
            workspace: row.get(7)?,
            timeout_ms: row.get(8)?,
            retries: row.get(9)?,
            status: row.get(10)?,
            failures: row.get(11)?,
            stands_in_for: row.get(12)?,
            handover: row.get(13)?,
            detail: row.get(14)?,
            resets_at: row.get(15)?,
            result: row.get(16)?,
            created_at: row.get(17)?,
            updated_at: row.get(18)?,
            skill_problem: row.get(19)?,
        })
    }

    fn into_step(self) -> Result<Step, FlowStoreError> {
        Ok(Step {
            id: StepId(self.id),
            flow: FlowId(self.flow_id),
            stage: self.stage,
            role: self.role,
            provider: untag(self.provider)?,
            model: self.model,
            profile: untag(self.profile)?,
            workspace: untag(self.workspace)?,
            timeout_ms: self.timeout_ms,
            retries: self.retries,
            status: untag(self.status)?,
            failures: self.failures,
            stands_in_for: self.stands_in_for,
            handover: self.handover,
            detail: self.detail,
            resets_at: self.resets_at,
            skill_problem: self.skill_problem.as_deref().map(from_json).transpose()?,
            result: self.result.as_deref().map(from_json).transpose()?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// An `invocations` row as SQLite hands it back.
struct InvocationRow {
    id: i64,
    flow_id: i64,
    step_id: i64,
    attempt: u32,
    role: String,
    provider: String,
    model: Option<String>,
    profile: String,
    snapshot_id: Option<i64>,
    cwd: String,
    session: Option<String>,
    provider_session: Option<String>,
    status: String,
    detail: Option<String>,
    progress: Option<String>,
    started_at: UnixMs,
    ended_at: Option<UnixMs>,
    instruction_sources: Option<String>,
}

impl InvocationRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow_id: row.get(1)?,
            step_id: row.get(2)?,
            attempt: row.get(3)?,
            role: row.get(4)?,
            provider: row.get(5)?,
            model: row.get(6)?,
            profile: row.get(7)?,
            snapshot_id: row.get(8)?,
            cwd: row.get(9)?,
            session: row.get(10)?,
            provider_session: row.get(11)?,
            status: row.get(12)?,
            detail: row.get(13)?,
            progress: row.get(14)?,
            started_at: row.get(15)?,
            ended_at: row.get(16)?,
            instruction_sources: row.get(17)?,
        })
    }

    fn into_invocation(self) -> Result<Invocation, FlowStoreError> {
        Ok(Invocation {
            id: InvocationId(self.id),
            flow: FlowId(self.flow_id),
            step: StepId(self.step_id),
            attempt: self.attempt,
            role: self.role,
            provider: untag(self.provider)?,
            model: self.model,
            profile: untag(self.profile)?,
            snapshot: self.snapshot_id.map(SnapshotId),
            cwd: self.cwd,
            session: self.session,
            provider_session: self.provider_session,
            status: untag(self.status)?,
            detail: self.detail,
            progress: self.progress,
            instruction_sources: match self.instruction_sources {
                Some(text) => from_json(&text)?,
                None => Vec::new(),
            },
            started_at: self.started_at,
            ended_at: self.ended_at,
        })
    }
}

//! [`LeaseStore`] and [`FixStore`] over SQLite (migration 8).
//!
//! # The lease rule is checked where the lease is granted
//!
//! Acquiring reads the worktree's open leases and inserts in one `IMMEDIATE` transaction, so the
//! read cannot be overtaken by another acquire between the two. The rule it applies is
//! [`lease_conflict`]'s, and the trigger and partial index in the migration hold the same rule for
//! anything that reaches the table another way: a refusal from them is reported as the same
//! [`FlowStoreError::Leased`], naming whoever holds the worktree.

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use wtm_core::flow::{
    Fix, FixChange, FixId, FlowId, Lease, LeaseId, LeaseKind, LeaseRelease, NewFix, NewLease,
    OutlierQuestion, StepId, UnixMs, lease_conflict,
};
use wtm_core::ports::{FixStore, FlowStoreError, LeaseStore};

use crate::AppDb;
use crate::runs::{ensure_flow, query};
use crate::store::{from_json, json, storage, tag, untag};

const LEASE_COLUMNS: &str =
    "id, worktree, flow_id, kind, holder, acquired_at, released_at, released_why";

const FIX_COLUMNS: &str = "id, flow_id, step_id, role, findings, scope, base_sha, head_sha, \
     diff_hash, state, detail, asked, allowed, started_at, ended_at";

struct LeaseRow {
    id: i64,
    worktree: String,
    flow: i64,
    kind: String,
    holder: String,
    acquired_at: UnixMs,
    released_at: Option<UnixMs>,
    released_why: Option<String>,
}

impl LeaseRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            worktree: row.get(1)?,
            flow: row.get(2)?,
            kind: row.get(3)?,
            holder: row.get(4)?,
            acquired_at: row.get(5)?,
            released_at: row.get(6)?,
            released_why: row.get(7)?,
        })
    }

    fn into_lease(self) -> Result<Lease, FlowStoreError> {
        Ok(Lease {
            id: LeaseId(self.id),
            worktree: self.worktree,
            flow: FlowId(self.flow),
            kind: untag::<LeaseKind>(self.kind)?,
            holder: self.holder,
            acquired_at: self.acquired_at,
            released_at: self.released_at,
            released_why: self.released_why.map(untag::<LeaseRelease>).transpose()?,
        })
    }
}

fn open_on(conn: &Connection, worktree: Option<&str>) -> Result<Vec<Lease>, FlowStoreError> {
    let rows = match worktree {
        Some(worktree) => query(
            conn,
            &format!(
                "SELECT {LEASE_COLUMNS} FROM worktree_leases
                 WHERE released_at IS NULL AND worktree = ?1 ORDER BY id"
            ),
            params![worktree],
            LeaseRow::read,
        )?,
        None => query(
            conn,
            &format!(
                "SELECT {LEASE_COLUMNS} FROM worktree_leases WHERE released_at IS NULL ORDER BY id"
            ),
            [],
            LeaseRow::read,
        )?,
    };
    rows.into_iter().map(LeaseRow::into_lease).collect()
}

fn lease(conn: &Connection, id: LeaseId) -> Result<Option<Lease>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {LEASE_COLUMNS} FROM worktree_leases WHERE id = ?1"),
        params![id.0],
        LeaseRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(LeaseRow::into_lease)
    .transpose()
}

impl LeaseStore for AppDb {
    fn acquire_lease(&self, new: &NewLease, at: UnixMs) -> Result<Lease, FlowStoreError> {
        let mut conn = self.conn.lock();
        ensure_flow(&conn, new.flow)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let open = open_on(&tx, Some(&new.worktree))?;
        if let Some(held) = lease_conflict(new.kind, &open) {
            return Err(FlowStoreError::Leased {
                lease: Box::new(held.clone()),
            });
        }
        tx.execute(
            "INSERT INTO worktree_leases (worktree, flow_id, kind, holder, acquired_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![new.worktree, new.flow.0, tag(&new.kind)?, new.holder, at],
        )
        .map_err(storage)?;
        let id = LeaseId(tx.last_insert_rowid());
        let granted = lease(&tx, id)?.ok_or(FlowStoreError::UnknownLease(id))?;
        tx.commit().map_err(storage)?;
        Ok(granted)
    }

    fn release_lease(
        &self,
        id: LeaseId,
        why: LeaseRelease,
        at: UnixMs,
    ) -> Result<Option<Lease>, FlowStoreError> {
        let conn = self.conn.lock();
        let found = lease(&conn, id)?.ok_or(FlowStoreError::UnknownLease(id))?;
        if !found.is_open() {
            return Ok(None);
        }
        conn.execute(
            "UPDATE worktree_leases SET released_at = ?1, released_why = ?2 WHERE id = ?3",
            params![at, tag(&why)?, id.0],
        )
        .map_err(storage)?;
        lease(&conn, id)
    }

    fn open_leases(&self, worktree: Option<&str>) -> Result<Vec<Lease>, FlowStoreError> {
        open_on(&self.conn.lock(), worktree)
    }
}

struct FixRow {
    id: i64,
    flow: i64,
    step: i64,
    role: String,
    findings: String,
    scope: String,
    base_sha: String,
    head_sha: Option<String>,
    diff_hash: Option<String>,
    state: String,
    detail: Option<String>,
    asked: String,
    allowed: String,
    started_at: UnixMs,
    ended_at: Option<UnixMs>,
}

impl FixRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            step: row.get(2)?,
            role: row.get(3)?,
            findings: row.get(4)?,
            scope: row.get(5)?,
            base_sha: row.get(6)?,
            head_sha: row.get(7)?,
            diff_hash: row.get(8)?,
            state: row.get(9)?,
            detail: row.get(10)?,
            asked: row.get(11)?,
            allowed: row.get(12)?,
            started_at: row.get(13)?,
            ended_at: row.get(14)?,
        })
    }

    fn into_fix(self) -> Result<Fix, FlowStoreError> {
        Ok(Fix {
            id: FixId(self.id),
            flow: FlowId(self.flow),
            step: StepId(self.step),
            role: self.role,
            findings: from_json(&self.findings)?,
            scope: from_json(&self.scope)?,
            base_sha: self.base_sha,
            head_sha: self.head_sha,
            diff_hash: self.diff_hash,
            state: untag(self.state)?,
            detail: self.detail,
            asked: from_json::<Vec<OutlierQuestion>>(&self.asked)?,
            allowed: from_json(&self.allowed)?,
            started_at: self.started_at,
            ended_at: self.ended_at,
        })
    }
}

fn fix(conn: &Connection, id: FixId) -> Result<Option<Fix>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {FIX_COLUMNS} FROM fixes WHERE id = ?1"),
        params![id.0],
        FixRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(FixRow::into_fix)
    .transpose()
}

impl FixStore for AppDb {
    fn start_fix(&self, new: &NewFix, at: UnixMs) -> Result<Fix, FlowStoreError> {
        let mut conn = self.conn.lock();
        ensure_flow(&conn, new.flow)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let writing: Option<i64> = tx
            .query_row(
                "SELECT id FROM fixes WHERE flow_id = ?1 AND state = 'writing'",
                params![new.flow.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        if let Some(existing) = writing {
            return Err(FlowStoreError::FixWriting {
                existing: FixId(existing),
            });
        }
        tx.execute(
            "INSERT INTO fixes (flow_id, step_id, role, findings, scope, base_sha, state,
                 started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'writing', ?7)",
            params![
                new.flow.0,
                new.step.0,
                new.role,
                json(&new.findings)?,
                json(&new.scope)?,
                new.base_sha,
                at
            ],
        )
        .map_err(storage)?;
        let id = FixId(tx.last_insert_rowid());
        let started = fix(&tx, id)?.ok_or(FlowStoreError::UnknownFix(id))?;
        tx.commit().map_err(storage)?;
        Ok(started)
    }

    fn change_fix(&self, id: FixId, change: &FixChange, at: UnixMs) -> Result<Fix, FlowStoreError> {
        let conn = self.conn.lock();
        let mut current = fix(&conn, id)?.ok_or(FlowStoreError::UnknownFix(id))?;
        if let Some(state) = change.state
            && state != current.state
        {
            if current.state.is_over() {
                return Err(FlowStoreError::Storage(format!(
                    "fix {id} is already {:?} and stays so",
                    current.state
                )));
            }
            current.state = state;
            if state.is_over() {
                current.ended_at = Some(at);
            }
        }
        change.head_sha.apply(&mut current.head_sha);
        change.diff_hash.apply(&mut current.diff_hash);
        change.detail.apply(&mut current.detail);
        if let Some(asked) = &change.asked {
            current.asked.push(asked.clone());
        }
        for path in &change.allowed {
            if !current.allowed.contains(path) {
                current.allowed.push(path.clone());
            }
        }
        conn.execute(
            "UPDATE fixes SET head_sha = ?1, diff_hash = ?2, state = ?3, detail = ?4, asked = ?5,
                 allowed = ?6, ended_at = ?7
             WHERE id = ?8",
            params![
                current.head_sha,
                current.diff_hash,
                tag(&current.state)?,
                current.detail,
                json(&current.asked)?,
                json(&current.allowed)?,
                current.ended_at,
                id.0
            ],
        )
        .map_err(storage)?;
        Ok(current)
    }

    fn fix(&self, id: FixId) -> Result<Option<Fix>, FlowStoreError> {
        fix(&self.conn.lock(), id)
    }

    fn fixes(&self, flow: FlowId) -> Result<Vec<Fix>, FlowStoreError> {
        query(
            &self.conn.lock(),
            &format!("SELECT {FIX_COLUMNS} FROM fixes WHERE flow_id = ?1 ORDER BY id"),
            params![flow.0],
            FixRow::read,
        )?
        .into_iter()
        .map(FixRow::into_fix)
        .collect()
    }
}

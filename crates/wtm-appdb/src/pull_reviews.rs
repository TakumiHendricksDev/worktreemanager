//! [`PullReviewStore`] over SQLite: a teammate's pull request's last forge reading, your flag on
//! it, the heads its review moved on from, and the outcomes you chose (migration 5).
//!
//! The pull request and the id lists are JSON, as the step results are in `runs.rs`: what is
//! stored is what the forge or the review said, read back whole.

use rusqlite::{OptionalExtension, Row, params};
use wtm_core::flow::{
    FlowId, MirroredPr, NewOutcome, NewReviewedHead, Outcome, ReviewClaim, ReviewedHead,
    SnapshotId, UnixMs, WorkItemId,
};
use wtm_core::ports::{FlowStoreError, PullReviewStore};

use crate::AppDb;
use crate::store::{from_json, json, storage, tag, untag};

const HEAD_COLUMNS: &str =
    "id, flow_id, head_sha, snapshot_id, findings, checks, next_head, left_at";

const OUTCOME_COLUMNS: &str = "id, flow_id, kind, head_sha, notes, findings, decided_at";

/// A reviewed head's columns, read before its JSON is.
struct HeadRow {
    id: i64,
    flow: i64,
    head_sha: String,
    snapshot: i64,
    findings: String,
    checks: String,
    next_head: String,
    left_at: UnixMs,
}

impl HeadRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            head_sha: row.get(2)?,
            snapshot: row.get(3)?,
            findings: row.get(4)?,
            checks: row.get(5)?,
            next_head: row.get(6)?,
            left_at: row.get(7)?,
        })
    }

    fn into_head(self) -> Result<ReviewedHead, FlowStoreError> {
        Ok(ReviewedHead {
            id: self.id,
            head: NewReviewedHead {
                flow: FlowId(self.flow),
                head_sha: self.head_sha,
                snapshot: SnapshotId(self.snapshot),
                findings: from_json(&self.findings)?,
                checks: from_json(&self.checks)?,
                next_head: self.next_head,
            },
            left_at: self.left_at,
        })
    }
}

struct OutcomeRow {
    id: i64,
    flow: i64,
    kind: String,
    head_sha: String,
    notes: String,
    findings: String,
    decided_at: UnixMs,
}

impl OutcomeRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            kind: row.get(2)?,
            head_sha: row.get(3)?,
            notes: row.get(4)?,
            findings: row.get(5)?,
            decided_at: row.get(6)?,
        })
    }

    fn into_outcome(self) -> Result<Outcome, FlowStoreError> {
        Ok(Outcome {
            id: self.id,
            decision: NewOutcome {
                flow: FlowId(self.flow),
                kind: untag(self.kind)?,
                head_sha: self.head_sha,
                notes: self.notes,
                findings: from_json(&self.findings)?,
            },
            decided_at: self.decided_at,
        })
    }
}

fn ensure_item(conn: &rusqlite::Connection, item: WorkItemId) -> Result<(), FlowStoreError> {
    conn.query_row(
        "SELECT 1 FROM work_items WHERE id = ?1",
        params![item.0],
        |_| Ok(()),
    )
    .optional()
    .map_err(storage)?
    .ok_or(FlowStoreError::UnknownWorkItem(item))
}

fn ensure_flow(conn: &rusqlite::Connection, flow: FlowId) -> Result<(), FlowStoreError> {
    conn.query_row("SELECT 1 FROM flows WHERE id = ?1", params![flow.0], |_| {
        Ok(())
    })
    .optional()
    .map_err(storage)?
    .ok_or(FlowStoreError::UnknownFlow(flow))
}

impl PullReviewStore for AppDb {
    fn mirror_pr(&self, mirror: &MirroredPr) -> Result<(), FlowStoreError> {
        let conn = self.conn.lock();
        ensure_item(&conn, mirror.item)?;
        conn.execute(
            "INSERT INTO mirror_prs (work_item_id, pull_request, permission, read_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (work_item_id) DO UPDATE SET
                 pull_request = excluded.pull_request,
                 permission = excluded.permission,
                 read_at = excluded.read_at",
            params![
                mirror.item.0,
                json(&mirror.pull_request)?,
                tag(&mirror.permission)?,
                mirror.read_at
            ],
        )
        .map_err(storage)?;
        Ok(())
    }

    fn mirrored_pr(&self, item: WorkItemId) -> Result<Option<MirroredPr>, FlowStoreError> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT pull_request, permission, read_at FROM mirror_prs WHERE work_item_id = ?1",
                params![item.0],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, UnixMs>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(storage)?;
        row.map(|(pull_request, permission, read_at)| {
            Ok(MirroredPr {
                item,
                pull_request: from_json(&pull_request)?,
                permission: untag(permission)?,
                read_at,
            })
        })
        .transpose()
    }

    fn set_claim(
        &self,
        item: WorkItemId,
        since: Option<UnixMs>,
    ) -> Result<Option<ReviewClaim>, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_item(&conn, item)?;
        match since {
            // Setting it again keeps the first time: "since" is when you picked it up.
            Some(at) => conn.execute(
                "INSERT INTO review_claims (work_item_id, claimed_at) VALUES (?1, ?2)
                 ON CONFLICT (work_item_id) DO NOTHING",
                params![item.0, at],
            ),
            None => conn.execute(
                "DELETE FROM review_claims WHERE work_item_id = ?1",
                params![item.0],
            ),
        }
        .map_err(storage)?;
        drop(conn);
        self.claim(item)
    }

    fn claim(&self, item: WorkItemId) -> Result<Option<ReviewClaim>, FlowStoreError> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT claimed_at FROM review_claims WHERE work_item_id = ?1",
            params![item.0],
            |row| {
                Ok(ReviewClaim {
                    item,
                    since: row.get(0)?,
                })
            },
        )
        .optional()
        .map_err(storage)
    }

    fn record_reviewed_head(
        &self,
        head: &NewReviewedHead,
        at: UnixMs,
    ) -> Result<ReviewedHead, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, head.flow)?;
        conn.execute(
            "INSERT INTO reviewed_heads (flow_id, head_sha, snapshot_id, findings, checks,
                                         next_head, left_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                head.flow.0,
                head.head_sha,
                head.snapshot.0,
                json(&head.findings)?,
                json(&head.checks)?,
                head.next_head,
                at
            ],
        )
        .map_err(storage)?;
        let id = conn.last_insert_rowid();
        conn.query_row(
            &format!("SELECT {HEAD_COLUMNS} FROM reviewed_heads WHERE id = ?1"),
            params![id],
            HeadRow::read,
        )
        .map_err(storage)?
        .into_head()
    }

    fn reviewed_heads(&self, flow: FlowId) -> Result<Vec<ReviewedHead>, FlowStoreError> {
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare(&format!(
                "SELECT {HEAD_COLUMNS} FROM reviewed_heads WHERE flow_id = ?1 ORDER BY id"
            ))
            .map_err(storage)?;
        let rows = statement
            .query_map(params![flow.0], HeadRow::read)
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        rows.into_iter().map(HeadRow::into_head).collect()
    }

    fn record_outcome(&self, outcome: &NewOutcome, at: UnixMs) -> Result<Outcome, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, outcome.flow)?;
        conn.execute(
            "INSERT INTO review_outcomes (flow_id, kind, head_sha, notes, findings, decided_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                outcome.flow.0,
                tag(&outcome.kind)?,
                outcome.head_sha,
                outcome.notes,
                json(&outcome.findings)?,
                at
            ],
        )
        .map_err(storage)?;
        let id = conn.last_insert_rowid();
        conn.query_row(
            &format!("SELECT {OUTCOME_COLUMNS} FROM review_outcomes WHERE id = ?1"),
            params![id],
            OutcomeRow::read,
        )
        .map_err(storage)?
        .into_outcome()
    }

    fn outcomes(&self, flow: FlowId) -> Result<Vec<Outcome>, FlowStoreError> {
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare(&format!(
                "SELECT {OUTCOME_COLUMNS} FROM review_outcomes WHERE flow_id = ?1 ORDER BY id"
            ))
            .map_err(storage)?;
        let rows = statement
            .query_map(params![flow.0], OutcomeRow::read)
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        rows.into_iter().map(OutcomeRow::into_outcome).collect()
    }
}

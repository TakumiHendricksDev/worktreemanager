//! [`OutboxStore`] over SQLite: approvals and the operations they cover (migration 7).
//!
//! An operation's payload, result, attention and resolution are JSON, as a step's result is in
//! `runs.rs`: each kind of operation carries different fields, and what is stored is what will be,
//! or was, sent. The key's four columns are plain, because the unique index that keeps one row per
//! write has to see them.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use wtm_core::flow::{
    Approval, ApprovalId, ApprovalSubject, FlowId, NewApproval, NewOperation, Operation,
    OperationChange, OperationId, OperationKey, OperationState, Publication, UnixMs,
};
use wtm_core::ports::{FlowStoreError, OutboxStore};

use crate::AppDb;
use crate::runs::{ensure_flow, query};
use crate::store::{from_json, json, storage, tag, untag};

const OPERATION_COLUMNS: &str = "id, flow_id, kind, target, payload_hash, payload, approval_id, \
     position, state, attempts, last_error, result, attention, resolution, created_at, updated_at";

const APPROVAL_COLUMNS: &str = "id, flow_id, intent, binding, subject, granted_at";

/// An operation's columns, read before its JSON is.
struct OperationRow {
    id: i64,
    flow: i64,
    kind: String,
    target: String,
    payload_hash: String,
    payload: String,
    approval: i64,
    position: u32,
    state: String,
    attempts: u32,
    last_error: Option<String>,
    result: Option<String>,
    attention: Option<String>,
    resolution: Option<String>,
    created_at: UnixMs,
    updated_at: UnixMs,
}

impl OperationRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            kind: row.get(2)?,
            target: row.get(3)?,
            payload_hash: row.get(4)?,
            payload: row.get(5)?,
            approval: row.get(6)?,
            position: row.get(7)?,
            state: row.get(8)?,
            attempts: row.get(9)?,
            last_error: row.get(10)?,
            result: row.get(11)?,
            attention: row.get(12)?,
            resolution: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        })
    }

    fn into_operation(self) -> Result<Operation, FlowStoreError> {
        Ok(Operation {
            id: OperationId(self.id),
            key: OperationKey {
                flow: FlowId(self.flow),
                kind: untag(self.kind)?,
                target: self.target,
                payload_hash: self.payload_hash,
            },
            approval: ApprovalId(self.approval),
            position: self.position,
            payload: from_json(&self.payload)?,
            state: untag(self.state)?,
            attempts: self.attempts,
            last_error: self.last_error,
            result: optional(self.result.as_deref())?,
            attention: optional(self.attention.as_deref())?,
            resolution: optional(self.resolution.as_deref())?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn optional<T: serde::de::DeserializeOwned>(
    text: Option<&str>,
) -> Result<Option<T>, FlowStoreError> {
    text.map(from_json).transpose()
}

struct ApprovalRow {
    id: i64,
    flow: i64,
    intent: String,
    binding: String,
    subject: String,
    granted_at: UnixMs,
}

impl ApprovalRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            intent: row.get(2)?,
            binding: row.get(3)?,
            subject: row.get(4)?,
            granted_at: row.get(5)?,
        })
    }
}

fn operations_of(
    conn: &Connection,
    approval: ApprovalId,
) -> Result<Vec<Operation>, FlowStoreError> {
    query(
        conn,
        &format!(
            "SELECT {OPERATION_COLUMNS} FROM operations WHERE approval_id = ?1 \
             ORDER BY position, id"
        ),
        params![approval.0],
        OperationRow::read,
    )?
    .into_iter()
    .map(OperationRow::into_operation)
    .collect()
}

fn publication_from(conn: &Connection, row: ApprovalRow) -> Result<Publication, FlowStoreError> {
    let approval = Approval {
        id: ApprovalId(row.id),
        flow: FlowId(row.flow),
        intent: untag(row.intent)?,
        binding: from_json(&row.binding)?,
        granted_at: row.granted_at,
    };
    let ApprovalSubject::Review(review) = from_json(&row.subject)?;
    Ok(Publication {
        operations: operations_of(conn, approval.id)?,
        approval,
        review: *review,
    })
}

fn publication(
    conn: &Connection,
    approval: ApprovalId,
) -> Result<Option<Publication>, FlowStoreError> {
    let row = conn
        .query_row(
            &format!("SELECT {APPROVAL_COLUMNS} FROM approvals WHERE id = ?1"),
            params![approval.0],
            ApprovalRow::read,
        )
        .optional()
        .map_err(storage)?;
    row.map(|row| publication_from(conn, row)).transpose()
}

fn operation(conn: &Connection, id: OperationId) -> Result<Option<Operation>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {OPERATION_COLUMNS} FROM operations WHERE id = ?1"),
        params![id.0],
        OperationRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(OperationRow::into_operation)
    .transpose()
}

/// The queued operation with `key`, if there is one: its id and state.
fn existing(
    tx: &Transaction<'_>,
    key: &OperationKey,
) -> Result<Option<(OperationId, OperationState)>, FlowStoreError> {
    let row = tx
        .query_row(
            "SELECT id, state FROM operations
             WHERE flow_id = ?1 AND kind = ?2 AND target = ?3 AND payload_hash = ?4",
            params![key.flow.0, key.kind.as_str(), key.target, key.payload_hash],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(storage)?;
    row.map(|(id, state)| Ok((OperationId(id), untag(state)?)))
        .transpose()
}

impl OutboxStore for AppDb {
    fn approve(
        &self,
        approval: &NewApproval,
        operations: &[NewOperation],
        at: UnixMs,
    ) -> Result<Publication, FlowStoreError> {
        let mut conn = self.conn.lock();
        ensure_flow(&conn, approval.flow)?;
        let tx = conn.transaction().map_err(storage)?;
        let mut found = Vec::with_capacity(operations.len());
        for op in operations {
            let queued = existing(&tx, &op.key)?;
            if let Some((id, OperationState::Executing)) = queued {
                return Err(FlowStoreError::StillSending { operation: id });
            }
            found.push(queued);
        }
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
        let id = ApprovalId(tx.last_insert_rowid());
        for (op, queued) in operations.iter().zip(found) {
            match queued {
                None => {
                    tx.execute(
                        "INSERT INTO operations
                             (flow_id, kind, target, payload_hash, payload, approval_id, position,
                              state, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?8)",
                        params![
                            op.key.flow.0,
                            op.key.kind.as_str(),
                            op.key.target,
                            op.key.payload_hash,
                            json(&op.payload)?,
                            id.0,
                            op.position,
                            at
                        ],
                    )
                    .map_err(storage)?;
                }
                Some((existing, state)) => {
                    // The same write, approved again. Done stays done; one that stopped or was
                    // cancelled is queued again; one that failed keeps its error for the retry.
                    let state = match state {
                        OperationState::NeedsAttention | OperationState::Cancelled => {
                            OperationState::Pending
                        }
                        other => other,
                    };
                    tx.execute(
                        "UPDATE operations SET approval_id = ?1, position = ?2, state = ?3,
                             attention = CASE WHEN ?3 = 'needs_attention' THEN attention END,
                             updated_at = ?4
                         WHERE id = ?5",
                        params![id.0, op.position, tag(&state)?, at, existing.0],
                    )
                    .map_err(storage)?;
                }
            }
        }
        tx.commit().map_err(storage)?;
        publication(&conn, id)?.ok_or(FlowStoreError::UnknownApproval(id))
    }

    fn publication(&self, approval: ApprovalId) -> Result<Option<Publication>, FlowStoreError> {
        publication(&self.conn.lock(), approval)
    }

    fn publications(&self, flow: FlowId) -> Result<Vec<Publication>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!("SELECT {APPROVAL_COLUMNS} FROM approvals WHERE flow_id = ?1 ORDER BY id"),
            params![flow.0],
            ApprovalRow::read,
        )?
        .into_iter()
        .map(|row| publication_from(&conn, row))
        .collect()
    }

    fn operation(&self, id: OperationId) -> Result<Option<Operation>, FlowStoreError> {
        operation(&self.conn.lock(), id)
    }

    fn change_operation(
        &self,
        id: OperationId,
        change: &OperationChange,
        at: UnixMs,
    ) -> Result<Operation, FlowStoreError> {
        let conn = self.conn.lock();
        let mut op = operation(&conn, id)?.ok_or(FlowStoreError::UnknownOperation(id))?;
        op.state = op.state.move_to(change.state)?;
        change.error.apply(&mut op.last_error);
        change.result.apply(&mut op.result);
        change.attention.apply(&mut op.attention);
        change.resolution.apply(&mut op.resolution);
        if op.state != OperationState::NeedsAttention {
            op.attention = None;
        }
        if change.attempt {
            op.attempts += 1;
        }
        op.updated_at = at;
        conn.execute(
            "UPDATE operations SET state = ?1, attempts = ?2, last_error = ?3, result = ?4,
                 attention = ?5, resolution = ?6, updated_at = ?7
             WHERE id = ?8",
            params![
                tag(&op.state)?,
                op.attempts,
                op.last_error,
                op.result.as_ref().map(json).transpose()?,
                op.attention.as_ref().map(json).transpose()?,
                op.resolution.as_ref().map(json).transpose()?,
                at,
                id.0
            ],
        )
        .map_err(storage)?;
        Ok(op)
    }

    fn executing_operations(&self) -> Result<Vec<Operation>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!(
                "SELECT {OPERATION_COLUMNS} FROM operations WHERE state = 'executing' ORDER BY id"
            ),
            [],
            OperationRow::read,
        )?
        .into_iter()
        .map(OperationRow::into_operation)
        .collect()
    }
}

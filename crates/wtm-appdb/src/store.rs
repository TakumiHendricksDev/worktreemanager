//! [`FlowStore`] over SQLite.
//!
//! # Uniqueness is the database's, and the error names who won
//!
//! The `UNIQUE` constraints and the partial index in migration 1 decide every race between two
//! clicks. When one fires, the store looks up the row that already holds the place and returns its
//! id, so the caller can focus the existing work item or flow instead of showing a failure.
//!
//! # Enums are stored as their serde names
//!
//! A kind or phase is stored as the same `snake_case` text it has on the IPC wire and inside
//! event JSON, so the database, the events and the webview never disagree about a name, and
//! `sqlite3 flow.db` stays readable. The [`tag`] and [`untag`] helpers are that one conversion.

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use wtm_core::flow::{
    End, Flow, FlowEvent, FlowId, FlowKind, FlowState, IssueRef, NewWorkItem, PullRequestRef,
    RecordedEvent, RecordedLink, UnixMs, WorkItem, WorkItemId, WorkKey, WorkLink,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, FlowStoreError};

use crate::AppDb;

impl FlowStore for AppDb {
    fn create_work_item(&self, item: &NewWorkItem, at: UnixMs) -> Result<WorkItem, FlowStoreError> {
        let conn = self.conn.lock();
        let (tracker, issue_key) = issue_columns(item.issue.as_ref());
        let (repo, pr_number) = pull_request_columns(item.pull_request.as_ref());
        let inserted = conn.execute(
            "INSERT INTO work_items (project, title, tracker, issue_key, repo, pr_number, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                item.project.as_str(),
                item.title,
                tracker,
                issue_key,
                repo,
                pr_number,
                at
            ],
        );
        match inserted {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                let existing = conn
                    .query_row(
                        "SELECT id FROM work_items
                         WHERE (tracker = ?1 AND issue_key = ?2) OR (repo = ?3 AND pr_number = ?4)
                         ORDER BY id LIMIT 1",
                        params![tracker, issue_key, repo, pr_number],
                        |row| row.get(0),
                    )
                    .map_err(storage)?;
                return Err(FlowStoreError::WorkItemExists {
                    existing: WorkItemId(existing),
                });
            }
            Err(e) => return Err(storage(e)),
        }
        Ok(WorkItem {
            id: WorkItemId(conn.last_insert_rowid()),
            project: item.project.clone(),
            title: item.title.clone(),
            issue: item.issue.clone(),
            pull_request: item.pull_request.clone(),
            created_at: at,
        })
    }

    fn work_item(&self, id: WorkItemId) -> Result<Option<WorkItem>, FlowStoreError> {
        let conn = self.conn.lock();
        load_work_item(&conn, "id = ?1", params![id.0])
    }

    fn find_work_item(&self, key: &WorkKey) -> Result<Option<WorkItem>, FlowStoreError> {
        let conn = self.conn.lock();
        match key {
            WorkKey::Issue(issue) => load_work_item(
                &conn,
                "tracker = ?1 AND issue_key = ?2",
                params![issue.tracker, issue.key],
            ),
            WorkKey::PullRequest(pr) => load_work_item(
                &conn,
                "repo = ?1 AND pr_number = ?2",
                params![pr.repo, pr.number],
            ),
        }
    }

    fn work_items(&self, project: &ProjectId) -> Result<Vec<WorkItem>, FlowStoreError> {
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare(&format!(
                "SELECT {WORK_ITEM_COLUMNS} FROM work_items WHERE project = ?1 ORDER BY id"
            ))
            .map_err(storage)?;
        statement
            .query_map(params![project.as_str()], read_work_item)
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)
    }

    fn link(&self, item: WorkItemId, link: &WorkLink, at: UnixMs) -> Result<(), FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        ensure_work_item(&tx, item)?;

        if let WorkLink::PullRequest(pr) = link {
            let moved = tx.execute(
                "UPDATE work_items SET repo = ?1, pr_number = ?2 WHERE id = ?3",
                params![pr.repo, pr.number, item.0],
            );
            match moved {
                Ok(_) => {}
                Err(e) if is_unique_violation(&e) => {
                    let owner = tx
                        .query_row(
                            "SELECT id FROM work_items WHERE repo = ?1 AND pr_number = ?2",
                            params![pr.repo, pr.number],
                            |row| row.get(0),
                        )
                        .map_err(storage)?;
                    return Err(FlowStoreError::PullRequestTaken {
                        owner: WorkItemId(owner),
                    });
                }
                Err(e) => return Err(storage(e)),
            }
        }

        let kind = match link {
            WorkLink::Branch { .. } => "branch",
            WorkLink::Worktree { .. } => "worktree",
            WorkLink::PullRequest(_) => "pull_request",
        };
        tx.execute(
            "INSERT INTO work_item_links (work_item_id, kind, link, linked_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![item.0, kind, json(link)?, at],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)
    }

    fn links(&self, item: WorkItemId) -> Result<Vec<RecordedLink>, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_work_item(&conn, item)?;
        let mut statement = conn
            .prepare(
                "SELECT link, linked_at FROM work_item_links WHERE work_item_id = ?1 ORDER BY id",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map(params![item.0], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, UnixMs>(1)?))
            })
            .map_err(storage)?;
        rows.map(|row| {
            let (link, at) = row.map_err(storage)?;
            Ok(RecordedLink {
                link: from_json(&link)?,
                at,
            })
        })
        .collect()
    }

    fn start_flow(
        &self,
        item: WorkItemId,
        kind: FlowKind,
        at: UnixMs,
    ) -> Result<Flow, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        ensure_work_item(&tx, item)?;

        let state = FlowState::new(kind);
        let inserted = tx.execute(
            "INSERT INTO flows (work_item_id, kind, phase, last_seq, started_at, updated_at)
             VALUES (?1, ?2, ?3, 1, ?4, ?4)",
            params![item.0, tag(&kind)?, tag(&state.phase)?, at],
        );
        match inserted {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                let existing = tx
                    .query_row(
                        "SELECT id FROM flows
                         WHERE work_item_id = ?1 AND kind = ?2 AND outcome IS NULL",
                        params![item.0, tag(&kind)?],
                        |row| row.get(0),
                    )
                    .map_err(storage)?;
                return Err(FlowStoreError::FlowAlreadyOpen {
                    existing: FlowId(existing),
                });
            }
            Err(e) => return Err(storage(e)),
        }
        let id = FlowId(tx.last_insert_rowid());
        tx.execute(
            "INSERT INTO flow_events (flow_id, seq, at, event) VALUES (?1, 1, ?2, ?3)",
            params![id.0, at, json(&FlowEvent::Started { kind })?],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;

        Ok(Flow {
            id,
            work_item: item,
            state,
            seq: 1,
            started_at: at,
            updated_at: at,
        })
    }

    fn record(&self, flow: FlowId, event: &FlowEvent, at: UnixMs) -> Result<Flow, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let current = load_flow(&tx, flow)?.ok_or(FlowStoreError::UnknownFlow(flow))?;

        // The rules decide first. A refused event leaves this function before anything is
        // written, and the dropped transaction rolls back nothing.
        let state = current.state.apply(event)?;
        let seq = current.seq + 1;

        tx.execute(
            "INSERT INTO flow_events (flow_id, seq, at, event) VALUES (?1, ?2, ?3, ?4)",
            params![flow.0, seq, at, json(event)?],
        )
        .map_err(storage)?;
        let (outcome, failure) = outcome_columns(state.end.as_ref());
        tx.execute(
            "UPDATE flows
             SET phase = ?1, hold = ?2, outcome = ?3, failure = ?4, last_seq = ?5, updated_at = ?6
             WHERE id = ?7",
            params![
                tag(&state.phase)?,
                state.hold.as_ref().map(json).transpose()?,
                outcome,
                failure,
                seq,
                at,
                flow.0
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;

        Ok(Flow {
            state,
            seq,
            updated_at: at,
            ..current
        })
    }

    fn flow(&self, id: FlowId) -> Result<Option<Flow>, FlowStoreError> {
        let conn = self.conn.lock();
        load_flow(&conn, id)
    }

    fn open_flows(&self, item: WorkItemId) -> Result<Vec<Flow>, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_work_item(&conn, item)?;
        let mut statement = conn
            .prepare(&format!(
                "SELECT {FLOW_COLUMNS} FROM flows
                 WHERE work_item_id = ?1 AND outcome IS NULL ORDER BY id"
            ))
            .map_err(storage)?;
        let rows = statement
            .query_map(params![item.0], FlowRow::read)
            .map_err(storage)?;
        rows.map(|row| row.map_err(storage)?.into_flow()).collect()
    }

    fn events(&self, flow: FlowId) -> Result<Vec<RecordedEvent>, FlowStoreError> {
        let conn = self.conn.lock();
        if load_flow(&conn, flow)?.is_none() {
            return Err(FlowStoreError::UnknownFlow(flow));
        }
        let mut statement = conn
            .prepare("SELECT seq, at, event FROM flow_events WHERE flow_id = ?1 ORDER BY seq")
            .map_err(storage)?;
        let rows = statement
            .query_map(params![flow.0], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, UnixMs>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(storage)?;
        rows.map(|row| {
            let (seq, at, event) = row.map_err(storage)?;
            Ok(RecordedEvent {
                seq,
                at,
                event: from_json(&event)?,
            })
        })
        .collect()
    }
}

const WORK_ITEM_COLUMNS: &str =
    "id, project, title, tracker, issue_key, repo, pr_number, created_at";

fn load_work_item(
    conn: &Connection,
    filter: &str,
    params: impl rusqlite::Params,
) -> Result<Option<WorkItem>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {WORK_ITEM_COLUMNS} FROM work_items WHERE {filter}"),
        params,
        read_work_item,
    )
    .optional()
    .map_err(storage)
}

fn read_work_item(row: &Row<'_>) -> rusqlite::Result<WorkItem> {
    let project: String = row.get(1)?;
    let issue = match (row.get::<_, Option<String>>(3)?, row.get(4)?) {
        (Some(tracker), Some(key)) => Some(IssueRef { tracker, key }),
        _ => None,
    };
    let pull_request = match (row.get::<_, Option<String>>(5)?, row.get(6)?) {
        (Some(repo), Some(number)) => Some(PullRequestRef { repo, number }),
        _ => None,
    };
    Ok(WorkItem {
        id: WorkItemId(row.get(0)?),
        project: ProjectId::from_root(std::path::Path::new(&project)),
        title: row.get(2)?,
        issue,
        pull_request,
        created_at: row.get(7)?,
    })
}

fn ensure_work_item(conn: &Connection, item: WorkItemId) -> Result<(), FlowStoreError> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM work_items WHERE id = ?1",
            params![item.0],
            |_| Ok(()),
        )
        .optional()
        .map_err(storage)?;
    exists.ok_or(FlowStoreError::UnknownWorkItem(item))
}

const FLOW_COLUMNS: &str =
    "id, work_item_id, kind, phase, hold, outcome, failure, last_seq, started_at, updated_at";

fn load_flow(conn: &Connection, id: FlowId) -> Result<Option<Flow>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {FLOW_COLUMNS} FROM flows WHERE id = ?1"),
        params![id.0],
        FlowRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(FlowRow::into_flow)
    .transpose()
}

/// A `flows` row as SQLite holds it, before its text columns are parsed. Kept separate because a
/// row closure can only fail with a driver error, and a parse failure is the store's.
struct FlowRow {
    id: i64,
    work_item_id: i64,
    kind: String,
    phase: String,
    hold: Option<String>,
    outcome: Option<String>,
    failure: Option<String>,
    last_seq: u64,
    started_at: UnixMs,
    updated_at: UnixMs,
}

impl FlowRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            work_item_id: row.get(1)?,
            kind: row.get(2)?,
            phase: row.get(3)?,
            hold: row.get(4)?,
            outcome: row.get(5)?,
            failure: row.get(6)?,
            last_seq: row.get(7)?,
            started_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    fn into_flow(self) -> Result<Flow, FlowStoreError> {
        let end = match (self.outcome.as_deref(), self.failure) {
            (None, _) => None,
            (Some("done"), _) => Some(End::Done),
            (Some("cancelled"), _) => Some(End::Cancelled),
            (Some("failed"), Some(reason)) => Some(End::Failed { reason }),
            (Some(other), _) => {
                return Err(FlowStoreError::Storage(format!(
                    "flow {} has an unreadable outcome {other:?}",
                    self.id
                )));
            }
        };
        Ok(Flow {
            id: FlowId(self.id),
            work_item: WorkItemId(self.work_item_id),
            state: FlowState {
                kind: untag(self.kind)?,
                phase: untag(self.phase)?,
                hold: self.hold.as_deref().map(from_json).transpose()?,
                end,
            },
            seq: self.last_seq,
            started_at: self.started_at,
            updated_at: self.updated_at,
        })
    }
}

fn outcome_columns(end: Option<&End>) -> (Option<&'static str>, Option<&str>) {
    match end {
        None => (None, None),
        Some(End::Done) => (Some("done"), None),
        Some(End::Failed { reason }) => (Some("failed"), Some(reason)),
        Some(End::Cancelled) => (Some("cancelled"), None),
    }
}

fn issue_columns(issue: Option<&IssueRef>) -> (Option<&str>, Option<&str>) {
    issue.map_or((None, None), |issue| {
        (Some(issue.tracker.as_str()), Some(issue.key.as_str()))
    })
}

fn pull_request_columns(pr: Option<&PullRequestRef>) -> (Option<&str>, Option<u64>) {
    pr.map_or((None, None), |pr| (Some(pr.repo.as_str()), Some(pr.number)))
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

/// A unit enum variant as its serde name: `FlowKind::ReviewPr` is `review_pr`.
pub(crate) fn tag<T: Serialize>(value: &T) -> Result<String, FlowStoreError> {
    match serde_json::to_value(value).map_err(storage)? {
        serde_json::Value::String(name) => Ok(name),
        other => Err(FlowStoreError::Storage(format!(
            "expected a name, got {other}"
        ))),
    }
}

pub(crate) fn untag<T: DeserializeOwned>(name: String) -> Result<T, FlowStoreError> {
    serde_json::from_value(serde_json::Value::String(name)).map_err(storage)
}

pub(crate) fn json<T: Serialize>(value: &T) -> Result<String, FlowStoreError> {
    serde_json::to_string(value).map_err(storage)
}

pub(crate) fn from_json<T: DeserializeOwned>(text: &str) -> Result<T, FlowStoreError> {
    serde_json::from_str(text).map_err(storage)
}

fn storage(error: impl std::fmt::Display) -> FlowStoreError {
    FlowStoreError::Storage(error.to_string())
}

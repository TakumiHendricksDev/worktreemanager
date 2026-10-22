//! [`TrackerMirror`] over SQLite: each project's queue as its last sync read it, each issue and
//! sprint in it, and how the last attempt went (migration 9).
//!
//! What a search returned is kept as JSON beside the columns a person reading the file would look
//! for, as `mirror_prs` keeps a pull request: read back whole, never queried field by field.
//!
//! These methods touch the four mirror tables and nothing else. That is the contract the sync
//! use-case relies on, and `tests/tracker_mirror.rs` checks it from the other side by counting what
//! a sync left in every table that a run would write.

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use wtm_core::flow::{
    Issue, IssueDetail, MirroredQueue, QueueReading, QueuedIssue, Sprint, SyncFailure, TrackerKind,
    UnixMs,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStoreError, TrackerMirror};

use crate::AppDb;
use crate::store::{from_json, json, storage};

impl TrackerMirror for AppDb {
    fn mirror_queue(&self, reading: &QueueReading) -> Result<(), FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let tracker = reading.tracker.as_str();
        for queued in &reading.issues {
            tx.execute(
                "INSERT INTO mirror_issues (tracker, issue_key, status, issue, read_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (tracker, issue_key) DO UPDATE SET
                     status = excluded.status,
                     issue = excluded.issue,
                     read_at = excluded.read_at",
                params![
                    tracker,
                    queued.issue.key,
                    queued.issue.status.name,
                    json(&queued.issue)?,
                    reading.read_at
                ],
            )
            .map_err(storage)?;
        }
        for sprint in &reading.sprints {
            tx.execute(
                "INSERT INTO mirror_sprints (tracker, sprint_id, sprint, read_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (tracker, sprint_id) DO UPDATE SET
                     sprint = excluded.sprint,
                     read_at = excluded.read_at",
                params![tracker, sprint.id, json(sprint)?, reading.read_at],
            )
            .map_err(storage)?;
        }
        let sprint_ids: Vec<u64> = reading.sprints.iter().map(|s| s.id).collect();
        tx.execute(
            "INSERT INTO mirror_queues
                 (project, tracker, query, board, sprint_ids, read_at, failed_at, failure)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)
             ON CONFLICT (project) DO UPDATE SET
                 tracker = excluded.tracker,
                 query = excluded.query,
                 board = excluded.board,
                 sprint_ids = excluded.sprint_ids,
                 read_at = excluded.read_at,
                 failed_at = NULL,
                 failure = NULL",
            params![
                reading.project.as_str(),
                tracker,
                reading.query,
                reading.board,
                json(&sprint_ids)?,
                reading.read_at
            ],
        )
        .map_err(storage)?;
        tx.execute(
            "DELETE FROM mirror_queue_issues WHERE project = ?1",
            params![reading.project.as_str()],
        )
        .map_err(storage)?;
        for (position, queued) in reading.issues.iter().enumerate() {
            tx.execute(
                "INSERT INTO mirror_queue_issues (project, position, tracker, issue_key, sprint_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    reading.project.as_str(),
                    position,
                    tracker,
                    queued.issue.key,
                    queued.sprint
                ],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }

    fn record_sync_failure(
        &self,
        project: &ProjectId,
        tracker: TrackerKind,
        failure: &SyncFailure,
    ) -> Result<(), FlowStoreError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO mirror_queues (project, tracker, failed_at, failure)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (project) DO UPDATE SET
                 failed_at = excluded.failed_at,
                 failure = excluded.failure",
            params![
                project.as_str(),
                tracker.as_str(),
                failure.at,
                failure.message
            ],
        )
        .map_err(storage)?;
        Ok(())
    }

    fn mirrored_queue(&self, project: &ProjectId) -> Result<Option<MirroredQueue>, FlowStoreError> {
        let conn = self.conn.lock();
        let Some(row) = conn
            .query_row(
                "SELECT tracker, query, board, sprint_ids, read_at, failed_at, failure
                 FROM mirror_queues WHERE project = ?1",
                params![project.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<u64>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<UnixMs>>(4)?,
                        row.get::<_, Option<UnixMs>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(storage)?
        else {
            return Ok(None);
        };
        let (tracker, query, board, sprint_ids, read_at, failed_at, failure) = row;
        let tracker = TrackerKind::parse(&tracker)
            .ok_or_else(|| storage(format!("unknown tracker `{tracker}` in mirror_queues")))?;
        let failure = failed_at.map(|at| SyncFailure {
            at,
            message: failure.unwrap_or_default(),
        });
        let (Some(query), Some(read_at)) = (query, read_at) else {
            return Ok(Some(MirroredQueue {
                reading: None,
                failure,
            }));
        };

        let sprint_ids: Vec<u64> = from_json(&sprint_ids)?;
        let mut sprints = Vec::with_capacity(sprint_ids.len());
        for id in sprint_ids {
            let sprint: Option<String> = conn
                .query_row(
                    "SELECT sprint FROM mirror_sprints WHERE tracker = ?1 AND sprint_id = ?2",
                    params![tracker.as_str(), id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(storage)?;
            if let Some(sprint) = sprint {
                sprints.push(from_json::<Sprint>(&sprint)?);
            }
        }

        let mut statement = conn
            .prepare(
                "SELECT q.sprint_id, i.issue
                 FROM mirror_queue_issues q
                 JOIN mirror_issues i ON i.tracker = q.tracker AND i.issue_key = q.issue_key
                 WHERE q.project = ?1
                 ORDER BY q.position",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map(params![project.as_str()], |row| {
                Ok((row.get::<_, Option<u64>>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage)?;
        let mut issues = Vec::new();
        for row in rows {
            let (sprint, issue) = row.map_err(storage)?;
            issues.push(QueuedIssue {
                issue: from_json::<Issue>(&issue)?,
                sprint,
            });
        }

        Ok(Some(MirroredQueue {
            reading: Some(QueueReading {
                project: project.clone(),
                tracker,
                query,
                board,
                sprints,
                issues,
                read_at,
            }),
            failure,
        }))
    }

    fn mirror_issue(
        &self,
        tracker: TrackerKind,
        detail: &IssueDetail,
        at: UnixMs,
    ) -> Result<(), FlowStoreError> {
        let conn = self.conn.lock();
        // The row's fields come along: an issue opened after a sync shows its newer status in the
        // queue too, rather than two different statuses on one screen.
        conn.execute(
            "INSERT INTO mirror_issues
                 (tracker, issue_key, status, issue, read_at, detail, detail_read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5)
             ON CONFLICT (tracker, issue_key) DO UPDATE SET
                 status = excluded.status,
                 issue = excluded.issue,
                 read_at = excluded.read_at,
                 detail = excluded.detail,
                 detail_read_at = excluded.detail_read_at",
            params![
                tracker.as_str(),
                detail.issue.key,
                detail.issue.status.name,
                json(&detail.issue)?,
                at,
                json(detail)?
            ],
        )
        .map_err(storage)?;
        Ok(())
    }

    fn mirrored_issue(
        &self,
        tracker: TrackerKind,
        key: &str,
    ) -> Result<Option<(IssueDetail, UnixMs)>, FlowStoreError> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT detail, detail_read_at FROM mirror_issues
                 WHERE tracker = ?1 AND issue_key = ?2 AND detail IS NOT NULL",
                params![tracker.as_str(), key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, UnixMs>(1)?)),
            )
            .optional()
            .map_err(storage)?;
        row.map(|(detail, at)| Ok((from_json(&detail)?, at)))
            .transpose()
    }
}

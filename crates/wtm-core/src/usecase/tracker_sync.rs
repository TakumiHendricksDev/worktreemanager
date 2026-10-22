//! Syncing a project's queue from its tracker into the mirror, and reading one issue whole.
//!
//! # Sync cannot start a run, and the signature is the proof
//!
//! The plan's rule is that an issue moved in the tracker updates what the Work view shows and
//! starts nothing (§4.1): no flow, no step, no agent session. [`sync_queue`] keeps it by what it is
//! handed: a [`Tracker`], which only reads, and a [`TrackerMirror`], which only writes the mirror
//! tables. No [`FlowStore`](crate::ports::FlowStore), no [`RunStore`](crate::ports::RunStore), no
//! engine, no app. What it returns, [`SyncReport`], is plain data the window redraws from; no part
//! of the engine takes one. A later change that wanted a status change to start something would
//! have to change this signature to get the means, which is a review conversation rather than an
//! accident.
//!
//! `src-tauri/tests/tracker_queue.rs` holds the behavioural half: an external status change
//! synced through the real app and the real database leaves no flow, step or invocation behind.
//!
//! # When it runs
//!
//! On demand, when the Work view opens or comes back into focus, and after the app's own actions
//! (ARCHITECTURE §8 and §6a). Never on a timer; [`SyncReason`] records which, for the trace.
//!
//! # A failure keeps the last good reading
//!
//! A tracker that is down, or a sign-in that lapsed, is recorded beside the last reading that
//! worked rather than replacing it, so the queue keeps showing what it showed, with the failure
//! said above it.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::TrackerError;
use crate::flow::{
    IssueDetail, IssueQuery, QueueChanges, QueueReading, QueueSpec, QueuedIssue, Sprint, SprintRef,
    SprintState, SyncFailure, TrackerAccount, TrackerKind, UnixMs,
};
use crate::model::ProjectId;
use crate::ports::flow::FlowStoreError;
use crate::ports::{Tracker, TrackerMirror};

/// Why a sync ran. Only for the trace and the report: every reason syncs the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncReason {
    /// The Sync button.
    Demand,
    /// The Work view opened on the project, or the project changed under it.
    Open,
    /// The window came back into focus.
    Focus,
    /// The app just did something the queue shows the result of.
    AfterAction,
}

/// One sync, as asked.
#[derive(Debug, Clone)]
pub struct SyncRequest<'a> {
    pub project: ProjectId,
    pub tracker: TrackerKind,
    /// Where the tracker's CLI runs: the project's root.
    pub cwd: &'a Path,
    pub queue: &'a QueueSpec,
    pub reason: SyncReason,
    pub now: UnixMs,
}

/// What a sync did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub reason: SyncReason,
    pub account: TrackerAccount,
    pub issues: usize,
    /// The board's active sprints, when the queue names a board.
    pub sprints: Vec<Sprint>,
    pub changes: QueueChanges,
    pub read_at: UnixMs,
}

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncError {
    #[error(transparent)]
    Tracker(#[from] TrackerError),

    #[error(transparent)]
    Store(#[from] FlowStoreError),
}

/// Read the project's queue from the tracker and replace its mirror with what came back.
///
/// With a board, the queue is the board's active sprints, each searched with the project's query,
/// in sprint order; an issue in two of them is listed once, under the first. A board with no active
/// sprint falls back to the query alone, so the queue is not empty between sprints. Without a
/// board, the query alone is the queue.
///
/// # Errors
///
/// The tracker's error, after recording it in the mirror beside the last reading that worked; or
/// the mirror's, when even that could not be written.
pub fn sync_queue(
    tracker: &dyn Tracker,
    mirror: &dyn TrackerMirror,
    request: &SyncRequest<'_>,
) -> Result<SyncReport, SyncError> {
    let read = read_queue(tracker, request);
    let (account, reading) = match read {
        Ok(read) => read,
        Err(err) => {
            mirror.record_sync_failure(
                &request.project,
                request.tracker,
                &SyncFailure {
                    at: request.now,
                    message: err.to_string(),
                },
            )?;
            return Err(err.into());
        }
    };
    let before = mirror
        .mirrored_queue(&request.project)?
        .and_then(|queue| queue.reading)
        // A reading of another query is not "before" this one: everything in it would read as
        // leaving, and everything here as joining, when only the config changed.
        .filter(|before| before.query == reading.query && before.board == reading.board);
    let changes = QueueChanges::between(before.as_ref(), &reading);
    mirror.mirror_queue(&reading)?;
    Ok(SyncReport {
        reason: request.reason,
        account,
        issues: reading.issues.len(),
        sprints: reading.sprints,
        changes,
        read_at: reading.read_at,
    })
}

fn read_queue(
    tracker: &dyn Tracker,
    request: &SyncRequest<'_>,
) -> Result<(TrackerAccount, QueueReading), TrackerError> {
    let account = tracker.check(request.cwd)?;
    let limit = request.queue.limit();
    let query = request.queue.query.trim().to_owned();
    let search = |sprint: Option<SprintRef>| {
        tracker.search(
            request.cwd,
            &IssueQuery {
                query: query.clone(),
                sprint,
                limit,
            },
        )
    };

    let mut sprints = Vec::new();
    let mut issues: Vec<QueuedIssue> = Vec::new();
    if let Some(board) = request.queue.board {
        sprints = tracker.sprints(request.cwd, board, &[SprintState::Active])?;
        for sprint in &sprints {
            let found = search(Some(SprintRef {
                board,
                sprint: sprint.id,
            }))?;
            for issue in found {
                if issues.len() >= limit as usize {
                    break;
                }
                if !issues.iter().any(|queued| queued.issue.key == issue.key) {
                    issues.push(QueuedIssue {
                        issue,
                        sprint: Some(sprint.id),
                    });
                }
            }
        }
    }
    if sprints.is_empty() {
        issues = search(None)?
            .into_iter()
            .map(|issue| QueuedIssue {
                issue,
                sprint: None,
            })
            .collect();
    }

    Ok((
        account,
        QueueReading {
            project: request.project.clone(),
            tracker: request.tracker,
            query,
            board: request.queue.board,
            sprints,
            issues,
            read_at: request.now,
        },
    ))
}

/// Read one issue whole and keep it in the mirror.
///
/// Asked when an issue is opened in the Work view, which is an action; the queue's sync never reads
/// issues one by one, because each is a process and a request.
///
/// Returns who the tracker's CLI read it as, beside the issue.
///
/// # Errors
///
/// The tracker's, with nothing recorded; or the mirror's.
pub fn read_issue(
    tracker: &dyn Tracker,
    mirror: &dyn TrackerMirror,
    kind: TrackerKind,
    cwd: &Path,
    key: &str,
    fields: &BTreeMap<String, String>,
    now: UnixMs,
) -> Result<(TrackerAccount, IssueDetail), SyncError> {
    let account = tracker.check(cwd)?;
    let detail = tracker.issue(cwd, key, fields)?;
    mirror.mirror_issue(kind, &detail, now)?;
    Ok((account, detail))
}

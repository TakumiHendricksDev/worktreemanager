//! A tracker whose moves stay on this machine, for click-throughs of a debug build.
//!
//! # Why
//!
//! Start Work moves an issue, and a drop on the board moves one. Clicking those through against a
//! team's real tracker would move real issues that other people are looking at, and a sandbox
//! project is not always there to use instead. So a debug build started with
//! `WTM_DEV_TRACKER_WRITES=fake` keeps reading the real tracker — the sprint, its issues, one issue
//! whole — and keeps its moves to itself: each move is written to a file beside the app database
//! and laid over every read of that issue afterwards, so the queue, the board and the move's own
//! read-before-and-after see the issue where wtm put it, and the tracker sees nothing.
//!
//! A release build never reads the variable, so this cannot stand between a user and their
//! tracker.
//!
//! # What it cannot show
//!
//! A move someone else makes in the tracker meanwhile: the overlay wins over the real status for an
//! issue it moved. Stopping a move because the issue moved elsewhere is covered by tests against a
//! fake tracker, not by this.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use wtm_core::flow::{
    Issue, IssueDetail, IssueQuery, IssueStatus, Sprint, SprintState, StatusCategory,
    TrackerAccount, Transition,
};
use wtm_core::ports::{Tracker, TrackerError, TrackerWriter};

/// The variable that turns it on, in a debug build only.
pub const VARIABLE: &str = "WTM_DEV_TRACKER_WRITES";

/// The real tracker's reads, with this machine's moves laid over them.
pub struct DevTracker {
    real: Arc<dyn Tracker>,
    file: PathBuf,
    moved: Mutex<BTreeMap<String, IssueStatus>>,
    /// Each status name's category, as the real tracker has reported it, so a moved issue is
    /// tinted the way its column is.
    categories: Mutex<BTreeMap<String, StatusCategory>>,
}

impl std::fmt::Debug for DevTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevTracker").finish_non_exhaustive()
    }
}

impl DevTracker {
    /// The fake, when this is a debug build started with `WTM_DEV_TRACKER_WRITES=fake`. Its moves
    /// are kept in a file beside `flow_db`, so a relaunch of the same isolated build keeps them.
    #[must_use]
    pub fn from_env(real: Arc<dyn Tracker>, flow_db: &Path) -> Option<Arc<Self>> {
        if !cfg!(debug_assertions) || std::env::var(VARIABLE).ok()?.trim() != "fake" {
            return None;
        }
        let file = flow_db.with_file_name("dev-tracker-moves.json");
        let moved = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        tracing::warn!(
            file = %file.display(),
            "WTM_DEV_TRACKER_WRITES=fake: issue moves are kept on this machine, not sent"
        );
        Some(Arc::new(Self {
            real,
            file,
            moved: Mutex::new(moved),
            categories: Mutex::new(BTreeMap::new()),
        }))
    }

    fn overlay(&self, issue: &mut Issue) {
        self.categories
            .lock()
            .entry(issue.status.name.to_ascii_lowercase())
            .or_insert(issue.status.category);
        if let Some(status) = self.moved.lock().get(&issue.key) {
            issue.status = status.clone();
        }
    }
}

impl Tracker for DevTracker {
    fn check(&self, cwd: &Path) -> Result<TrackerAccount, TrackerError> {
        self.real.check(cwd)
    }

    fn search(&self, cwd: &Path, query: &IssueQuery) -> Result<Vec<Issue>, TrackerError> {
        let mut issues = self.real.search(cwd, query)?;
        for issue in &mut issues {
            self.overlay(issue);
        }
        Ok(issues)
    }

    fn issue(
        &self,
        cwd: &Path,
        key: &str,
        fields: &BTreeMap<String, String>,
    ) -> Result<IssueDetail, TrackerError> {
        let mut detail = self.real.issue(cwd, key, fields)?;
        self.overlay(&mut detail.issue);
        Ok(detail)
    }

    fn sprints(
        &self,
        cwd: &Path,
        board: u64,
        states: &[SprintState],
    ) -> Result<Vec<Sprint>, TrackerError> {
        self.real.sprints(cwd, board, states)
    }

    fn sprint(&self, cwd: &Path, id: u64) -> Result<Sprint, TrackerError> {
        self.real.sprint(cwd, id)
    }

    fn transitions(&self, cwd: &Path, key: &str) -> Result<Vec<Transition>, TrackerError> {
        self.real.transitions(cwd, key)
    }
}

impl TrackerWriter for DevTracker {
    fn move_to(&self, _cwd: &Path, key: &str, status: &str) -> Result<(), TrackerError> {
        let category = self
            .categories
            .lock()
            .get(&status.to_ascii_lowercase())
            .copied()
            .unwrap_or(StatusCategory::Unknown);
        let snapshot = {
            let mut moved = self.moved.lock();
            moved.insert(
                key.to_owned(),
                IssueStatus {
                    name: status.to_owned(),
                    category,
                },
            );
            moved.clone()
        };
        tracing::warn!(
            key,
            status,
            "WTM_DEV_TRACKER_WRITES=fake: moved on this machine only"
        );
        let text = serde_json::to_string_pretty(&snapshot).map_err(|e| TrackerError::Failed {
            argv: "dev tracker".to_owned(),
            message: e.to_string(),
        })?;
        std::fs::write(&self.file, text).map_err(|e| TrackerError::Failed {
            argv: format!("write {}", self.file.display()),
            message: e.to_string(),
        })
    }
}

//! Trackers whose writes stay on this machine, for click-throughs of a debug build.
//!
//! # Why
//!
//! Start Work moves an issue, a drop on the board moves one, and Send to Jira comments on one.
//! Clicking those through against a team's real tracker would change real issues that other people
//! are looking at, and a sandbox project is not always there to use instead. Two ways round it, both
//! for a debug build only — a release build never reads either variable, so neither can stand
//! between a user and their tracker:
//!
//! - **`WTM_DEV_TRACKER_WRITES=fake`** ([`DevTracker`]) keeps reading the real tracker — the sprint,
//!   its issues, one issue whole, its comments — and keeps its writes to itself. Each move is
//!   written to a file beside the app database and laid over every read of that issue afterwards,
//!   so the queue, the board and the move's own read-before-and-after see the issue where wtm put
//!   it; each comment is kept in another file and listed after the real ones, so Send to Jira's
//!   read-after finds it. The tracker sees nothing.
//! - **`WTM_DEV_TRACKER=<file.json>`** ([`FixtureTracker`]) reads no tracker at all: its sprints,
//!   issues and comments are a file, read again on every call, so editing the file is someone
//!   editing the issue in the tracker, and its moves and comments are written back into it. That is
//!   what a click-through needs when the issue has to be one written for the occasion — ambiguous on
//!   purpose so a planner asks — and later edited to make an approval stale.
//!
//! # What they cannot show
//!
//! With the overlay, a move someone else makes in the tracker meanwhile: the overlay wins over the
//! real status for an issue it moved. Stopping a move because the issue moved elsewhere is covered
//! by tests against a fake tracker, not by this.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use wtm_core::flow::{
    Issue, IssueDetail, IssueQuery, IssueStatus, Sprint, SprintState, StatusCategory,
    TrackerAccount, TrackerComment, Transition,
};
use wtm_core::ports::{Tracker, TrackerError, TrackerWriter};

/// The variable that turns on the overlay, in a debug build only.
pub const VARIABLE: &str = "WTM_DEV_TRACKER_WRITES";

/// The variable that names a fixture file to read instead of any tracker, in a debug build only.
pub const FIXTURE_VARIABLE: &str = "WTM_DEV_TRACKER";

/// Who a comment made here is said to be by.
const LOCAL_AUTHOR: &str = "You (on this machine)";

/// The real tracker's reads, with this machine's moves and comments laid over them.
pub struct DevTracker {
    real: Arc<dyn Tracker>,
    file: PathBuf,
    moved: Mutex<BTreeMap<String, IssueStatus>>,
    /// Each status name's category, as the real tracker has reported it, so a moved issue is
    /// tinted the way its column is.
    categories: Mutex<BTreeMap<String, StatusCategory>>,
    comments_file: PathBuf,
    /// Comments made here, by issue, listed after the real ones.
    comments: Mutex<BTreeMap<String, Vec<TrackerComment>>>,
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
        let comments_file = flow_db.with_file_name("dev-tracker-comments.json");
        let comments = std::fs::read_to_string(&comments_file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        tracing::warn!(
            file = %file.display(),
            "WTM_DEV_TRACKER_WRITES=fake: issue moves and comments are kept on this machine, not sent"
        );
        Some(Arc::new(Self {
            real,
            file,
            moved: Mutex::new(moved),
            categories: Mutex::new(BTreeMap::new()),
            comments_file,
            comments: Mutex::new(comments),
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

    fn comments(&self, cwd: &Path, key: &str) -> Result<Vec<TrackerComment>, TrackerError> {
        let mut comments = self.real.comments(cwd, key)?;
        comments.extend(self.comments.lock().get(key).cloned().unwrap_or_default());
        Ok(comments)
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

    fn comment(&self, _cwd: &Path, key: &str, body: &str) -> Result<(), TrackerError> {
        let snapshot = {
            let mut comments = self.comments.lock();
            let next = comments.values().map(Vec::len).sum::<usize>() + 1;
            comments
                .entry(key.to_owned())
                .or_default()
                .push(TrackerComment {
                    id: format!("local-{next}"),
                    author: Some(LOCAL_AUTHOR.to_owned()),
                    body: body.to_owned(),
                });
            comments.clone()
        };
        tracing::warn!(
            key,
            "WTM_DEV_TRACKER_WRITES=fake: commented on this machine only"
        );
        write_json(&self.comments_file, &snapshot)
    }
}

fn write_json<T: Serialize>(file: &Path, value: &T) -> Result<(), TrackerError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| TrackerError::Failed {
        argv: "dev tracker".to_owned(),
        message: e.to_string(),
    })?;
    std::fs::write(file, text).map_err(|e| TrackerError::Failed {
        argv: format!("write {}", file.display()),
        message: e.to_string(),
    })
}

/// One issue in a fixture file: the issue read whole, and the sprints it is in by id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureIssue {
    #[serde(flatten)]
    detail: IssueDetail,
    #[serde(default)]
    sprint_ids: Vec<u64>,
}

/// A fixture file: what [`FixtureTracker`] reads, and writes its moves and comments back into.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    #[serde(default)]
    site: String,
    #[serde(default)]
    sprints: Vec<Sprint>,
    #[serde(default)]
    issues: Vec<FixtureIssue>,
    #[serde(default)]
    comments: BTreeMap<String, Vec<TrackerComment>>,
}

/// A tracker that is a file. See the module docs.
pub struct FixtureTracker {
    file: PathBuf,
    /// Held across a write's read and write of the file, so a move and a comment at once both land.
    writing: Mutex<()>,
}

impl std::fmt::Debug for FixtureTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixtureTracker")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

impl FixtureTracker {
    /// The fixture tracker, when this is a debug build started with `WTM_DEV_TRACKER=<file>`.
    #[must_use]
    pub fn from_env() -> Option<Arc<Self>> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let file = PathBuf::from(std::env::var_os(FIXTURE_VARIABLE)?);
        tracing::warn!(
            file = %file.display(),
            "WTM_DEV_TRACKER: reading issues from a fixture file; no tracker is read or written"
        );
        Some(Arc::new(Self::at(file)))
    }

    #[must_use]
    pub fn at(file: PathBuf) -> Self {
        Self {
            file,
            writing: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<Fixture, TrackerError> {
        let text = std::fs::read_to_string(&self.file).map_err(|e| TrackerError::Failed {
            argv: format!("read {}", self.file.display()),
            message: e.to_string(),
        })?;
        serde_json::from_str(&text).map_err(|e| TrackerError::Unreadable {
            what: self.file.display().to_string(),
            message: e.to_string(),
        })
    }

    fn found(fixture: &Fixture, key: &str) -> Result<FixtureIssue, TrackerError> {
        fixture
            .issues
            .iter()
            .find(|i| i.detail.issue.key == key)
            .cloned()
            .ok_or_else(|| TrackerError::NotFound {
                what: format!("issue {key}"),
            })
    }
}

impl Tracker for FixtureTracker {
    fn check(&self, _cwd: &Path) -> Result<TrackerAccount, TrackerError> {
        let fixture = self.read()?;
        Ok(TrackerAccount {
            site: if fixture.site.is_empty() {
                "fixture.invalid".to_owned()
            } else {
                fixture.site
            },
            login: Some("you@fixture.invalid".to_owned()),
            version: Some("fixture".to_owned()),
        })
    }

    fn search(&self, _cwd: &Path, query: &IssueQuery) -> Result<Vec<Issue>, TrackerError> {
        Ok(self
            .read()?
            .issues
            .into_iter()
            .filter(|i| {
                query
                    .sprint
                    .is_none_or(|s| i.sprint_ids.contains(&s.sprint))
            })
            .map(|i| i.detail.issue)
            .take(query.limit as usize)
            .collect())
    }

    fn issue(
        &self,
        _cwd: &Path,
        key: &str,
        _fields: &BTreeMap<String, String>,
    ) -> Result<IssueDetail, TrackerError> {
        let fixture = self.read()?;
        let found = Self::found(&fixture, key)?;
        let mut detail = found.detail;
        detail.sprints = fixture
            .sprints
            .into_iter()
            .filter(|s| found.sprint_ids.contains(&s.id))
            .collect();
        Ok(detail)
    }

    fn sprints(
        &self,
        _cwd: &Path,
        board: u64,
        states: &[SprintState],
    ) -> Result<Vec<Sprint>, TrackerError> {
        Ok(self
            .read()?
            .sprints
            .into_iter()
            .filter(|s| s.board == Some(board) && states.contains(&s.state))
            .collect())
    }

    fn sprint(&self, _cwd: &Path, id: u64) -> Result<Sprint, TrackerError> {
        self.read()?
            .sprints
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| TrackerError::NotFound {
                what: format!("sprint {id}"),
            })
    }

    fn transitions(&self, _cwd: &Path, _key: &str) -> Result<Vec<Transition>, TrackerError> {
        Ok(Vec::new())
    }

    fn comments(&self, _cwd: &Path, key: &str) -> Result<Vec<TrackerComment>, TrackerError> {
        let fixture = self.read()?;
        Self::found(&fixture, key)?;
        Ok(fixture.comments.get(key).cloned().unwrap_or_default())
    }
}

impl TrackerWriter for FixtureTracker {
    fn move_to(&self, _cwd: &Path, key: &str, status: &str) -> Result<(), TrackerError> {
        let _one = self.writing.lock();
        let mut fixture = self.read()?;
        let category = fixture
            .issues
            .iter()
            .map(|i| &i.detail.issue.status)
            .find(|s| s.name.eq_ignore_ascii_case(status))
            .map_or(StatusCategory::Unknown, |s| s.category);
        let issue = fixture
            .issues
            .iter_mut()
            .find(|i| i.detail.issue.key == key)
            .ok_or_else(|| TrackerError::NotFound {
                what: format!("issue {key}"),
            })?;
        issue.detail.issue.status = IssueStatus {
            name: status.to_owned(),
            category,
        };
        write_json(&self.file, &fixture)
    }

    fn comment(&self, _cwd: &Path, key: &str, body: &str) -> Result<(), TrackerError> {
        let _one = self.writing.lock();
        let mut fixture = self.read()?;
        Self::found(&fixture, key)?;
        let next = fixture.comments.values().map(Vec::len).sum::<usize>() + 1;
        fixture
            .comments
            .entry(key.to_owned())
            .or_default()
            .push(TrackerComment {
                id: format!("{}", 10_000 + next),
                author: Some(LOCAL_AUTHOR.to_owned()),
                body: body.to_owned(),
            });
        tracing::warn!(key, "WTM_DEV_TRACKER: commented in the fixture file only");
        write_json(&self.file, &fixture)
    }
}

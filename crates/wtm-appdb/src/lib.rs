//! The app's own database: work items, flows and their history, review rules and runs, the inbox,
//! approvals with the outbox's operations, local fixes with the leases that hold their worktrees,
//! and the mirror of what a project's tracker last said, in SQLite.
//!
//! # Why this name
//!
//! `wtm-db` is the Database viewer, which connects to *your project's* databases, and
//! `wtm-config::store` is the TOML config store. This crate is the one database that belongs to the
//! app itself. ARCHITECTURE §9 records why the app has one at all when everything else it keeps is
//! a file.
//!
//! # Where it lives
//!
//! `AppPaths::flow_db`, which is `~/.local/share/wtm/flow.db`: the data directory, because this is
//! state the app writes, and never the config directory, which is often somebody's dotfiles.
//!
//! # How it is opened
//!
//! - **WAL.** A crash mid-transaction leaves the last commit intact, and a reader in another
//!   process, such as `sqlite3 flow.db` while the app runs, never blocks the app.
//! - **`synchronous = FULL`**, not the `NORMAL` WAL usually gets. `NORMAL` can lose the last few
//!   commits to a power cut, and the outbox depends on a write being marked as started *before* it
//!   reaches the remote. Writes here happen at the pace of clicks and agent turns, so the extra
//!   sync costs nothing anyone could notice.
//! - **Foreign keys on**, which SQLite leaves off unless asked, per connection.
//!
//! # One connection
//!
//! The store holds one connection behind a mutex, so every port call is one serialized
//! transaction and there is no pool to tune. Flows change at the pace of clicks and agent turns;
//! nothing here needs concurrent writers.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod findings;
mod fixes;
mod inbox;
mod migrate;
mod outbox;
mod pull_reviews;
mod rules;
mod runs;
mod store;
mod tracker;

use std::path::{Path, PathBuf};
use std::time::Duration;

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::migrate::{MIGRATIONS, Migration};

/// How long a write waits for another process holding the database, such as a second copy of the
/// app, before giving up. Long enough to ride out a migration, short enough to report a stuck lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum AppDbError {
    #[error("cannot open {}: {message}", path.display())]
    Open { path: PathBuf, message: String },

    #[error("cannot back up {} before migrating it, so it was not migrated: {message}", path.display())]
    Backup { path: PathBuf, message: String },

    #[error("migration {version} ({name}) failed and was rolled back: {message}")]
    Migration {
        version: u32,
        name: &'static str,
        message: String,
    },

    #[error(
        "{} was written by a newer wtm (schema version {found}; this build knows up to {known})",
        path.display()
    )]
    NewerSchema {
        path: PathBuf,
        found: u32,
        known: u32,
    },
}

/// The open database. Implements [`FlowStore`](wtm_core::ports::FlowStore),
/// [`RuleStore`](wtm_core::ports::RuleStore), [`RunStore`](wtm_core::ports::RunStore),
/// [`PullReviewStore`](wtm_core::ports::PullReviewStore), [`InboxStore`](wtm_core::ports::InboxStore),
/// [`OutboxStore`](wtm_core::ports::OutboxStore), [`LeaseStore`](wtm_core::ports::LeaseStore),
/// [`FixStore`](wtm_core::ports::FixStore) and [`TrackerMirror`](wtm_core::ports::TrackerMirror).
#[derive(Debug)]
pub struct AppDb {
    conn: Mutex<Connection>,
}

impl AppDb {
    /// Open the database at `path`, creating it and its directory if needed, and migrate it to the
    /// current schema, backing it up first if it already has one.
    pub fn open(path: &Path) -> Result<Self, AppDbError> {
        Self::open_with(path, MIGRATIONS)
    }

    pub(crate) fn open_with(path: &Path, migrations: &[Migration]) -> Result<Self, AppDbError> {
        let failed = |message: String| AppDbError::Open {
            path: path.to_path_buf(),
            message,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| failed(e.to_string()))?;
        }
        let mut conn = Connection::open(path).map_err(|e| failed(e.to_string()))?;
        configure(&conn).map_err(failed)?;
        migrate::migrate(&mut conn, path, migrations)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_connection<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        f(&self.conn.lock())
    }
}

fn configure(conn: &Connection) -> Result<(), String> {
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(|e| e.to_string())?;
    // SQLite answers with the mode it actually chose rather than failing, for instance on a
    // filesystem that cannot hold the shared-memory file WAL needs.
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(format!(
            "the journal mode is {mode}, and this store needs WAL"
        ));
    }
    conn.pragma_update(None, "synchronous", "FULL")
        .and_then(|()| conn.pragma_update(None, "foreign_keys", true))
        .and_then(|()| conn.busy_timeout(BUSY_TIMEOUT))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_connection_enforces_foreign_keys() {
        // SQLite leaves them off unless each connection asks, and an event for a flow that does
        // not exist is an orphan nothing could read back.
        let dir = tempfile::tempdir().unwrap();
        let db = AppDb::open(&dir.path().join("flow.db")).unwrap();

        let orphan = db.with_connection(|conn| {
            conn.execute(
                "INSERT INTO flow_events (flow_id, seq, at, event) VALUES (404, 1, 0, '{}')",
                [],
            )
        });

        let message = orphan.unwrap_err().to_string();
        assert!(message.contains("FOREIGN KEY"), "{message}");
    }

    #[test]
    fn writes_are_synchronous_in_full() {
        let dir = tempfile::tempdir().unwrap();
        let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
        // 2 is FULL. See the module docs for why the outbox needs it.
        let level: i64 = db
            .with_connection(|conn| conn.pragma_query_value(None, "synchronous", |row| row.get(0)))
            .unwrap();
        assert_eq!(level, 2);
    }
}

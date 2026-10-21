//! The app's database, opened the first time something needs it.
//!
//! # Why lazily
//!
//! Opening is not only a read: it creates `flow.db`, runs its migrations and, before migrating a
//! database that already has a schema, copies it aside. That belongs on the path of the first thing
//! that reads a flow or a rule, not on every launch. Somebody who never turns the Work view on
//! should not find a database in their data directory, and a launch should not wait on a migration
//! for a view that is not showing.
//!
//! # One handle for every port
//!
//! The database implements both `FlowStore` and `RuleStore`, and both are served from the one
//! `Arc<AppDb>` here. Two handles would be two connections to one file and, on a first launch, two
//! migrations racing at it.
//!
//! # Why a failure is not remembered
//!
//! A database that cannot be opened, because the disk is full or another copy of the app holds a
//! lock past the busy timeout, may open on the next try. Caching the error would make every later
//! call fail until a relaunch for a cause that has already gone.

use std::path::PathBuf;
use std::sync::Arc;

use wtm_appdb::{AppDb, AppDbError};

use crate::view::ErrorView;

#[derive(Debug)]
pub struct Handle {
    path: PathBuf,
    db: parking_lot::Mutex<Option<Arc<AppDb>>>,
}

impl Handle {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            db: parking_lot::Mutex::new(None),
        }
    }

    /// The open database, opening it now if this is the first call to succeed.
    ///
    /// The lock is held across the open on purpose, so two commands arriving together migrate the
    /// file once rather than racing two migrations at it.
    pub fn open(&self) -> Result<Arc<AppDb>, AppDbError> {
        let mut slot = self.db.lock();
        if let Some(db) = slot.as_ref() {
            return Ok(Arc::clone(db));
        }
        let db = Arc::new(AppDb::open(&self.path).inspect_err(|error| {
            tracing::warn!(%error, "cannot open the app database");
        })?);
        *slot = Some(Arc::clone(&db));
        Ok(db)
    }

    /// [`Self::open`], with a failure as the window shows one.
    pub fn get(&self) -> Result<Arc<AppDb>, ErrorView> {
        self.open()
            .map_err(|error| ErrorView::new("appdb", error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_database_is_opened_once_and_shared_by_every_later_call() {
        let dir = tempfile::tempdir().unwrap();
        let handle = Handle::new(dir.path().join("data/flow.db"));

        let first = handle.get().unwrap();
        let second = handle.get().unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert!(dir.path().join("data/flow.db").exists());
    }

    #[test]
    fn a_database_that_failed_to_open_is_tried_again_next_time() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the data directory should be, so the first open cannot create it.
        let blocked = dir.path().join("data");
        std::fs::write(&blocked, "not a directory").unwrap();
        let handle = Handle::new(blocked.join("flow.db"));

        assert_eq!(handle.get().unwrap_err().kind, "appdb");

        std::fs::remove_file(&blocked).unwrap();
        assert!(handle.get().is_ok());
    }
}

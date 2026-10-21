//! Numbered schema migrations, each preceded by a backup copy.
//!
//! # The version lives in the file
//!
//! `PRAGMA user_version` records which migrations a database has had, in the database header,
//! so there is no bookkeeping table to keep in step. It is set inside the migration's own
//! transaction, which makes a migration and its number commit together or not at all.
//!
//! # A copy before each migration
//!
//! A migration that fails rolls back. One that succeeds and is wrong, say by dropping a column a
//! later build turns out to need, cannot be undone from the database alone. So before each
//! migration of a database that already holds a schema, its contents are copied to
//! `<name>.v<N>.bak` beside it, where N is the version being migrated from.
//!
//! The copy is `VACUUM INTO`, not a file copy, because in WAL mode the main file alone is not the
//! database: committed pages can still be waiting in the `-wal` file. It is written under a
//! temporary name and renamed into place, so a crash mid-copy never leaves a truncated file under
//! the name someone would restore from. A brand-new database is not copied, since there is nothing
//! in it to lose.
//!
//! # A newer database is not opened
//!
//! A version above every migration this build knows means a newer wtm wrote the file, and this
//! build cannot know what that wtm's tables mean. Writing to it anyway could break data the newer
//! build relies on, so opening fails and says why.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};

use crate::AppDbError;

#[derive(Clone, Copy)]
pub(crate) struct Migration {
    pub(crate) version: u32,
    pub(crate) name: &'static str,
    pub(crate) sql: &'static str,
}

/// Every migration, in order. Append only: a shipped migration has already run on someone's
/// database, so editing it would leave theirs different from a fresh one.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "work items and flows",
        sql: include_str!("../migrations/0001_work_items_and_flows.sql"),
    },
    Migration {
        version: 2,
        name: "rules",
        sql: include_str!("../migrations/0002_rules.sql"),
    },
    Migration {
        version: 3,
        name: "review runs",
        sql: include_str!("../migrations/0003_review_runs.sql"),
    },
];

/// Bring the database at `path`, open as `conn`, up to the last of `migrations`.
pub(crate) fn migrate(
    conn: &mut Connection,
    path: &Path,
    migrations: &[Migration],
) -> Result<(), AppDbError> {
    let mut version: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|e| AppDbError::Open {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
    let known = migrations.last().map_or(0, |migration| migration.version);
    if version > known {
        return Err(AppDbError::NewerSchema {
            path: path.to_path_buf(),
            found: version,
            known,
        });
    }

    let found = version;
    for migration in migrations.iter().filter(|m| m.version > found) {
        // `found`, not `version`: a database created by this open gains its schema from the first
        // migration, and copying it again before each later one would only copy empty tables.
        if found > 0 {
            let copy = backup(conn, path, version)?;
            tracing::info!(backup = %copy.display(), from = version, "backed up the app database");
        }
        apply(conn, migration).map_err(|e| AppDbError::Migration {
            version: migration.version,
            name: migration.name,
            message: e.to_string(),
        })?;
        tracing::info!(
            version = migration.version,
            name = migration.name,
            "migrated the app database"
        );
        version = migration.version;
    }
    Ok(())
}

fn apply(conn: &mut Connection, migration: &Migration) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
    tx.execute_batch(migration.sql)?;
    tx.pragma_update(None, "user_version", migration.version)?;
    tx.commit()
}

/// Where the copy taken before migrating from `version` goes.
pub(crate) fn backup_path(path: &Path, version: u32) -> PathBuf {
    with_suffix(path, &format!(".v{version}.bak"))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn backup(conn: &Connection, path: &Path, version: u32) -> Result<PathBuf, AppDbError> {
    let failed = |message: String| AppDbError::Backup {
        path: path.to_path_buf(),
        message,
    };
    let target = backup_path(path, version);
    let partial = with_suffix(&target, ".partial");

    // `VACUUM INTO` refuses an existing file, and a leftover partial copy is worthless anyway.
    match std::fs::remove_file(&partial) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(failed(e.to_string())),
    }
    let Some(partial_name) = partial.to_str() else {
        return Err(failed(format!(
            "{} is not valid UTF-8, which SQLite needs for a file name",
            partial.display()
        )));
    };
    conn.execute("VACUUM INTO ?1", [partial_name])
        .map_err(|e| failed(e.to_string()))?;
    std::fs::rename(&partial, &target).map_err(|e| failed(e.to_string()))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppDb;

    // The tests below build a version-1 database and migrate it with their own lists, so they keep
    // proving the backup and rollback rules however many migrations ship.
    const ADD_NOTES: Migration = Migration {
        version: 2,
        name: "notes",
        sql: "CREATE TABLE notes (body TEXT NOT NULL);",
    };

    const BROKEN: Migration = Migration {
        version: 2,
        name: "broken",
        sql: "CREATE TABLE half_done (x INTEGER); THIS IS NOT SQL;",
    };

    fn version_of(path: &Path) -> u32 {
        Connection::open(path)
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    fn has_table(path: &Path, table: &str) -> bool {
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }

    fn work_items_in(path: &Path) -> i64 {
        Connection::open(path)
            .unwrap()
            .query_row("SELECT count(*) FROM work_items", [], |row| row.get(0))
            .unwrap()
    }

    /// A version-1 database holding one work item, closed again.
    fn a_version_one_database(dir: &Path) -> PathBuf {
        let path = dir.join("flow.db");
        let db = AppDb::open_with(&path, &MIGRATIONS[..1]).unwrap();
        db.with_connection(|conn| {
            conn.execute(
                "INSERT INTO work_items (project, title, created_at) VALUES ('/work/webapp', 'Export', 1)",
                [],
            )
        })
        .unwrap();
        drop(db);
        path
    }

    fn reopen(path: &Path, migrations: &[Migration]) -> Result<AppDb, AppDbError> {
        AppDb::open_with(path, migrations)
    }

    #[test]
    fn the_migrations_are_numbered_from_one_without_gaps() {
        // A gap or a repeat would make `user_version` skip a migration on some databases and not
        // on others.
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                usize::try_from(migration.version).unwrap(),
                index + 1,
                "{}",
                migration.name
            );
        }
    }

    #[test]
    fn a_new_database_is_migrated_without_a_backup_because_there_is_nothing_to_lose() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/data/flow.db");

        AppDb::open(&path).unwrap();

        assert_eq!(
            version_of(&path),
            MIGRATIONS.last().unwrap().version,
            "every migration ran"
        );
        assert!(!backup_path(&path, 0).exists());
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.contains(".bak"))
            .collect();
        assert!(entries.is_empty(), "{entries:?}");
    }

    #[test]
    fn an_existing_database_is_copied_before_each_migration_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = a_version_one_database(dir.path());

        reopen(&path, &[MIGRATIONS[0], ADD_NOTES]).unwrap();

        let copy = backup_path(&path, 1);
        assert_eq!(copy.file_name().unwrap(), "flow.db.v1.bak");
        assert_eq!(
            version_of(&copy),
            1,
            "the copy is of the database before migrating"
        );
        assert!(!has_table(&copy, "notes"));
        assert_eq!(work_items_in(&copy), 1, "the copy holds the data");

        assert_eq!(version_of(&path), 2);
        assert!(has_table(&path, "notes"));
        assert!(!with_suffix(&copy, ".partial").exists());
    }

    #[test]
    fn a_failed_migration_leaves_the_database_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = a_version_one_database(dir.path());

        let error = reopen(&path, &[MIGRATIONS[0], BROKEN]).unwrap_err();

        assert!(
            matches!(error, AppDbError::Migration { version: 2, .. }),
            "{error}"
        );
        assert_eq!(version_of(&path), 1);
        assert!(
            !has_table(&path, "half_done"),
            "the migration rolled back whole"
        );
        assert_eq!(work_items_in(&path), 1);
        assert!(backup_path(&path, 1).exists());
    }

    #[test]
    fn reopening_an_up_to_date_database_takes_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flow.db");
        drop(AppDb::open(&path).unwrap());

        AppDb::open(&path).unwrap();

        let current = MIGRATIONS.last().unwrap().version;
        assert!(!backup_path(&path, current).exists());
    }

    #[test]
    fn a_version_one_database_gains_the_rule_and_review_run_tables_and_keeps_its_work_items() {
        let dir = tempfile::tempdir().unwrap();
        let path = a_version_one_database(dir.path());

        AppDb::open(&path).unwrap();

        assert!(has_table(&path, "rules"));
        assert!(has_table(&path, "rule_revisions"));
        assert!(has_table(&path, "rule_applications"));
        for table in [
            "review_runs",
            "snapshots",
            "steps",
            "invocations",
            "inbox_items",
        ] {
            assert!(has_table(&path, table), "{table}");
        }
        assert_eq!(work_items_in(&path), 1);
        assert_eq!(work_items_in(&backup_path(&path, 1)), 1);
    }

    #[test]
    fn a_database_from_a_newer_wtm_is_refused_rather_than_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = a_version_one_database(dir.path());
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();

        let error = AppDb::open(&path).unwrap_err();

        let newest = MIGRATIONS.last().unwrap().version;
        assert!(
            matches!(
                error,
                AppDbError::NewerSchema {
                    found: 99,
                    known,
                    ..
                } if known == newest
            ),
            "{error}"
        );
        assert_eq!(version_of(&path), 99);
    }

    #[test]
    fn a_second_migration_replaces_a_stale_backup_of_the_same_version() {
        // A migration that failed once and is retried later must back up the database as it is
        // now, not keep the copy from the first attempt.
        let dir = tempfile::tempdir().unwrap();
        let path = a_version_one_database(dir.path());
        reopen(&path, &[MIGRATIONS[0], BROKEN]).unwrap_err();

        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO work_items (project, title, created_at) VALUES ('/work/webapp', 'Later', 2)",
                [],
            )
            .unwrap();
        reopen(&path, &[MIGRATIONS[0], ADD_NOTES]).unwrap();

        assert_eq!(work_items_in(&backup_path(&path, 1)), 2);
    }
}

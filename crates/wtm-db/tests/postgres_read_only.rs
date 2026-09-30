//! The read-only guarantee against a real Postgres server.
//!
//! The unit tests prove `QueryMode::ReadOnly` against an in-process database, where the check is
//! the engine's own answer to "does this statement write". Postgres is guarded differently —
//! `prepare` to prove there is one statement, then a read-only transaction the server enforces —
//! and the attacks that matter are the ones only a real server answers: `COMMIT;` to leave the
//! transaction, and turning the session's read-only setting off before writing. So this runs
//! against one.
//!
//! `#[ignore]`d because it needs a server, and it creates and drops a table of its own
//! (`wtm_read_only_probe_<uuid>`, dropped by a guard even if an assertion panics). Point it at a
//! scratch database, never one whose contents matter:
//!
//! ```sh
//! docker run --rm -d --name wtm-probe -e POSTGRES_PASSWORD=probe -p 55439:5432 postgres:16-alpine
//! WTM_TEST_POSTGRES_URL=postgres://postgres:probe@127.0.0.1:55439/postgres \
//!   cargo test -p wtm-db --test postgres_read_only -- --ignored
//! ```
//!
//! An unset variable skips rather than fails, so `--ignored` on a fresh clone is quiet, not red.

#![allow(clippy::unwrap_used, clippy::print_stderr)]

use std::sync::Arc;

use wtm_core::error::DatabaseError;
use wtm_core::model::{
    DatabaseAccess, DatabaseEngine, DatabaseEnvironment, DatabaseScope, DatabaseTls,
};
use wtm_core::ports::database::{DatabaseConnection, DatabaseHost, QueryMode, TablePageRequest};
use wtm_db::Host;
use wtm_testkit::FakeClock;

fn url() -> Option<String> {
    match std::env::var("WTM_TEST_POSTGRES_URL") {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => {
            eprintln!("skipping: set WTM_TEST_POSTGRES_URL to a scratch database");
            None
        }
    }
}

/// Drops the probe table however the test ends.
struct Probe<'a> {
    host: &'a Host,
    session: String,
    table: String,
}

impl Drop for Probe<'_> {
    fn drop(&mut self) {
        let _ = self.host.query(
            &self.session,
            &format!("DROP TABLE IF EXISTS {}", self.table),
            1,
            QueryMode::Normal,
        );
    }
}

impl Probe<'_> {
    fn run(&self, sql: &str, mode: QueryMode) -> Result<Vec<Vec<Option<String>>>, DatabaseError> {
        self.host
            .query(&self.session, &sql.replace("probe", &self.table), 100, mode)
            .map(|result| {
                result
                    .rows
                    .into_iter()
                    .map(|row| row.into_iter().map(|cell| cell.value).collect())
                    .collect()
            })
    }

    fn count(&self) -> String {
        self.run("SELECT count(*) FROM probe", QueryMode::Normal)
            .unwrap()[0][0]
            .clone()
            .unwrap()
    }

    fn page(&self, filter: &str, order_by: &str) -> Result<usize, DatabaseError> {
        self.host
            .table_page(
                &self.session,
                &TablePageRequest {
                    schema: "public".to_owned(),
                    table: self.table.clone(),
                    offset: 0,
                    limit: 100,
                    filter: Some(filter.to_owned()),
                    order_by: Some(order_by.to_owned()),
                },
            )
            .map(|result| result.rows.len())
    }
}

#[test]
#[ignore = "needs WTM_TEST_POSTGRES_URL pointing at a scratch database"]
fn a_read_only_run_cannot_write_to_a_real_postgres_server() {
    let Some(url) = url() else { return };
    let host = Host::new(Arc::new(FakeClock::new()));
    let session = host
        .connect(DatabaseConnection {
            profile_id: "probe".to_owned(),
            label: "Probe".to_owned(),
            engine: DatabaseEngine::Postgres,
            scope: DatabaseScope::Worktree,
            environment: DatabaseEnvironment::Local,
            access: DatabaseAccess::ReadWrite,
            url: Some(url),
            host: None,
            port: None,
            name: None,
            user: None,
            password: None,
            path: None,
            tls: DatabaseTls::Disable,
        })
        .unwrap()
        .id;
    let probe = Probe {
        host: &host,
        // Unique per run, so two runs against one database never share a table.
        table: format!("wtm_read_only_probe_{}", uuid::Uuid::new_v4().simple()),
        session,
    };
    probe
        .run(
            "CREATE TABLE probe (id int PRIMARY KEY, name text); \
             INSERT INTO probe VALUES (1, 'Ada'), (2, 'Grace'), (3, 'Linus')",
            QueryMode::Normal,
        )
        .unwrap();

    // Reading works, trailing semicolon and all.
    let rows = probe
        .run("SELECT name FROM probe WHERE id = 2;", QueryMode::ReadOnly)
        .unwrap();
    assert_eq!(rows, [[Some("Grace".to_owned())]]);

    for attack in [
        "DELETE FROM probe",
        "UPDATE probe SET name = 'x'",
        // Leave the read-only transaction, then write outside it.
        "COMMIT; DELETE FROM probe",
        // Turn the session's own read-only setting off first — what defeats a read-only profile.
        "SET default_transaction_read_only = off; DELETE FROM probe",
        "SET TRANSACTION READ WRITE; DELETE FROM probe",
        // A data-modifying CTE is one statement, so only the transaction can stop it.
        "WITH gone AS (DELETE FROM probe RETURNING id) SELECT count(*) FROM gone",
    ] {
        let outcome = probe.run(attack, QueryMode::ReadOnly);
        assert!(outcome.is_err(), "{attack} was allowed: {outcome:?}");
        assert_eq!(probe.count(), "3", "{attack} changed the table");
    }

    // The same attacks through a table page's WHERE and ORDER BY.
    assert_eq!(probe.page("name <> 'Linus';", "id DESC").unwrap(), 2);
    for (filter, order_by) in [
        ("1 = 1; DELETE FROM probe", "id"),
        ("true", "id; DELETE FROM probe"),
        ("true", "id; COMMIT; DELETE FROM probe"),
    ] {
        let outcome = probe.page(filter, order_by);
        assert!(outcome.is_err(), "{filter} / {order_by} was allowed");
        assert_eq!(
            probe.count(),
            "3",
            "{filter} / {order_by} changed the table"
        );
    }
    // A `--` comment ends at its line, so it cannot comment out the page's LIMIT.
    assert_eq!(probe.page("true -- every row", "id --").unwrap(), 3);
}

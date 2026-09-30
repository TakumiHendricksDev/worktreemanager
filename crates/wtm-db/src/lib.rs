//! Concrete database sessions.
//!
//! Connections stay in Rust and only structured metadata or explicitly requested result cells cross
//! IPC. In particular, neither a password nor a credential-bearing URL is logged or serialized.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use native_tls::TlsConnector;
use parking_lot::Mutex;
use postgres::{Client, Config, NoTls, SimpleQueryMessage};
use postgres_native_tls::MakeTlsConnector;
use rusqlite::{Connection as SqliteConnection, OpenFlags, types::ValueRef};
use uuid::Uuid;
use wtm_core::error::DatabaseError;
use wtm_core::model::{DatabaseAccess, DatabaseEngine, DatabaseTls};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::database::{
    DatabaseColumn, DatabaseConnection, DatabaseHost, DatabaseRelation, DatabaseSchema,
    DatabaseSession, QueryCell, QueryColumn, QueryMode, QueryResult, RelationKind,
    TablePageRequest,
};

const MAX_PAGE_ROWS: u32 = 500;
const MAX_CELL_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;

/// Whether this build contains a concrete adapter for `engine`.
#[must_use]
pub const fn supports(engine: DatabaseEngine) -> bool {
    matches!(engine, DatabaseEngine::Postgres | DatabaseEngine::Sqlite)
}

struct PostgresSession {
    client: Mutex<Client>,
    cancel: postgres::CancelToken,
    tls: DatabaseTls,
}

struct SqliteSession {
    client: Mutex<SqliteConnection>,
    interrupt: rusqlite::InterruptHandle,
}

enum Connection {
    Postgres(Box<PostgresSession>),
    Sqlite(SqliteSession),
}

struct Entry {
    connection: Connection,
}

/// The live connection registry.
pub struct Host {
    clock: Arc<dyn Clock>,
    sessions: Mutex<BTreeMap<String, Arc<Entry>>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("sessions", &self.sessions.lock().len())
            .finish_non_exhaustive()
    }
}

impl Host {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    fn entry(&self, session: &str) -> Result<Arc<Entry>, DatabaseError> {
        self.sessions
            .lock()
            .get(session)
            .cloned()
            .ok_or_else(|| DatabaseError::NoSuchSession(session.to_owned()))
    }
}

impl DatabaseHost for Host {
    fn connect(&self, connection: DatabaseConnection) -> Result<DatabaseSession, DatabaseError> {
        let (driver, server_version) = match connection.engine {
            DatabaseEngine::Postgres => {
                let mut config = postgres_config(&connection)?;
                let (mut client, cancel) = connect_postgres(&mut config, connection.tls)?;
                if connection.access == DatabaseAccess::ReadOnly {
                    client
                        .batch_execute("SET default_transaction_read_only = on")
                        .map_err(|error| postgres_query_error(&error))?;
                }
                let server_version = client
                    .query_one("SHOW server_version", &[])
                    .ok()
                    .map(|row| row.get::<_, String>(0));
                (
                    Connection::Postgres(Box::new(PostgresSession {
                        client: Mutex::new(client),
                        cancel,
                        tls: connection.tls,
                    })),
                    server_version,
                )
            }
            DatabaseEngine::Sqlite => {
                let path = sqlite_path(&connection)?;
                let flags = if connection.access == DatabaseAccess::ReadOnly {
                    OpenFlags::SQLITE_OPEN_READ_ONLY
                } else {
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
                };
                let client = SqliteConnection::open_with_flags(path, flags)
                    .map_err(|error| sqlite_connection_error(&error))?;
                let interrupt = client.get_interrupt_handle();
                (
                    Connection::Sqlite(SqliteSession {
                        client: Mutex::new(client),
                        interrupt,
                    }),
                    Some(rusqlite::version().to_owned()),
                )
            }
            DatabaseEngine::Mysql => {
                return Err(DatabaseError::UnsupportedEngine("mysql".to_owned()));
            }
        };

        let facts = DatabaseSession {
            id: Uuid::new_v4().to_string(),
            profile_id: connection.profile_id,
            label: connection.label,
            engine: connection.engine,
            environment: connection.environment,
            access: connection.access,
            server_version,
        };
        self.sessions
            .lock()
            .insert(facts.id.clone(), Arc::new(Entry { connection: driver }));
        Ok(facts)
    }

    fn disconnect(&self, session: &str) -> Result<(), DatabaseError> {
        self.sessions
            .lock()
            .remove(session)
            .map(|_| ())
            .ok_or_else(|| DatabaseError::NoSuchSession(session.to_owned()))
    }

    fn schemas(&self, session: &str) -> Result<Vec<DatabaseSchema>, DatabaseError> {
        let entry = self.entry(session)?;
        match &entry.connection {
            Connection::Postgres(postgres) => postgres
                .client
                .lock()
                .query(
                    "SELECT nspname FROM pg_catalog.pg_namespace \
                     WHERE nspname NOT LIKE 'pg\\_%' ESCAPE '\\' \
                     AND nspname <> 'information_schema' ORDER BY nspname",
                    &[],
                )
                .map_err(|error| postgres_query_error(&error))
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| DatabaseSchema { name: row.get(0) })
                        .collect()
                }),
            Connection::Sqlite(sqlite) => sqlite_schemas(&sqlite.client.lock()),
        }
    }

    fn relations(
        &self,
        session: &str,
        schema: &str,
    ) -> Result<Vec<DatabaseRelation>, DatabaseError> {
        let entry = self.entry(session)?;
        match &entry.connection {
            Connection::Postgres(postgres) => postgres
                .client
                .lock()
                .query(
                    "SELECT n.nspname, c.relname, c.relkind::text \
                     FROM pg_catalog.pg_class c \
                     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm') \
                     ORDER BY c.relname",
                    &[&schema],
                )
                .map_err(|error| postgres_query_error(&error))
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| {
                            let kind: String = row.get(2);
                            DatabaseRelation {
                                schema: row.get(0),
                                name: row.get(1),
                                kind: match kind.as_str() {
                                    "v" => RelationKind::View,
                                    "m" => RelationKind::MaterializedView,
                                    _ => RelationKind::Table,
                                },
                            }
                        })
                        .collect()
                }),
            Connection::Sqlite(sqlite) => sqlite_relations(&sqlite.client.lock(), schema),
        }
    }

    fn columns(
        &self,
        session: &str,
        schema: &str,
        relation: &str,
    ) -> Result<Vec<DatabaseColumn>, DatabaseError> {
        let entry = self.entry(session)?;
        match &entry.connection {
            Connection::Postgres(postgres) => postgres
                .client
                .lock()
                .query(
                    "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                            NOT a.attnotnull, pg_catalog.pg_get_expr(d.adbin, d.adrelid), \
                            EXISTS (SELECT 1 FROM pg_catalog.pg_index i \
                                    WHERE i.indrelid = c.oid AND i.indisprimary \
                                    AND a.attnum = ANY(i.indkey)) \
                     FROM pg_catalog.pg_attribute a \
                     JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
                     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                     LEFT JOIN pg_catalog.pg_attrdef d \
                       ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                     WHERE n.nspname = $1 AND c.relname = $2 \
                       AND a.attnum > 0 AND NOT a.attisdropped \
                     ORDER BY a.attnum",
                    &[&schema, &relation],
                )
                .map_err(|error| postgres_query_error(&error))
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| DatabaseColumn {
                            name: row.get(0),
                            type_name: row.get(1),
                            nullable: row.get(2),
                            default: row.get(3),
                            primary_key: row.get(4),
                        })
                        .collect()
                }),
            Connection::Sqlite(sqlite) => sqlite_columns(&sqlite.client.lock(), schema, relation),
        }
    }

    fn query(
        &self,
        session: &str,
        sql: &str,
        max_rows: u32,
        mode: QueryMode,
    ) -> Result<QueryResult, DatabaseError> {
        if sql.trim().is_empty() {
            return Err(DatabaseError::Query("the query is empty".to_owned()));
        }
        let entry = self.entry(session)?;
        let started = self.clock.monotonic_ms();
        let limit = max_rows.clamp(1, MAX_PAGE_ROWS);
        match &entry.connection {
            Connection::Postgres(postgres) => {
                let mut client = postgres.client.lock();
                let messages = match mode {
                    QueryMode::Normal => client.simple_query(sql),
                    QueryMode::ReadOnly => postgres_read_only(&mut client, sql),
                }
                .map_err(|error| postgres_query_error(&error))?;
                Ok(messages_result(
                    messages,
                    limit,
                    self.clock.monotonic_ms().saturating_sub(started),
                ))
            }
            Connection::Sqlite(sqlite) => {
                let mut client = sqlite.client.lock();
                let mut result = sqlite_query(&mut client, sql, limit, mode)?;
                result.duration_ms = self.clock.monotonic_ms().saturating_sub(started);
                Ok(result)
            }
        }
    }

    fn table_page(
        &self,
        session: &str,
        request: &TablePageRequest,
    ) -> Result<QueryResult, DatabaseError> {
        let limit = request.limit.clamp(1, MAX_PAGE_ROWS);
        let columns = self.columns(session, &request.schema, &request.table)?;
        if columns.is_empty() {
            return Err(DatabaseError::Query(
                "the relation no longer exists".to_owned(),
            ));
        }

        let projection = columns
            .iter()
            .map(|column| quote_identifier(&column.name))
            .collect::<Vec<_>>()
            .join(", ");
        let mut sql = format!(
            "SELECT {projection} FROM {}.{}",
            quote_identifier(&request.schema),
            quote_identifier(&request.table)
        );
        // Each typed fragment on lines of its own. A `--` comment in one then ends at its newline;
        // on the same line as the LIMIT it would have commented the LIMIT out and fetched the whole
        // table into memory. What keeps a fragment from doing anything *else* is the read-only run
        // below, not this layout.
        if let Some(filter) = fragment(request.filter.as_deref()) {
            let _ = write!(sql, "\nWHERE\n{filter}");
        }
        if let Some(order) = fragment(request.order_by.as_deref()) {
            let _ = write!(sql, "\nORDER BY\n{order}");
        }
        let _ = write!(sql, "\nLIMIT {limit} OFFSET {}", request.offset);
        // Always read-only, fragments or not: browsing a table never needs to write, and this is
        // what lets table data stay open on a production connection whose console is locked.
        self.query(session, &sql, limit, QueryMode::ReadOnly)
    }

    fn cancel(&self, session: &str) -> Result<(), DatabaseError> {
        let entry = self.entry(session)?;
        match &entry.connection {
            Connection::Postgres(postgres) => match postgres.tls {
                DatabaseTls::Disable => postgres
                    .cancel
                    .cancel_query(NoTls)
                    .map_err(|error| postgres_query_error(&error)),
                DatabaseTls::Require => postgres
                    .cancel
                    .cancel_query(tls_connector()?)
                    .map_err(|error| postgres_query_error(&error)),
            },
            Connection::Sqlite(sqlite) => {
                sqlite.interrupt.interrupt();
                Ok(())
            }
        }
    }
}

fn sqlite_path(connection: &DatabaseConnection) -> Result<std::path::PathBuf, DatabaseError> {
    connection
        .path
        .clone()
        .or_else(|| {
            connection
                .url
                .as_deref()
                .and_then(|url| url.strip_prefix("sqlite://"))
                .map(std::path::PathBuf::from)
        })
        .ok_or_else(|| {
            DatabaseError::InvalidConnection(
                "the SQLite profile needs a path or sqlite:// URL".to_owned(),
            )
        })
}

fn sqlite_schemas(client: &SqliteConnection) -> Result<Vec<DatabaseSchema>, DatabaseError> {
    let mut statement = client
        .prepare("PRAGMA database_list")
        .map_err(|error| sqlite_query_error(&error))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| sqlite_query_error(&error))?;
    rows.map(|row| {
        row.map(|name| DatabaseSchema { name })
            .map_err(|error| sqlite_query_error(&error))
    })
    .collect()
}

fn sqlite_relations(
    client: &SqliteConnection,
    schema: &str,
) -> Result<Vec<DatabaseRelation>, DatabaseError> {
    let sql = format!(
        "SELECT name, type FROM {}.sqlite_schema \
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
        quote_identifier(schema)
    );
    let mut statement = client
        .prepare(&sql)
        .map_err(|error| sqlite_query_error(&error))?;
    let rows = statement
        .query_map([], |row| {
            let name: String = row.get(0)?;
            let kind: String = row.get(1)?;
            Ok(DatabaseRelation {
                schema: schema.to_owned(),
                name,
                kind: if kind == "view" {
                    RelationKind::View
                } else {
                    RelationKind::Table
                },
            })
        })
        .map_err(|error| sqlite_query_error(&error))?;
    rows.map(|row| row.map_err(|error| sqlite_query_error(&error)))
        .collect()
}

fn sqlite_columns(
    client: &SqliteConnection,
    schema: &str,
    relation: &str,
) -> Result<Vec<DatabaseColumn>, DatabaseError> {
    let sql = format!(
        "PRAGMA {}.table_xinfo({})",
        quote_identifier(schema),
        quote_identifier(relation)
    );
    let mut statement = client
        .prepare(&sql)
        .map_err(|error| sqlite_query_error(&error))?;
    let rows = statement
        .query_map([], |row| {
            let type_name: String = row.get(2)?;
            let not_null: i64 = row.get(3)?;
            let primary_key: i64 = row.get(5)?;
            Ok(DatabaseColumn {
                name: row.get(1)?,
                type_name: if type_name.is_empty() {
                    "untyped".to_owned()
                } else {
                    type_name
                },
                nullable: not_null == 0,
                default: row.get(4)?,
                primary_key: primary_key > 0,
            })
        })
        .map_err(|error| sqlite_query_error(&error))?;
    rows.map(|row| row.map_err(|error| sqlite_query_error(&error)))
        .collect()
}

/// A typed fragment, or nothing when it is blank. One trailing `;` is dropped: it is what a hand
/// used to finishing statements types, and inside the page's statement it could only be an error.
fn fragment(text: Option<&str>) -> Option<&str> {
    let text = text?.trim();
    let text = text.strip_suffix(';').unwrap_or(text).trim_end();
    (!text.is_empty()).then_some(text)
}

/// One statement, in a transaction the server keeps read-only, and rolled back afterwards.
///
/// `prepare` comes first because Postgres's extended protocol parses exactly one statement and
/// refuses more — the only test of "one statement" that parses the text the way the server will.
/// The simple protocol that then runs it executes whatever it is handed, so text that got a
/// `COMMIT;` past a check of our own would have left the read-only transaction and written. One
/// statement cannot: a read-only transaction refuses to become read-write, and a `COMMIT` has
/// nothing to keep. Rolling back rather than committing is the same point made twice.
///
/// The session's own `default_transaction_read_only`, set for read-only profiles, is no defence
/// here: it is a setting, and `SET … = off;` in front of the statement would turn it off.
fn postgres_read_only(
    client: &mut Client,
    sql: &str,
) -> Result<Vec<SimpleQueryMessage>, postgres::Error> {
    let mut transaction = client.build_transaction().read_only(true).start()?;
    transaction.prepare(sql)?;
    let messages = transaction.simple_query(sql)?;
    transaction.rollback()?;
    Ok(messages)
}

fn sqlite_query(
    client: &mut SqliteConnection,
    sql: &str,
    max_rows: u32,
    mode: QueryMode,
) -> Result<QueryResult, DatabaseError> {
    // rusqlite's `prepare` already refuses a second statement, so only the write needs checking —
    // and SQLite answers that from the compiled statement, before anything runs.
    let mut statement = client
        .prepare(sql)
        .map_err(|error| sqlite_query_error(&error))?;
    if mode == QueryMode::ReadOnly && !statement.readonly() {
        return Err(DatabaseError::Query(
            "a read-only run cannot execute a statement that writes".to_owned(),
        ));
    }
    if statement.column_count() == 0 {
        let affected_rows = statement
            .execute([])
            .map_err(|error| sqlite_query_error(&error))? as u64;
        return Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            affected_rows,
            // The caller times the whole call, connection lock included.
            duration_ms: 0,
            truncated: false,
            message: None,
        });
    }

    let columns = statement
        .columns()
        .iter()
        .map(|column| QueryColumn {
            name: column.name().to_owned(),
            type_name: column.decl_type().map(str::to_owned),
        })
        .collect();
    let column_count = statement.column_count();
    let mut query = statement
        .query([])
        .map_err(|error| sqlite_query_error(&error))?;
    let mut rows = Vec::new();
    let mut bytes = 0_usize;
    let mut truncated = false;
    while let Some(row) = query.next().map_err(|error| sqlite_query_error(&error))? {
        if rows.len() >= max_rows as usize || bytes >= MAX_RESULT_BYTES {
            truncated = true;
            break;
        }
        let cells = (0..column_count)
            .map(|index| {
                row.get_ref(index)
                    .map(|value| sqlite_cell(value, &mut bytes))
                    .map_err(|error| sqlite_query_error(&error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        rows.push(cells);
    }

    Ok(QueryResult {
        columns,
        rows,
        affected_rows: 0,
        duration_ms: 0,
        truncated,
        message: None,
    })
}

fn sqlite_cell(value: ValueRef<'_>, result_bytes: &mut usize) -> QueryCell {
    match value {
        ValueRef::Null => QueryCell {
            value: None,
            truncated: false,
        },
        ValueRef::Integer(value) => bounded_cell(Some(&value.to_string()), result_bytes),
        ValueRef::Real(value) => bounded_cell(Some(&value.to_string()), result_bytes),
        ValueRef::Text(value) => bounded_cell(Some(&String::from_utf8_lossy(value)), result_bytes),
        ValueRef::Blob(value) => {
            bounded_cell(Some(&format!("<{} bytes>", value.len())), result_bytes)
        }
    }
}

fn postgres_config(connection: &DatabaseConnection) -> Result<Config, DatabaseError> {
    let mut config = if let Some(url) = &connection.url {
        url.parse::<Config>().map_err(|_| {
            DatabaseError::InvalidConnection("the PostgreSQL URL is not valid".to_owned())
        })?
    } else {
        Config::new()
    };
    if let Some(host) = &connection.host {
        config.host(host);
    }
    if let Some(port) = connection.port {
        config.port(port);
    }
    if let Some(name) = &connection.name {
        config.dbname(name);
    }
    if let Some(user) = &connection.user {
        config.user(user);
    }
    if let Some(password) = &connection.password {
        config.password(password);
    }
    Ok(config)
}

fn connect_postgres(
    config: &mut Config,
    tls: DatabaseTls,
) -> Result<(Client, postgres::CancelToken), DatabaseError> {
    let client = match tls {
        DatabaseTls::Disable => config
            .connect(NoTls)
            .map_err(|error| connection_error(&error))?,
        DatabaseTls::Require => config
            .connect(tls_connector()?)
            .map_err(|error| connection_error(&error))?,
    };
    let cancel = client.cancel_token();
    Ok((client, cancel))
}

fn tls_connector() -> Result<MakeTlsConnector, DatabaseError> {
    TlsConnector::builder()
        .build()
        .map(MakeTlsConnector::new)
        .map_err(|_| DatabaseError::Connection("TLS could not be initialized".to_owned()))
}

fn connection_error(error: &postgres::Error) -> DatabaseError {
    DatabaseError::Connection(driver_message(error))
}

fn postgres_query_error(error: &postgres::Error) -> DatabaseError {
    if error.code().is_some_and(|code| code.code() == "57014") {
        DatabaseError::Cancelled
    } else {
        DatabaseError::Query(driver_message(error))
    }
}

fn sqlite_connection_error(error: &rusqlite::Error) -> DatabaseError {
    DatabaseError::Connection(sqlite_driver_message(error))
}

fn sqlite_query_error(error: &rusqlite::Error) -> DatabaseError {
    if matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::OperationInterrupted
    ) {
        DatabaseError::Cancelled
    } else {
        DatabaseError::Query(sqlite_driver_message(error))
    }
}

fn sqlite_driver_message(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(_, Some(message)) => message.clone(),
        rusqlite::Error::MultipleStatement => "SQLite runs one statement at a time".to_owned(),
        _ => "SQLite refused the operation".to_owned(),
    }
}

fn driver_message(error: &postgres::Error) -> String {
    error.as_db_error().map_or_else(
        || "the server refused the operation".to_owned(),
        |database| database.message().to_owned(),
    )
}

fn messages_result(
    messages: Vec<SimpleQueryMessage>,
    max_rows: u32,
    duration_ms: u64,
) -> QueryResult {
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    let mut affected_rows = 0_u64;
    let mut bytes = 0_usize;
    let mut truncated = false;

    for message in messages {
        match message {
            SimpleQueryMessage::Row(row) => {
                if columns.is_empty() {
                    columns = row
                        .columns()
                        .iter()
                        .map(|column| QueryColumn {
                            name: column.name().to_owned(),
                            type_name: None,
                        })
                        .collect();
                }
                if rows.len() >= max_rows as usize || bytes >= MAX_RESULT_BYTES {
                    truncated = true;
                    continue;
                }
                let cells: Vec<QueryCell> = (0..row.len())
                    .map(|index| bounded_cell(row.get(index), &mut bytes))
                    .collect();
                rows.push(cells);
            }
            SimpleQueryMessage::CommandComplete(count) => affected_rows += count,
            _ => {}
        }
    }

    QueryResult {
        columns,
        rows,
        affected_rows,
        duration_ms,
        truncated,
        message: None,
    }
}

fn bounded_cell(value: Option<&str>, result_bytes: &mut usize) -> QueryCell {
    let Some(value) = value else {
        return QueryCell {
            value: None,
            truncated: false,
        };
    };
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= MAX_CELL_BYTES)
        .last()
        .unwrap_or(0);
    let truncated = value.len() > MAX_CELL_BYTES;
    let rendered = if truncated {
        value[..boundary].to_owned()
    } else {
        value.to_owned()
    };
    *result_bytes = result_bytes.saturating_add(rendered.len());
    QueryCell {
        value: Some(rendered),
        truncated,
    }
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtm_core::model::{DatabaseEnvironment, DatabaseScope};
    use wtm_testkit::FakeClock;

    fn sqlite_connection(path: std::path::PathBuf, access: DatabaseAccess) -> DatabaseConnection {
        DatabaseConnection {
            profile_id: "local".to_owned(),
            label: "Local SQLite".to_owned(),
            engine: DatabaseEngine::Sqlite,
            scope: DatabaseScope::Worktree,
            environment: DatabaseEnvironment::Local,
            access,
            url: None,
            host: None,
            port: None,
            name: None,
            user: None,
            password: None,
            path: Some(path),
            tls: DatabaseTls::Disable,
        }
    }

    #[test]
    fn identifiers_are_quoted_as_data_not_sql() {
        assert_eq!(quote_identifier("odd\"name"), "\"odd\"\"name\"");
    }

    #[test]
    fn a_large_cell_is_bounded_on_a_character_boundary() {
        let value = "🚀".repeat(MAX_CELL_BYTES);
        let mut bytes = 0;
        let cell = bounded_cell(Some(&value), &mut bytes);
        assert!(cell.truncated);
        assert!(cell.value.unwrap().is_char_boundary(bytes));
        assert!(bytes <= MAX_CELL_BYTES);
    }

    #[test]
    fn sqlite_sessions_introspect_and_page_a_real_database() {
        let directory = tempfile::tempdir().unwrap();
        let host = Host::new(Arc::new(FakeClock::new()));
        let session = host
            .connect(sqlite_connection(
                directory.path().join("app.sqlite3"),
                DatabaseAccess::ReadWrite,
            ))
            .unwrap();

        host.query(
            &session.id,
            "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT)",
            100,
            QueryMode::Normal,
        )
        .unwrap();
        host.query(
            &session.id,
            "INSERT INTO people (name, note) VALUES ('Ada', NULL)",
            100,
            QueryMode::Normal,
        )
        .unwrap();

        assert_eq!(host.schemas(&session.id).unwrap()[0].name, "main");
        let relations = host.relations(&session.id, "main").unwrap();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].name, "people");
        let columns = host.columns(&session.id, "main", "people").unwrap();
        assert_eq!(columns.len(), 3);
        assert!(columns[0].primary_key);
        assert!(!columns[1].nullable);

        let page = host
            .table_page(
                &session.id,
                &TablePageRequest {
                    schema: "main".to_owned(),
                    table: "people".to_owned(),
                    offset: 0,
                    limit: 100,
                    filter: None,
                    order_by: Some("\"name\" ASC".to_owned()),
                },
            )
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0][1].value.as_deref(), Some("Ada"));
        assert_eq!(page.rows[0][2].value, None);
    }

    #[test]
    fn a_read_only_sqlite_session_refuses_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("app.sqlite3");
        let host = Host::new(Arc::new(FakeClock::new()));
        let writable = host
            .connect(sqlite_connection(path.clone(), DatabaseAccess::ReadWrite))
            .unwrap();
        host.query(
            &writable.id,
            "CREATE TABLE records (id INTEGER)",
            100,
            QueryMode::Normal,
        )
        .unwrap();
        host.disconnect(&writable.id).unwrap();

        let readonly = host
            .connect(sqlite_connection(path, DatabaseAccess::ReadOnly))
            .unwrap();
        let error = host
            .query(
                &readonly.id,
                "INSERT INTO records VALUES (1)",
                100,
                QueryMode::Normal,
            )
            .unwrap_err();
        assert!(matches!(error, DatabaseError::Query(_)));
    }

    /// A writable session on a fresh file holding `people`: Ada, Grace and Linus, in id order.
    fn people(directory: &tempfile::TempDir) -> (Host, String) {
        let host = Host::new(Arc::new(FakeClock::new()));
        let session = host
            .connect(sqlite_connection(
                directory.path().join("app.sqlite3"),
                DatabaseAccess::ReadWrite,
            ))
            .unwrap()
            .id;
        for sql in [
            "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "INSERT INTO people (name) VALUES ('Ada'), ('Grace'), ('Linus')",
        ] {
            host.query(&session, sql, 100, QueryMode::Normal).unwrap();
        }
        (host, session)
    }

    fn count(host: &Host, session: &str) -> String {
        host.query(session, "SELECT count(*) FROM people", 1, QueryMode::Normal)
            .unwrap()
            .rows[0][0]
            .value
            .clone()
            .unwrap()
    }

    fn page(filter: Option<&str>, order_by: Option<&str>, limit: u32) -> TablePageRequest {
        TablePageRequest {
            schema: "main".to_owned(),
            table: "people".to_owned(),
            offset: 0,
            limit,
            filter: filter.map(str::to_owned),
            order_by: order_by.map(str::to_owned),
        }
    }

    fn names(result: &QueryResult) -> Vec<&str> {
        result
            .rows
            .iter()
            .map(|row| row[1].value.as_deref().unwrap())
            .collect()
    }

    #[test]
    fn a_read_only_run_refuses_a_statement_that_writes() {
        let directory = tempfile::tempdir().unwrap();
        let (host, session) = people(&directory);

        let error = host
            .query(&session, "DELETE FROM people", 100, QueryMode::ReadOnly)
            .unwrap_err();

        assert!(matches!(error, DatabaseError::Query(_)));
        assert_eq!(count(&host, &session), "3");
    }

    #[test]
    fn a_read_only_run_refuses_a_second_statement() {
        let directory = tempfile::tempdir().unwrap();
        let (host, session) = people(&directory);

        let error = host
            .query(
                &session,
                "SELECT 1; DELETE FROM people",
                100,
                QueryMode::ReadOnly,
            )
            .unwrap_err();

        assert!(matches!(error, DatabaseError::Query(_)));
        assert_eq!(count(&host, &session), "3");
    }

    #[test]
    fn a_read_only_run_still_reads() {
        let directory = tempfile::tempdir().unwrap();
        let (host, session) = people(&directory);

        let result = host
            .query(
                &session,
                "SELECT * FROM people WHERE name = 'Grace';",
                100,
                QueryMode::ReadOnly,
            )
            .unwrap();

        assert_eq!(names(&result), ["Grace"]);
    }

    #[test]
    fn a_table_page_applies_its_where_and_order_by_fragments() {
        let directory = tempfile::tempdir().unwrap();
        let (host, session) = people(&directory);

        // The trailing semicolon is what a hand used to finishing statements types.
        let result = host
            .table_page(
                &session,
                &page(Some("name <> 'Linus';"), Some("name DESC"), 100),
            )
            .unwrap();

        assert_eq!(names(&result), ["Grace", "Ada"]);
    }

    #[test]
    fn a_where_fragment_cannot_smuggle_in_a_second_statement() {
        let directory = tempfile::tempdir().unwrap();
        let (host, session) = people(&directory);

        let error = host
            .table_page(
                &session,
                &page(Some("1 = 1; DELETE FROM people"), None, 100),
            )
            .unwrap_err();

        assert!(matches!(error, DatabaseError::Query(_)));
        assert_eq!(count(&host, &session), "3");
    }

    #[test]
    fn a_line_comment_in_a_fragment_cannot_swallow_the_pages_limit() {
        let directory = tempfile::tempdir().unwrap();
        let (host, session) = people(&directory);

        let result = host
            .table_page(
                &session,
                &page(Some("1 = 1 -- every row"), Some("id --"), 2),
            )
            .unwrap();

        assert_eq!(names(&result), ["Ada", "Grace"]);
        assert!(!result.truncated);
    }
}

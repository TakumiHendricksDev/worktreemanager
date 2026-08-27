//! Engine-neutral database sessions.
//!
//! The domain owns the contract and no driver. Connections are blocking resources, so this port
//! follows the workspace's synchronous-port rule; Tauri commands move calls off the webview thread.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::DatabaseError;
use crate::model::{
    DatabaseAccess, DatabaseEngine, DatabaseEnvironment, DatabaseScope, DatabaseTls,
};

/// A fully rendered connection. `Debug` is deliberately redacted because it may carry a password.
#[derive(Clone)]
pub struct DatabaseConnection {
    pub profile_id: String,
    pub label: String,
    pub engine: DatabaseEngine,
    pub scope: DatabaseScope,
    pub environment: DatabaseEnvironment,
    pub access: DatabaseAccess,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub name: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub path: Option<PathBuf>,
    pub tls: DatabaseTls,
}

impl std::fmt::Debug for DatabaseConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseConnection")
            .field("profile_id", &self.profile_id)
            .field("engine", &self.engine)
            .field("scope", &self.scope)
            .field("environment", &self.environment)
            .field("access", &self.access)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("name", &self.name)
            .field("user", &self.user)
            .field("path", &self.path)
            .field("tls", &self.tls)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSession {
    pub id: String,
    pub profile_id: String,
    pub label: String,
    pub engine: DatabaseEngine,
    pub environment: DatabaseEnvironment,
    pub access: DatabaseAccess,
    pub server_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSchema {
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseRelation {
    pub schema: String,
    pub name: String,
    pub kind: RelationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Table,
    View,
    MaterializedView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseColumn {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary_key: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryColumn {
    pub name: String,
    pub type_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryCell {
    pub value: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<QueryColumn>,
    pub rows: Vec<Vec<QueryCell>>,
    pub affected_rows: u64,
    pub duration_ms: u64,
    pub truncated: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePageRequest {
    pub schema: String,
    pub table: String,
    pub offset: u64,
    pub limit: u32,
    pub sort_column: Option<String>,
    pub sort_direction: Option<SortDirection>,
}

/// Live database sessions. Implementations own concrete connections and cancellation handles.
pub trait DatabaseHost: Send + Sync + std::fmt::Debug {
    fn connect(&self, connection: DatabaseConnection) -> Result<DatabaseSession, DatabaseError>;
    fn disconnect(&self, session: &str) -> Result<(), DatabaseError>;
    fn schemas(&self, session: &str) -> Result<Vec<DatabaseSchema>, DatabaseError>;
    fn relations(
        &self,
        session: &str,
        schema: &str,
    ) -> Result<Vec<DatabaseRelation>, DatabaseError>;
    fn columns(
        &self,
        session: &str,
        schema: &str,
        relation: &str,
    ) -> Result<Vec<DatabaseColumn>, DatabaseError>;
    fn query(&self, session: &str, sql: &str, max_rows: u32) -> Result<QueryResult, DatabaseError>;
    fn table_page(
        &self,
        session: &str,
        request: &TablePageRequest,
    ) -> Result<QueryResult, DatabaseError>;
    fn cancel(&self, session: &str) -> Result<(), DatabaseError>;
}

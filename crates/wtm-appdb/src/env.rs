//! [`EnvStore`] over SQLite (migration 10).
//!
//! # A service is one row, rewritten
//!
//! `(worktree, service)` is unique, and saving replaces it. What a relaunch needs is the process
//! that may still be running, with the start time that proves it is the same one; a history of the
//! processes before it would be one more place to find the wrong pid.
//!
//! # One command waits at a time
//!
//! A destructive command's request files an inbox item against the environment's flow, and the
//! inbox keeps one open item per target and kind. So the store refuses a second waiting run for the
//! same flow, in the same statement that would insert it (the partial unique index), and names the
//! one already waiting.

use rusqlite::{Connection, OptionalExtension, Row, params};
use wtm_core::flow::{
    ApprovalId, Binding, CommandRun, CommandRunChange, CommandRunId, CommandRunRefused,
    CommandRunState, FlowId, NewCommandRun, ProcessIdentity, ServiceRecord, ServiceState, UnixMs,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{EnvStore, FlowStoreError};

use crate::AppDb;
use crate::runs::{ensure_flow, query};
use crate::store::{from_json, is_unique_violation, json, storage, tag, untag};

const SERVICE_COLUMNS: &str = "project, worktree, service, state, pid, started, detached, \
     log_path, detail, started_at, updated_at";

const RUN_COLUMNS: &str = "id, flow_id, command, label, argv, binding, state, approval_id, \
     exit_code, output, requested_at, finished_at";

struct ServiceRow {
    project: String,
    worktree: String,
    service: String,
    state: String,
    pid: Option<u32>,
    started: Option<String>,
    detached: bool,
    log: String,
    detail: Option<String>,
    started_at: Option<UnixMs>,
    updated_at: UnixMs,
}

impl ServiceRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            project: row.get(0)?,
            worktree: row.get(1)?,
            service: row.get(2)?,
            state: row.get(3)?,
            pid: row.get(4)?,
            started: row.get(5)?,
            detached: row.get(6)?,
            log: row.get(7)?,
            detail: row.get(8)?,
            started_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    }

    fn into_record(self) -> Result<ServiceRecord, FlowStoreError> {
        Ok(ServiceRecord {
            project: self.project,
            worktree: self.worktree,
            service: self.service,
            state: untag::<ServiceState>(self.state)?,
            process: match (self.pid, self.started) {
                (Some(pid), Some(started)) => Some(ProcessIdentity { pid, started }),
                _ => None,
            },
            detached: self.detached,
            log: self.log,
            detail: self.detail,
            started_at: self.started_at,
            updated_at: self.updated_at,
        })
    }
}

struct RunRow {
    id: i64,
    flow: i64,
    command: String,
    label: String,
    argv: String,
    binding: String,
    state: String,
    approval: Option<i64>,
    exit_code: Option<i32>,
    output: Option<String>,
    requested_at: UnixMs,
    finished_at: Option<UnixMs>,
}

impl RunRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow: row.get(1)?,
            command: row.get(2)?,
            label: row.get(3)?,
            argv: row.get(4)?,
            binding: row.get(5)?,
            state: row.get(6)?,
            approval: row.get(7)?,
            exit_code: row.get(8)?,
            output: row.get(9)?,
            requested_at: row.get(10)?,
            finished_at: row.get(11)?,
        })
    }

    fn into_run(self) -> Result<CommandRun, FlowStoreError> {
        Ok(CommandRun {
            id: CommandRunId(self.id),
            flow: FlowId(self.flow),
            command: self.command,
            label: self.label,
            argv: from_json(&self.argv)?,
            binding: from_json::<Binding>(&self.binding)?,
            state: untag::<CommandRunState>(self.state)?,
            approval: self.approval.map(ApprovalId),
            exit_code: self.exit_code,
            output: self.output,
            requested_at: self.requested_at,
            finished_at: self.finished_at,
        })
    }
}

fn run(conn: &Connection, id: CommandRunId) -> Result<Option<CommandRun>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {RUN_COLUMNS} FROM env_command_runs WHERE id = ?1"),
        params![id.0],
        RunRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(RunRow::into_run)
    .transpose()
}

fn waiting(conn: &Connection, flow: FlowId) -> Result<Option<CommandRunId>, FlowStoreError> {
    conn.query_row(
        "SELECT id FROM env_command_runs WHERE flow_id = ?1 AND state = 'awaiting_approval'",
        params![flow.0],
        |row| row.get(0).map(CommandRunId),
    )
    .optional()
    .map_err(storage)
}

impl EnvStore for AppDb {
    fn save_service(&self, record: &ServiceRecord) -> Result<(), FlowStoreError> {
        let conn = self.conn.lock();
        let (pid, started) = record
            .process
            .as_ref()
            .map_or((None, None), |p| (Some(p.pid), Some(p.started.as_str())));
        conn.execute(
            "INSERT INTO env_services
                 (project, worktree, service, state, pid, started, detached, log_path, detail,
                  started_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (worktree, service) DO UPDATE SET
                 project = excluded.project, state = excluded.state, pid = excluded.pid,
                 started = excluded.started, detached = excluded.detached,
                 log_path = excluded.log_path, detail = excluded.detail,
                 started_at = excluded.started_at, updated_at = excluded.updated_at",
            params![
                record.project,
                record.worktree,
                record.service,
                tag(&record.state)?,
                pid,
                started,
                record.detached,
                record.log,
                record.detail,
                record.started_at,
                record.updated_at,
            ],
        )
        .map_err(storage)?;
        Ok(())
    }

    fn services(&self, worktree: Option<&str>) -> Result<Vec<ServiceRecord>, FlowStoreError> {
        let conn = self.conn.lock();
        let rows = match worktree {
            Some(worktree) => query(
                &conn,
                &format!(
                    "SELECT {SERVICE_COLUMNS} FROM env_services WHERE worktree = ?1 \
                     ORDER BY service"
                ),
                params![worktree],
                ServiceRow::read,
            )?,
            None => query(
                &conn,
                &format!("SELECT {SERVICE_COLUMNS} FROM env_services ORDER BY worktree, service"),
                [],
                ServiceRow::read,
            )?,
        };
        rows.into_iter().map(ServiceRow::into_record).collect()
    }

    fn environment_flow(&self, worktree: &str) -> Result<Option<FlowId>, FlowStoreError> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT e.flow_id FROM environment_flows e JOIN flows f ON f.id = e.flow_id
             WHERE e.worktree = ?1 AND f.outcome IS NULL",
            params![worktree],
            |row| row.get(0).map(FlowId),
        )
        .optional()
        .map_err(storage)
    }

    fn environment_place(
        &self,
        flow: FlowId,
    ) -> Result<Option<(ProjectId, String)>, FlowStoreError> {
        self.conn
            .lock()
            .query_row(
                "SELECT project, worktree FROM environment_flows WHERE flow_id = ?1",
                params![flow.0],
                |row| {
                    let project: String = row.get(0)?;
                    Ok((
                        ProjectId::from_root(std::path::Path::new(&project)),
                        row.get(1)?,
                    ))
                },
            )
            .optional()
            .map_err(storage)
    }

    fn bind_environment_flow(
        &self,
        flow: FlowId,
        project: &ProjectId,
        worktree: &str,
    ) -> Result<(), FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, flow)?;
        conn.execute(
            "INSERT INTO environment_flows (flow_id, project, worktree) VALUES (?1, ?2, ?3)
             ON CONFLICT (worktree) DO UPDATE SET flow_id = excluded.flow_id,
                                                  project = excluded.project",
            params![flow.0, project.as_str(), worktree],
        )
        .map_err(storage)?;
        Ok(())
    }

    fn record_command_run(
        &self,
        new: &NewCommandRun,
        at: UnixMs,
    ) -> Result<CommandRun, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, new.flow)?;
        if new.state.is_over() {
            return Err(FlowStoreError::Storage(format!(
                "a command run starts awaiting approval or running, not {:?}",
                new.state
            )));
        }
        let inserted = conn.execute(
            "INSERT INTO env_command_runs
                 (flow_id, command, label, argv, binding, state, requested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                new.flow.0,
                new.command,
                new.label,
                json(&new.argv)?,
                json(&new.binding)?,
                tag(&new.state)?,
                at,
            ],
        );
        match inserted {
            Ok(_) => {}
            Err(error) if is_unique_violation(&error) => {
                let existing = waiting(&conn, new.flow)?.ok_or_else(|| storage(&error))?;
                return Err(FlowStoreError::CommandWaiting { existing });
            }
            Err(error) => return Err(storage(error)),
        }
        let id = CommandRunId(conn.last_insert_rowid());
        run(&conn, id)?.ok_or(FlowStoreError::UnknownCommandRun(id))
    }

    fn change_command_run(
        &self,
        id: CommandRunId,
        change: &CommandRunChange,
        at: UnixMs,
    ) -> Result<CommandRun, FlowStoreError> {
        let conn = self.conn.lock();
        let found = run(&conn, id)?.ok_or(FlowStoreError::UnknownCommandRun(id))?;
        let state = match change.state {
            Some(to) if to != found.state => {
                if !found.state.can_move(to) {
                    return Err(CommandRunRefused {
                        from: found.state,
                        to,
                    }
                    .into());
                }
                to
            }
            _ => found.state,
        };
        let finished_at = if state.is_over() {
            Some(found.finished_at.unwrap_or(at))
        } else {
            None
        };
        conn.execute(
            "UPDATE env_command_runs
             SET state = ?1, approval_id = ?2, exit_code = ?3, output = ?4, finished_at = ?5
             WHERE id = ?6",
            params![
                tag(&state)?,
                change.approval.or(found.approval).map(|a| a.0),
                change.exit_code.or(found.exit_code),
                change.output.clone().or(found.output),
                finished_at,
                id.0,
            ],
        )
        .map_err(storage)?;
        run(&conn, id)?.ok_or(FlowStoreError::UnknownCommandRun(id))
    }

    fn command_run(&self, id: CommandRunId) -> Result<Option<CommandRun>, FlowStoreError> {
        run(&self.conn.lock(), id)
    }

    fn command_runs(&self, flow: FlowId, limit: usize) -> Result<Vec<CommandRun>, FlowStoreError> {
        let conn = self.conn.lock();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        query(
            &conn,
            &format!(
                "SELECT {RUN_COLUMNS} FROM env_command_runs WHERE flow_id = ?1 \
                 ORDER BY id DESC LIMIT ?2"
            ),
            params![flow.0, limit],
            RunRow::read,
        )?
        .into_iter()
        .map(RunRow::into_run)
        .collect()
    }

    fn idle_stop(&self, project: &ProjectId) -> Result<Option<u32>, FlowStoreError> {
        self.conn
            .lock()
            .query_row(
                "SELECT minutes FROM env_idle_stop WHERE project = ?1",
                params![project.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)
    }

    fn set_idle_stop(
        &self,
        project: &ProjectId,
        minutes: Option<u32>,
    ) -> Result<(), FlowStoreError> {
        let conn = self.conn.lock();
        match minutes.filter(|m| *m > 0) {
            Some(minutes) => conn.execute(
                "INSERT INTO env_idle_stop (project, minutes) VALUES (?1, ?2)
                 ON CONFLICT (project) DO UPDATE SET minutes = excluded.minutes",
                params![project.as_str(), minutes],
            ),
            None => conn.execute(
                "DELETE FROM env_idle_stop WHERE project = ?1",
                params![project.as_str()],
            ),
        }
        .map_err(storage)?;
        Ok(())
    }
}

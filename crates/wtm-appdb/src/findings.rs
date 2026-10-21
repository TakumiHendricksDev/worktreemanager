//! A review's findings, their evidence, and its check records, over SQLite.
//!
//! The bodies of `RunStore`'s finding and check methods, kept out of `runs.rs` because they are a
//! different family of rows: what a review concluded, rather than how its run went. A finding's
//! class is stored as its JSON, the shape `FindingClass` serializes to, because each class carries
//! different evidence and one column per field would be mostly empty.

use rusqlite::{Connection, OptionalExtension, Row, params};
use wtm_core::flow::{
    BlobRef, CheckRun, CheckRunId, Dismissal, Evidence, Finding, FindingId, FindingRecord, FlowId,
    InvocationId, Location, NewCheckRun, Reconciled, Source, StepId, UnixMs,
};
use wtm_core::ports::FlowStoreError;

use crate::store::{from_json, json, storage, tag, untag};

const FINDING_COLUMNS: &str = "id, flow_id, class, severity, path, start_line, end_line, lines, \
     title, rationale, requirement, tree_sha, dismissed_reason, dismissed_at";

const CHECK_COLUMNS: &str = "id, flow_id, check_id, step_id, invocation_id, role, status, detail, \
     exit_code, tree_sha, cwd, output_path, output_sha256, output_bytes, started_at, ended_at";

pub(crate) fn findings(
    conn: &Connection,
    flow: FlowId,
) -> Result<Vec<FindingRecord>, FlowStoreError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {FINDING_COLUMNS} FROM findings
             WHERE flow_id = ?1 AND superseded_at IS NULL ORDER BY id"
        ))
        .map_err(storage)?;
    let rows = statement
        .query_map(params![flow.0], FindingRow::read)
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    rows.into_iter().map(|row| row.into_record(conn)).collect()
}

pub(crate) fn finding(
    conn: &Connection,
    id: FindingId,
) -> Result<Option<FindingRecord>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {FINDING_COLUMNS} FROM findings WHERE id = ?1"),
        params![id.0],
        FindingRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(|row| row.into_record(conn))
    .transpose()
}

pub(crate) fn save_consolidation(
    conn: &mut Connection,
    flow: FlowId,
    change: &Reconciled,
    at: UnixMs,
) -> Result<Vec<FindingRecord>, FlowStoreError> {
    let tx = conn.transaction().map_err(storage)?;
    for id in &change.supersede {
        tx.execute(
            "UPDATE findings SET superseded_at = ?1
             WHERE id = ?2 AND flow_id = ?3 AND superseded_at IS NULL",
            params![at, id.0, flow.0],
        )
        .map_err(storage)?;
    }
    for new in &change.create {
        let (reason, dismissed_at) = new
            .dismissed
            .as_ref()
            .map_or((None, None), |d| (Some(d.reason.as_str()), Some(d.at)));
        tx.execute(
            "INSERT INTO findings (flow_id, class, severity, path, start_line, end_line, lines,
                                   title, rationale, requirement, tree_sha, dismissed_reason,
                                   dismissed_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                flow.0,
                json(&new.class)?,
                tag(&new.severity)?,
                new.location.as_ref().map(|l| l.path.as_str()),
                new.location.as_ref().map(|l| l.start_line),
                new.location.as_ref().map(|l| l.end_line),
                new.lines,
                new.title,
                new.rationale,
                new.requirement,
                new.tree_sha,
                reason,
                dismissed_at,
                at
            ],
        )
        .map_err(storage)?;
        let finding = tx.last_insert_rowid();
        for evidence in &new.evidence {
            let source = &evidence.source;
            tx.execute(
                "INSERT INTO finding_evidence (finding_id, invocation_id, ordinal, step_id, role,
                                               provider, model, workspace, tree_sha, reported)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    finding,
                    source.invocation.0,
                    source.ordinal,
                    source.step.0,
                    source.role,
                    tag(&source.provider)?,
                    source.model,
                    tag(&source.workspace)?,
                    source.tree_sha,
                    json(&evidence.reported)?
                ],
            )
            .map_err(storage)?;
        }
    }
    tx.commit().map_err(storage)?;
    findings(conn, flow)
}

pub(crate) fn dismiss(
    conn: &Connection,
    id: FindingId,
    reason: Option<&str>,
    at: UnixMs,
) -> Result<FindingRecord, FlowStoreError> {
    let changed = conn
        .execute(
            "UPDATE findings SET dismissed_reason = ?1, dismissed_at = ?2 WHERE id = ?3",
            params![reason, reason.map(|_| at), id.0],
        )
        .map_err(storage)?;
    if changed == 0 {
        return Err(FlowStoreError::UnknownFinding(id));
    }
    finding(conn, id)?.ok_or(FlowStoreError::UnknownFinding(id))
}

pub(crate) fn record_check(
    conn: &Connection,
    run: &NewCheckRun,
    at: UnixMs,
) -> Result<CheckRun, FlowStoreError> {
    conn.execute(
        "INSERT INTO check_runs (flow_id, check_id, step_id, invocation_id, role, status, detail,
                                 exit_code, tree_sha, cwd, output_path, output_sha256,
                                 output_bytes, started_at, ended_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            run.flow.0,
            run.check,
            run.step.map(|s| s.0),
            run.invocation.map(|i| i.0),
            run.role,
            tag(&run.status)?,
            run.detail,
            run.exit_code,
            run.tree_sha,
            run.cwd,
            run.output.as_ref().map(|o| o.path.as_str()),
            run.output.as_ref().map(|o| o.sha256.as_str()),
            run.output.as_ref().map(|o| o.bytes),
            run.started_at,
            at
        ],
    )
    .map_err(storage)?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        &format!("SELECT {CHECK_COLUMNS} FROM check_runs WHERE id = ?1"),
        params![id],
        CheckRow::read,
    )
    .map_err(storage)?
    .into_run()
}

pub(crate) fn checks(conn: &Connection, flow: FlowId) -> Result<Vec<CheckRun>, FlowStoreError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {CHECK_COLUMNS} FROM check_runs WHERE flow_id = ?1 ORDER BY id"
        ))
        .map_err(storage)?;
    statement
        .query_map(params![flow.0], CheckRow::read)
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?
        .into_iter()
        .map(CheckRow::into_run)
        .collect()
}

/// A `findings` row as SQLite hands it back.
struct FindingRow {
    id: i64,
    flow_id: i64,
    class: String,
    severity: String,
    path: Option<String>,
    start_line: Option<u32>,
    end_line: Option<u32>,
    lines: bool,
    title: String,
    rationale: String,
    requirement: Option<String>,
    tree_sha: String,
    dismissed_reason: Option<String>,
    dismissed_at: Option<UnixMs>,
}

impl FindingRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow_id: row.get(1)?,
            class: row.get(2)?,
            severity: row.get(3)?,
            path: row.get(4)?,
            start_line: row.get(5)?,
            end_line: row.get(6)?,
            lines: row.get(7)?,
            title: row.get(8)?,
            rationale: row.get(9)?,
            requirement: row.get(10)?,
            tree_sha: row.get(11)?,
            dismissed_reason: row.get(12)?,
            dismissed_at: row.get(13)?,
        })
    }

    fn into_record(self, conn: &Connection) -> Result<FindingRecord, FlowStoreError> {
        let location = match (self.path, self.start_line, self.end_line) {
            (Some(path), Some(start_line), Some(end_line)) => Some(Location {
                path,
                start_line,
                end_line,
            }),
            _ => None,
        };
        let dismissed = match (self.dismissed_reason, self.dismissed_at) {
            (Some(reason), Some(at)) => Some(Dismissal { reason, at }),
            _ => None,
        };
        Ok(FindingRecord {
            evidence: evidence(conn, FindingId(self.id))?,
            finding: Finding {
                id: FindingId(self.id),
                flow: FlowId(self.flow_id),
                class: from_json(&self.class)?,
                severity: untag(self.severity)?,
                location,
                title: self.title,
                rationale: self.rationale,
                requirement: self.requirement,
                tree_sha: self.tree_sha,
                dismissed,
            },
            lines: self.lines,
        })
    }
}

fn evidence(conn: &Connection, finding: FindingId) -> Result<Vec<Evidence>, FlowStoreError> {
    let mut statement = conn
        .prepare(
            "SELECT invocation_id, ordinal, step_id, role, provider, model, workspace, tree_sha,
                    reported
             FROM finding_evidence WHERE finding_id = ?1 ORDER BY invocation_id, ordinal",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![finding.0], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    rows.into_iter()
        .map(
            |(invocation, ordinal, step, role, provider, model, workspace, tree_sha, reported)| {
                Ok(Evidence {
                    source: Source {
                        step: StepId(step),
                        invocation: InvocationId(invocation),
                        ordinal,
                        role,
                        provider: untag(provider)?,
                        model,
                        workspace: untag(workspace)?,
                        tree_sha,
                    },
                    reported: from_json(&reported)?,
                })
            },
        )
        .collect()
}

/// A `check_runs` row as SQLite hands it back.
struct CheckRow {
    id: i64,
    flow_id: i64,
    check_id: String,
    step_id: Option<i64>,
    invocation_id: Option<i64>,
    role: Option<String>,
    status: String,
    detail: Option<String>,
    exit_code: Option<i32>,
    tree_sha: String,
    cwd: String,
    output_path: Option<String>,
    output_sha256: Option<String>,
    output_bytes: Option<u64>,
    started_at: UnixMs,
    ended_at: UnixMs,
}

impl CheckRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow_id: row.get(1)?,
            check_id: row.get(2)?,
            step_id: row.get(3)?,
            invocation_id: row.get(4)?,
            role: row.get(5)?,
            status: row.get(6)?,
            detail: row.get(7)?,
            exit_code: row.get(8)?,
            tree_sha: row.get(9)?,
            cwd: row.get(10)?,
            output_path: row.get(11)?,
            output_sha256: row.get(12)?,
            output_bytes: row.get(13)?,
            started_at: row.get(14)?,
            ended_at: row.get(15)?,
        })
    }

    fn into_run(self) -> Result<CheckRun, FlowStoreError> {
        let output = match (self.output_path, self.output_sha256, self.output_bytes) {
            (Some(path), Some(sha256), Some(bytes)) => Some(BlobRef {
                path,
                sha256,
                bytes,
            }),
            _ => None,
        };
        Ok(CheckRun {
            id: CheckRunId(self.id),
            flow: FlowId(self.flow_id),
            check: self.check_id,
            step: self.step_id.map(StepId),
            invocation: self.invocation_id.map(InvocationId),
            role: self.role,
            status: untag(self.status)?,
            detail: self.detail,
            exit_code: self.exit_code,
            tree_sha: self.tree_sha,
            cwd: self.cwd,
            output,
            started_at: self.started_at,
            ended_at: self.ended_at,
        })
    }
}

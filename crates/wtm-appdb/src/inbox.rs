//! [`InboxStore`] over SQLite: what runs need you for, and the questions they ask.
//!
//! An item's target is two columns, a kind and an id, because the unique index that files an item
//! once has to see it: `(kind, target_kind, target_id)` among open items. A flow-level item's id is
//! its own flow's, so two runs' "interrupted" never collide and one run's never repeats.
//!
//! The project and worktree an item is about come from its run's `review_runs` row, joined on read,
//! so they are never a second copy that could disagree with the run; for an environment's item,
//! from its `environment_flows` row, the same way.

use rusqlite::{Connection, OptionalExtension, Row, params};
use wtm_core::flow::ApprovalId;
use wtm_core::flow::{
    Answer, ContextLink, FindingId, FlowId, InboxItem, InboxItemId, InboxKind, InboxState,
    InboxTarget, NewInboxItem, NewQuestion, Phase, Question, QuestionId, StepId, UnixMs,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStoreError, InboxStore};

use crate::AppDb;
use crate::runs::{ensure_flow, query};
use crate::store::{from_json, is_unique_violation, json, storage, tag, untag};

const ITEM_COLUMNS: &str = "i.id, i.flow_id, i.kind, i.target_kind, i.target_id, i.subject, \
     i.title, i.body, i.state, i.created_at, i.closed_at, COALESCE(r.project, e.project), \
     COALESCE(r.worktree, e.worktree)";

/// Every item read joins its run, for the place it is about.
const ITEM_FROM: &str = "inbox_items i LEFT JOIN review_runs r ON r.flow_id = i.flow_id \
     LEFT JOIN environment_flows e ON e.flow_id = i.flow_id";

const QUESTION_COLUMNS: &str = "id, flow_id, step_id, role, text, why, unblocks, blocks, context, \
     recommendation, options, asked_at, answer_choice, answer_text, answered_at";

/// The target as its two columns.
fn target_key(flow: FlowId, target: &InboxTarget) -> (&'static str, i64) {
    match target {
        InboxTarget::Flow => ("flow", flow.0),
        InboxTarget::Step { step, .. } => ("step", step.0),
        InboxTarget::Finding { finding } => ("finding", finding.0),
        InboxTarget::Question { question } => ("question", question.0),
        InboxTarget::Approval { approval } => ("approval", approval.0),
    }
}

fn target_from(kind: &str, id: i64, subject: &str) -> Result<InboxTarget, FlowStoreError> {
    Ok(match kind {
        "flow" => InboxTarget::Flow,
        "step" => InboxTarget::Step {
            step: StepId(id),
            role: subject.to_owned(),
        },
        "finding" => InboxTarget::Finding {
            finding: FindingId(id),
        },
        "question" => InboxTarget::Question {
            question: QuestionId(id),
        },
        "approval" => InboxTarget::Approval {
            approval: ApprovalId(id),
        },
        other => {
            return Err(FlowStoreError::Storage(format!(
                "an inbox item has an unknown target kind `{other}`"
            )));
        }
    })
}

/// Insert an item, or report that an open one already has its target and kind.
fn insert(
    conn: &Connection,
    item: &NewInboxItem,
    at: UnixMs,
) -> Result<Option<InboxItemId>, FlowStoreError> {
    let (target_kind, target_id) = target_key(item.flow, &item.target);
    let inserted = conn.execute(
        "INSERT INTO inbox_items
             (flow_id, kind, target_kind, target_id, subject, title, body, state, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', ?8)",
        params![
            item.flow.0,
            tag(&item.kind)?,
            target_kind,
            target_id,
            item.target.subject(),
            item.title,
            item.body,
            at
        ],
    );
    match inserted {
        Ok(_) => Ok(Some(InboxItemId(conn.last_insert_rowid()))),
        // An open item already says this. Not refreshed either: its creation time is when the
        // problem started, which is the useful thing to keep.
        Err(e) if is_unique_violation(&e) => Ok(None),
        Err(e) => Err(storage(e)),
    }
}

fn item_by_id(conn: &Connection, id: InboxItemId) -> Result<Option<InboxItem>, FlowStoreError> {
    let row = conn
        .query_row(
            &format!("SELECT {ITEM_COLUMNS} FROM {ITEM_FROM} WHERE i.id = ?1"),
            params![id.0],
            ItemRow::read,
        )
        .optional()
        .map_err(storage)?;
    row.map(|row| row.into_item(conn)).transpose()
}

fn question_by_id(conn: &Connection, id: QuestionId) -> Result<Option<Question>, FlowStoreError> {
    conn.query_row(
        &format!("SELECT {QUESTION_COLUMNS} FROM questions WHERE id = ?1"),
        params![id.0],
        QuestionRow::read,
    )
    .optional()
    .map_err(storage)?
    .map(QuestionRow::into_question)
    .transpose()
}

/// Close one open item as `state`, returning it as it now is.
fn close(
    conn: &Connection,
    item: InboxItemId,
    state: InboxState,
    at: UnixMs,
) -> Result<Option<InboxItem>, FlowStoreError> {
    let changed = conn
        .execute(
            "UPDATE inbox_items SET state = ?1, closed_at = ?2 WHERE id = ?3 AND state = 'open'",
            params![tag(&state)?, at, item.0],
        )
        .map_err(storage)?;
    if changed == 0 {
        return Ok(None);
    }
    item_by_id(conn, item)
}

impl InboxStore for AppDb {
    fn file_inbox(
        &self,
        item: &NewInboxItem,
        at: UnixMs,
    ) -> Result<Option<InboxItem>, FlowStoreError> {
        let conn = self.conn.lock();
        ensure_flow(&conn, item.flow)?;
        match insert(&conn, item, at)? {
            Some(id) => item_by_id(&conn, id),
            None => Ok(None),
        }
    }

    fn resolve_inbox(
        &self,
        flow: FlowId,
        kind: InboxKind,
        target: Option<&InboxTarget>,
        at: UnixMs,
    ) -> Result<usize, FlowStoreError> {
        let conn = self.conn.lock();
        let (target_kind, target_id) = match target.map(|target| target_key(flow, target)) {
            Some((kind, id)) => (Some(kind), Some(id)),
            None => (None, None),
        };
        conn.execute(
            "UPDATE inbox_items SET state = 'resolved', closed_at = ?1
             WHERE flow_id = ?2 AND kind = ?3 AND state = 'open'
               AND (?4 IS NULL OR (target_kind = ?4 AND target_id = ?5))",
            params![at, flow.0, tag(&kind)?, target_kind, target_id],
        )
        .map_err(storage)
    }

    fn resolve_inbox_item(
        &self,
        item: InboxItemId,
        at: UnixMs,
    ) -> Result<Option<InboxItem>, FlowStoreError> {
        close(&self.conn.lock(), item, InboxState::Resolved, at)
    }

    fn dismiss_inbox_item(
        &self,
        item: InboxItemId,
        at: UnixMs,
    ) -> Result<Option<InboxItem>, FlowStoreError> {
        close(&self.conn.lock(), item, InboxState::Dismissed, at)
    }

    fn inbox_item(&self, item: InboxItemId) -> Result<Option<InboxItem>, FlowStoreError> {
        item_by_id(&self.conn.lock(), item)
    }

    fn inbox(
        &self,
        flow: Option<FlowId>,
        project: Option<&ProjectId>,
    ) -> Result<Vec<InboxItem>, FlowStoreError> {
        let conn = self.conn.lock();
        let rows = query(
            &conn,
            &format!(
                "SELECT {ITEM_COLUMNS} FROM {ITEM_FROM}
                 WHERE i.state = 'open'
                   AND (?1 IS NULL OR i.flow_id = ?1)
                   AND (?2 IS NULL OR COALESCE(r.project, e.project) = ?2)
                 ORDER BY i.id DESC"
            ),
            params![flow.map(|f| f.0), project.map(ProjectId::as_str)],
            ItemRow::read,
        )?;
        rows.into_iter().map(|row| row.into_item(&conn)).collect()
    }

    fn ask(
        &self,
        question: &NewQuestion,
        title: &str,
        at: UnixMs,
    ) -> Result<(Question, Option<InboxItem>), FlowStoreError> {
        let mut conn = self.conn.lock();
        ensure_flow(&conn, question.flow)?;
        let tx = conn.transaction().map_err(storage)?;
        tx.execute(
            "INSERT INTO questions
                 (flow_id, step_id, role, text, why, unblocks, blocks, context, recommendation,
                  options, asked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                question.flow.0,
                question.step.map(|s| s.0),
                question.role,
                question.text,
                question.why,
                question.unblocks,
                question.blocks.map(|phase| tag(&phase)).transpose()?,
                json(&question.context)?,
                question.recommendation,
                json(&question.options)?,
                at
            ],
        )
        .map_err(storage)?;
        let id = QuestionId(tx.last_insert_rowid());
        let filed = insert(
            &tx,
            &NewInboxItem {
                flow: question.flow,
                kind: InboxKind::Question,
                target: InboxTarget::Question { question: id },
                title: title.to_owned(),
                body: question.why.clone(),
            },
            at,
        )?;
        let asked = question_by_id(&tx, id)?
            .ok_or_else(|| FlowStoreError::Storage("a question vanished as it was asked".into()))?;
        let item = filed
            .map(|item| item_by_id(&tx, item))
            .transpose()?
            .flatten();
        tx.commit().map_err(storage)?;
        Ok((asked, item))
    }

    fn question(&self, id: QuestionId) -> Result<Option<Question>, FlowStoreError> {
        question_by_id(&self.conn.lock(), id)
    }

    fn questions(&self, flow: FlowId) -> Result<Vec<Question>, FlowStoreError> {
        let conn = self.conn.lock();
        query(
            &conn,
            &format!("SELECT {QUESTION_COLUMNS} FROM questions WHERE flow_id = ?1 ORDER BY id"),
            params![flow.0],
            QuestionRow::read,
        )?
        .into_iter()
        .map(QuestionRow::into_question)
        .collect()
    }

    fn answer_question(&self, id: QuestionId, answer: &Answer) -> Result<Question, FlowStoreError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(storage)?;
        let asked = question_by_id(&tx, id)?.ok_or(FlowStoreError::UnknownQuestion(id))?;
        // The rules first, so a refused answer writes nothing; the trigger is the backstop for a
        // writer that skips them.
        let answered = asked.answer(answer.clone())?;
        tx.execute(
            "UPDATE questions SET answer_choice = ?1, answer_text = ?2, answered_at = ?3
             WHERE id = ?4 AND answered_at IS NULL",
            params![
                answer
                    .choice
                    .map(|c| i64::try_from(c).map_err(storage))
                    .transpose()?,
                answer.text,
                answer.at,
                id.0
            ],
        )
        .map_err(storage)?;
        tx.execute(
            "UPDATE inbox_items SET state = 'resolved', closed_at = ?1
             WHERE target_kind = 'question' AND target_id = ?2 AND state = 'open'",
            params![answer.at, id.0],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(answered)
    }
}

/// An `inbox_items` row, with its run's place, as SQLite hands it back.
struct ItemRow {
    id: i64,
    flow_id: i64,
    kind: String,
    target_kind: String,
    target_id: i64,
    subject: String,
    title: String,
    body: String,
    state: String,
    created_at: UnixMs,
    closed_at: Option<UnixMs>,
    project: Option<String>,
    worktree: Option<String>,
}

impl ItemRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow_id: row.get(1)?,
            kind: row.get(2)?,
            target_kind: row.get(3)?,
            target_id: row.get(4)?,
            subject: row.get(5)?,
            title: row.get(6)?,
            body: row.get(7)?,
            state: row.get(8)?,
            created_at: row.get(9)?,
            closed_at: row.get(10)?,
            project: row.get(11)?,
            worktree: row.get(12)?,
        })
    }

    fn into_item(self, conn: &Connection) -> Result<InboxItem, FlowStoreError> {
        let target = target_from(&self.target_kind, self.target_id, &self.subject)?;
        let question = match &target {
            InboxTarget::Question { question } => question_by_id(conn, *question)?,
            _ => None,
        };
        Ok(InboxItem {
            id: InboxItemId(self.id),
            flow: FlowId(self.flow_id),
            kind: untag(self.kind)?,
            target,
            title: self.title,
            body: self.body,
            state: untag(self.state)?,
            created_at: self.created_at,
            closed_at: self.closed_at,
            project: self
                .project
                .map(|p| ProjectId::from_root(std::path::Path::new(&p))),
            worktree: self.worktree,
            question,
        })
    }
}

/// A `questions` row as SQLite hands it back.
struct QuestionRow {
    id: i64,
    flow_id: i64,
    step_id: Option<i64>,
    role: Option<String>,
    text: String,
    why: String,
    unblocks: String,
    blocks: Option<String>,
    context: String,
    recommendation: Option<String>,
    options: String,
    asked_at: UnixMs,
    answer_choice: Option<i64>,
    answer_text: Option<String>,
    answered_at: Option<UnixMs>,
}

impl QuestionRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            flow_id: row.get(1)?,
            step_id: row.get(2)?,
            role: row.get(3)?,
            text: row.get(4)?,
            why: row.get(5)?,
            unblocks: row.get(6)?,
            blocks: row.get(7)?,
            context: row.get(8)?,
            recommendation: row.get(9)?,
            options: row.get(10)?,
            asked_at: row.get(11)?,
            answer_choice: row.get(12)?,
            answer_text: row.get(13)?,
            answered_at: row.get(14)?,
        })
    }

    fn into_question(self) -> Result<Question, FlowStoreError> {
        let answer = match (self.answered_at, self.answer_text) {
            (Some(at), Some(text)) => Some(Answer {
                choice: self
                    .answer_choice
                    .map(|c| usize::try_from(c).map_err(storage))
                    .transpose()?,
                text,
                at,
            }),
            _ => None,
        };
        Ok(Question {
            id: QuestionId(self.id),
            flow: FlowId(self.flow_id),
            step: self.step_id.map(StepId),
            role: self.role,
            text: self.text,
            why: self.why,
            unblocks: self.unblocks,
            blocks: self.blocks.map(untag::<Phase>).transpose()?,
            context: from_json::<Vec<ContextLink>>(&self.context)?,
            recommendation: self.recommendation,
            options: from_json(&self.options)?,
            asked_at: self.asked_at,
            answer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_reads_back_as_the_target_it_was_written_as() {
        let flow = FlowId(4);
        for target in [
            InboxTarget::Flow,
            InboxTarget::Step {
                step: StepId(2),
                role: "conventions".to_owned(),
            },
            InboxTarget::Finding {
                finding: FindingId(9),
            },
            InboxTarget::Question {
                question: QuestionId(5),
            },
            InboxTarget::Approval {
                approval: ApprovalId(6),
            },
        ] {
            let (kind, id) = target_key(flow, &target);
            assert_eq!(target_from(kind, id, &target.subject()).unwrap(), target);
        }
        assert_eq!(target_key(flow, &InboxTarget::Flow), ("flow", 4));
    }
}

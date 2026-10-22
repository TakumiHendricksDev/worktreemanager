//! Send to Jira: a question and your answer, posted to the issue as a comment you wrote and approved.
//!
//! # Answers stay here unless you send one
//!
//! A planner's questions and your answers are kept in wtm. Posting one to the tracker is your
//! decision about one exchange: **Send to Jira…** builds a draft ([`draft`]) you can edit, and
//! nothing posts until a second, explicit click sends exactly the text in front of you. That text is
//! the approval's payload, hashed and pinned, so the comment that posts is the one you read.
//!
//! The draft holds the question as it was asked, why it mattered, and your answer — never the
//! discussion you had with the planner first, its progress lines, or anything else it said. Agent
//! chatter is not something a team's tracker should carry, and the draft is the one place it could
//! leak from, so what goes into it is decided here, where a test can hold it.
//!
//! # Reconciled by its text, because the tracker hides nothing
//!
//! A review on GitHub carries an invisible marker the outbox finds it by. A Jira comment cannot: its
//! rich text has no hidden node a marker could live in, and a visible one would be noise in a
//! comment the whole team reads. So a comment is found by what it says. Before sending, and after,
//! the executor lists the issue's comments and looks for one whose text, normalized the way the
//! tracker rewrites it ([`normalized`]), is this payload's. The text is a question and its answer
//! quoted whole, which no one else writes by accident, and the same text approved twice is the same
//! operation by the outbox's own key, so "this text is on the issue" means "this operation
//! happened". `acli` lists comments with their author's display name and no time, so neither an
//! author nor a time window can narrow the match further from here; the author is kept with the
//! result for the window to show.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::question::{Question, QuestionId};
use super::tracker::{TrackerComment, TrackerKind};

/// Exactly what one comment sends: the issue and the text. Its hash is the operation's key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentPayload {
    pub tracker: TrackerKind,
    pub key: String,
    pub body: String,
}

impl CommentPayload {
    /// What the comment writes to, as an operation's target: the tracker and the issue.
    #[must_use]
    pub fn target(&self) -> String {
        format!("{}:{}", self.tracker.as_str(), self.key)
    }
}

/// A comment as its approval keeps it: what is sent, and the question it is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentRequest {
    pub payload: CommentPayload,
    pub question: QuestionId,
}

/// Why a comment cannot be sent as written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommentError {
    #[error("a comment needs some text")]
    Empty,
    #[error("Jira takes at most {MAX_BODY} characters in a comment; this one has {length}")]
    TooLong { length: usize },
    /// `acli` reads a body that is a JSON object as Atlassian Document Format, so text starting
    /// with `{` would post as something other than what you read.
    #[error(
        "a comment cannot start with `{{`: acli would read it as a document rather than as the text you wrote"
    )]
    LooksLikeDocument,
}

/// Jira's limit on a comment's length.
pub const MAX_BODY: usize = 32_767;

/// Whether `body` can be sent as it is written.
///
/// # Errors
///
/// What stops it, in words for the draft.
pub fn check_body(body: &str) -> Result<(), CommentError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(CommentError::Empty);
    }
    let length = trimmed.chars().count();
    if length > MAX_BODY {
        return Err(CommentError::TooLong { length });
    }
    if trimmed.starts_with('{') {
        return Err(CommentError::LooksLikeDocument);
    }
    Ok(())
}

/// The draft **Send to Jira…** opens with: the question, why it mattered, and your answer.
///
/// Nothing the planner said besides its question: see the module docs. `None` while the question
/// has no answer, because a question alone is not what Send to Jira is for.
#[must_use]
pub fn draft(question: &Question) -> Option<String> {
    let answer = question.answer.as_ref()?;
    let mut out = String::new();
    let _ = writeln!(out, "Question: {}", question.text.trim());
    let why = question.why.trim();
    if !why.is_empty() {
        let _ = writeln!(out, "Why it matters: {why}");
    }
    let picked = answer
        .choice
        .and_then(|choice| question.options.get(choice))
        .map(|option| option.trim().to_owned());
    let said = Some(answer.text.trim())
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let reply = match (picked, said) {
        (Some(picked), Some(said)) => format!("{picked}. {said}"),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => return None,
    };
    let _ = write!(out, "Answer: {reply}");
    Some(out)
}

/// `text` as a tracker gives it back: whitespace runs, line breaks and paragraph breaks as one
/// space, and typographic quotes as plain ones. Jira stores plain text as rich text and `acli` turns
/// it back into plain text with its own line breaks, so two spellings of one comment differ in
/// exactly these ways and no other.
#[must_use]
pub fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201c}', '\u{201d}'], "\"")
}

/// The comment on the issue that is this payload, if it is there: see the module docs.
#[must_use]
pub fn find_posted<'a>(
    payload: &CommentPayload,
    comments: &'a [TrackerComment],
) -> Option<&'a TrackerComment> {
    let ours = normalized(&payload.body);
    comments
        .iter()
        .rev()
        .find(|comment| normalized(&comment.body) == ours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::question::Answer;
    use crate::flow::state::FlowId;

    fn question(choice: Option<usize>, text: &str) -> Question {
        Question {
            id: QuestionId(3),
            flow: FlowId(1),
            step: None,
            role: Some("planner".to_owned()),
            text: "CSV or XLSX for the export?".to_owned(),
            why: "The issue says \"spreadsheet\".".to_owned(),
            unblocks: "the plan".to_owned(),
            blocks: None,
            context: Vec::new(),
            recommendation: Some("CSV".to_owned()),
            options: vec!["CSV".to_owned(), "XLSX".to_owned()],
            asked_at: 1,
            answer: Some(Answer {
                choice,
                text: text.to_owned(),
                at: 2,
            }),
        }
    }

    #[test]
    fn the_draft_is_the_question_why_it_matters_and_your_answer() {
        let body = draft(&question(Some(0), "Keep the list's column order.")).unwrap();
        assert_eq!(
            body,
            "Question: CSV or XLSX for the export?\nWhy it matters: The issue says \
             \"spreadsheet\".\nAnswer: CSV. Keep the list's column order."
        );
    }

    #[test]
    fn an_unanswered_question_has_no_draft() {
        let mut open = question(None, "");
        open.answer = None;
        assert_eq!(draft(&open), None);
    }

    #[test]
    fn a_comment_jira_rewrapped_is_still_found_as_the_one_sent() {
        let payload = CommentPayload {
            tracker: TrackerKind::Jira,
            key: "ACME-7".to_owned(),
            body: "Question: CSV or XLSX?\nAnswer: CSV. Don't reorder.".to_owned(),
        };
        let listed = [
            TrackerComment {
                id: "10".to_owned(),
                author: Some("Sam".to_owned()),
                body: "Question: CSV or XLSX? Answer: CSV.".to_owned(),
            },
            TrackerComment {
                id: "11".to_owned(),
                author: Some("You".to_owned()),
                body: "Question: CSV or XLSX?\n\nAnswer: CSV. Don\u{2019}t reorder.\n".to_owned(),
            },
        ];
        assert_eq!(
            find_posted(&payload, &listed).map(|c| c.id.as_str()),
            Some("11")
        );
        assert_eq!(find_posted(&payload, &listed[..1]), None);
    }

    #[test]
    fn a_body_acli_would_read_as_a_document_is_refused() {
        assert_eq!(
            check_body("  {\"type\": \"doc\"}"),
            Err(CommentError::LooksLikeDocument)
        );
        assert_eq!(check_body(" \n "), Err(CommentError::Empty));
        assert!(check_body("Question: x\nAnswer: y").is_ok());
        assert!(matches!(
            check_body(&"x".repeat(MAX_BODY + 1)),
            Err(CommentError::TooLong { .. })
        ));
    }
}

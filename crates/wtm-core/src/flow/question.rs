//! Questions a flow asks you.
//!
//! A question is one object wherever it appears. The issue workspace and the inbox show the same
//! question, so answering it in either place answers it, and the first answer is the one that
//! counts: it is what resumes the flow, and a flow must not resume twice. Answers stay on this
//! machine. Sending one to a tracker is a separate, previewed action.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::state::{FlowId, Phase};

id! {
    /// One question, as the store numbers it.
    QuestionId
}

/// Something a question points at so you can judge it without leaving the inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextLink {
    #[serde(rename_all = "camelCase")]
    File { path: String, line: Option<u32> },
    /// A quoted passage, such as the issue's acceptance criteria.
    Excerpt { source: String, text: String },
    #[serde(rename_all = "camelCase")]
    DesignRevision { prototype: String, revision: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    /// Index into the question's options, when one was picked.
    pub choice: Option<usize>,
    /// The free-text answer, which may stand alone or qualify the choice.
    pub text: String,
    pub at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestionError {
    #[error("this question was already answered")]
    AlreadyAnswered,
    #[error("the question offers {offered} option(s); there is no option {choice}")]
    NoSuchOption { choice: usize, offered: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: QuestionId,
    pub flow: FlowId,
    pub text: String,
    /// Why the answer matters: it changes behaviour, scope or architecture, or it is not asked.
    pub why: String,
    /// The phase the flow cannot enter until this is answered.
    pub blocks: Phase,
    pub context: Vec<ContextLink>,
    pub recommendation: Option<String>,
    pub options: Vec<String>,
    pub asked_at: UnixMs,
    pub answer: Option<Answer>,
}

impl Question {
    /// Record the answer, unless one is already recorded.
    pub fn answer(&self, answer: Answer) -> Result<Self, QuestionError> {
        if self.answer.is_some() {
            return Err(QuestionError::AlreadyAnswered);
        }
        if let Some(choice) = answer.choice
            && choice >= self.options.len()
        {
            return Err(QuestionError::NoSuchOption {
                choice,
                offered: self.options.len(),
            });
        }
        Ok(Self {
            answer: Some(answer),
            ..self.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question() -> Question {
        Question {
            id: QuestionId(1),
            flow: FlowId(1),
            text: "CSV or XLSX for the bulk export?".to_owned(),
            why: "The acceptance criteria say \"spreadsheet\"; existing exports use CSV."
                .to_owned(),
            blocks: Phase::Implementing,
            context: vec![ContextLink::File {
                path: "app/exports.py".to_owned(),
                line: Some(41),
            }],
            recommendation: Some("CSV".to_owned()),
            options: vec!["CSV".to_owned(), "XLSX".to_owned()],
            asked_at: 10,
            answer: None,
        }
    }

    fn picked(choice: usize) -> Answer {
        Answer {
            choice: Some(choice),
            text: String::new(),
            at: 20,
        }
    }

    #[test]
    fn a_question_answered_in_one_place_cannot_be_answered_again_in_another() {
        let answered = question().answer(picked(0)).unwrap();
        assert_eq!(
            answered.answer(picked(1)),
            Err(QuestionError::AlreadyAnswered)
        );
        assert_eq!(answered.answer.unwrap().choice, Some(0));
    }

    #[test]
    fn an_answer_cannot_pick_an_option_the_question_did_not_offer() {
        assert_eq!(
            question().answer(picked(2)),
            Err(QuestionError::NoSuchOption {
                choice: 2,
                offered: 2
            })
        );
    }

    #[test]
    fn a_free_text_answer_needs_no_option() {
        let answered = question()
            .answer(Answer {
                choice: None,
                text: "Both, behind a format picker".to_owned(),
                at: 20,
            })
            .unwrap();
        assert!(answered.answer.is_some());
    }
}

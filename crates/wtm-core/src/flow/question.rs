//! Questions a flow asks you.
//!
//! A question is one object wherever it appears. The issue workspace and the inbox show the same
//! question, so answering it in either place answers it, and the first answer is the one that
//! counts: it is what resumes the flow, and a flow must not resume twice. Answers stay on this
//! machine. Sending one to a tracker is a separate, previewed action.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::run::StepId;
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
    /// A reference that is not a file: a symbol, a rule, a ticket, a URL. Kept as the asker wrote
    /// it, because guessing what a bare word points at would make a link that goes nowhere.
    Mention { text: String },
}

impl ContextLink {
    /// Read one of a role's `refs`: `path:line` or a path is a file, anything else is words.
    ///
    /// A path is something with a `/` or a `.` in it and no spaces, which is what a role writes for
    /// a file and rarely for anything else. A URL has a scheme, and stays words: the window opens
    /// files at a line, and has no business opening a page a model named.
    #[must_use]
    pub fn from_ref(reference: &str) -> Self {
        let text = reference.trim();
        let words = || Self::Mention {
            text: text.to_owned(),
        };
        if text.is_empty() || text.contains(char::is_whitespace) || text.contains("://") {
            return words();
        }
        let (path, line) = match text.rsplit_once(':') {
            Some((path, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => {
                (path, line.parse::<u32>().ok())
            }
            _ => (text, None),
        };
        let looks_like_a_file =
            !path.is_empty() && (path.contains('/') || path.contains('.')) && !path.contains(':');
        if looks_like_a_file {
            Self::File {
                path: path.to_owned(),
                line,
            }
        } else {
            words()
        }
    }
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
    /// Neither an option nor any words: there is nothing to give the asker.
    #[error("an answer picks an option or says something")]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: QuestionId,
    pub flow: FlowId,
    /// The role that asked, when one did, and its step.
    pub step: Option<StepId>,
    pub role: Option<String>,
    pub text: String,
    /// Why the answer matters: it changes behaviour, scope or architecture, or it is not asked.
    pub why: String,
    /// What the asker does once it has the answer, in its own words.
    pub unblocks: String,
    /// The phase the flow cannot enter until this is answered, when the flow waits for it. `None`
    /// when nothing waits: a review role is told to choose a reading and say so, and its run goes
    /// on.
    pub blocks: Option<Phase>,
    pub context: Vec<ContextLink>,
    pub recommendation: Option<String>,
    pub options: Vec<String>,
    pub asked_at: UnixMs,
    pub answer: Option<Answer>,
}

/// A question as it is asked, before the store numbers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewQuestion {
    pub flow: FlowId,
    pub step: Option<StepId>,
    pub role: Option<String>,
    pub text: String,
    pub why: String,
    pub unblocks: String,
    pub blocks: Option<Phase>,
    pub context: Vec<ContextLink>,
    pub recommendation: Option<String>,
    pub options: Vec<String>,
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
        if answer.choice.is_none() && answer.text.trim().is_empty() {
            return Err(QuestionError::Empty);
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
            step: None,
            role: Some("planner".to_owned()),
            text: "CSV or XLSX for the bulk export?".to_owned(),
            why: "The acceptance criteria say \"spreadsheet\"; existing exports use CSV."
                .to_owned(),
            unblocks: "Plan approval, then Implementing".to_owned(),
            blocks: Some(Phase::Implementing),
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
    fn an_answer_with_no_option_and_no_words_is_refused() {
        assert_eq!(
            question().answer(Answer {
                choice: None,
                text: "   ".to_owned(),
                at: 20,
            }),
            Err(QuestionError::Empty)
        );
    }

    #[test]
    fn a_reference_with_a_line_number_becomes_a_link_to_that_line() {
        assert_eq!(
            ContextLink::from_ref("src/totals.py:7"),
            ContextLink::File {
                path: "src/totals.py".to_owned(),
                line: Some(7)
            }
        );
        assert_eq!(
            ContextLink::from_ref(" README.md "),
            ContextLink::File {
                path: "README.md".to_owned(),
                line: None
            }
        );
    }

    #[test]
    fn a_reference_that_is_not_a_file_is_kept_as_the_words_it_was() {
        for words in [
            "Rule R-14",
            "https://example.com/a.html",
            "ExportWriter",
            "",
            "a:b:3",
        ] {
            assert_eq!(
                ContextLink::from_ref(words),
                ContextLink::Mention {
                    text: words.trim().to_owned()
                },
                "{words}"
            );
        }
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

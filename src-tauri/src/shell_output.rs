//! A run ends only when its private PTY boundary and helper outcome both arrive.
//!
//! A byte cursor names exactly the output returned, including after truncation.
//! UTF-8 fragments at the live tail wait for the next read instead of becoming
//! replacement characters which a caller could never repair.

use std::collections::VecDeque;

use serde::Serialize;
use wtm_core::model::ExitOutcome;
use wtm_exec::shell_frames::{FrameFilter, Framing};

const OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Read {
    pub text: String,
    pub bytes_base64: String,
    pub next_cursor: u64,
    pub gap: bool,
    pub complete: bool,
    pub outcome: Option<ExitOutcome>,
    pub problem: Option<String>,
}

#[derive(Debug)]
pub struct Output {
    filter: FrameFilter,
    bytes: VecDeque<u8>,
    next: u64,
    outcome: Option<ExitOutcome>,
    problem: Option<String>,
    limit: usize,
}

impl Output {
    pub fn new(framing: Framing) -> Self {
        Self {
            filter: FrameFilter::new(framing),
            bytes: VecDeque::new(),
            next: 0,
            outcome: None,
            problem: None,
            limit: OUTPUT_LIMIT,
        }
    }

    /// Returns only bytes safe to paint in the ordinary terminal replay.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        let chunk = self.filter.push(bytes);
        if self.problem.is_none() {
            self.next += chunk.captured.len() as u64;
            self.bytes.extend(chunk.captured);
        }
        if self.bytes.len() > self.limit {
            self.bytes.drain(..self.bytes.len() - self.limit);
        }
        chunk.display
    }

    pub fn finish(&mut self, outcome: ExitOutcome) {
        if self.outcome.is_none() && self.problem.is_none() {
            self.outcome = Some(outcome);
        }
    }

    pub fn interrupt(&mut self, problem: &str) {
        if !self.complete() {
            self.problem = Some(problem.into());
            self.outcome = None;
        }
    }

    pub fn complete(&self) -> bool {
        self.problem.is_some() || (self.filter.complete() && self.outcome.is_some())
    }

    pub fn read(&self, cursor: u64) -> Result<Read, String> {
        if cursor > self.next {
            return Err("The output cursor is ahead of this run.".into());
        }
        let first = self.next - self.bytes.len() as u64;
        let start = cursor.max(first);
        let offset = usize::try_from(start - first).map_err(|e| e.to_string())?;
        let mut bytes: Vec<u8> = self.bytes.iter().skip(offset).copied().collect();
        // Keep raw output byte-exact. Only the *display text* is decoded; a gap
        // can begin mid-codepoint and is honestly represented by U+FFFD.
        if !self.complete() {
            let mut valid = 0;
            while valid < bytes.len() {
                match std::str::from_utf8(&bytes[valid..]) {
                    Ok(_) => break,
                    Err(error) => {
                        valid += error.valid_up_to();
                        if let Some(length) = error.error_len() {
                            valid += length;
                        } else {
                            bytes.truncate(valid);
                            break;
                        }
                    }
                }
            }
        }
        Ok(Read {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            bytes_base64: crate::pty_bridge::base64_encode(&bytes),
            next_cursor: start + bytes.len() as u64,
            gap: cursor < first,
            complete: self.complete(),
            outcome: if self.filter.complete() && self.problem.is_none() {
                self.outcome.clone()
            } else {
                None
            },
            problem: self.problem.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_a_stream_boundary_nor_a_control_outcome_alone_claims_completion() {
        for outcome_first in [true, false] {
            let frame = Framing::default();
            let mut output = Output::new(frame.clone());
            output.push(format!("previous{}answer", frame.begin()).as_bytes());
            if outcome_first {
                output.finish(ExitOutcome::Failed { code: 7 });
            } else {
                output.push(frame.end().as_bytes());
            }
            assert!(!output.read(0).unwrap().complete);
            if outcome_first {
                output.push(frame.end().as_bytes());
            } else {
                output.finish(ExitOutcome::Failed { code: 7 });
            }
            output.push(b"later");
            let read = output.read(0).unwrap();
            assert!(read.complete);
            assert_eq!(read.text, "answer");
            assert_eq!(read.outcome, Some(ExitOutcome::Failed { code: 7 }));
        }
    }

    #[test]
    fn a_split_utf8_tail_waits_without_advancing_its_cursor() {
        let frame = Framing::default();
        let mut output = Output::new(frame.clone());
        output.push(frame.begin().as_bytes());
        output.push(&"a日".as_bytes()[..3]);
        let read = output.read(0).unwrap();
        assert_eq!((read.text.as_str(), read.next_cursor), ("a", 1));
        output.push(&"日".as_bytes()[2..]);
        let read = output.read(read.next_cursor).unwrap();
        assert_eq!((read.text.as_str(), read.next_cursor), ("日", 4));
        assert!(!read.gap);
    }

    #[test]
    fn truncation_reports_a_cursor_gap_and_ahead_cursors_are_refused() {
        let frame = Framing::default();
        let mut output = Output::new(frame.clone());
        output.limit = 4;
        output.push(frame.begin().as_bytes());
        output.push(b"123456789");
        let read = output.read(0).unwrap();
        assert_eq!(read.text, "6789");
        assert_eq!(read.next_cursor, 9);
        assert!(read.gap);
        assert!(output.read(10).is_err());
    }

    #[test]
    fn helper_loss_never_turns_partial_framing_into_success() {
        let frame = Framing::default();
        let mut output = Output::new(frame.clone());
        output.push(frame.begin().as_bytes());
        output.push(b"work");
        output.finish(ExitOutcome::Success);
        output.interrupt("The helper disconnected before output settled.");
        output.push(frame.end().as_bytes());
        let read = output.read(0).unwrap();
        assert!(read.complete);
        assert!(read.outcome.is_none());
        assert!(read.problem.is_some());
    }
}

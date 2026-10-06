//! Private stream boundaries separate a run's output from its shell's history.
//!
//! They describe byte ownership only. A matching end record cannot establish an
//! exit status; that must arrive through the helper's authenticated control channel.

#[derive(Clone)]
pub struct Framing {
    begin: String,
    end: String,
}

impl std::fmt::Debug for Framing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Framing").finish_non_exhaustive()
    }
}

impl Default for Framing {
    fn default() -> Self {
        Self {
            begin: format!("\u{1b}]1337;wtm:{}:begin\u{7}", uuid::Uuid::new_v4()),
            end: format!("\u{1b}]1337;wtm:{}:end\u{7}", uuid::Uuid::new_v4()),
        }
    }
}

impl Framing {
    pub fn begin(&self) -> &str {
        &self.begin
    }
    pub fn end(&self) -> &str {
        &self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Before,
    During,
    After,
}

pub struct FrameFilter {
    framing: Framing,
    phase: Phase,
    pending: Vec<u8>,
}

impl std::fmt::Debug for FrameFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameFilter")
            .field("phase", &self.phase)
            .field("pending_bytes", &self.pending.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FrameChunk {
    /// All ordinary terminal bytes, with private framing removed.
    pub display: Vec<u8>,
    /// Only this run's bytes. Never previous history or later prompt/output.
    pub captured: Vec<u8>,
    pub started: bool,
    pub ended: bool,
}

impl FrameFilter {
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            phase: Phase::Before,
            pending: Vec::new(),
        }
    }

    pub fn complete(&self) -> bool {
        self.phase == Phase::After
    }

    pub fn push(&mut self, bytes: &[u8]) -> FrameChunk {
        let mut chunk = FrameChunk::default();
        self.pending.extend_from_slice(bytes);
        loop {
            let expected = match self.phase {
                Phase::Before => self.framing.begin.as_bytes(),
                Phase::During => self.framing.end.as_bytes(),
                Phase::After => {
                    chunk.display.append(&mut self.pending);
                    break;
                }
            };
            if let Some(index) = self
                .pending
                .windows(expected.len())
                .position(|part| part == expected)
            {
                chunk.display.extend_from_slice(&self.pending[..index]);
                if self.phase == Phase::During {
                    chunk.captured.extend_from_slice(&self.pending[..index]);
                }
                self.pending.drain(..index + expected.len());
                match self.phase {
                    Phase::Before => {
                        self.phase = Phase::During;
                        chunk.started = true;
                    }
                    Phase::During => {
                        self.phase = Phase::After;
                        chunk.ended = true;
                    }
                    Phase::After => unreachable!(),
                }
            } else {
                // Only a suffix that could become the record is held between PTY
                // reads. A command printing megabytes without a record stays bounded.
                let keep = (1..expected.len())
                    .rev()
                    .find(|&len| self.pending.ends_with(&expected[..len]))
                    .unwrap_or(0);
                let ready = self.pending.len() - keep;
                chunk.display.extend_from_slice(&self.pending[..ready]);
                if self.phase == Phase::During {
                    chunk.captured.extend_from_slice(&self.pending[..ready]);
                }
                self.pending.drain(..ready);
                break;
            }
        }
        chunk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_records_exclude_previous_and_later_output_without_damaging_utf8() {
        let frame = Framing::default();
        let body = "first\n日本語\nlast\n";
        let stream = format!(
            "old prompt{}{}{}next prompt",
            frame.begin(),
            body,
            frame.end()
        );
        for size in 1..=stream.len() {
            let mut parser = FrameFilter::new(frame.clone());
            let mut display = Vec::new();
            let mut captured = Vec::new();
            let (mut starts, mut ends) = (0, 0);
            for bytes in stream.as_bytes().chunks(size) {
                let chunk = parser.push(bytes);
                display.extend(chunk.display);
                captured.extend(chunk.captured);
                starts += usize::from(chunk.started);
                ends += usize::from(chunk.ended);
            }
            assert_eq!(captured, body.as_bytes());
            assert_eq!(display, format!("old prompt{body}next prompt").as_bytes());
            assert_eq!((starts, ends), (1, 1));
            assert!(parser.complete());
        }
    }

    #[test]
    fn forged_records_and_large_output_cannot_finish_a_run_or_grow_pending_storage() {
        let frame = Framing::default();
        let forged = Framing::default();
        let mut parser = FrameFilter::new(frame.clone());
        assert!(parser.push(frame.begin().as_bytes()).started);
        let output = format!(
            "{}{}{}",
            "x".repeat(1024 * 1024),
            forged.end(),
            frame.begin()
        );
        let result = parser.push(output.as_bytes());
        assert_eq!(result.captured, output.as_bytes());
        assert!(!result.ended);
        assert!(!parser.complete());
        assert!(parser.pending.len() < frame.end().len());
        assert!(parser.push(frame.end().as_bytes()).ended);
        assert!(parser.push(b"background descendant").captured.is_empty());
    }

    #[test]
    fn a_partial_end_record_never_claims_complete_output() {
        let frame = Framing::default();
        let mut parser = FrameFilter::new(frame.clone());
        parser.push(frame.begin().as_bytes());
        parser.push(&frame.end().as_bytes()[..frame.end().len() - 1]);
        assert!(!parser.complete());
    }
}

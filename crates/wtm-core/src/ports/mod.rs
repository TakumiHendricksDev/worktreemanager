//! Ports: the traits through which the domain reaches the outside world.
//!
//! # Every port is synchronous, on purpose
//!
//! `git`, a PTY's `Read`/`Write`, and `child.wait()` are blocking syscalls. The
//! shape at the edge is:
//!
//! ```text
//! #[tauri::command]
//! async fn create(..) { spawn_blocking(move || pipeline.execute(..)).await }
//! ```
//!
//! Given that, making these traits async would buy nothing and cost a lot:
//! `#[async_trait]` boxing, `Send + Sync + 'static` bounds spreading through every
//! closure, and fakes that need a runtime to test. Synchronous trait objects are
//! object-safe, trivially fakeable, and testable with a plain `#[test]`.
//!
//! PTY streaming does need concurrency — but it needs *threads*, not tasks, since
//! a pty reader is a blocking `Read`. One OS thread per session is the right
//! primitive for a handful of terminals.
//!
//! # Interface segregation
//!
//! These are deliberately several narrow traits rather than one `Platform`
//! god-trait, so a test that only needs to fake time doesn't have to stub out a
//! process runner.

pub mod clock;
pub mod config;
pub mod exec;
pub mod fs;
pub mod git;
pub mod progress;
pub mod pty;
pub mod template;

pub use clock::Clock;
pub use config::{ConfigStore, TrustDecision};
pub use exec::{CancelToken, CommandRunner, Invocation, Output};
pub use fs::FileStore;
pub use git::{AddOptions, BranchFilter, Git};
pub use progress::{ProgressEvent, ProgressSink};
pub use pty::{PtyHost, PtySession, PtySink, Spawned};
pub use template::TemplateEngine;

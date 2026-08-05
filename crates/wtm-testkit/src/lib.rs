//! Test doubles and fixtures, shared across the workspace.
//!
//! # Why this is a crate and not `#[cfg(test)]` code
//!
//! Rust cannot share test-only modules across crate boundaries. These fakes are
//! needed by `wtm-core`'s use-case tests, `wtm-config`'s snapshot tests, `wtm-git`'s
//! adapter tests and `src-tauri`'s integration tests alike — so either they live in
//! one `publish = false` crate, or they get copy-pasted four times and drift.
//!
//! # Fakes versus a real binary
//!
//! Both, deliberately, and the split matters:
//!
//! - **Logic** — argv construction, dedup rules, error mapping, pipeline ordering —
//!   is tested against the fakes in [`fakes`]. Fast, deterministic, no repository.
//! - **Grammar and behaviour** — what `git` actually prints and does — is tested
//!   against a real `git` binary in a temporary directory via [`GitFixture`].
//!   Mocking git's output to test our parsing of git's output would only test the
//!   mock.

#![allow(clippy::missing_panics_doc)]

pub mod fakes;
pub mod fixture;

pub use fakes::{
    FakeClock, FakeFileStore, FakeGit, FakePipe, FakePty, FakeRunner, NullPipeSink, NullPtySink,
    RecordedProgress,
};
pub use fixture::GitFixture;

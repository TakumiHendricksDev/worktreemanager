//! Agent CLI protocol adapters.
//!
//! One module per provider. Each one knows three things and nothing else: the argv that starts
//! its CLI in the right mode, the frames to write for a handshake and a turn, and how to map
//! that CLI's wire events onto [`AgentEvent`](wtm_core::model::AgentEvent).
//!
//! # What this crate deliberately does not do
//!
//! It does not spawn. It is handed a [`PipeHost`](wtm_core::ports::pipe::PipeHost), the way
//! `wtm-git` is handed a `CommandRunner`, so its `Cargo.toml` is the proof: no `wtm-exec`
//! dependency means no process can be started from here, and every mapping test runs against
//! `FakePipe` with no child in sight.
//!
//! It also holds no session state that the UI owns. Which panes exist, which is focused, and
//! which worktree they belong to are frontend concepts that live in `src-tauri` — the same call
//! `App::shells` records for the terminal dock, and for the same reason.
//!
//! # Why a compiled module and not TOML
//!
//! wtm's central claim is that *project*-specific behaviour is data. A provider's wire protocol
//! is not project-specific — it is machine-wide, like the `openers.rs` catalogue, which
//! ARCHITECTURE deliberately compiles in and says so. Two further reasons it could not be data
//! even if that claim were stretched:
//!
//! - Claude Code carries a **second logical channel** on the same stdio pair —
//!   `control_request`/`control_response` with its own id namespace, interleaved with the
//!   message stream. Expressing "read this, correlate it by that field, reply on the same pipe
//!   with this shape" in TOML is a programming language, which is the plugin host
//!   ARCHITECTURE §9 rejects.
//! - The program name has to be trustworthy. If a repository's `wtm.toml` could set it, the
//!   word "Claude" in wtm's own UI would be whatever a file in someone's branch said it was.
//!
//! So the honest cost of a third provider is **one module and one catalogue entry, roughly
//! 300–600 lines** — not zero. Saying that is better than a "no code changes" claim the third
//! provider quietly falsifies. What genuinely is data is everything a repo or a user would
//! want to vary: which providers are offered, the model, the effort, the mode, extra argv, env,
//! and MCP servers.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod catalogue;
pub mod claude;
pub mod codex;
pub mod provider;
pub mod session;

pub use catalogue::{CATALOGUE, ProviderEntry, entry};
pub use provider::{Provider, ProviderId, SessionRequest, Step};
pub use session::AgentSession;

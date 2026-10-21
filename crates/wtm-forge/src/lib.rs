//! The [`Forge`](wtm_core::ports::Forge) port, over the user's own GitHub CLI.
//!
//! # Why `gh`, and not an API client
//!
//! ARCHITECTURE §6a: workflow egress goes through the tools the user already has, authenticated
//! by the credentials those tools already hold. `gh` keeps its token in the keychain, honours
//! `GH_HOST`, Enterprise hosts and the user's proxy, and is what they would run in a terminal; wtm
//! stores no token and links no HTTP client. The cost is a process per question and a JSON shape
//! that is `gh`'s rather than the API's, which is what [`parse`] is for.
//!
//! # Reads only, and visibly so
//!
//! Every argv here is `gh --version`, `gh auth status`, `gh pr view`, `gh pr list`,
//! `gh repo view` or `gh api --method GET`. `tests/gh.rs` asserts that of every call the adapter
//! makes, so a write (a `POST`, a field flag, a `pr review`) cannot arrive by accident in this
//! crate: the write side is a later item with approvals of its own.
//!
//! # Tolerant parsing
//!
//! `gh` adds fields, and a forge adds states. Anything this build does not know is carried as
//! "unknown" rather than refused, so a new check conclusion never stops a review. What *is*
//! refused is a reply missing what a review cannot do without: the head commit.
//!
//! # Version and sign-in
//!
//! [`GhForge::check`] runs before the first read of an action. It refuses a `gh` older than
//! [`MINIMUM_VERSION`], the first release whose `auth status` answers in JSON, so "upgrade gh" is
//! the message rather than an unknown-flag error; and it refuses a signed-out host, so "sign in" is
//! the message rather than a parse error from a CLI that printed a login prompt.

mod gh;
mod parse;

pub use gh::{GhForge, MINIMUM_VERSION, PROGRAM};

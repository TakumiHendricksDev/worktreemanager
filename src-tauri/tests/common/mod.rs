//! What more than one test file needs. Not a test target itself: Cargo builds only the files
//! directly under `tests/` as tests.

pub(crate) mod agents;
pub(crate) mod github;
pub(crate) mod pull_requests;

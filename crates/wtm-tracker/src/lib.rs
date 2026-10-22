//! The [`Tracker`](wtm_core::ports::Tracker) port, over the user's own Atlassian CLI.
//!
//! # Why `acli`, and not an API client
//!
//! ARCHITECTURE §6a, as for `gh`: workflow egress goes through the tools the user already has,
//! authenticated by the sign-in those tools already hold. `acli` keeps its OAuth sign-in to itself
//! and is what the user would run in a terminal; wtm stores no Atlassian token and links no HTTP
//! client. The cost is a process per question, and a narrower window onto the tracker than its API
//! gives, recorded below because it decides what the Work view can show.
//!
//! # Reads only
//!
//! Every argv this crate builds is one of `acli --version`, `acli jira auth status`,
//! `acli jira workitem search`, `acli jira workitem view`, `acli jira board list-sprints`,
//! `acli jira sprint view` and `acli jira sprint list-workitems`; `tests/acli.rs` holds the adapter
//! to that list. [`TrackerWriter`](wtm_core::ports::TrackerWriter), the port that would move an
//! issue or comment on one, is not implemented here, so no code in this build can write to a
//! tracker.
//!
//! # What acli 1.3.18 can and cannot read (measured 2026-10-21 against a Jira Cloud site)
//!
//! | Need | How | Gap |
//! |---|---|---|
//! | Queue search | `workitem search --jql --fields --limit --paginate --json`: one JSON array | `--fields` takes only `key, summary, status, assignee, issuetype, priority, labels, reporter, description, creator`; custom fields, `issuelinks`, `parent`, `updated` and `created` are refused ("field '…' is not allowed") |
//! | Sprint membership | `sprint list-workitems --sprint --board --jql --paginate --json`: one JSON document *per page*, printed one after another | Returns only `summary, status, assignee, issuetype, priority`, whatever `--fields` asks for; needs the board id |
//! | Sprint name, goal, dates | `board list-sprints --id --state --json`, `sprint view --id --json` | None: the goal is there, an empty string when unset |
//! | One issue whole | `workitem view KEY --fields … --json`: any field, custom ones, `issuelinks`, `parent`, `updated` | One key per process |
//! | Which field holds sprints | Nothing: `names`/`schema` expansions are always `null` | The id differs per site, so it is config (`[workflow.fields] sprint`) |
//! | Transitions | Nothing lists them; `workitem transition --key --status` moves by status name | [`list`](wtm_core::ports::Tracker::transitions) is `Unsupported`; a move does not need it |
//! | Sign-in | `jira auth status`: text, not JSON | Parsed from its `✓ Authenticated` and `Site:` lines |
//!
//! None of the gaps is essential to a sprint queue and board: membership and the sprint's goal
//! come from the board, a mapped field is read when an issue is opened, and a query can filter on a
//! custom field server-side without reading it. What would need another route is a queue whose rows
//! show a custom field: that is Jira's REST search with an API token, which is a credential wtm
//! would have to store, and so a decision of its own rather than part of this crate.
//!
//! # Tolerant parsing
//!
//! `acli` adds fields and a tracker adds states. Anything this build does not know becomes the
//! domain's "unknown" or is left out; what is refused is an issue without a key or a status. Output
//! is read from its first `{` or `[`, as a run of JSON documents, because `--paginate` prints one
//! per page and an update notice may come before them.
//!
//! # Version and sign-in
//!
//! [`AcliTracker`]'s `check` runs before the first read of an action. It refuses an `acli` older
//! than [`MINIMUM_VERSION`], the release these replies were recorded from, so "upgrade acli" is the
//! message rather than an unknown-flag error from a CLI that predates a command; and it refuses a
//! CLI that is not signed in, so "sign in" is the message rather than a parse error.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod acli;
mod parse;

pub use acli::{AcliTracker, MINIMUM_VERSION, PROGRAM};

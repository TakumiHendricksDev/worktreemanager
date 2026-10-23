//! What an issue tracker says about the work a project's queue holds, and how a project's own
//! status names map onto the workflow's.
//!
//! # Team state, mirrored and read only
//!
//! The tracker owns an issue's status, its assignee and its sprint (plan §4.1). wtm reads them,
//! keeps the last reading in the app database as a mirror, and shows it. Nothing here moves an
//! issue: that is [`TrackerWriter`](crate::ports::TrackerWriter)'s, a separate port only the
//! outbox's executor holds, for the moves Start Work and the board ask for ([`super::moves`]), and
//! a sync has no route to a flow at all (`usecase::tracker_sync` says how that is held).
//!
//! # A project's words, not wtm's
//!
//! "In Progress", "Needs Reviewed" and "Done/Completed" are one team's names. ARCHITECTURE §1 says
//! wtm knows tools, never projects, so every status name and custom field id here arrives from the
//! project's `[workflow]` table, and nothing in code compares against one. The tracker's own
//! [`StatusCategory`], which every workflow on a Jira site shares, is the only fixed vocabulary.
//!
//! # Tolerant by construction
//!
//! A tracker adds fields, states and categories. Anything this build does not know becomes
//! `Unknown` or `None` rather than an error, so a new sprint state never empties the queue. What
//! is refused is an issue without the two things a row cannot do without: its key and its status.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::recipe::WorkflowError;

/// Which tracker a project's workflow reads, as `[workflow] tracker` names it.
///
/// One variant, for the reason [`ForgeKind`](super::ForgeKind) gives: naming it is the opt-in, so a
/// project that has not said it uses Jira causes no Jira call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerKind {
    Jira,
}

impl TrackerKind {
    /// How work items and the mirror name it: the config's own spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jira => "jira",
        }
    }

    /// The config spelling back, for a value read from the store.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        (text == "jira").then_some(Self::Jira)
    }
}

/// Who the tracker's CLI is signed in as, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerAccount {
    /// The site the CLI talks to, such as `acme.atlassian.net`.
    pub site: String,
    pub login: Option<String>,
    /// The CLI's version as it printed it. `None` when unreadable, which is no reason to refuse.
    pub version: Option<String>,
}

/// The tracker's own grouping of a status. Every workflow on a site maps its statuses onto these,
/// which is why a board can fall back on them for a status the project has not mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusCategory {
    ToDo,
    InProgress,
    Done,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueStatus {
    /// The team's name for it, exactly as the tracker spells it.
    pub name: String,
    pub category: StatusCategory,
}

/// One issue, as a search returns it: what a queue row and a board card show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub key: String,
    pub summary: String,
    pub status: IssueStatus,
    /// The issue's type as the tracker names it: Story, Bug, Task.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    /// The assignee's display name. Not their account id or address: the queue shows a name, and
    /// the mirror keeps no more about a colleague than that.
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

/// Another issue this one links to, and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLink {
    /// The relation as read from this issue's side, such as "blocks" or "is blocked by".
    pub relation: String,
    pub key: String,
    pub summary: Option<String>,
    pub status: Option<IssueStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueParent {
    pub key: String,
    pub summary: Option<String>,
}

/// A field the project mapped in `[workflow.fields]`, with its value as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueField {
    /// The project's name for it: the key in `[workflow.fields]`.
    pub name: String,
    /// The tracker's id for it, such as `customfield_12345`.
    pub id: String,
    /// `None` when the issue has no value, or the tracker did not return the field.
    pub value: Option<String>,
}

/// One issue read whole: what the issue workspace shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueDetail {
    pub issue: Issue,
    /// Plain text. A tracker's rich text keeps its words and paragraphs and loses its formatting,
    /// which is all a workspace that renders no markup can use.
    pub description: Option<String>,
    pub parent: Option<IssueParent>,
    pub links: Vec<IssueLink>,
    /// When the tracker last changed it, as the tracker wrote the time.
    pub updated: Option<String>,
    /// The project's mapped fields, in `[workflow.fields]` order, the sprint field excepted.
    pub fields: Vec<IssueField>,
    /// The sprints the issue is in, from the field `[workflow.fields] sprint` names. Empty when that
    /// is not mapped: which field holds sprints differs per site, so wtm cannot guess it.
    pub sprints: Vec<Sprint>,
}

/// One comment on an issue, as the tracker lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerComment {
    /// The tracker's id for it.
    pub id: String,
    /// Who wrote it, as the tracker names them: a display name, not an account id.
    pub author: Option<String>,
    /// Plain text. As with a description, a tracker's rich text keeps its words and loses its
    /// formatting, and mentions may come back without the name they mentioned.
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SprintState {
    Active,
    Future,
    Closed,
    Unknown,
}

impl SprintState {
    /// The tracker's word for it, which is also what its CLI takes as a filter.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Future => "future",
            Self::Closed => "closed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sprint {
    pub id: u64,
    pub name: String,
    pub state: SprintState,
    /// `None` for an empty goal: a tracker stores "no goal" as an empty string.
    pub goal: Option<String>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    /// The board the sprint was created on.
    pub board: Option<u64>,
}

/// A sprint on a board: what a search is narrowed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SprintRef {
    pub board: u64,
    pub sprint: u64,
}

/// A search: the project's query, optionally inside one sprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueQuery {
    /// The tracker's own query language, passed through untouched.
    pub query: String,
    pub sprint: Option<SprintRef>,
    /// At most this many issues come back.
    pub limit: u32,
}

/// A transition an issue's workflow offers from where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transition {
    pub id: String,
    pub name: String,
    /// The status it leads to.
    pub to: Option<String>,
}

/// The key in `[workflow.fields]` that names the field holding an issue's sprints.
pub const SPRINT_FIELD: &str = "sprint";

/// How many issues a queue holds when its config does not say. A sprint is tens of issues; a
/// query that matches thousands is a mistake, and showing the first few hundred says so without
/// spending a minute of the tracker's time on each focus.
pub const DEFAULT_QUEUE_LIMIT: u32 = 200;

/// The most a queue may ask for: past this a sync is a bulk export, not a refresh.
pub const MAX_QUEUE_LIMIT: u32 = 1000;

/// `[workflow.queue]`: what fills the Work view's queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueSpec {
    /// The tracker's own query language, such as Jira's JQL. Passed through as written.
    pub query: String,
    /// A board whose active sprint scopes the queue, and whose sprint's name and goal the queue's
    /// header shows. Without one the query alone is the queue and no sprint is shown.
    #[serde(default)]
    pub board: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl QueueSpec {
    #[must_use]
    pub fn limit(&self) -> u32 {
        self.limit.unwrap_or(DEFAULT_QUEUE_LIMIT)
    }
}

/// A role a status plays in the workflow, whatever the team calls it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusRole {
    /// Not started.
    Todo,
    /// Being worked on. Start Work moves an issue to the first of these.
    Started,
    /// Waiting for review. Publishing moves an issue to the first of these.
    NeedsReview,
    Done,
}

impl StatusRole {
    pub const ALL: [Self; 4] = [Self::Todo, Self::Started, Self::NeedsReview, Self::Done];

    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Started => "started",
            Self::NeedsReview => "needs_review",
            Self::Done => "done",
        }
    }
}

/// `[workflow.statuses]`: the project's status names, grouped by the role each plays.
///
/// Each role takes one name or a list, so `started = "In Progress"` and
/// `done = ["Done", "Accepted"]` both read. The board's columns are these names, in role order and
/// then in the order written; an issue whose status none of them names is still shown, in a column
/// of its own, so a status the team adds later is visible rather than silently dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusMap {
    #[serde(default, deserialize_with = "one_or_many")]
    pub todo: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub started: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub needs_review: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub done: Vec<String>,
}

/// One board column: a mapped status and the role it plays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusColumn {
    pub role: StatusRole,
    pub status: String,
}

impl StatusMap {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        StatusRole::ALL
            .iter()
            .all(|role| self.names(*role).is_empty())
    }

    #[must_use]
    pub fn names(&self, role: StatusRole) -> &[String] {
        match role {
            StatusRole::Todo => &self.todo,
            StatusRole::Started => &self.started,
            StatusRole::NeedsReview => &self.needs_review,
            StatusRole::Done => &self.done,
        }
    }

    /// The board's columns: every mapped name, in role order.
    #[must_use]
    pub fn columns(&self) -> Vec<StatusColumn> {
        StatusRole::ALL
            .iter()
            .flat_map(|role| {
                self.names(*role).iter().map(|status| StatusColumn {
                    role: *role,
                    status: status.clone(),
                })
            })
            .collect()
    }

    /// The column an issue in `status` sits in. Compared as the tracker compares names: ignoring
    /// case and the spaces around them, so `"in progress"` in a config still finds "In Progress".
    #[must_use]
    pub fn column_of(&self, status: &str) -> Option<usize> {
        self.columns()
            .iter()
            .position(|column| same_status(&column.status, status))
    }

    #[must_use]
    pub fn role_of(&self, status: &str) -> Option<StatusRole> {
        StatusRole::ALL.into_iter().find(|role| {
            self.names(*role)
                .iter()
                .any(|name| same_status(name, status))
        })
    }

    /// The status a move to `role` would ask for: the first name written for it.
    #[must_use]
    pub fn target(&self, role: StatusRole) -> Option<&str> {
        self.names(role).first().map(String::as_str)
    }

    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        let mut seen: Vec<(&str, StatusRole)> = Vec::new();
        for role in StatusRole::ALL {
            for name in self.names(role) {
                let key = format!("statuses.{}", role.key());
                if name.trim().is_empty() {
                    return Err(WorkflowError::new(key, "a status name cannot be empty"));
                }
                if let Some((_, other)) = seen.iter().find(|(seen, _)| same_status(seen, name)) {
                    return Err(WorkflowError::new(
                        key,
                        format!(
                            "\"{name}\" is already mapped to `{}`: a status plays one role, or \
                             the board could not tell which column an issue in it belongs in",
                            other.key()
                        ),
                    ));
                }
                seen.push((name, role));
            }
        }
        Ok(())
    }
}

/// Whether two status names are the same status, as the board places an issue in a column:
/// ignoring case and the spaces around them.
#[must_use]
pub fn same_status(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// A string or a list of strings, as a list.
fn one_or_many<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(name) => vec![name],
        OneOrMany::Many(names) => names,
    })
}

/// Check `[workflow.fields]`: each id goes into a comma-separated field list on a command line, so
/// one that holds a comma or a space would quietly ask for something else.
pub(crate) fn validate_fields(fields: &BTreeMap<String, String>) -> Result<(), WorkflowError> {
    for (name, id) in fields {
        if name.trim().is_empty() {
            return Err(WorkflowError::new("fields", "a field needs a name"));
        }
        let usable = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if !usable {
            return Err(WorkflowError::new(
                format!("fields.{name}"),
                format!(
                    "`{id}` is not a field id; write the tracker's own id, such as \
                     `customfield_12345`"
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_queue(queue: &QueueSpec) -> Result<(), WorkflowError> {
    if queue.query.trim().is_empty() {
        return Err(WorkflowError::new(
            "queue.query",
            "the queue needs a query, such as `assignee = currentUser() AND statusCategory != Done`",
        ));
    }
    match queue.limit {
        Some(0) => Err(WorkflowError::new(
            "queue.limit",
            "a queue of no issues is not a queue",
        )),
        Some(limit) if limit > MAX_QUEUE_LIMIT => Err(WorkflowError::new(
            "queue.limit",
            format!("at most {MAX_QUEUE_LIMIT}: past that a sync is an export, not a refresh"),
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statuses(toml: &str) -> StatusMap {
        serde_json::from_value(toml_like(toml)).unwrap()
    }

    /// The handful of shapes these tests need, without a TOML parser in core's dependencies: each
    /// line is `role = "name"` or `role = ["a", "b"]`.
    fn toml_like(text: &str) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let (key, value) = line.split_once('=').unwrap();
            let value = value.trim();
            let parsed = if value.starts_with('[') {
                serde_json::from_str(value).unwrap()
            } else {
                serde_json::Value::String(value.trim_matches('"').to_owned())
            };
            map.insert(key.trim().to_owned(), parsed);
        }
        serde_json::Value::Object(map)
    }

    #[test]
    fn a_role_takes_one_status_name_or_a_list_of_them() {
        let map = statuses(
            "todo = [\"Backlog\", \"Ready\"]\nstarted = \"In Progress\"\ndone = [\"Done\"]",
        );
        assert_eq!(map.todo, ["Backlog", "Ready"]);
        assert_eq!(map.started, ["In Progress"]);
        assert!(map.needs_review.is_empty());
    }

    #[test]
    fn the_board_columns_follow_the_workflow_roles_and_then_the_order_written() {
        let map = statuses(
            "done = [\"Done\", \"Accepted\"]\nneeds_review = \"In Review\"\ntodo = \"Ready\"\nstarted = \"Doing\"",
        );
        let columns: Vec<_> = map
            .columns()
            .into_iter()
            .map(|c| (c.role, c.status))
            .collect();
        assert_eq!(
            columns,
            [
                (StatusRole::Todo, "Ready".to_owned()),
                (StatusRole::Started, "Doing".to_owned()),
                (StatusRole::NeedsReview, "In Review".to_owned()),
                (StatusRole::Done, "Done".to_owned()),
                (StatusRole::Done, "Accepted".to_owned()),
            ]
        );
    }

    #[test]
    fn a_status_is_found_whatever_case_the_config_wrote_it_in() {
        let map = statuses("started = \"in progress\"\ndone = [\"Done\", \"Accepted\"]");
        assert_eq!(map.column_of("In Progress"), Some(0));
        assert_eq!(map.column_of(" Accepted "), Some(2));
        assert_eq!(map.role_of("ACCEPTED"), Some(StatusRole::Done));
    }

    #[test]
    fn a_status_nobody_mapped_has_no_column_and_no_role() {
        let map = statuses("started = \"In Progress\"");
        assert_eq!(map.column_of("Triage"), None);
        assert_eq!(map.role_of("Triage"), None);
    }

    #[test]
    fn a_move_asks_for_the_first_name_written_for_its_role() {
        let map = statuses("done = [\"Done\", \"Accepted\"]");
        assert_eq!(map.target(StatusRole::Done), Some("Done"));
        assert_eq!(map.target(StatusRole::Started), None);
    }

    #[test]
    fn one_status_mapped_to_two_roles_is_refused_naming_both() {
        let map = statuses("started = \"Doing\"\ndone = [\"doing\"]");
        let err = map.validate().unwrap_err();
        assert_eq!(err.key, "statuses.done");
        assert!(err.message.contains("`started`"), "{}", err.message);
    }

    #[test]
    fn an_empty_status_name_is_refused() {
        let map = statuses("todo = [\"\"]");
        assert_eq!(map.validate().unwrap_err().key, "statuses.todo");
    }

    #[test]
    fn a_field_id_that_would_split_a_command_line_field_list_is_refused() {
        let fields = BTreeMap::from([("owner".to_owned(), "customfield_1, summary".to_owned())]);
        assert_eq!(validate_fields(&fields).unwrap_err().key, "fields.owner");
        let fields = BTreeMap::from([("owner".to_owned(), "customfield_12345".to_owned())]);
        assert!(validate_fields(&fields).is_ok());
    }

    #[test]
    fn a_queue_needs_a_query_and_a_sensible_limit() {
        let queue = |query: &str, limit| QueueSpec {
            query: query.to_owned(),
            board: None,
            limit,
        };
        assert_eq!(
            validate_queue(&queue("  ", None)).unwrap_err().key,
            "queue.query"
        );
        assert_eq!(
            validate_queue(&queue("assignee = currentUser()", Some(0)))
                .unwrap_err()
                .key,
            "queue.limit"
        );
        assert!(
            validate_queue(&queue(
                "assignee = currentUser()",
                Some(MAX_QUEUE_LIMIT + 1)
            ))
            .is_err()
        );
        assert_eq!(queue("x", None).limit(), DEFAULT_QUEUE_LIMIT);
    }

    #[test]
    fn the_tracker_is_named_as_the_config_spells_it() {
        assert_eq!(TrackerKind::Jira.as_str(), "jira");
        assert_eq!(TrackerKind::parse("jira"), Some(TrackerKind::Jira));
        assert_eq!(TrackerKind::parse("Jira"), None);
    }
}

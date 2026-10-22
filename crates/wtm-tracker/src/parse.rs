//! `acli`'s replies, read tolerantly.
//!
//! Each reader takes the text `acli` printed and returns a domain value, or a sentence saying what
//! was missing. A field this build does not read is ignored, a value it does not recognise becomes
//! the domain's "unknown", and an absent one becomes `None`. Only an issue's key and status are
//! required, because a row without either cannot be drawn on a board.

use std::collections::BTreeMap;

use serde_json::Value;
use wtm_core::flow::{
    Issue, IssueDetail, IssueField, IssueLink, IssueParent, IssueStatus, SPRINT_FIELD, Sprint,
    SprintState, StatusCategory, TrackerComment,
};

/// `acli version 1.3.18-stable` → `(1, 3, 18)`. Only the first line is read: an outdated `acli`
/// follows it with an upgrade notice.
pub(crate) fn version(printed: &str) -> Option<(u64, u64, u64)> {
    let line = printed.lines().next()?;
    let word = line
        .split_whitespace()
        .skip_while(|word| *word != "version")
        .nth(1)?;
    let mut parts = word.trim_start_matches('v').split(['.', '-']).map(|part| {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u64>().ok()
    });
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// The site and login `acli jira auth status` reports, or why it reports none.
///
/// The reply is text, not JSON: a `✓ Authenticated` line, then `Site:` and `Email:` lines. Anything
/// without both the mark and the site is not signed in, whatever else it says, because "not
/// authenticated" contains "authenticated" too.
pub(crate) fn signed_in(printed: &str) -> Result<(String, Option<String>), String> {
    let authenticated = printed.lines().any(|line| {
        let line = line.trim();
        line.starts_with('✓') && line.to_ascii_lowercase().contains("authenticated")
    });
    let field = |name: &str| {
        printed.lines().find_map(|line| {
            let (key, value) = line.trim().split_once(':')?;
            (key.trim().eq_ignore_ascii_case(name) && !value.trim().is_empty())
                .then(|| value.trim().to_owned())
        })
    };
    if let (true, Some(site)) = (authenticated, field("Site")) {
        return Ok((site, field("Email")));
    }
    let first = printed
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("acli printed nothing");
    Err(first.trim_start_matches('✗').trim().to_owned())
}

/// Every JSON document in `printed`, in order, read from the first `{` or `[`.
///
/// `--paginate` prints one document per page, one after the other, and anything before the first
/// bracket (an upgrade notice) is not part of the answer. Reading stops at the first thing that is
/// not JSON after at least one document has been read.
pub(crate) fn documents(printed: &str) -> Result<Vec<Value>, String> {
    let start = printed
        .find(['{', '['])
        .ok_or_else(|| "acli printed no JSON".to_owned())?;
    let mut documents = Vec::new();
    for document in serde_json::Deserializer::from_str(&printed[start..]).into_iter::<Value>() {
        match document {
            Ok(value) => documents.push(value),
            Err(err) if documents.is_empty() => return Err(err.to_string()),
            Err(_) => break,
        }
    }
    Ok(documents)
}

/// The issues of a search (one array) or of a sprint listing (one `{ "issues": [...] }` per page).
pub(crate) fn issues(printed: &str) -> Result<Vec<Issue>, String> {
    let mut issues = Vec::new();
    for document in documents(printed)? {
        let page = match &document {
            Value::Array(items) => items.as_slice(),
            Value::Object(_) => document["issues"].as_array().map_or(&[][..], Vec::as_slice),
            _ => &[],
        };
        for item in page {
            issues.push(issue(item)?);
        }
    }
    Ok(issues)
}

/// An issue's comments: `{ "comments": [...] }` per page, pages one after the other, oldest first.
///
/// `acli` gives each body as plain text on one line, its author as a display name, and no time.
pub(crate) fn comments(printed: &str) -> Result<Vec<TrackerComment>, String> {
    let mut comments = Vec::new();
    for document in documents(printed)? {
        let page = document["comments"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        for item in page {
            let id = match &item["id"] {
                Value::String(id) => id.clone(),
                Value::Number(id) => id.to_string(),
                _ => return Err("a comment has no id".to_owned()),
            };
            comments.push(TrackerComment {
                author: text(item, "author"),
                body: item["body"].as_str().unwrap_or_default().to_owned(),
                id,
            });
        }
    }
    Ok(comments)
}

fn issue(value: &Value) -> Result<Issue, String> {
    let key = text(value, "key").ok_or("an issue has no key")?;
    let fields = &value["fields"];
    let status = status(&fields["status"]).ok_or_else(|| format!("{key} has no status"))?;
    Ok(Issue {
        summary: text(fields, "summary").unwrap_or_default(),
        status,
        kind: text(&fields["issuetype"], "name"),
        priority: text(&fields["priority"], "name"),
        assignee: text(&fields["assignee"], "displayName"),
        labels: fields["labels"]
            .as_array()
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|label| label.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        key,
    })
}

fn status(value: &Value) -> Option<IssueStatus> {
    Some(IssueStatus {
        name: text(value, "name")?,
        category: match value["statusCategory"]["key"].as_str() {
            Some("new") => StatusCategory::ToDo,
            Some("indeterminate") => StatusCategory::InProgress,
            Some("done") => StatusCategory::Done,
            _ => StatusCategory::Unknown,
        },
    })
}

/// A board's sprints: `{ "sprints": [...] }` per page.
pub(crate) fn sprints(printed: &str) -> Result<Vec<Sprint>, String> {
    let mut sprints = Vec::new();
    for document in documents(printed)? {
        let page = match &document {
            Value::Array(items) => items.as_slice(),
            Value::Object(_) => document["sprints"]
                .as_array()
                .or_else(|| document["values"].as_array())
                .map_or(&[][..], Vec::as_slice),
            _ => &[],
        };
        for item in page {
            sprints.push(sprint_of(item)?);
        }
    }
    Ok(sprints)
}

pub(crate) fn sprint(printed: &str) -> Result<Sprint, String> {
    let document = documents(printed)?
        .into_iter()
        .next()
        .ok_or("acli printed no sprint")?;
    sprint_of(&document)
}

fn sprint_of(value: &Value) -> Result<Sprint, String> {
    let id = value["id"].as_u64().ok_or("a sprint has no id")?;
    Ok(Sprint {
        id,
        name: text(value, "name").unwrap_or_else(|| format!("Sprint {id}")),
        state: match value["state"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("active") => SprintState::Active,
            Some("future") => SprintState::Future,
            Some("closed") => SprintState::Closed,
            _ => SprintState::Unknown,
        },
        goal: text(value, "goal"),
        starts_at: text(value, "startDate"),
        ends_at: text(value, "endDate"),
        board: value["originBoardId"]
            .as_u64()
            .or_else(|| value["boardId"].as_u64()),
    })
}

/// One issue read whole by `workitem view`, with the project's mapped `fields`.
pub(crate) fn detail(
    printed: &str,
    mapped: &BTreeMap<String, String>,
) -> Result<IssueDetail, String> {
    let document = documents(printed)?
        .into_iter()
        .next()
        .ok_or("acli printed no issue")?;
    let row = issue(&document)?;
    let fields = &document["fields"];
    let description = match &fields["description"] {
        Value::String(text) => non_empty(text.trim().to_owned()),
        Value::Null => None,
        document => non_empty(rich_text(document)),
    };
    let parent = fields["parent"].as_object().and_then(|_| {
        Some(IssueParent {
            key: text(&fields["parent"], "key")?,
            summary: text(&fields["parent"]["fields"], "summary"),
        })
    });
    let links = fields["issuelinks"]
        .as_array()
        .map(|links| links.iter().filter_map(link).collect())
        .unwrap_or_default();
    let mut sprints = Vec::new();
    let mut shown = Vec::new();
    for (name, id) in mapped {
        if name == SPRINT_FIELD {
            sprints = fields[id.as_str()]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| sprint_of(item).ok())
                        .collect()
                })
                .unwrap_or_default();
        } else {
            shown.push(IssueField {
                name: name.clone(),
                id: id.clone(),
                value: field_text(&fields[id.as_str()]),
            });
        }
    }
    Ok(IssueDetail {
        issue: row,
        description,
        parent,
        links,
        updated: text(fields, "updated"),
        fields: shown,
        sprints,
    })
}

/// A link, read from this issue's side: `outwardIssue` means this one does the outward thing to it
/// ("blocks"), `inwardIssue` that it is done to this one ("is blocked by").
fn link(value: &Value) -> Option<IssueLink> {
    let (relation, other) = if value["outwardIssue"].is_object() {
        (text(&value["type"], "outward"), &value["outwardIssue"])
    } else if value["inwardIssue"].is_object() {
        (text(&value["type"], "inward"), &value["inwardIssue"])
    } else {
        return None;
    };
    Some(IssueLink {
        relation: relation
            .or_else(|| text(&value["type"], "name"))
            .unwrap_or_else(|| "links to".to_owned()),
        key: text(other, "key")?,
        summary: text(&other["fields"], "summary"),
        status: status(&other["fields"]["status"]),
    })
}

/// A field's value as one line of text, whatever shape the tracker gives it: a person's name, an
/// option's value, a list joined with commas, rich text flattened. Something unrecognisable is
/// shown as compact JSON, cut short, rather than hidden: it is still the field's value.
fn field_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => non_empty(text.trim().to_owned()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::Array(items) => non_empty(
            items
                .iter()
                .filter_map(field_text)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Value::Object(_) if value["type"].as_str() == Some("doc") => non_empty(rich_text(value)),
        Value::Object(_) => ["displayName", "value", "name", "key"]
            .iter()
            .find_map(|key| text(value, key))
            .or_else(|| {
                let compact = value.to_string();
                Some(compact.chars().take(200).collect())
            }),
    }
}

/// Atlassian's rich-text document as plain text: its words in order, one line per block, list items
/// marked, everything else (marks, colours, panels' frames) dropped.
fn rich_text(document: &Value) -> String {
    fn walk(node: &Value, out: &mut String) {
        match node["type"].as_str() {
            Some("text") => out.push_str(node["text"].as_str().unwrap_or_default()),
            Some("hardBreak") => out.push('\n'),
            Some("mention" | "emoji" | "status") => {
                out.push_str(node["attrs"]["text"].as_str().unwrap_or_default());
            }
            Some("inlineCard" | "blockCard") => {
                out.push_str(node["attrs"]["url"].as_str().unwrap_or_default());
            }
            Some(kind) => {
                let block = matches!(
                    kind,
                    "paragraph"
                        | "heading"
                        | "listItem"
                        | "codeBlock"
                        | "blockquote"
                        | "panel"
                        | "expand"
                        | "tableRow"
                        | "rule"
                );
                // A list item's own paragraph continues the line its marker started.
                if block && !out.is_empty() && !out.ends_with('\n') && !out.ends_with("- ") {
                    out.push('\n');
                }
                if kind == "listItem" {
                    out.push_str("- ");
                }
                if kind == "expand"
                    && let Some(title) = node["attrs"]["title"].as_str()
                {
                    out.push_str(title);
                    out.push('\n');
                }
                for child in node["content"].as_array().into_iter().flatten() {
                    walk(child, out);
                }
                if block && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => {}
        }
    }
    let mut out = String::new();
    walk(document, &mut out);
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_version_is_read_from_the_first_line_and_the_upgrade_notice_is_ignored() {
        let printed = "acli version 1.3.18-stable\nYou're using an outdated version 1.3.18-stable. \
                       Update to the latest version 1.3.39-stable.\n";
        assert_eq!(version(printed), Some((1, 3, 18)));
        assert_eq!(version("acli version dev"), None);
    }

    #[test]
    fn not_authenticated_is_not_mistaken_for_authenticated() {
        assert!(signed_in("✗ Not authenticated\n  Run acli jira auth login\n").is_err());
        assert!(
            signed_in("✓ Authenticated\n").is_err(),
            "no site, no sign-in"
        );
        let (site, login) =
            signed_in("✓ Authenticated\n  Site: acme.atlassian.net\n  Email: you@example.com\n")
                .unwrap();
        assert_eq!(site, "acme.atlassian.net");
        assert_eq!(login.as_deref(), Some("you@example.com"));
    }

    #[test]
    fn json_after_a_notice_and_pages_printed_back_to_back_are_all_read() {
        let printed = "Update available\n{\"issues\":[]}\n{\"issues\":[]}\ntrailing words";
        assert_eq!(documents(printed).unwrap().len(), 2);
        assert!(documents("no json here").is_err());
    }

    #[test]
    fn an_issue_without_a_status_is_refused_and_one_without_extras_is_not() {
        let bare = json!({"key": "ACME-1", "fields": {"status": {"name": "Doing"}}});
        let read = issue(&bare).unwrap();
        assert_eq!(read.status.category, StatusCategory::Unknown);
        assert_eq!(read.summary, "");
        assert!(read.assignee.is_none());
        let no_status = json!({"key": "ACME-1", "fields": {}});
        assert!(issue(&no_status).unwrap_err().contains("ACME-1"));
    }

    #[test]
    fn a_field_value_reads_as_the_name_a_person_would_say() {
        assert_eq!(
            field_text(&json!({"displayName": "Sam Rivera", "accountId": "1"})).as_deref(),
            Some("Sam Rivera")
        );
        assert_eq!(
            field_text(&json!({"value": "High", "id": "3"})).as_deref(),
            Some("High")
        );
        assert_eq!(
            field_text(&json!([{"value": "Web"}, {"value": "API"}])).as_deref(),
            Some("Web, API")
        );
        assert_eq!(field_text(&json!(5.0)).as_deref(), Some("5.0"));
        assert_eq!(field_text(&json!(null)), None);
        assert_eq!(field_text(&json!("  ")), None);
        assert_eq!(
            field_text(&json!({"odd": true})).as_deref(),
            Some("{\"odd\":true}")
        );
    }

    #[test]
    fn rich_text_keeps_its_words_one_line_per_block() {
        let doc = json!({"type": "doc", "content": [
            {"type": "paragraph", "content": [
                {"type": "text", "text": "Export "},
                {"type": "text", "text": "units", "marks": [{"type": "strong"}]},
            ]},
            {"type": "bulletList", "content": [
                {"type": "listItem", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "as CSV"}]}
                ]}
            ]},
            {"type": "paragraph", "content": [
                {"type": "mention", "attrs": {"text": "@Sam"}},
                {"type": "hardBreak"},
                {"type": "text", "text": "thanks"},
            ]},
        ]});
        assert_eq!(rich_text(&doc), "Export units\n- as CSV\n@Sam\nthanks");
    }
}

//! The `code_*` MCP tools: how an agent reads the comments the user left in the Code tab.
//!
//! # Scoped by the caller, like every other tool here
//!
//! The token names a worktree, and these tools see only that worktree's comments — a comment in
//! another worktree answers exactly like one that does not exist. The model never names the
//! worktree, for the reason §6b gives: there is then no way to aim a call at a place the user is
//! not looking.
//!
//! # Pulled, and also drafted
//!
//! Sending from the Review drawer puts the comments in the composer, which is the route the user
//! sees. These tools are the other route: "I left some comments on the code" in a turn, and the
//! agent reads them itself, then marks each one resolved with a note saying what it did. The note
//! shows on the comment's card, so the user reads the answer where they asked the question.
//!
//! The comments are the user's words and the lines of their own repository, which this agent can
//! already read with its own tools. Unlike a browser page, nothing here is authored by a third
//! party, so the text is not fenced as untrusted.

use std::sync::Arc;

use serde_json::{Value, json};
use tauri::AppHandle;

use crate::app::App;
use crate::code_comments::{self, CodeComment, CommentStatus};
use crate::handoff::{CodeCall, Response};

/// Every tool here shares this prefix, which is what the bridge keys its routing on.
pub const PREFIX: &str = "code_";

/// A resolution note longer than this is a report, and the card is not the place for one.
const MAX_NOTE_CHARS: usize = 2_000;

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "code_read_comments",
            "description":
                "Comments the user left on lines of this worktree's code in Worktree Manager's Code \
                 tab — review notes and questions like \"why is this ordered here?\", each with its \
                 file, line range and the lines as they were when it was written. Check this when \
                 the user says they left comments on the code or asks you to look at their review. \
                 When you have dealt with one, mark it with `code_resolve_comment` and a short note \
                 saying what you did or what the answer is.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "include_resolved": {
                        "type": "boolean",
                        "description": "Also list comments already resolved."
                    },
                    "path": {
                        "type": "string",
                        "description": "Only comments on this file, relative to the worktree."
                    }
                }
            }
        }),
        json!({
            "name": "code_resolve_comment",
            "description":
                "Mark one of the user's code comments as dealt with. The note is shown on the \
                 comment in the Code tab, so say briefly what you changed or what the answer is.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "comment_id": { "type": "integer", "minimum": 1 },
                    "note": { "type": "string" }
                },
                "required": ["comment_id"]
            }
        }),
    ]
}

/// Run one `code_*` call for the session behind `token`.
pub fn run(handle: &AppHandle, app: &Arc<App>, token: &str, call: &CodeCall) -> Response {
    let Some(caller) = app.handoff.resolve(token) else {
        tracing::warn!("a code tool call arrived with an unknown token");
        return Response::failed("this session is not registered with Worktree Manager any more");
    };
    // A comment is on a line of a worktree's file, and Home has no worktree. `handoff::run`
    // refuses Home first; this keeps the refusal true if that ever moves.
    let Some(worktree) = caller.scope.worktree_id().map(str::to_owned) else {
        return Response::failed("code comments belong to a worktree, and the Home agent has none");
    };
    match call.tool.as_str() {
        "code_read_comments" => {
            let include_resolved = call.args["include_resolved"].as_bool().unwrap_or(false);
            let path = call.args["path"].as_str();
            let comments = app.code_comments.list(&worktree);
            Response::ok(read_text(&worktree, &comments, include_resolved, path))
        }
        "code_resolve_comment" => {
            let Some(id) = call.args["comment_id"]
                .as_u64()
                .and_then(|id| u32::try_from(id).ok())
            else {
                return Response::failed("`comment_id` has to be a comment's number");
            };
            let note = call.args["note"]
                .as_str()
                .map(str::trim)
                .filter(|note| !note.is_empty())
                .map(|note| note.chars().take(MAX_NOTE_CHARS).collect::<String>());
            match app.code_comments.edit(&worktree, id, |comment| {
                comment.status = CommentStatus::Resolved;
                comment.note = note;
            }) {
                Ok(comments) => {
                    code_comments::announce(handle, &worktree, &comments);
                    Response::ok(format!("Comment {id} is resolved."))
                }
                // The same words for a comment in another worktree as for one that does not exist.
                Err(_) => {
                    Response::failed(format!("there is no code comment {id} in this worktree"))
                }
            }
        }
        other => Response::failed(format!("no tool named `{other}`")),
    }
}

/// The comments as an agent reads them: numbered by their ids, with their lines quoted.
fn read_text(
    worktree: &str,
    comments: &[CodeComment],
    include_resolved: bool,
    path: Option<&str>,
) -> String {
    let shown: Vec<&CodeComment> = comments
        .iter()
        .filter(|c| include_resolved || c.status == CommentStatus::Open)
        .filter(|c| path.is_none_or(|p| c.path == p))
        .collect();
    if shown.is_empty() {
        return if include_resolved {
            "There are no code comments in this worktree.".to_owned()
        } else {
            "There are no open code comments in this worktree.".to_owned()
        };
    }

    let mut out = vec![format!(
        "{} code comment{} in this worktree, left in the Code tab:",
        shown.len(),
        if shown.len() == 1 { "" } else { "s" }
    )];
    for comment in shown {
        let lines = if comment.start == comment.end {
            format!("{}", comment.start)
        } else {
            format!("{}-{}", comment.start, comment.end)
        };
        let resolved = if comment.status == CommentStatus::Resolved {
            " (resolved)"
        } else {
            ""
        };
        let changed = if outdated(worktree, comment) {
            " — the file has changed since; these are the lines as they were"
        } else {
            ""
        };
        let fence = fence_for(&comment.excerpt);
        out.push(String::new());
        out.push(format!(
            "#{} `{}:{lines}`{resolved}{changed}",
            comment.id, comment.path
        ));
        out.push(format!("{fence}\n{}\n{fence}", comment.excerpt));
        out.push(comment.text.clone());
        if let Some(note) = &comment.note {
            out.push(format!("Resolved with: {note}"));
        }
    }
    out.join("\n") + "\n"
}

/// Whether a comment's lines read differently now than when it was written.
fn outdated(worktree: &str, comment: &CodeComment) -> bool {
    let Ok(file) = wtm_code::read_file(std::path::Path::new(worktree), &comment.path) else {
        return true;
    };
    let wtm_code::Content::Text(text) = file.content else {
        return true;
    };
    let start = usize::try_from(comment.start)
        .unwrap_or(usize::MAX)
        .saturating_sub(1);
    let count = usize::try_from(comment.end - comment.start + 1).unwrap_or(0);
    let now: Vec<&str> = text.split('\n').skip(start).take(count).collect();
    now.join("\n") != comment.excerpt
}

/// A code fence one backtick longer than any run of them in the excerpt, never fewer than three.
fn fence_for(excerpt: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in excerpt.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat((longest + 1).max(3))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::code_comments::{NewComment, Store};

    #[test]
    fn every_tool_carries_the_prefix_the_bridge_routes_on() {
        for tool in definitions() {
            assert!(tool["name"].as_str().unwrap().starts_with(PREFIX), "{tool}");
        }
    }

    fn store_with(worktree: &str) -> Store {
        let store = Store::default();
        store
            .add(
                worktree,
                NewComment {
                    path: "app.py".to_owned(),
                    start: 2,
                    end: 3,
                    excerpt: "def a():\n    return 1".to_owned(),
                    text: "Why 1?".to_owned(),
                },
            )
            .unwrap();
        store
            .add(
                worktree,
                NewComment {
                    path: "b.py".to_owned(),
                    start: 1,
                    end: 1,
                    excerpt: "x = ```".to_owned(),
                    text: "odd".to_owned(),
                },
            )
            .unwrap();
        store
    }

    #[test]
    fn open_comments_are_read_with_their_lines_and_a_changed_file_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.py"),
            "import os\ndef a():\n    return 1\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("b.py"), "x = 2\n").unwrap();
        let worktree = dir.path().to_string_lossy().into_owned();
        let store = store_with(&worktree);

        let text = read_text(&worktree, &store.list(&worktree), false, None);

        assert!(text.starts_with("2 code comments"), "{text}");
        assert!(
            text.contains("#1 `app.py:2-3`\n```\ndef a():\n    return 1\n```\nWhy 1?"),
            "{text}"
        );
        assert!(
            text.contains("#2 `b.py:1` — the file has changed since"),
            "b.py no longer says what the excerpt does: {text}"
        );
        assert!(
            text.contains("````\nx = ```\n````"),
            "the fence outlasts the backticks inside"
        );
    }

    #[test]
    fn resolved_comments_are_left_out_unless_asked_for_and_a_path_narrows_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().to_string_lossy().into_owned();
        let store = store_with(&worktree);
        store
            .edit(&worktree, 1, |c| {
                c.status = CommentStatus::Resolved;
                c.note = Some("changed it to 2".to_owned());
            })
            .unwrap();

        let open = read_text(&worktree, &store.list(&worktree), false, None);
        assert!(open.starts_with("1 code comment in"), "{open}");
        assert!(!open.contains("#1"));

        let all = read_text(&worktree, &store.list(&worktree), true, Some("app.py"));
        assert!(all.contains("#1 `app.py:2-3` (resolved)"), "{all}");
        assert!(all.contains("Resolved with: changed it to 2"), "{all}");
        assert!(!all.contains("#2"), "b.py is not app.py");
    }

    #[test]
    fn another_worktree_s_comments_are_not_this_one_s() {
        let store = store_with("/elsewhere");
        let text = read_text("/here", &store.list("/here"), true, None);
        assert_eq!(text, "There are no code comments in this worktree.");
    }
}

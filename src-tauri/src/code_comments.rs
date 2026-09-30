//! Comments on lines of code, left in the Code tab, for an agent to read.
//!
//! # Rust's, not the window's
//!
//! The list is drawn by the Code tab but read by agents too, through `code_read_comments`, and an
//! MCP call arrives here from another process with no window in the loop. So the one copy lives
//! on [`crate::app::App`], and the window mirrors it from `code:comments`, which always carries the
//! whole list rather than a change to merge — the same shape as a browser's comments (§6c).
//!
//! # In memory, per worktree
//!
//! They are review notes on a working tree that is changing under them, not a document, so they
//! are not written to disk: a reload of the window keeps them, and quitting the app ends them,
//! as it ends a browser pane's. Each stores its excerpt — the lines as they were when it was
//! written — so an agent reads what the user read, and the viewer can tell when the file has moved
//! on since.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::app::App;
use crate::commands::{AppState, Reply, blocking};
use crate::view::ErrorView;

/// The comments changed. Payload: [`CommentsChanged`], the worktree and its whole list.
pub const CODE_COMMENTS_EVENT: &str = "code:comments";

/// The most of a file one comment quotes. A comment on a thousand lines is a comment on the file;
/// the excerpt is there so an agent sees the lines, not so the prompt carries a file it can read.
pub const MAX_EXCERPT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentStatus {
    Open,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeComment {
    /// Unique within its worktree, and never reused while the app runs.
    pub id: u32,
    /// Relative to the worktree.
    pub path: String,
    /// 1-based and inclusive.
    pub start: u32,
    pub end: u32,
    /// The lines `start..=end` as they were when the comment was written.
    pub excerpt: String,
    pub text: String,
    pub status: CommentStatus,
    /// The agent it was last drafted to, by the label the user saw, once it has been.
    pub sent_to: Option<String>,
    /// What an agent said when it resolved it.
    pub note: Option<String>,
}

/// A comment as the viewer adds it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewComment {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub excerpt: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentsChanged<'a> {
    pub worktree_id: &'a str,
    pub comments: &'a [CodeComment],
}

#[derive(Default)]
struct Entry {
    next: u32,
    comments: Vec<CodeComment>,
}

/// Every worktree's comments. Owned by [`App`].
#[derive(Default)]
pub struct Store {
    entries: parking_lot::Mutex<BTreeMap<String, Entry>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    #[must_use]
    pub fn list(&self, worktree: &str) -> Vec<CodeComment> {
        self.entries
            .lock()
            .get(worktree)
            .map(|entry| entry.comments.clone())
            .unwrap_or_default()
    }

    /// Add a comment and answer with the worktree's whole list.
    pub fn add(&self, worktree: &str, new: NewComment) -> Result<Vec<CodeComment>, ErrorView> {
        let text = new.text.trim();
        if text.is_empty() {
            return Err(ErrorView::new("emptyComment", "a comment needs some text"));
        }
        if new.start == 0 || new.end < new.start {
            return Err(ErrorView::new(
                "badLines",
                format!("lines {}–{} are not a range", new.start, new.end),
            ));
        }
        let mut entries = self.entries.lock();
        let entry = entries.entry(worktree.to_owned()).or_default();
        entry.next += 1;
        entry.comments.push(CodeComment {
            id: entry.next,
            path: new.path,
            start: new.start,
            end: new.end,
            excerpt: clip(new.excerpt),
            text: text.to_owned(),
            status: CommentStatus::Open,
            sent_to: None,
            note: None,
        });
        Ok(entry.comments.clone())
    }

    /// Change one comment in place and answer with the list, or say there is no such comment.
    pub fn edit(
        &self,
        worktree: &str,
        id: u32,
        change: impl FnOnce(&mut CodeComment),
    ) -> Result<Vec<CodeComment>, ErrorView> {
        let mut entries = self.entries.lock();
        let entry = entries.get_mut(worktree).ok_or_else(no_comment)?;
        let comment = entry
            .comments
            .iter_mut()
            .find(|comment| comment.id == id)
            .ok_or_else(no_comment)?;
        change(comment);
        Ok(entry.comments.clone())
    }

    /// Drop the comments `doomed` picks and answer with what is left.
    pub fn remove(
        &self,
        worktree: &str,
        doomed: impl Fn(&CodeComment) -> bool,
    ) -> Vec<CodeComment> {
        let mut entries = self.entries.lock();
        let Some(entry) = entries.get_mut(worktree) else {
            return Vec::new();
        };
        entry.comments.retain(|comment| !doomed(comment));
        entry.comments.clone()
    }

    /// A worktree is gone, and so are its comments.
    pub fn forget(&self, worktree: &str) {
        self.entries.lock().remove(worktree);
    }
}

fn no_comment() -> ErrorView {
    ErrorView::new(
        "noComment",
        "there is no comment with that id in this worktree",
    )
}

/// An excerpt at most [`MAX_EXCERPT_BYTES`] long, cut on a character boundary.
fn clip(mut excerpt: String) -> String {
    if excerpt.len() > MAX_EXCERPT_BYTES {
        let mut cut = MAX_EXCERPT_BYTES;
        while !excerpt.is_char_boundary(cut) {
            cut -= 1;
        }
        excerpt.truncate(cut);
    }
    excerpt
}

/// Tell the window a worktree's comments changed.
pub fn announce(handle: &AppHandle, worktree: &str, comments: &[CodeComment]) {
    let payload = CommentsChanged {
        worktree_id: worktree,
        comments,
    };
    if let Err(error) = handle.emit(CODE_COMMENTS_EVENT, payload) {
        tracing::debug!(%error, "could not announce code comments");
    }
}

/// Run an edit on the app's comments and announce what it left.
async fn publish(
    handle: AppHandle,
    app: &Arc<App>,
    worktree_id: String,
    edit: impl FnOnce(&Store, &str) -> Reply<Vec<CodeComment>> + Send + 'static,
) -> Reply<Vec<CodeComment>> {
    let app = Arc::clone(app);
    blocking(move || {
        let comments = edit(&app.code_comments, &worktree_id)?;
        announce(&handle, &worktree_id, &comments);
        Ok(comments)
    })
    .await
}

#[tauri::command]
pub async fn code_list_comments(app: AppState<'_>, worktree_id: String) -> Reply<Vec<CodeComment>> {
    Ok(app.code_comments.list(&worktree_id))
}

#[tauri::command]
pub async fn code_add_comment(
    handle: AppHandle,
    app: AppState<'_>,
    worktree_id: String,
    comment: NewComment,
) -> Reply<Vec<CodeComment>> {
    publish(handle, &app, worktree_id, move |store, wt| {
        store.add(wt, comment)
    })
    .await
}

#[tauri::command]
pub async fn code_update_comment(
    handle: AppHandle,
    app: AppState<'_>,
    worktree_id: String,
    id: u32,
    text: String,
) -> Reply<Vec<CodeComment>> {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Err(ErrorView::new("emptyComment", "a comment needs some text"));
    }
    publish(handle, &app, worktree_id, move |store, wt| {
        store.edit(wt, id, |comment| comment.text = text)
    })
    .await
}

/// Mark a comment resolved or open again. A note is what an agent said; the user resolves without.
#[tauri::command]
pub async fn code_resolve_comment(
    handle: AppHandle,
    app: AppState<'_>,
    worktree_id: String,
    id: u32,
    resolved: bool,
) -> Reply<Vec<CodeComment>> {
    publish(handle, &app, worktree_id, move |store, wt| {
        store.edit(wt, id, |comment| {
            comment.status = if resolved {
                CommentStatus::Resolved
            } else {
                CommentStatus::Open
            };
            if !resolved {
                comment.note = None;
            }
        })
    })
    .await
}

#[tauri::command]
pub async fn code_remove_comment(
    handle: AppHandle,
    app: AppState<'_>,
    worktree_id: String,
    id: u32,
) -> Reply<Vec<CodeComment>> {
    publish(handle, &app, worktree_id, move |store, wt| {
        Ok(store.remove(wt, |comment| comment.id == id))
    })
    .await
}

/// The Review drawer's Clear: every resolved comment, or every one.
#[tauri::command]
pub async fn code_clear_comments(
    handle: AppHandle,
    app: AppState<'_>,
    worktree_id: String,
    resolved_only: bool,
) -> Reply<Vec<CodeComment>> {
    publish(handle, &app, worktree_id, move |store, wt| {
        Ok(store.remove(wt, |comment| {
            !resolved_only || comment.status == CommentStatus::Resolved
        }))
    })
    .await
}

/// Record that these comments were drafted into an agent's composer, and which agent.
#[tauri::command]
pub async fn code_mark_comments_sent(
    handle: AppHandle,
    app: AppState<'_>,
    worktree_id: String,
    ids: Vec<u32>,
    to: String,
) -> Reply<Vec<CodeComment>> {
    publish(handle, &app, worktree_id, move |store, wt| {
        let mut last = store.list(wt);
        for id in ids {
            let to = to.clone();
            last = store.edit(wt, id, move |comment| comment.sent_to = Some(to))?;
        }
        Ok(last)
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn new(path: &str, start: u32, end: u32, text: &str) -> NewComment {
        NewComment {
            path: path.to_owned(),
            start,
            end,
            excerpt: "x = 1\n".to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn comments_are_kept_per_worktree_with_ids_that_are_never_reused() {
        let store = Store::default();
        store.add("/w/a", new("a.py", 1, 2, "why?")).unwrap();
        store.add("/w/b", new("b.py", 3, 3, "and this")).unwrap();
        let listed = store.add("/w/a", new("a.py", 5, 5, "second")).unwrap();

        assert_eq!(listed.iter().map(|c| c.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(store.list("/w/b").len(), 1);

        store.remove("/w/a", |c| c.id == 2);
        let after = store.add("/w/a", new("a.py", 9, 9, "third")).unwrap();
        assert_eq!(
            after.last().unwrap().id,
            3,
            "a removed id is not handed out again"
        );
    }

    #[test]
    fn an_empty_comment_or_an_upside_down_range_is_refused() {
        let store = Store::default();
        assert_eq!(
            store.add("/w", new("a.py", 1, 1, "   ")).unwrap_err().kind,
            "emptyComment"
        );
        assert_eq!(
            store.add("/w", new("a.py", 4, 2, "x")).unwrap_err().kind,
            "badLines"
        );
        assert_eq!(
            store.add("/w", new("a.py", 0, 2, "x")).unwrap_err().kind,
            "badLines"
        );
    }

    #[test]
    fn text_is_trimmed_and_a_huge_excerpt_is_clipped_on_a_character_boundary() {
        let store = Store::default();
        let mut comment = new("a.md", 1, 1, "  look  ");
        comment.excerpt = "é".repeat(MAX_EXCERPT_BYTES);
        let listed = store.add("/w", comment).unwrap();

        assert_eq!(listed[0].text, "look");
        assert!(listed[0].excerpt.len() <= MAX_EXCERPT_BYTES);
        assert!(listed[0].excerpt.chars().all(|c| c == 'é'));
    }

    #[test]
    fn editing_a_comment_that_is_not_there_says_so() {
        let store = Store::default();
        store.add("/w", new("a.py", 1, 1, "x")).unwrap();
        assert_eq!(store.edit("/w", 99, |_| {}).unwrap_err().kind, "noComment");
        assert_eq!(
            store.edit("/other", 1, |_| {}).unwrap_err().kind,
            "noComment",
            "an id from another worktree is not this one's"
        );
    }

    #[test]
    fn a_forgotten_worktree_has_no_comments() {
        let store = Store::default();
        store.add("/w", new("a.py", 1, 1, "x")).unwrap();
        store.forget("/w");
        assert!(store.list("/w").is_empty());
    }

    #[test]
    fn the_comment_view_is_camel_case_and_its_status_lowercase() {
        let store = Store::default();
        let listed = store.add("/w", new("a.py", 1, 1, "x")).unwrap();
        let json = serde_json::to_value(&listed[0]).unwrap();
        assert_eq!(json["status"], "open");
        assert!(json.as_object().unwrap().contains_key("sentTo"));
    }
}

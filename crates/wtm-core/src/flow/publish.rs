//! Publishing a review of a teammate's pull request: what you chose, exactly what posts, and how wtm
//! finds a post again.
//!
//! # Exactly what posts is a value, and an approval pins its hash
//!
//! The outcome sheet sends a [`ReviewDraft`]: the findings you picked, the words you left on each, a
//! summary and a review state. [`ReviewPayload::build`] turns that into exactly what the forge will
//! be sent — which comments sit on a line, which cannot and go into the summary instead, at which
//! commit — and the preview shows that value, not the draft. Approving pins its
//! [`hash`](ReviewPayload::hash), so an edit after the preview, or a finding that changed under it,
//! is a different payload that the approval does not cover.
//!
//! # A comment sits on a changed line or in the summary, never nowhere
//!
//! GitHub refuses a review comment on a line its diff of the pull request does not show ("Line could
//! not be resolved"), and through GraphQL it drops one without saying so. So placement is decided
//! before the preview, from the forge's own diff of the reviewed commit against the base
//! ([`ChangedLines`]): a finding whose lines are in a hunk is an inline comment, and one about a whole
//! file, the whole change, or lines the pull request did not touch is [folded](FoldedComment) into
//! the summary with its place named. The preview says which went where, so nothing you picked
//! disappears.
//!
//! # Two writes, so a half-done publish says which half
//!
//! A review is posted the way GitHub's own page posts one: the inline comments go on a pending
//! review, then the pending review is submitted with the summary and the state. Those are two
//! operations in the outbox ([`ReviewPayload::operations`]). The first leaves nothing anyone but you
//! can see, so a publish stopped between them has told the author nothing yet; the second is the one
//! that does.
//!
//! # Markers find what was posted
//!
//! Every body wtm posts ends with a [`Marker`]: an HTML comment, which GitHub renders as nothing,
//! naming the operation that posted it. Before an operation is sent, and before one found mid-write
//! after a crash is sent again, the outbox reads the pull request's reviews and looks for its marker
//! ([`find_pending`], [`find_submitted`], [`missing_comments`]). Found, the write happened and is
//! adopted; not found, it did not, and only then is it sent. A marker is derived from the operation's
//! key, and each write's payload names the whole review it belongs to, so approving the same review
//! again finds the same posts, and a different review never mistakes another's posts for its own.
//!
//! # One pending review per person
//!
//! GitHub gives each person at most one pending review on a pull request and refuses to start a
//! second (HTTP 422). A pending review that is not this operation's — started in the browser, or left
//! by an earlier publish — is therefore something you decide about rather than something to post
//! around, and [`find_pending`] reports it as in the way, with what it holds.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::consolidate::FindingRecord;
use super::forge::RepoSlug;
use super::outbox::{NewOperation, OperationKey, OperationPayload};
use super::review::FindingId;
use super::state::FlowId;

/// The state a review is submitted with, as GitHub names the three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvent {
    Comment,
    RequestChanges,
    Approve,
}

/// One finding picked for the review, in your words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftComment {
    pub finding: FindingId,
    pub body: String,
}

/// What the outcome sheet sends: the findings you picked with the text you left on each, the
/// summary, and the state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDraft {
    pub comments: Vec<DraftComment>,
    pub summary: String,
    pub event: ReviewEvent,
}

/// The new side's lines of one hunk, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

/// What the forge's diff shows of one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "hunks", rename_all = "snake_case")]
pub enum FileLines {
    /// The new side's lines in each hunk, in order.
    Hunks(Vec<LineRange>),
    /// No line diff at all: a binary file, one too large to show, or a rename with no changes.
    NoDiff,
}

/// The lines of a pull request a review comment can sit on, by the forge's own diff of the reviewed
/// commit against the base. A file the diff does not list is not part of the change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedLines {
    pub files: BTreeMap<String, FileLines>,
}

/// Why a picked finding is posted in the summary rather than on a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FoldReason {
    /// It is about the change as a whole.
    WholeChange,
    /// It is about a file, not about lines in it.
    WholeFile,
    /// Its lines are not in the diff: the pull request did not change them or the lines near them.
    NotChanged,
    /// The diff shows no lines of its file.
    NoDiff,
    /// Its file is not part of the pull request.
    NotInChange,
}

impl ChangedLines {
    /// Where a comment about `start..=end` of `path` can sit: `(start_line, line)`, with
    /// `start_line` only for more than one line.
    ///
    /// GitHub takes a range only within one hunk, so a range that crosses out of one is narrowed to
    /// its lines in the first hunk it touches. A range none of whose lines are in a hunk cannot sit
    /// on the diff at all.
    ///
    /// # Errors
    ///
    /// Why the comment cannot sit on a line, which the preview says beside it.
    pub fn place(
        &self,
        path: &str,
        start: u32,
        end: u32,
    ) -> Result<(Option<u32>, u32), FoldReason> {
        let (start, end) = (start.min(end), start.max(end));
        let hunks = match self.files.get(path) {
            None => return Err(FoldReason::NotInChange),
            Some(FileLines::NoDiff) => return Err(FoldReason::NoDiff),
            Some(FileLines::Hunks(hunks)) => hunks,
        };
        let hunk = hunks
            .iter()
            .find(|hunk| hunk.start <= end && start <= hunk.end)
            .ok_or(FoldReason::NotChanged)?;
        let from = start.max(hunk.start);
        let to = end.min(hunk.end);
        Ok(((from < to).then_some(from), to))
    }
}

/// A comment exactly as it posts on a line of the diff, before its marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineComment {
    pub finding: FindingId,
    pub path: String,
    /// The first line of a range; `None` for one line.
    pub start_line: Option<u32>,
    /// The line it sits on, or the last of its range.
    pub line: u32,
    pub body: String,
}

/// A picked finding that has no changed line to sit on, so it posts in the summary, under its place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoldedComment {
    pub finding: FindingId,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub body: String,
    pub reason: FoldReason,
}

impl FoldedComment {
    /// Where it is about, as the summary names it.
    #[must_use]
    pub fn place(&self) -> String {
        match (&self.path, self.line) {
            (Some(path), Some(line)) => format!("`{path}:{line}`"),
            (Some(path), None) => format!("`{path}`"),
            (None, _) => "The whole change".to_owned(),
        }
    }
}

/// Why a review was not built. Nothing was approved or sent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("{0}")]
pub struct PublishError(pub String);

/// What a review is built against: the pull request, the commit it is pinned to, the review's
/// findings and the diff's lines.
#[derive(Debug, Clone, Copy)]
pub struct PublishContext<'a> {
    pub repo: &'a RepoSlug,
    pub number: u64,
    /// The head the review read. Every comment is pinned to it.
    pub commit_id: &'a str,
    pub findings: &'a [FindingRecord],
    pub lines: &'a ChangedLines,
    /// Whether the pull request is yours, which GitHub lets you comment on and nothing more.
    pub own_pull_request: bool,
}

/// Exactly what a publish posts, apart from the [`Marker`]s that let wtm find it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPayload {
    pub repo: RepoSlug,
    pub number: u64,
    pub commit_id: String,
    pub event: ReviewEvent,
    pub comments: Vec<InlineComment>,
    pub folded: Vec<FoldedComment>,
    /// The summary as you wrote it. [`ReviewPayload::body`] is what posts.
    pub summary: String,
}

/// The heading folded comments sit under in the summary.
const FOLDED_HEADING: &str = "**Not on a changed line**";

impl ReviewPayload {
    /// Exactly what `draft` posts in `context`.
    ///
    /// # Errors
    ///
    /// When it would post nothing, an approval or a change request on your own pull request (GitHub
    /// allows neither), an empty comment, a finding twice, or a finding that is not one of the
    /// review's open findings. Each says what to change.
    pub fn build(context: &PublishContext<'_>, draft: &ReviewDraft) -> Result<Self, PublishError> {
        if context.own_pull_request && draft.event != ReviewEvent::Comment {
            return Err(PublishError(
                "This pull request is yours, and GitHub lets you comment on your own pull request \
                 but not approve it or request changes: post it as a comment."
                    .to_owned(),
            ));
        }
        let mut seen = BTreeSet::new();
        let mut comments = Vec::new();
        let mut folded = Vec::new();
        for picked in &draft.comments {
            if !seen.insert(picked.finding) {
                return Err(PublishError(format!(
                    "Finding {} is picked twice.",
                    picked.finding
                )));
            }
            let record = context
                .findings
                .iter()
                .find(|record| record.finding.id == picked.finding)
                .filter(|record| record.finding.dismissed.is_none())
                .ok_or_else(|| {
                    PublishError(format!(
                        "Finding {} is not one of this review's open findings any more; read the \
                         review again.",
                        picked.finding
                    ))
                })?;
            let body = picked.body.trim();
            let location = record.finding.location.as_ref();
            if body.is_empty() {
                let place = location.map_or_else(
                    || "the whole change".to_owned(),
                    |at| format!("`{}`", at.path),
                );
                return Err(PublishError(format!(
                    "The comment on {place} is empty: write something, or leave that finding out."
                )));
            }
            let body = body.to_owned();
            let Some(at) = location else {
                folded.push(FoldedComment {
                    finding: picked.finding,
                    path: None,
                    line: None,
                    body,
                    reason: FoldReason::WholeChange,
                });
                continue;
            };
            if !record.lines {
                folded.push(FoldedComment {
                    finding: picked.finding,
                    path: Some(at.path.clone()),
                    line: None,
                    body,
                    reason: FoldReason::WholeFile,
                });
                continue;
            }
            match context.lines.place(&at.path, at.start_line, at.end_line) {
                Ok((start_line, line)) => comments.push(InlineComment {
                    finding: picked.finding,
                    path: at.path.clone(),
                    start_line,
                    line,
                    body,
                }),
                Err(reason) => folded.push(FoldedComment {
                    finding: picked.finding,
                    path: Some(at.path.clone()),
                    line: Some(at.start_line),
                    body,
                    reason,
                }),
            }
        }
        let summary = draft.summary.trim().to_owned();
        // An approval with nothing to say is a review GitHub takes and people give; a comment or a
        // change request with nothing in it is not.
        if comments.is_empty()
            && folded.is_empty()
            && summary.is_empty()
            && draft.event != ReviewEvent::Approve
        {
            return Err(PublishError(
                "There is nothing to post: pick a finding or write a summary.".to_owned(),
            ));
        }
        Ok(Self {
            repo: context.repo.clone(),
            number: context.number,
            commit_id: context.commit_id.to_owned(),
            event: draft.event,
            comments,
            folded,
            summary,
        })
    }

    /// The review's own text as it posts, before its marker: the summary, then each folded comment
    /// under its place.
    #[must_use]
    pub fn body(&self) -> String {
        let mut body = self.summary.clone();
        if !self.folded.is_empty() {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(FOLDED_HEADING);
            for comment in &self.folded {
                let _ = write!(body, "\n\n{}: {}", comment.place(), comment.body);
            }
        }
        body
    }

    /// The hash an approval pins: of this value whole, so any change to what would post changes it.
    #[must_use]
    pub fn hash(&self) -> String {
        hash_of(self)
    }

    /// What the outbox writes to: `owner/name#number`.
    #[must_use]
    pub fn target(&self) -> String {
        target(&self.repo, self.number)
    }

    /// The two writes that post this review, in order: the inline comments on a pending review, then
    /// the submission with the summary and the state.
    #[must_use]
    pub fn operations(&self, flow: FlowId) -> Vec<NewOperation> {
        let review = self.hash();
        let pending = OperationPayload::PendingReview(PendingReviewPayload {
            review: review.clone(),
            repo: self.repo.clone(),
            number: self.number,
            commit_id: self.commit_id.clone(),
            comments: self.comments.clone(),
        });
        let submit = OperationPayload::SubmitReview(SubmitReviewPayload {
            review,
            repo: self.repo.clone(),
            number: self.number,
            commit_id: self.commit_id.clone(),
            event: self.event,
            body: self.body(),
        });
        vec![
            NewOperation::new(flow, 0, pending),
            NewOperation::new(flow, 1, submit),
        ]
    }
}

/// The first write: a pending review at `commit_id` holding the inline comments.
///
/// Each write names the review it is part of by that review's hash. Keyed by what it sends alone,
/// a second review reusing the first one's comment text would find the first one's comments write
/// already done and adopt it, and the first click-through found exactly that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingReviewPayload {
    /// [`ReviewPayload::hash`] of the review this write posts part of.
    pub review: String,
    pub repo: RepoSlug,
    pub number: u64,
    pub commit_id: String,
    pub comments: Vec<InlineComment>,
}

/// The second write: the pending review submitted with its body and state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitReviewPayload {
    /// [`ReviewPayload::hash`] of the review this write submits.
    pub review: String,
    pub repo: RepoSlug,
    pub number: u64,
    pub commit_id: String,
    pub event: ReviewEvent,
    /// The body as it posts, before the marker.
    pub body: String,
}

/// `owner/name#number`, how an operation names the pull request it writes to.
#[must_use]
pub fn target(repo: &RepoSlug, number: u64) -> String {
    format!("{}#{number}", repo.full_name())
}

/// `sha256:<hex>` of a value's JSON. Struct fields serialize in declaration order and maps are
/// ordered, so the same value always hashes the same.
pub(crate) fn hash_of<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("sha256:{}", hex(&Sha256::digest(&bytes)))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// ═══════════════════════════════ markers ═══════════════════════════════

/// The hidden text in everything one operation posts.
///
/// An HTML comment, because GitHub renders one as nothing in a review body or a review comment, so
/// the author reads only what you wrote. Derived from the operation's key, which includes the flow
/// and the hash of exactly what it sends: two operations never share one, and the same operation
/// approved twice keeps its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Marker(String);

/// What every marker starts with, inside its comment.
const MARKER_PREFIX: &str = "wtm:";

impl Marker {
    #[must_use]
    pub fn of(key: &OperationKey) -> Self {
        let seed = format!(
            "{}|{}|{}|{}",
            key.flow,
            key.kind.as_str(),
            key.target,
            key.payload_hash
        );
        let digest = hex(&Sha256::digest(seed.as_bytes()));
        Self(format!("{MARKER_PREFIX}{}", &digest[..16]))
    }

    /// The marker as it ends a review's body.
    #[must_use]
    pub fn tag(&self) -> String {
        format!("<!-- {} -->", self.0)
    }

    /// The marker as it ends the operation's comment number `index`.
    #[must_use]
    pub fn comment_tag(&self, index: usize) -> String {
        format!("<!-- {} c{index} -->", self.0)
    }

    /// Whether `body` was posted by this operation as a review's body.
    #[must_use]
    pub fn signs(&self, body: &str) -> bool {
        body.contains(&self.tag())
    }

    /// Which of this operation's comments `body` is, if it is one.
    #[must_use]
    pub fn comment_index(&self, body: &str) -> Option<usize> {
        let opening = format!("<!-- {} c", self.0);
        let rest = &body[body.find(&opening)? + opening.len()..];
        let digits = rest.split_once(" -->")?.0;
        digits.parse().ok()
    }
}

/// `text` with `tag` after it, a blank line between, as everything wtm posts ends.
#[must_use]
pub fn signed(text: &str, tag: &str) -> String {
    if text.is_empty() {
        tag.to_owned()
    } else {
        format!("{text}\n\n{tag}")
    }
}

/// Whether some operation of wtm's posted `body`.
#[must_use]
pub fn carries_marker(body: &str) -> bool {
    body.contains(&format!("<!-- {MARKER_PREFIX}"))
}

// ═══════════════════════════════ what the forge holds ═══════════════════════════════

/// A review's state, as the forge reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteReviewState {
    /// Started and not submitted. Only its author can see it.
    Pending,
    Commented,
    ChangesRequested,
    Approved,
    Dismissed,
    /// A state this build has no name for.
    Unknown,
}

/// A review on a pull request, as the forge reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteReview {
    pub id: u64,
    /// The id GraphQL knows it by, which adding a comment to it takes.
    pub node_id: String,
    pub author: Option<String>,
    pub state: RemoteReviewState,
    pub body: String,
    pub commit_id: Option<String>,
    pub url: String,
}

/// One comment of a review, as the forge reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteComment {
    pub id: u64,
    pub path: String,
    pub body: String,
}

/// What the pull request's reviews say about the pending review an operation would start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingFound {
    /// This operation's review is there, pending or submitted since: it happened.
    Posted(RemoteReview),
    /// A pending review of yours is there and it is not this operation's.
    InTheWay(RemoteReview),
    /// Neither: start it.
    Absent,
}

/// Look for the review `marker` started, then for a pending review of `viewer`'s that would stop
/// another from starting.
///
/// A forge shows a pending review only to its author, so any pending review in the list is the
/// signed-in account's; `viewer` narrows it further when the login is known.
#[must_use]
pub fn find_pending(
    marker: &Marker,
    viewer: Option<&str>,
    reviews: &[RemoteReview],
) -> PendingFound {
    if let Some(ours) = reviews.iter().find(|review| marker.signs(&review.body)) {
        return PendingFound::Posted(ours.clone());
    }
    let yours = |review: &&RemoteReview| match (viewer, review.author.as_deref()) {
        (Some(viewer), Some(author)) => author.eq_ignore_ascii_case(viewer),
        _ => true,
    };
    reviews
        .iter()
        .filter(yours)
        .find(|review| review.state == RemoteReviewState::Pending)
        .map_or(PendingFound::Absent, |review| {
            PendingFound::InTheWay(review.clone())
        })
}

/// What the pull request's reviews say about submitting the pending review `review`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitFound {
    /// Submitted with this operation's body: it happened.
    Submitted(RemoteReview),
    /// Still pending: submit it.
    Pending(RemoteReview),
    /// Submitted, but not by this operation: on the forge, by you, without this body.
    SubmittedElsewhere(RemoteReview),
    /// Not there any more: deleted on the forge.
    Gone,
}

#[must_use]
pub fn find_submitted(marker: &Marker, review: u64, reviews: &[RemoteReview]) -> SubmitFound {
    if let Some(ours) = reviews
        .iter()
        .find(|r| marker.signs(&r.body) && r.state != RemoteReviewState::Pending)
    {
        return SubmitFound::Submitted(ours.clone());
    }
    match reviews.iter().find(|r| r.id == review) {
        Some(r) if r.state == RemoteReviewState::Pending => SubmitFound::Pending(r.clone()),
        Some(r) => SubmitFound::SubmittedElsewhere(r.clone()),
        None => SubmitFound::Gone,
    }
}

/// Which of an operation's `count` comments are not among `existing`, by their markers, in order.
#[must_use]
pub fn missing_comments(marker: &Marker, count: usize, existing: &[RemoteComment]) -> Vec<usize> {
    let there: BTreeSet<usize> = existing
        .iter()
        .filter_map(|comment| marker.comment_index(&comment.body))
        .collect();
    (0..count).filter(|index| !there.contains(index)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::consolidate::FindingRecord;
    use crate::flow::outbox::OperationKind;
    use crate::flow::review::{Dismissal, Finding, FindingClass, Location, Severity};

    fn repo() -> RepoSlug {
        RepoSlug::new("github.com", "acme", "webapp")
    }

    fn record(id: i64, at: Option<(&str, u32, u32)>, lines: bool) -> FindingRecord {
        FindingRecord {
            finding: Finding {
                id: FindingId(id),
                flow: FlowId(1),
                class: FindingClass::Suggestion,
                severity: Severity::Notable,
                location: at.map(|(path, start_line, end_line)| Location {
                    path: path.to_owned(),
                    start_line,
                    end_line,
                }),
                title: format!("finding {id}"),
                rationale: "because".to_owned(),
                requirement: None,
                tree_sha: "t".to_owned(),
                dismissed: None,
            },
            lines,
            evidence: Vec::new(),
        }
    }

    /// `export.py` changed in lines 9–23, `logo.png` with no line diff.
    fn lines() -> ChangedLines {
        ChangedLines {
            files: BTreeMap::from([
                (
                    "export.py".to_owned(),
                    FileLines::Hunks(vec![LineRange { start: 9, end: 23 }]),
                ),
                ("logo.png".to_owned(), FileLines::NoDiff),
            ]),
        }
    }

    fn draft(picked: &[(i64, &str)], summary: &str, event: ReviewEvent) -> ReviewDraft {
        ReviewDraft {
            comments: picked
                .iter()
                .map(|(id, body)| DraftComment {
                    finding: FindingId(*id),
                    body: (*body).to_owned(),
                })
                .collect(),
            summary: summary.to_owned(),
            event,
        }
    }

    fn build(
        findings: &[FindingRecord],
        draft: &ReviewDraft,
        own: bool,
    ) -> Result<ReviewPayload, PublishError> {
        let repo = repo();
        let lines = lines();
        ReviewPayload::build(
            &PublishContext {
                repo: &repo,
                number: 4031,
                commit_id: "7c9e1d",
                findings,
                lines: &lines,
                own_pull_request: own,
            },
            draft,
        )
    }

    #[test]
    fn a_finding_on_changed_lines_is_an_inline_comment_and_every_other_kind_is_named_in_the_summary()
     {
        let findings = [
            record(1, Some(("export.py", 20, 20)), true),
            record(2, Some(("export.py", 12, 13)), true),
            record(3, Some(("export.py", 26, 26)), true),
            record(4, None, false),
            record(5, Some(("logo.png", 1, 1)), true),
            record(6, Some(("README.md", 3, 3)), true),
            record(7, Some(("export.py", 1, 1)), false),
        ];
        let payload = build(
            &findings,
            &draft(
                &[
                    (1, "Drops the last unit."),
                    (2, " Two lines. "),
                    (3, "Outside."),
                    (4, "Whole thing."),
                    (5, "Binary."),
                    (6, "Not here."),
                    (7, "The file."),
                ],
                "Nice work.",
                ReviewEvent::Comment,
            ),
            false,
        )
        .unwrap();

        assert_eq!(
            payload
                .comments
                .iter()
                .map(|c| (c.finding.0, c.start_line, c.line, c.body.as_str()))
                .collect::<Vec<_>>(),
            [
                (1, None, 20, "Drops the last unit."),
                (2, Some(12), 13, "Two lines.")
            ]
        );
        assert_eq!(
            payload
                .folded
                .iter()
                .map(|c| (c.finding.0, c.reason))
                .collect::<Vec<_>>(),
            [
                (3, FoldReason::NotChanged),
                (4, FoldReason::WholeChange),
                (5, FoldReason::NoDiff),
                (6, FoldReason::NotInChange),
                (7, FoldReason::WholeFile),
            ]
        );
        assert_eq!(
            payload.body(),
            "Nice work.\n\n**Not on a changed line**\n\n`export.py:26`: Outside.\n\nThe whole \
             change: Whole thing.\n\n`logo.png:1`: Binary.\n\n`README.md:3`: Not here.\n\n\
             `export.py`: The file."
        );
    }

    #[test]
    fn a_range_that_leaves_its_hunk_is_narrowed_to_the_lines_inside_it() {
        let lines = ChangedLines {
            files: BTreeMap::from([(
                "a.rs".to_owned(),
                FileLines::Hunks(vec![
                    LineRange { start: 5, end: 10 },
                    LineRange { start: 40, end: 44 },
                ]),
            )]),
        };
        assert_eq!(lines.place("a.rs", 8, 14), Ok((Some(8), 10)));
        assert_eq!(lines.place("a.rs", 2, 5), Ok((None, 5)));
        assert_eq!(lines.place("a.rs", 41, 41), Ok((None, 41)));
        assert_eq!(lines.place("a.rs", 20, 30), Err(FoldReason::NotChanged));
        assert_eq!(lines.place("b.rs", 1, 1), Err(FoldReason::NotInChange));
    }

    #[test]
    fn your_own_pull_request_can_only_be_commented_on() {
        let findings = [record(1, Some(("export.py", 20, 20)), true)];
        for event in [ReviewEvent::Approve, ReviewEvent::RequestChanges] {
            let error = build(&findings, &draft(&[(1, "x")], "", event), true).unwrap_err();
            assert!(error.0.contains("comment"), "{error}");
        }
        assert!(
            build(
                &findings,
                &draft(&[(1, "x")], "", ReviewEvent::Comment),
                true
            )
            .is_ok()
        );
    }

    #[test]
    fn a_review_that_says_nothing_is_refused_unless_it_approves() {
        assert!(build(&[], &draft(&[], "  ", ReviewEvent::Comment), false).is_err());
        assert!(build(&[], &draft(&[], "", ReviewEvent::RequestChanges), false).is_err());
        let approve = build(&[], &draft(&[], "", ReviewEvent::Approve), false).unwrap();
        assert_eq!(approve.body(), "");
    }

    #[test]
    fn an_empty_comment_a_finding_twice_or_a_dismissed_finding_is_refused() {
        let mut waved = record(2, Some(("export.py", 12, 12)), true);
        waved.finding.dismissed = Some(Dismissal {
            reason: "intended".to_owned(),
            at: 1,
        });
        let findings = [record(1, Some(("export.py", 20, 20)), true), waved];
        let empty = build(
            &findings,
            &draft(&[(1, "  ")], "", ReviewEvent::Comment),
            false,
        );
        assert!(empty.unwrap_err().0.contains("empty"));
        let twice = build(
            &findings,
            &draft(&[(1, "a"), (1, "b")], "", ReviewEvent::Comment),
            false,
        );
        assert!(twice.unwrap_err().0.contains("twice"));
        let dismissed = build(
            &findings,
            &draft(&[(2, "a")], "", ReviewEvent::Comment),
            false,
        );
        assert!(dismissed.unwrap_err().0.contains("open findings"));
    }

    #[test]
    fn any_edit_to_what_would_post_changes_the_hash_an_approval_pins() {
        let findings = [record(1, Some(("export.py", 20, 20)), true)];
        let base = build(
            &findings,
            &draft(&[(1, "a")], "s", ReviewEvent::Comment),
            false,
        )
        .unwrap();
        let same = build(
            &findings,
            &draft(&[(1, "a")], "s", ReviewEvent::Comment),
            false,
        )
        .unwrap();
        assert_eq!(base.hash(), same.hash());
        assert!(base.hash().starts_with("sha256:"));

        let mut edited = base.clone();
        edited.comments[0].body = "b".to_owned();
        let mut moved = base.clone();
        moved.commit_id = "91ab44".to_owned();
        let mut state = base.clone();
        state.event = ReviewEvent::Approve;
        for other in [edited, moved, state] {
            assert_ne!(base.hash(), other.hash());
        }
    }

    #[test]
    fn a_review_is_two_operations_the_comments_then_the_submission_each_keyed_by_what_it_sends() {
        let findings = [record(1, Some(("export.py", 20, 20)), true)];
        let payload = build(
            &findings,
            &draft(&[(1, "a")], "s", ReviewEvent::Comment),
            false,
        )
        .unwrap();
        let ops = payload.operations(FlowId(9));
        assert_eq!(
            ops.iter().map(|op| op.key.kind).collect::<Vec<_>>(),
            [OperationKind::PendingReview, OperationKind::SubmitReview]
        );
        assert!(ops.iter().all(|op| op.key.target == "acme/webapp#4031"));
        assert_ne!(ops[0].key.payload_hash, ops[1].key.payload_hash);
        assert_eq!(ops, payload.operations(FlowId(9)));
        assert_ne!(
            Marker::of(&ops[0].key),
            Marker::of(&payload.operations(FlowId(10))[0].key),
            "another run's identical review is another post"
        );
    }

    #[test]
    fn two_reviews_that_share_their_comments_but_not_their_summary_are_two_reviews_writes() {
        // Found in the first click-through: keyed by what it sends alone, the second review's
        // comments were "already done" — the first review's — and its submission then had only
        // the first review, already submitted, to submit.
        let findings = [record(1, Some(("export.py", 20, 20)), true)];
        let first = build(
            &findings,
            &draft(&[(1, "a")], "first", ReviewEvent::Comment),
            false,
        )
        .unwrap();
        let second = build(
            &findings,
            &draft(&[(1, "a")], "second", ReviewEvent::Comment),
            false,
        )
        .unwrap();
        let (a, b) = (first.operations(FlowId(1)), second.operations(FlowId(1)));
        assert_ne!(
            a[0].key, b[0].key,
            "each review starts a pending review of its own"
        );
        assert_ne!(Marker::of(&a[0].key), Marker::of(&b[0].key));
        assert_eq!(
            a,
            first.operations(FlowId(1)),
            "the same review is still the same writes"
        );
    }

    fn review(id: u64, state: RemoteReviewState, body: &str, author: &str) -> RemoteReview {
        RemoteReview {
            id,
            node_id: format!("PRR_{id}"),
            author: Some(author.to_owned()),
            state,
            body: body.to_owned(),
            commit_id: Some("7c9e1d".to_owned()),
            url: format!("https://github.com/acme/webapp/pull/4031#pullrequestreview-{id}"),
        }
    }

    fn marker() -> Marker {
        Marker::of(&OperationKey {
            flow: FlowId(1),
            kind: OperationKind::PendingReview,
            target: "acme/webapp#4031".to_owned(),
            payload_hash: "sha256:aa".to_owned(),
        })
    }

    #[test]
    fn the_operations_own_review_is_found_by_its_marker_whoever_else_has_reviewed() {
        let m = marker();
        let reviews = [
            review(1, RemoteReviewState::Approved, "LGTM", "sam"),
            review(
                2,
                RemoteReviewState::Pending,
                &signed("", &m.tag()),
                "takumi",
            ),
        ];
        assert_eq!(
            find_pending(&m, Some("takumi"), &reviews),
            PendingFound::Posted(reviews[1].clone())
        );
    }

    #[test]
    fn a_pending_review_of_yours_without_the_marker_is_in_the_way_and_nobody_elses_is() {
        let m = marker();
        let browser = review(3, RemoteReviewState::Pending, "", "takumi");
        assert_eq!(
            find_pending(&m, Some("Takumi"), std::slice::from_ref(&browser)),
            PendingFound::InTheWay(browser.clone())
        );
        let sams = review(4, RemoteReviewState::Pending, "", "sam");
        assert_eq!(
            find_pending(&m, Some("takumi"), &[sams]),
            PendingFound::Absent
        );
        let submitted = review(5, RemoteReviewState::Commented, "done", "takumi");
        assert_eq!(
            find_pending(&m, Some("takumi"), &[submitted]),
            PendingFound::Absent
        );
    }

    #[test]
    fn a_submission_is_found_by_its_marker_and_one_made_elsewhere_is_told_apart() {
        let m = marker();
        let ours = review(
            7,
            RemoteReviewState::Commented,
            &signed("Nice.", &m.tag()),
            "takumi",
        );
        assert_eq!(
            find_submitted(&m, 7, std::slice::from_ref(&ours)),
            SubmitFound::Submitted(ours)
        );
        let pending = review(7, RemoteReviewState::Pending, "", "takumi");
        assert!(matches!(
            find_submitted(&m, 7, std::slice::from_ref(&pending)),
            SubmitFound::Pending(_)
        ));
        let browser = review(7, RemoteReviewState::Commented, "from the page", "takumi");
        assert!(matches!(
            find_submitted(&m, 7, &[browser]),
            SubmitFound::SubmittedElsewhere(_)
        ));
        assert_eq!(find_submitted(&m, 7, &[]), SubmitFound::Gone);
    }

    #[test]
    fn comments_already_posted_are_found_by_their_markers_so_only_the_rest_are_sent() {
        let m = marker();
        let posted = |index: usize| RemoteComment {
            id: index as u64,
            path: "export.py".to_owned(),
            body: signed("text", &m.comment_tag(index)),
        };
        let theirs = RemoteComment {
            id: 99,
            path: "export.py".to_owned(),
            body: "a comment of your own, from the browser".to_owned(),
        };
        assert_eq!(
            missing_comments(&m, 4, &[posted(0), theirs, posted(2)]),
            [1, 3]
        );
        assert_eq!(m.comment_index(&posted(12).body), Some(12));
        assert!(carries_marker(&posted(0).body));
        assert!(!carries_marker("plain"));
    }
}

//! Publishing a teammate's pull request review through the outbox, end to end.
//!
//! What is real: the app, the engine, a real review of a real repository by `common::agents`'
//! scripts, the SQLite store, and `GhForge` reading the pull request and its diff from `gh`'s own
//! JSON (`common::pull_requests`). What is fake is GitHub's review API: [`FakeGitHub`] holds
//! reviews in memory the way GitHub does — one pending review per person, a body replaced on
//! submit, markers wherever they were posted — and can be told to lose a reply, fail a request, or
//! die mid-write the way a killed process does.
//!
//! Each test is a promise a reviewer would notice broken: something posted that was not approved
//! exactly; a moved head that posted anyway; a review posted twice after a lost reply, a crash or a
//! retry; a pending review of yours posted around or duplicated into; or an agent that could reach
//! any of it.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde_json::json;
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::publish::{HEAD_MOVED, PublicationView, PublishPreview};
use wtm_app_lib::flow::{Engine, Notice, Tuning};
use wtm_core::flow::{
    ApprovalId, Attention, DraftComment, FlowId, InboxKind, InboxTarget, Marker, OperationResult,
    OperationState, PublicationState, RemoteComment, RemoteReview, RemoteReviewState, RepoSlug,
    ReviewDraft, ReviewEvent,
};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::forge::{ForgeError, ReviewWriter, SentComment};
use wtm_core::ports::{InboxStore, OutboxStore};
use wtm_testkit::FakeClock;

mod common;
use common::agents::{Behaviour, Heard, Script};
use common::pull_requests::{Harness, finding_at, reviewers};

// ═══════════════════════════════ GitHub's review API, in memory ═══════════════════════════════

/// The account `gh` is signed in as in the fixtures.
const YOU: &str = "dana-lee";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Create,
    Add,
    Submit,
}

/// Something the fake does instead of answering normally, the next time `Call` is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trick {
    /// Fail without doing anything, as an unreachable GitHub does.
    Fail(Call),
    /// Do it, then lose the reply, as a dropped connection does.
    LoseReply(Call),
    /// Die before the request leaves, as a process killed at that moment does.
    DieBefore(Call),
    /// Do it, then die before the reply is read, as a process killed at that moment does.
    DieAfter(Call),
    /// Start a pending review in the browser just before it, as you might between wtm's read and
    /// its send.
    BrowserFirst(Call),
}

/// What `resume_unwind` carries: a thread ending the way a killed process's would, without a panic
/// message, so the test's output shows only what failed.
struct Died;

#[derive(Debug, Clone)]
struct Held {
    review: RemoteReview,
    comments: Vec<RemoteComment>,
    event: Option<ReviewEvent>,
}

#[derive(Default)]
struct FakeGitHub {
    held: Mutex<Vec<Held>>,
    next: AtomicU64,
    tricks: Mutex<Vec<Trick>>,
    /// Every write that reached GitHub, in order.
    writes: Mutex<Vec<Call>>,
}

impl FakeGitHub {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            next: AtomicU64::new(100),
            ..Self::default()
        })
    }

    fn trick(&self, trick: Trick) {
        self.tricks.lock().push(trick);
    }

    /// The trick for `call`, taken.
    fn take(&self, call: Call) -> Option<Trick> {
        let mut tricks = self.tricks.lock();
        let index = tricks.iter().position(|t| {
            matches!(
                t,
                Trick::Fail(c)
                    | Trick::LoseReply(c)
                    | Trick::DieBefore(c)
                    | Trick::DieAfter(c)
                    | Trick::BrowserFirst(c) if *c == call
            )
        })?;
        Some(tricks.remove(index))
    }

    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst)
    }

    /// A pending review you started in the browser, holding one comment of your own.
    fn started_in_the_browser(&self) -> u64 {
        let id = self.id();
        self.held.lock().push(Held {
            review: review(id, RemoteReviewState::Pending, ""),
            comments: vec![RemoteComment {
                id: self.id(),
                path: "src/export.rs".to_owned(),
                body: "a note of your own, from the browser".to_owned(),
            }],
            event: None,
        });
        id
    }

    fn submitted(&self) -> Vec<Held> {
        self.held
            .lock()
            .iter()
            .filter(|h| h.review.state != RemoteReviewState::Pending)
            .cloned()
            .collect()
    }

    fn writes(&self) -> Vec<Call> {
        self.writes.lock().clone()
    }

    /// Apply `trick` around a write that does `work`.
    fn write<T>(
        &self,
        call: Call,
        work: impl FnOnce() -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        let trick = self.take(call);
        match trick {
            Some(Trick::Fail(_)) => {
                return Err(ForgeError::Failed {
                    argv: "gh api".to_owned(),
                    message: "error connecting to api.github.com".to_owned(),
                });
            }
            Some(Trick::DieBefore(_)) => std::panic::resume_unwind(Box::new(Died)),
            Some(Trick::BrowserFirst(_)) => {
                self.started_in_the_browser();
            }
            _ => {}
        }
        let done = work()?;
        self.writes.lock().push(call);
        match trick {
            Some(Trick::LoseReply(_)) => Err(ForgeError::Timeout {
                argv: "gh api".to_owned(),
                timeout_ms: 30_000,
            }),
            Some(Trick::DieAfter(_)) => std::panic::resume_unwind(Box::new(Died)),
            _ => Ok(done),
        }
    }
}

fn review(id: u64, state: RemoteReviewState, body: &str) -> RemoteReview {
    RemoteReview {
        id,
        node_id: format!("PRR_{id}"),
        author: Some(YOU.to_owned()),
        state,
        body: body.to_owned(),
        commit_id: None,
        url: format!("https://github.com/acme/webapp/pull/4031#pullrequestreview-{id}"),
    }
}

impl ReviewWriter for FakeGitHub {
    fn reviews(
        &self,
        _cwd: &std::path::Path,
        _repo: &RepoSlug,
        _number: u64,
    ) -> Result<Vec<RemoteReview>, ForgeError> {
        Ok(self.held.lock().iter().map(|h| h.review.clone()).collect())
    }

    fn review_comments(
        &self,
        _cwd: &std::path::Path,
        _repo: &RepoSlug,
        _number: u64,
        review: u64,
    ) -> Result<Vec<RemoteComment>, ForgeError> {
        Ok(self
            .held
            .lock()
            .iter()
            .find(|h| h.review.id == review)
            .map(|h| h.comments.clone())
            .unwrap_or_default())
    }

    fn create_pending_review(
        &self,
        _cwd: &std::path::Path,
        _repo: &RepoSlug,
        _number: u64,
        commit_id: &str,
        body: &str,
        comments: &[SentComment],
    ) -> Result<RemoteReview, ForgeError> {
        self.write(Call::Create, || {
            let mut held = self.held.lock();
            if held
                .iter()
                .any(|h| h.review.state == RemoteReviewState::Pending)
            {
                return Err(ForgeError::PendingReviewExists {
                    what: "a pending review on acme/webapp#4031".to_owned(),
                });
            }
            let id = self.id();
            let mut made = review(id, RemoteReviewState::Pending, body);
            made.commit_id = Some(commit_id.to_owned());
            held.push(Held {
                review: made.clone(),
                comments: comments
                    .iter()
                    .map(|c| RemoteComment {
                        id: self.id(),
                        path: c.path.clone(),
                        body: c.body.clone(),
                    })
                    .collect(),
                event: None,
            });
            Ok(made)
        })
    }

    fn add_review_comment(
        &self,
        _cwd: &std::path::Path,
        _repo: &RepoSlug,
        review_node_id: &str,
        comment: &SentComment,
    ) -> Result<(), ForgeError> {
        self.write(Call::Add, || {
            let mut held = self.held.lock();
            let target = held
                .iter_mut()
                .find(|h| {
                    h.review.node_id == review_node_id
                        && h.review.state == RemoteReviewState::Pending
                })
                .ok_or_else(|| ForgeError::NotFound {
                    what: review_node_id.to_owned(),
                })?;
            target.comments.push(RemoteComment {
                id: self.id(),
                path: comment.path.clone(),
                body: comment.body.clone(),
            });
            Ok(())
        })
    }

    fn submit_review(
        &self,
        _cwd: &std::path::Path,
        _repo: &RepoSlug,
        _number: u64,
        review: u64,
        event: ReviewEvent,
        body: &str,
    ) -> Result<RemoteReview, ForgeError> {
        self.write(Call::Submit, || {
            let mut held = self.held.lock();
            let target = held
                .iter_mut()
                .find(|h| h.review.id == review)
                .filter(|h| h.review.state == RemoteReviewState::Pending)
                .ok_or_else(|| ForgeError::Rejected {
                    what: "your review".to_owned(),
                    reason: "Could not comment pull request review.".to_owned(),
                })?;
            target.review.state = match event {
                ReviewEvent::Comment => RemoteReviewState::Commented,
                ReviewEvent::RequestChanges => RemoteReviewState::ChangesRequested,
                ReviewEvent::Approve => RemoteReviewState::Approved,
            };
            body.clone_into(&mut target.review.body);
            target.event = Some(event);
            Ok(target.review.clone())
        })
    }
}

// ═══════════════════════════════ the harness ═══════════════════════════════

struct Publish {
    h: Harness,
    github: Arc<FakeGitHub>,
    flow: FlowId,
}

impl Publish {
    /// A consolidated review of the pull request with two findings, one in each file.
    fn new() -> Self {
        Self::with_script(reviewers())
    }

    fn with_script(script: Script) -> Self {
        let github = FakeGitHub::new();
        let h = Harness::with_writer(script, Some(Arc::clone(&github) as Arc<dyn ReviewWriter>));
        let flow = h.start();
        h.consolidated(flow);
        Self { h, github, flow }
    }

    /// Every open finding, picked, with your words in place of the agent's.
    fn draft(&self, summary: &str) -> ReviewDraft {
        ReviewDraft {
            comments: self
                .h
                .view(self.flow)
                .findings
                .iter()
                .filter(|f| f.finding.dismissed.is_none())
                .map(|f| DraftComment {
                    finding: f.finding.id,
                    body: format!("You wrote: {}", f.finding.title),
                })
                .collect(),
            summary: summary.to_owned(),
            event: ReviewEvent::Comment,
        }
    }

    fn preview(&self, draft: &ReviewDraft) -> PublishPreview {
        self.h.engine.preview_publish(self.flow, draft).unwrap()
    }

    fn approve(&self, draft: &ReviewDraft) -> ApprovalId {
        let preview = self.preview(draft);
        self.h
            .engine
            .approve_publish(self.flow, draft, &preview.hash, None)
            .unwrap()
            .publication
            .approval
            .id
    }

    /// Wait until no thread of `engine`'s is sending it.
    fn settled_on(engine: &Engine, approval: ApprovalId) -> PublicationView {
        for _ in 0..4_000 {
            let view = engine.publication_view(approval).unwrap();
            if view.state != PublicationState::Posting {
                return view;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!(
            "never settled: {:#?}",
            engine.publication_view(approval).unwrap()
        );
    }

    fn settled(&self, approval: ApprovalId) -> PublicationView {
        Self::settled_on(&self.h.engine, approval)
    }

    /// Wait until the sending thread has died mid-write, leaving an operation executing.
    fn died(&self, approval: ApprovalId) -> PublicationView {
        for _ in 0..4_000 {
            let view = self.h.engine.publication_view(approval).unwrap();
            if view
                .publication
                .operations
                .iter()
                .any(|op| op.state == OperationState::Executing)
                && self.github.tricks.lock().is_empty()
            {
                // The thread is gone once its trick has fired; give it a moment to unwind.
                std::thread::sleep(std::time::Duration::from_millis(20));
                return view;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("never died mid-write");
    }

    /// The app launched again on the same data: a new engine, with nothing in memory.
    fn relaunch(&self) -> Arc<Engine> {
        let engine = Engine::new(
            Arc::clone(&self.h.app),
            Arc::clone(&self.h.host) as Arc<dyn RoleHost>,
            Arc::new(FakeClock::new()) as Arc<dyn Clock>,
            Arc::new(Heard::default()) as Arc<dyn Notice>,
            Tuning {
                tick_ms: 5,
                skills_wait_ms: 30_000,
                settle_ms: 30_000,
                nudges: 1,
            },
        );
        *self.h.host.engine.lock() = Arc::downgrade(&engine);
        engine
    }

    fn states(view: &PublicationView) -> Vec<OperationState> {
        view.publication
            .operations
            .iter()
            .map(|op| op.state)
            .collect()
    }

    fn open_items(&self) -> Vec<(InboxKind, String)> {
        self.h
            .app
            .appdb
            .get()
            .unwrap()
            .inbox(Some(self.flow), None)
            .unwrap()
            .into_iter()
            .filter(|item| matches!(item.target, InboxTarget::Approval { .. }))
            .map(|item| (item.kind, item.title))
            .collect()
    }

    /// How many comments on GitHub carry the marker of `approval`'s first operation's comment
    /// `index`.
    fn copies_of(&self, approval: ApprovalId, index: usize) -> usize {
        let view = self.h.engine.publication_view(approval).unwrap();
        let marker = Marker::of(&view.publication.operations[0].key);
        self.github
            .held
            .lock()
            .iter()
            .flat_map(|h| &h.comments)
            .filter(|c| marker.comment_index(&c.body) == Some(index))
            .count()
    }
}

// ═══════════════════════════════ the tests ═══════════════════════════════

#[test]
fn nothing_posts_until_you_approve_exactly_what_the_preview_showed_and_then_it_posts_exactly_that()
{
    let p = Publish::new();
    let draft = p.draft("Two things before merge.");
    let preview = p.preview(&draft);
    assert!(p.github.writes().is_empty(), "a preview posts nothing");
    assert_eq!(preview.comments.len(), 2);
    assert_eq!(
        preview.review.commit_id, p.h.first,
        "pinned to the head reviewed"
    );
    assert!(
        preview
            .body
            .starts_with("Two things before merge.\n\n<!-- wtm:")
    );

    // What you edit after the preview is not what you approved.
    let mut edited = draft.clone();
    edited.summary.push_str(" And a third.");
    let refused =
        p.h.engine
            .approve_publish(p.flow, &edited, &preview.hash, None)
            .unwrap_err();
    assert!(
        refused.message.contains("Preview it again"),
        "{}",
        refused.message
    );
    assert!(p.github.writes().is_empty());

    let approval =
        p.h.engine
            .approve_publish(p.flow, &draft, &preview.hash, None)
            .unwrap()
            .publication
            .approval
            .id;
    let done = p.settled(approval);
    assert_eq!(done.state, PublicationState::Published);
    assert_eq!(p.github.writes(), [Call::Create, Call::Submit]);

    let posted = p.github.submitted();
    assert_eq!(posted.len(), 1);
    let posted = &posted[0];
    assert_eq!(posted.review.commit_id.as_deref(), Some(p.h.first.as_str()));
    assert_eq!(
        posted.review.body, preview.body,
        "the body exactly as previewed"
    );
    assert_eq!(
        posted
            .comments
            .iter()
            .map(|c| c.body.as_str())
            .collect::<Vec<_>>(),
        preview
            .comments
            .iter()
            .map(|c| c.body.as_str())
            .collect::<Vec<_>>(),
        "every comment exactly as previewed, marker included"
    );
    assert_eq!(posted.event, Some(ReviewEvent::Comment));
    assert!(done.url.is_some());
    p.h.gh.assert_only_reads();
}

#[test]
fn approving_the_same_text_again_after_it_was_published_posts_nothing_more() {
    let p = Publish::new();
    let draft = p.draft("Once.");
    let first = p.approve(&draft);
    assert_eq!(p.settled(first).state, PublicationState::Published);
    let again = p.approve(&draft);
    let view = p.settled(again);
    assert_eq!(view.state, PublicationState::Published);
    assert_eq!(p.github.writes(), [Call::Create, Call::Submit]);
    assert_eq!(p.github.submitted().len(), 1);
}

#[test]
fn a_second_review_with_the_same_comments_and_a_new_summary_is_posted_as_a_review_of_its_own() {
    let p = Publish::new();
    let first = p.approve(&p.draft("First."));
    assert_eq!(p.settled(first).state, PublicationState::Published);
    let second = p.approve(&p.draft("Second, after a word with the author."));
    let done = p.settled(second);
    assert_eq!(done.state, PublicationState::Published, "{done:#?}");
    assert_eq!(
        p.github.writes(),
        [Call::Create, Call::Submit, Call::Create, Call::Submit]
    );
    let posted = p.github.submitted();
    assert_eq!(posted.len(), 2);
    assert!(posted[1].review.body.starts_with("Second"));
    assert_eq!(
        posted[1].comments.len(),
        2,
        "its own comments, not the first review's"
    );
    assert_eq!(p.copies_of(second, 0), 1);
}

#[test]
fn a_reply_lost_on_the_way_back_is_found_by_its_marker_and_never_posted_twice() {
    let p = Publish::new();
    p.github.trick(Trick::LoseReply(Call::Create));
    p.github.trick(Trick::LoseReply(Call::Submit));
    let approval = p.approve(&p.draft("Lost replies."));
    let view = p.settled(approval);

    assert_eq!(view.state, PublicationState::Published);
    assert_eq!(
        p.github.writes(),
        [Call::Create, Call::Submit],
        "each sent once"
    );
    for op in &view.publication.operations {
        assert!(
            matches!(
                op.result,
                Some(OperationResult::Review { adopted: true, .. })
            ),
            "{op:?} was found on GitHub rather than sent again"
        );
    }
    assert_eq!(p.github.submitted().len(), 1);
}

#[test]
fn killed_after_the_comments_reached_github_the_relaunch_checks_and_a_retry_posts_nothing_twice() {
    let p = Publish::new();
    p.github.trick(Trick::DieAfter(Call::Create));
    let approval = p.approve(&p.draft("Killed mid-publish."));
    let cut = p.died(approval);
    assert_eq!(
        Publish::states(&cut),
        [OperationState::Executing, OperationState::Pending]
    );
    assert_eq!(
        p.github.writes(),
        [Call::Create],
        "the comments reached GitHub"
    );

    let engine = p.relaunch();
    let cut_off = engine.publication_view(approval).unwrap();
    assert_eq!(cut_off.state, PublicationState::Checking);
    assert_eq!(engine.recover_publications().unwrap(), [approval]);
    let checked = engine.publication_view(approval).unwrap();
    assert_eq!(
        Publish::states(&checked),
        [OperationState::Done, OperationState::Pending],
        "found by its marker, and nothing sent by recovery"
    );
    assert_eq!(checked.state, PublicationState::Waiting);
    assert_eq!(p.github.writes(), [Call::Create]);
    assert!(
        p.open_items()
            .iter()
            .any(|(kind, title)| *kind == InboxKind::Failure && title.contains("wtm stopped")),
        "{:?}",
        p.open_items()
    );

    let retried = engine.retry_publication(approval).unwrap();
    let done = Publish::settled_on(&engine, retried.publication.approval.id);
    assert_eq!(done.state, PublicationState::Published);
    assert_eq!(p.github.writes(), [Call::Create, Call::Submit]);
    assert_eq!(p.github.submitted().len(), 1);
    assert_eq!(p.copies_of(approval, 0), 1);
    assert_eq!(p.copies_of(approval, 1), 1);
    assert!(p.open_items().is_empty(), "published settles its items");
}

#[test]
fn killed_before_the_comments_left_the_relaunch_finds_nothing_and_a_retry_posts_them_once() {
    let p = Publish::new();
    p.github.trick(Trick::DieBefore(Call::Create));
    let approval = p.approve(&p.draft("Killed before sending."));
    p.died(approval);
    assert!(p.github.writes().is_empty());

    let engine = p.relaunch();
    engine.recover_publications().unwrap();
    let checked = engine.publication_view(approval).unwrap();
    assert_eq!(
        Publish::states(&checked),
        [OperationState::Failed, OperationState::Pending]
    );
    assert!(
        checked.publication.operations[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("did not arrive")
    );
    assert!(p.github.writes().is_empty(), "recovery sends nothing");

    engine.retry_publication(approval).unwrap();
    let done = Publish::settled_on(&engine, approval);
    assert_eq!(done.state, PublicationState::Published);
    assert_eq!(p.github.writes(), [Call::Create, Call::Submit]);
    assert_eq!(p.copies_of(approval, 0), 1);
}

#[test]
fn a_submission_that_failed_after_the_comments_posted_is_partial_and_its_retry_sends_only_the_rest()
{
    let p = Publish::new();
    p.github.trick(Trick::Fail(Call::Submit));
    let approval = p.approve(&p.draft("Half of it."));
    let partial = p.settled(approval);
    assert_eq!(
        Publish::states(&partial),
        [OperationState::Done, OperationState::Failed]
    );
    assert_eq!(partial.state, PublicationState::Failed);
    assert_eq!(
        p.open_items(),
        [(
            InboxKind::Failure,
            "Your review of #4031 was only partly published".to_owned()
        )]
    );

    p.h.engine.retry_publication(approval).unwrap();
    let done = p.settled(approval);
    assert_eq!(done.state, PublicationState::Published);
    assert_eq!(p.github.writes(), [Call::Create, Call::Submit]);
    assert_eq!(p.github.submitted().len(), 1);
    assert!(p.open_items().is_empty());
}

#[test]
fn your_pending_review_stops_the_publish_and_adding_to_it_never_duplicates_a_comment() {
    let p = Publish::new();
    let browser = p.github.started_in_the_browser();
    let approval = p.approve(&p.draft("Into your pending review."));
    let stopped = p.settled(approval);
    assert_eq!(stopped.state, PublicationState::NeedsYou);
    assert_eq!(
        stopped.publication.operations[0].attention,
        Some(Attention::PendingReview {
            review: browser,
            node_id: format!("PRR_{browser}"),
            url: format!("https://github.com/acme/webapp/pull/4031#pullrequestreview-{browser}"),
            comments: 1,
            started_by_wtm: false,
        })
    );
    assert!(p.github.writes().is_empty(), "nothing posted around it");
    assert!(
        p.open_items()
            .iter()
            .any(|(_, title)| title.contains("you already have a pending review"))
    );

    // You choose to add to it, and wtm is killed after the first of the two comments is on it.
    p.github.trick(Trick::DieAfter(Call::Add));
    p.h.engine.add_to_pending_review(approval).unwrap();
    p.died(approval);
    assert_eq!(
        p.github.writes(),
        [Call::Add],
        "one comment added before it died"
    );

    let engine = p.relaunch();
    engine.recover_publications().unwrap();
    let checked = engine.publication_view(approval).unwrap();
    assert_eq!(
        checked.publication.operations[0].state,
        OperationState::Failed
    );
    assert!(
        checked.publication.operations[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("1 of 2")
    );

    engine.retry_publication(approval).unwrap();
    let done = Publish::settled_on(&engine, approval);
    assert_eq!(done.state, PublicationState::Published);
    assert_eq!(p.github.writes(), [Call::Add, Call::Add, Call::Submit]);
    assert_eq!(
        p.copies_of(approval, 0),
        1,
        "the comment added before the crash, once"
    );
    assert_eq!(p.copies_of(approval, 1), 1);

    let posted = p.github.submitted();
    assert_eq!(
        posted.len(),
        1,
        "your pending review, submitted: no second review"
    );
    assert_eq!(posted[0].review.id, browser);
    assert_eq!(
        posted[0].comments.len(),
        3,
        "yours and both of the review's"
    );
}

#[test]
fn a_pending_review_started_between_the_read_and_the_send_gets_github_s_422_and_is_not_posted_around()
 {
    let p = Publish::new();
    p.github.trick(Trick::BrowserFirst(Call::Create));
    let approval = p.approve(&p.draft("Raced."));
    let stopped = p.settled(approval);
    assert_eq!(stopped.state, PublicationState::NeedsYou);
    assert!(matches!(
        stopped.publication.operations[0].attention,
        Some(Attention::PendingReview {
            comments: 1,
            started_by_wtm: false,
            ..
        })
    ));
    assert!(
        p.github.writes().is_empty(),
        "refused by GitHub, so nothing was posted"
    );

    // You submit it yourself on GitHub, then Retry: wtm starts its own and posts once.
    for held in p.github.held.lock().iter_mut() {
        held.review.state = RemoteReviewState::Commented;
    }
    p.h.engine.retry_publication(approval).unwrap();
    let done = p.settled(approval);
    assert_eq!(done.state, PublicationState::Published);
    assert_eq!(p.github.writes(), [Call::Create, Call::Submit]);
}

#[test]
fn leaving_your_pending_review_as_it_is_stops_the_publish_and_posts_nothing() {
    let p = Publish::new();
    p.github.started_in_the_browser();
    let approval = p.approve(&p.draft("Leave it."));
    assert_eq!(p.settled(approval).state, PublicationState::NeedsYou);
    let left = p.h.engine.cancel_publication(approval).unwrap();
    assert_eq!(left.state, PublicationState::Cancelled);
    assert!(p.github.writes().is_empty());
    assert!(p.open_items().is_empty());
}

#[test]
fn a_head_that_moved_before_approval_is_refused_with_both_heads_and_nothing_is_recorded() {
    let p = Publish::new();
    let draft = p.draft("Moved.");
    let preview = p.preview(&draft);
    let moved = p.h.push(
        &p.h.first,
        &[("src/export.rs", "fn export() { for i in 0..n {} }\n")],
    );
    let refused =
        p.h.engine
            .approve_publish(p.flow, &draft, &preview.hash, None)
            .unwrap_err();
    assert_eq!(refused.kind, HEAD_MOVED);
    let detail = refused.detail.unwrap();
    assert_eq!(detail["reviewed"], json!(p.h.first));
    assert_eq!(detail["current"], json!(moved));
    let db = p.h.app.appdb.get().unwrap();
    assert!(
        db.publications(p.flow).unwrap().is_empty(),
        "nothing approved"
    );
    assert!(p.github.writes().is_empty());

    // Publishing against the reviewed commit anyway is its own approval, of the new head.
    let anyway =
        p.h.engine
            .approve_publish(p.flow, &draft, &preview.hash, Some(&moved))
            .unwrap();
    let done = p.settled(anyway.publication.approval.id);
    assert_eq!(done.state, PublicationState::Published);
    assert_eq!(
        p.github.submitted()[0].review.commit_id.as_deref(),
        Some(p.h.first.as_str()),
        "pinned to the commit reviewed, not the new head"
    );
}

#[test]
fn a_head_that_moves_after_approval_stops_the_publish_before_the_next_write_and_asks_again() {
    let p = Publish::new();
    p.github.trick(Trick::Fail(Call::Submit));
    let draft = p.draft("Approved, then pushed.");
    let preview = p.preview(&draft);
    let approval = p.approve(&draft);
    assert_eq!(
        Publish::states(&p.settled(approval)),
        [OperationState::Done, OperationState::Failed]
    );

    let moved =
        p.h.push(&p.h.first, &[("docs/guide.md", "Run with --new-flag.\n")]);
    p.h.engine.retry_publication(approval).unwrap();
    let stale = p.settled(approval);
    assert_eq!(stale.state, PublicationState::Stale);
    assert_eq!(
        stale.publication.operations[1].attention,
        Some(Attention::HeadMoved {
            approved: p.h.first.clone(),
            current: moved.clone(),
        })
    );
    assert_eq!(
        p.github.writes(),
        [Call::Create],
        "the submission was not sent"
    );
    assert!(
        p.open_items()
            .iter()
            .any(|(kind, _)| *kind == InboxKind::Approval),
        "{:?}",
        p.open_items()
    );
    assert!(
        p.h.engine.retry_publication(approval).is_err(),
        "a moved head needs approving again, not a retry"
    );

    let again =
        p.h.engine
            .approve_publish(p.flow, &draft, &preview.hash, Some(&moved))
            .unwrap();
    assert_ne!(again.publication.approval.id, approval);
    let done = p.settled(again.publication.approval.id);
    assert_eq!(done.state, PublicationState::Published);
    assert_eq!(p.github.writes(), [Call::Create, Call::Submit]);
    assert!(
        p.open_items()
            .iter()
            .all(|(kind, _)| *kind != InboxKind::Approval),
        "the earlier approval's question is settled: {:?}",
        p.open_items()
    );
}

#[test]
fn an_agent_has_no_tool_that_publishes_or_approves_and_asking_with_flow_request_posts_nothing() {
    use wtm_agent::authority::Authority;
    for authority in [Authority::Reviewer, Authority::Writer] {
        for tool in wtm_app_lib::flow_tools::tools_for(authority) {
            for word in ["publish", "approve", "review", "post", "merge"] {
                assert!(!tool.contains(word), "{tool} for {authority:?}");
            }
        }
    }

    // A role asks the app to post a comment. It is recorded as a request and nothing more.
    let p = Publish::with_script(Box::new(|role, _| {
        let found = finding_at(
            "A thing",
            if role == "docs" {
                "docs/guide.md"
            } else {
                "src/export.rs"
            },
        );
        if role == "correctness" {
            Behaviour::RequestThenSubmit(
                json!({ "intent": "comment", "details": { "body": "LGTM, merging" } }),
                found,
            )
        } else {
            Behaviour::Submit(vec![found])
        }
    }));
    let answers = p.h.host.answers.lock().clone();
    assert!(
        answers.iter().any(|(role, ok, text)| role == "correctness"
            && *ok
            && text.contains("request, not a result")),
        "{answers:?}"
    );
    let db = p.h.app.appdb.get().unwrap();
    assert!(
        db.publications(p.flow).unwrap().is_empty(),
        "no approval was made"
    );
    assert!(p.github.writes().is_empty(), "nothing reached GitHub");
}

//! Local fixes of a teammate's pull request, end to end: the writer lease, the app's commits, and
//! the routes a fix leaves by.
//!
//! What is real: the app, the engine, a real review of a real repository by `common::agents`'
//! scripts, a writer that edits the real review worktree and calls the real run tools, the SQLite
//! store, `git` committing and pushing to repositories that play the forge, and `GhForge` reading
//! the pull request from `gh`'s own JSON (`common::pull_requests`). What is fake is GitHub's write
//! API: [`FakePulls`] opens pull requests and [`FakeReviews`] holds a review, in memory.
//!
//! Each test is a promise a reviewer would notice broken: a writer sharing a worktree with a review
//! or another writer, or losing it across a restart; a commit with a file nobody picked in it, or a
//! hook run on the way; a push or a pull request nobody approved exactly, or one that went somewhere
//! other than its approval said; a push over a branch that moved, or to an integration branch, or to
//! a fork whose author did not allow it.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::Mutex;
use serde_json::json;
use wtm_app_lib::flow::fix::StartFix;
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::routes::{ApprovedRoute, RouteRequest};
use wtm_app_lib::flow::{Engine, FlowView, Notice, Refusal, Tuning};
use wtm_core::flow::{
    ALLOW_OUTLIERS, ApprovalIntent, Attention, FindingId, FixState, FlowEvent, FlowId, FlowKind,
    InboxKind, LeaseKind, LeaseRelease, NewLease, NewReviewRun, NewWorkItem, OperationState, Phase,
    Pinned, PrState, PublicationState, RemoteComment, RemoteReview, RemoteReviewState, RepoSlug,
    ReviewEvent, RouteKind,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::clock::Clock;
use wtm_core::ports::forge::{
    ForgeError, PullRequestWriter, RemotePullRequest, ReviewWriter, SentComment,
};
use wtm_core::ports::{FixStore, FlowStore, InboxStore, LeaseStore, OutboxStore, RunStore};
use wtm_testkit::FakeClock;

mod common;
use common::agents::{Act, Behaviour, Heard, Script};
use common::pull_requests::{BRANCH, Build, Harness, Shape, THEIR_FORK_URL, finding_at};

// ═══════════════════════════════ GitHub's writes, in memory ═══════════════════════════════

/// Pull requests opened through the route, in memory, as GitHub keeps them: one open pull request
/// per head and base.
#[derive(Default)]
struct FakePulls {
    open: Mutex<Vec<(RepoSlug, RemotePullRequest)>>,
    /// Every pull request that reached GitHub: where, from, into, title, body.
    opened: Mutex<Vec<(String, String, String, String, String)>>,
    next: AtomicU64,
}

impl PullRequestWriter for FakePulls {
    fn pull_requests_from(
        &self,
        _cwd: &Path,
        repo: &RepoSlug,
        head: &str,
    ) -> Result<Vec<RemotePullRequest>, ForgeError> {
        Ok(self
            .open
            .lock()
            .iter()
            .filter(|(r, pr)| r.same_repository(repo) && pr.head == head)
            .map(|(_, pr)| pr.clone())
            .collect())
    }

    fn open_pull_request(
        &self,
        _cwd: &Path,
        repo: &RepoSlug,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<RemotePullRequest, ForgeError> {
        let label = if head.contains(':') {
            head.to_owned()
        } else {
            format!("{}:{head}", repo.owner)
        };
        let mut open = self.open.lock();
        if open
            .iter()
            .any(|(r, pr)| r.same_repository(repo) && pr.head == label && pr.base == base)
        {
            return Err(ForgeError::Rejected {
                what: "a pull request".to_owned(),
                reason: format!("A pull request already exists for {label}."),
            });
        }
        let number = 100 + self.next.fetch_add(1, Ordering::SeqCst);
        let pr = RemotePullRequest {
            number,
            url: format!("https://github.com/{}/pull/{number}", repo.full_name()),
            state: PrState::Open,
            head: label,
            base: base.to_owned(),
            body: body.to_owned(),
        };
        open.push((repo.clone(), pr.clone()));
        self.opened.lock().push((
            repo.full_name(),
            head.to_owned(),
            base.to_owned(),
            title.to_owned(),
            body.to_owned(),
        ));
        Ok(pr)
    }
}

/// One review at a time, the way suggestions post: a pending review, then its submission.
#[derive(Default)]
struct FakeReviews {
    held: Mutex<Vec<(RemoteReview, Vec<SentComment>)>>,
}

impl ReviewWriter for FakeReviews {
    fn reviews(&self, _: &Path, _: &RepoSlug, _: u64) -> Result<Vec<RemoteReview>, ForgeError> {
        Ok(self.held.lock().iter().map(|(r, _)| r.clone()).collect())
    }

    fn review_comments(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: u64,
        review: u64,
    ) -> Result<Vec<RemoteComment>, ForgeError> {
        Ok(self
            .held
            .lock()
            .iter()
            .filter(|(r, _)| r.id == review)
            .flat_map(|(_, comments)| comments.iter())
            .enumerate()
            .map(|(i, c)| RemoteComment {
                id: i as u64,
                path: c.path.clone(),
                body: c.body.clone(),
            })
            .collect())
    }

    fn create_pending_review(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: u64,
        commit_id: &str,
        body: &str,
        comments: &[SentComment],
    ) -> Result<RemoteReview, ForgeError> {
        let review = RemoteReview {
            id: 7,
            node_id: "PRR_7".to_owned(),
            author: Some("dana-lee".to_owned()),
            state: RemoteReviewState::Pending,
            body: body.to_owned(),
            commit_id: Some(commit_id.to_owned()),
            url: "https://github.com/acme/webapp/pull/4031#pullrequestreview-7".to_owned(),
        };
        self.held.lock().push((review.clone(), comments.to_vec()));
        Ok(review)
    }

    fn add_review_comment(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: &str,
        _: &SentComment,
    ) -> Result<(), ForgeError> {
        unreachable!("suggestions start a review of their own")
    }

    fn submit_review(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: u64,
        review: u64,
        event: ReviewEvent,
        body: &str,
    ) -> Result<RemoteReview, ForgeError> {
        let mut held = self.held.lock();
        let (found, _) = held.iter_mut().find(|(r, _)| r.id == review).unwrap();
        found.state = match event {
            ReviewEvent::Comment => RemoteReviewState::Commented,
            _ => RemoteReviewState::Unknown,
        };
        body.clone_into(&mut found.body);
        Ok(found.clone())
    }
}

// ═══════════════════════════════ the harness ═══════════════════════════════

const FIXED: &str = "fn export() { for i in 0..n {} }\n";

/// Reviewers as usual; the writer does `acts` on its first attempt.
fn with_writer(acts: Vec<Act>) -> Script {
    let reviewers = common::pull_requests::reviewers();
    Box::new(move |role, attempt| {
        if role == "fixer" {
            Behaviour::Acts(acts.clone())
        } else {
            reviewers(role, attempt)
        }
    })
}

/// A writer that fixes the off-by-one and commits it.
fn fixes_export() -> Vec<Act> {
    vec![
        Act::Write("src/export.rs", FIXED),
        Act::Checkpoint("Stop the export loop one short of n"),
        Act::Handoff("Fixed the off-by-one in the export loop."),
    ]
}

struct Fixes {
    h: Harness,
    flow: FlowId,
    pulls: Arc<FakePulls>,
    reviews: Arc<FakeReviews>,
}

impl Fixes {
    fn new(shape: Shape, acts: Vec<Act>) -> Self {
        Self::build(shape, acts, false)
    }

    fn build(shape: Shape, acts: Vec<Act>, your_fork: bool) -> Self {
        let pulls = Arc::new(FakePulls::default());
        let reviews = Arc::new(FakeReviews::default());
        let h = Harness::build(
            with_writer(acts),
            Build {
                shape,
                review_writer: Some(Arc::clone(&reviews) as Arc<dyn ReviewWriter>),
                pr_writer: Some(Arc::clone(&pulls) as Arc<dyn PullRequestWriter>),
                your_fork,
            },
        );
        let flow = h.start();
        h.consolidated(flow);
        Self {
            h,
            flow,
            pulls,
            reviews,
        }
    }

    fn finding(&self, path: &str) -> FindingId {
        self.h
            .view(self.flow)
            .findings
            .iter()
            .find(|f| {
                f.finding
                    .location
                    .as_ref()
                    .is_some_and(|at| at.path == path)
            })
            .map(|f| f.finding.id)
            .unwrap()
    }

    fn start_fix(&self, paths: &[&str]) -> Result<wtm_core::flow::FixId, String> {
        self.h
            .engine
            .start_fix(
                self.flow,
                &StartFix {
                    findings: paths.iter().map(|p| self.finding(p)).collect(),
                    role: "fixer".to_owned(),
                },
            )
            .map_err(|e| e.message)
    }

    fn fix_ready(&self) -> FlowView {
        for _ in 0..4_000 {
            let view = self.h.view(self.flow);
            if view.flow.state.phase == Phase::FixReady && !view.running {
                return view;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the fix never finished: {:#?}", self.h.view(self.flow));
    }

    fn db(&self) -> Arc<wtm_appdb::AppDb> {
        self.h.app.appdb.get().unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        self.h
            .clone
            .git_in(&self.h.worktree, args)
            .trim()
            .to_owned()
    }

    fn worktree_path(&self) -> String {
        self.db().review(self.flow).unwrap().unwrap().worktree_path
    }

    fn writer_answers(&self) -> Vec<(bool, String)> {
        self.h
            .host
            .answers
            .lock()
            .iter()
            .filter(|(role, _, _)| role == "fixer")
            .map(|(_, ok, text)| (*ok, text.clone()))
            .collect()
    }

    fn request(kind: RouteKind) -> RouteRequest {
        RouteRequest {
            kind,
            title: "Fixes for #4031".to_owned(),
            body: "The off-by-one, fixed.".to_owned(),
        }
    }

    /// Wait for a route's operations to stop moving, and say how it ended.
    fn settled(&self, approval: wtm_core::flow::ApprovalId) -> PublicationState {
        for _ in 0..4_000 {
            let view = self.h.engine.route_view(approval).unwrap();
            if view.state != PublicationState::Posting {
                return view.state;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the route never settled");
    }

    fn approve(&self, kind: RouteKind) -> ApprovedRoute {
        let request = Self::request(kind);
        let preview = self.h.engine.preview_route(self.flow, &request).unwrap();
        self.h
            .engine
            .approve_route(self.flow, &request, &preview.hash)
            .unwrap()
    }
}

fn route_id(approved: &ApprovedRoute) -> wtm_core::flow::ApprovalId {
    match approved {
        ApprovedRoute::Route { route } => route.route.approval.id,
        ApprovedRoute::Suggestions { publication } => publication.publication.approval.id,
    }
}

// ═══════════════════════════════ the writer and its commits ═══════════════════════════════

#[test]
fn a_writer_fixes_the_picked_findings_and_the_app_commits_them_with_your_hooks_off() {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    // A pre-commit hook that marks and fails, through the clone's own hooks path, which the review
    // worktree shares. A plain commit runs it, so its not running below means something.
    let marker = f.h.clone.parent().join("hook-ran");
    let hooks = f.h.clone.parent().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in ["pre-commit", "commit-msg", "pre-push"] {
        let at = hooks.join(hook);
        std::fs::write(
            &at,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    f.h.clone
        .git(&["config", "core.hooksPath", &hooks.display().to_string()]);
    let (code, _) =
        f.h.clone
            .git_try(&["commit", "--allow-empty", "-m", "plain"]);
    assert_ne!(code, 0, "the planted hook stops a plain commit");
    assert!(marker.exists());
    std::fs::remove_file(&marker).unwrap();

    let id = f.start_fix(&["src/export.rs"]).unwrap();
    let view = f.fix_ready();

    let fix = view.fixes.last().unwrap();
    assert_eq!(fix.id, id);
    assert_eq!(fix.state, FixState::Ready);
    assert_eq!(fix.base_sha, f.h.first);
    let head = fix.head_sha.clone().unwrap();
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(
        f.git(&["log", "-1", "--format=%s"]),
        "Stop the export loop one short of n",
        "the writer's own message, committed by the app"
    );
    assert_eq!(f.git(&["show", "HEAD:src/export.rs"]), FIXED.trim());
    assert_eq!(
        f.git(&["diff", "--name-only", &f.h.first, &head]),
        "src/export.rs"
    );
    assert!(!marker.exists(), "no hook ran for the app's commits");
    assert!(
        f.writer_answers()
            .iter()
            .any(|(ok, text)| *ok && text.starts_with("Committed")),
        "{:?}",
        f.writer_answers()
    );
    assert!(view.leases.is_empty(), "the writer gave the worktree back");

    let brief = f.h.briefs("fixer").pop().unwrap();
    assert!(
        brief.contains("You may change only these files: `src/export.rs`"),
        "{brief}"
    );
    assert!(brief.contains("Off by one in the export loop"));
    assert!(brief.contains("flow_checkpoint"));

    let diff = f.h.engine.fix_diff(f.flow).unwrap();
    assert_eq!(Some(diff.hash.as_str()), fix.diff_hash.as_deref());
    assert_eq!(diff.files.len(), 1);
    assert!(diff.patch.contains("+fn export() { for i in 0..n {} }"));
    assert!(diff.uncommitted.is_empty());
    assert_eq!(
        f.h.their_branch().as_deref(),
        Some(f.h.first.as_str()),
        "nothing left the Mac"
    );
}

#[test]
fn a_change_outside_the_picked_findings_files_is_refused_and_asked_about_and_never_committed() {
    let f = Fixes::new(
        Shape::SameRepo,
        vec![
            Act::Write("src/export.rs", FIXED),
            Act::Write("docs/guide.md", "Run with --new-flag.\n"),
            Act::Checkpoint("Fix the loop and the guide"),
            Act::Restore("docs/guide.md"),
            Act::Checkpoint("Fix the loop"),
            Act::Handoff("Fixed the loop; the guide was not mine to change."),
        ],
    );
    f.start_fix(&["src/export.rs"]).unwrap();
    let view = f.fix_ready();

    let answers = f.writer_answers();
    let refusal = answers
        .iter()
        .find(|(ok, text)| !*ok && text.starts_with("Not committed"))
        .unwrap_or_else(|| panic!("{answers:?}"));
    assert!(refusal.1.contains("`docs/guide.md`"), "{}", refusal.1);
    let fix = view.fixes.last().unwrap();
    let head = fix.head_sha.clone().unwrap();
    assert_eq!(
        f.git(&["diff", "--name-only", &f.h.first, &head]),
        "src/export.rs",
        "the outlier never reached a commit"
    );

    let questions = f.db().questions(f.flow).unwrap();
    let asked = questions
        .iter()
        .find(|q| q.text.contains("`docs/guide.md`"))
        .unwrap();
    assert_eq!(asked.role.as_deref(), Some("fixer"));
    assert_eq!(asked.options[ALLOW_OUTLIERS], "Allow them in this fix");
    assert_eq!(
        fix.asked.len(),
        1,
        "asked once, however often it checkpointed"
    );
    assert!(
        view.inbox
            .iter()
            .any(|item| item.kind == InboxKind::Question),
        "the question is in the inbox"
    );
}

#[test]
fn allowing_an_outlier_after_the_writer_finished_commits_it_into_the_fix() {
    let f = Fixes::new(
        Shape::SameRepo,
        vec![
            Act::Write("src/export.rs", FIXED),
            Act::Write("docs/guide.md", "Run with --new-flag.\n"),
            Act::Handoff("Fixed the loop and the guide that documents it."),
        ],
    );
    f.start_fix(&["src/export.rs"]).unwrap();
    let view = f.fix_ready();
    let fix = view.fixes.last().unwrap().clone();
    assert_eq!(
        fix.head_sha.as_deref(),
        Some(f.h.first.as_str()),
        "nothing committed while a file is outside the fix"
    );
    assert!(
        fix.detail
            .as_deref()
            .is_some_and(|d| d.contains("not committed")),
        "{fix:?}"
    );
    let diff = f.h.engine.fix_diff(f.flow).unwrap();
    assert_eq!(diff.outliers, ["docs/guide.md"]);
    assert!(
        f.h.engine
            .route_options(f.flow)
            .is_err_and(|e| e.message.contains("committed nothing")),
        "an empty fix has nowhere to go"
    );

    let question = fix.asked[0].question;
    f.h.engine
        .answer(question, Some(ALLOW_OUTLIERS), "")
        .unwrap();
    let after = f.db().fix(fix.id).unwrap().unwrap();
    assert_eq!(after.allowed, ["docs/guide.md"]);
    let head = after.head_sha.clone().unwrap();
    assert_ne!(head, f.h.first);
    assert_eq!(
        f.git(&["diff", "--name-only", &f.h.first, &head]),
        "docs/guide.md\nsrc/export.rs"
    );
    assert_eq!(
        after.diff_hash,
        Some(f.h.engine.fix_diff(f.flow).unwrap().hash)
    );
    assert!(
        f.db().open_leases(None).unwrap().is_empty(),
        "the commit's own lease was let go"
    );
}

#[test]
fn a_fix_starts_only_from_a_clean_worktree_with_a_writer_role_and_findings_that_name_files() {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    let refused =
        f.h.engine
            .start_fix(
                f.flow,
                &StartFix {
                    findings: vec![f.finding("src/export.rs")],
                    role: "correctness".to_owned(),
                },
            )
            .unwrap_err();
    assert!(
        refused.message.contains("is a reviewer"),
        "{}",
        refused.message
    );

    std::fs::write(f.h.worktree.join("scratch.txt"), "mine\n").unwrap();
    assert!(
        f.start_fix(&["src/export.rs"])
            .is_err_and(|e| e.contains("clean tree"))
    );
    std::fs::remove_file(f.h.worktree.join("scratch.txt")).unwrap();
    assert!(f.start_fix(&[]).is_err_and(|e| e.contains("at least one")));
    assert!(
        f.db().fixes(f.flow).unwrap().is_empty(),
        "nothing was recorded"
    );
    assert!(f.db().open_leases(None).unwrap().is_empty());
}

// ═══════════════════════════════ the lease ═══════════════════════════════

/// A second run on the same worktree, consolidated, standing for anything else that would read it.
fn second_review(f: &Fixes) -> FlowId {
    let db = f.db();
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: ProjectId::from_root(f.h.clone.root()),
                title: "Another look".to_owned(),
                issue: None,
                pull_request: None,
            },
            1,
        )
        .unwrap();
    let flow = db.start_flow(item.id, FlowKind::ReviewMine, 1).unwrap().id;
    let review = db.review(f.flow).unwrap().unwrap();
    db.create_review(
        &NewReviewRun {
            flow,
            project: review.project.clone(),
            worktree: review.worktree.clone(),
            worktree_path: review.worktree_path.clone(),
            recipe: "standard".to_owned(),
            base: review.base.clone(),
        },
        1,
    )
    .unwrap();
    for to in [Phase::Preparing, Phase::Reviewing, Phase::Consolidated] {
        db.record(flow, &FlowEvent::Advanced { to }, 1).unwrap();
    }
    flow
}

#[test]
fn a_fix_cannot_start_while_a_review_reads_its_worktree() {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    let other = second_review(&f);
    let reading = f
        .db()
        .acquire_lease(
            &NewLease {
                worktree: f.worktree_path(),
                flow: other,
                kind: LeaseKind::Reviewer,
                holder: "standard".to_owned(),
            },
            2,
        )
        .unwrap();

    let refused = f.start_fix(&["src/export.rs"]).unwrap_err();
    assert!(
        refused.contains(&format!("A review (run {other}) is reading this worktree")),
        "{refused}"
    );
    assert!(f.db().fixes(f.flow).unwrap().is_empty());
    assert_eq!(f.h.view(f.flow).flow.state.phase, Phase::Consolidated);

    f.db()
        .release_lease(reading.id, LeaseRelease::Completed, 3)
        .unwrap();
    f.start_fix(&["src/export.rs"]).unwrap();
    f.fix_ready();
}

#[test]
fn while_a_writer_holds_the_worktree_no_review_reruns_or_resumes_on_it_and_no_second_writer_starts()
{
    let go = Arc::new(AtomicBool::new(false));
    let mut acts = vec![Act::WaitFor(Arc::clone(&go))];
    acts.extend(fixes_export());
    let f = Fixes::new(Shape::SameRepo, acts);
    let other = second_review(&f);
    f.start_fix(&["src/export.rs"]).unwrap();

    let open = f.db().open_leases(Some(&f.worktree_path())).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, LeaseKind::Writer);
    assert_eq!(open[0].flow, f.flow);
    assert_eq!(open[0].holder, "fixer");

    let rerun =
        f.h.engine
            .rerun(other, wtm_app_lib::flow::findings::Rerun::All)
            .unwrap_err();
    assert!(
        rerun
            .message
            .contains("A local fix is being written in this worktree"),
        "{rerun:?}"
    );
    let resumed: Result<(), Refusal> = f.h.engine.resume(other);
    assert!(resumed.is_err());
    assert!(
        f.start_fix(&["src/export.rs"])
            .is_err_and(|e| e.contains("still running"))
    );
    assert!(
        f.db()
            .acquire_lease(
                &NewLease {
                    worktree: f.worktree_path(),
                    flow: other,
                    kind: LeaseKind::Reviewer,
                    holder: "standard".to_owned(),
                },
                5,
            )
            .is_err(),
        "the store refuses a review beside the writer too"
    );

    go.store(true, Ordering::SeqCst);
    f.fix_ready();
    assert!(f.db().open_leases(None).unwrap().is_empty());
    if let Err(refusal) =
        f.h.engine
            .rerun(other, wtm_app_lib::flow::findings::Rerun::All)
    {
        assert!(
            !refusal.message.contains("local fix"),
            "still refused for the fix: {refusal:?}"
        );
    }
}

#[test]
fn a_fix_interrupted_by_a_quit_keeps_its_worktree_across_the_relaunch_and_resume_finishes_it() {
    let go = Arc::new(AtomicBool::new(false));
    let f = Fixes::new(Shape::SameRepo, vec![Act::WaitFor(Arc::clone(&go))]);
    f.start_fix(&["src/export.rs"]).unwrap();
    for _ in 0..2_000 {
        if f.h.host.attempts_of("fixer") == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    f.h.engine.shutdown();
    for _ in 0..2_000 {
        if !f.h.engine.is_running(f.flow) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // The relaunch: a new engine on the same database.
    let relaunched = Engine::new(
        Arc::clone(&f.h.app),
        Arc::clone(&f.h.host) as Arc<dyn RoleHost>,
        Arc::new(FakeClock::new()) as Arc<dyn Clock>,
        Arc::new(Heard::default()) as Arc<dyn Notice>,
        Tuning {
            tick_ms: 5,
            skills_wait_ms: 30_000,
            settle_ms: 30_000,
            nudges: 1,
        },
    );
    *f.h.host.engine.lock() = Arc::downgrade(&relaunched);
    relaunched.recover().unwrap();

    let state = relaunched.view(f.flow).unwrap().flow.state;
    assert_eq!(state.phase, Phase::Fixing);
    assert!(matches!(
        state.hold,
        Some(wtm_core::flow::Hold::Interrupted { .. })
    ));
    let held = f.db().open_leases(Some(&f.worktree_path())).unwrap();
    assert_eq!(held.len(), 1, "the lease survived the restart");
    assert_eq!(held[0].kind, LeaseKind::Writer);
    assert!(
        f.db()
            .acquire_lease(
                &NewLease {
                    worktree: f.worktree_path(),
                    flow: second_review(&f),
                    kind: LeaseKind::Reviewer,
                    holder: "standard".to_owned(),
                },
                9,
            )
            .is_err(),
        "and still keeps a review out"
    );

    f.h.host.rescript(with_writer(fixes_export()));
    relaunched.resume(f.flow).unwrap();
    let mut view = relaunched.view(f.flow).unwrap();
    for _ in 0..4_000 {
        if view.flow.state.phase == Phase::FixReady && !view.running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        view = relaunched.view(f.flow).unwrap();
    }
    assert_eq!(view.flow.state.phase, Phase::FixReady);
    assert_eq!(view.fixes.last().unwrap().state, FixState::Ready);
    assert!(
        f.db().open_leases(None).unwrap().is_empty(),
        "released on completion"
    );
    go.store(true, Ordering::SeqCst);
}

#[test]
fn a_crash_mid_fix_is_interrupted_at_launch_keeps_its_lease_and_stopping_it_releases_the_worktree()
{
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    // What a crash leaves: a fix being written, its lease, and no thread.
    let db = f.db();
    let step = db
        .add_steps(
            f.flow,
            &[wtm_core::flow::NewStep {
                stage: 9,
                role: "fixer".to_owned(),
                provider: wtm_core::flow::RoleProvider::Claude,
                model: None,
                profile: wtm_core::flow::RoleProfile::Writer,
                workspace: wtm_core::flow::Workspace::Worktree,
                timeout_ms: 60_000,
                retries: 0,
                status: wtm_core::flow::StepStatus::Running,
                stands_in_for: None,
                handover: None,
                detail: None,
            }],
            5,
        )
        .unwrap()[0]
        .id;
    db.acquire_lease(
        &NewLease {
            worktree: f.worktree_path(),
            flow: f.flow,
            kind: LeaseKind::Writer,
            holder: "fixer".to_owned(),
        },
        5,
    )
    .unwrap();
    db.start_fix(
        &wtm_core::flow::NewFix {
            flow: f.flow,
            step,
            role: "fixer".to_owned(),
            findings: vec![f.finding("src/export.rs")],
            scope: wtm_core::flow::FixScope {
                paths: ["src/export.rs".to_owned()].into(),
            },
            base_sha: f.h.first.clone(),
        },
        5,
    )
    .unwrap();
    db.record(f.flow, &FlowEvent::Advanced { to: Phase::Fixing }, 5)
        .unwrap();

    let interrupted = f.h.engine.recover().unwrap();
    assert_eq!(interrupted, [f.flow]);
    let state = f.h.view(f.flow).flow.state;
    assert!(matches!(
        state.hold,
        Some(wtm_core::flow::Hold::Interrupted {
            cause: wtm_core::flow::InterruptCause::Crash
        })
    ));
    assert_eq!(
        db.open_leases(None).unwrap().len(),
        1,
        "a crash does not hand a half-written worktree to anyone"
    );

    f.h.engine.cancel_fix(f.flow).unwrap();
    let view = f.h.view(f.flow);
    assert_eq!(view.flow.state.phase, Phase::FixReady);
    assert_eq!(view.flow.state.hold, None);
    assert_eq!(view.fixes.last().unwrap().state, FixState::Cancelled);
    assert!(db.open_leases(None).unwrap().is_empty());
}

#[test]
fn recovery_releases_the_leases_nothing_can_come_back_for() {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    let other = second_review(&f);
    let db = f.db();
    // A review's lease its thread never let go, and a writer's lease of a run that has ended.
    db.acquire_lease(
        &NewLease {
            worktree: f.worktree_path(),
            flow: other,
            kind: LeaseKind::Reviewer,
            holder: "standard".to_owned(),
        },
        5,
    )
    .unwrap();
    db.acquire_lease(
        &NewLease {
            worktree: "/elsewhere".to_owned(),
            flow: f.flow,
            kind: LeaseKind::Writer,
            holder: "fixer".to_owned(),
        },
        5,
    )
    .unwrap();
    f.h.engine.recover().unwrap();
    let left = db.open_leases(None).unwrap();
    assert!(left.is_empty(), "{left:?}");
}

// ═══════════════════════════════ routes ═══════════════════════════════

#[test]
fn pushing_to_their_branch_is_a_fast_forward_sent_only_once_its_exact_destination_and_diff_are_approved()
 {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    f.start_fix(&["src/export.rs"]).unwrap();
    let fix = f.fix_ready().fixes.last().unwrap().clone();
    let head = fix.head_sha.clone().unwrap();

    let options = f.h.engine.route_options(f.flow).unwrap();
    let push = options
        .routes
        .iter()
        .find(|r| r.kind == RouteKind::PushToBranch)
        .unwrap();
    assert_eq!(push.unavailable, None);
    assert!(
        push.destination
            .as_deref()
            .is_some_and(|d| d.contains(&format!("refs/heads/{BRANCH}"))),
        "{push:?}"
    );

    let request = Fixes::request(RouteKind::PushToBranch);
    let preview = f.h.engine.preview_route(f.flow, &request).unwrap();
    let plan = preview.plan.clone().unwrap();
    assert_eq!(plan.push.reference, format!("refs/heads/{BRANCH}"));
    assert_eq!(plan.push.remote.as_deref(), Some("origin"));
    assert_eq!(plan.refspec(), format!("{head}:refs/heads/{BRANCH}"));
    assert!(
        preview
            .steps
            .iter()
            .all(|step| !step.contains("force") && !step.contains(" +")),
        "{:?}",
        preview.steps
    );
    assert_eq!(
        f.h.their_branch().as_deref(),
        Some(f.h.first.as_str()),
        "a preview sends nothing"
    );

    let wrong = f.h.engine.approve_route(f.flow, &request, "sha256:not-it");
    assert!(wrong.is_err_and(|e| e.message.contains("Preview it again")));
    assert!(
        f.db().routes(f.flow).unwrap().is_empty(),
        "nothing approved"
    );

    let approved =
        f.h.engine
            .approve_route(f.flow, &request, &preview.hash)
            .unwrap();
    let id = route_id(&approved);
    assert_eq!(f.settled(id), PublicationState::Published);
    assert_eq!(f.h.their_branch().as_deref(), Some(head.as_str()));
    let sent = f.db().route(id).unwrap().unwrap();
    assert_eq!(sent.approval.intent, ApprovalIntent::Route);
    let pins = &sent.approval.binding.0;
    assert_eq!(
        pins.get(&Pinned::Ref).map(String::as_str),
        Some(plan.push.reference.as_str())
    );
    assert_eq!(pins.get(&Pinned::Diff), fix.diff_hash.as_ref());
    assert_eq!(
        pins.get(&Pinned::Commit).map(String::as_str),
        Some(head.as_str())
    );
    assert_eq!(
        pins.get(&Pinned::Head).map(String::as_str),
        Some(f.h.first.as_str())
    );
    assert!(pins.contains_key(&Pinned::Remote));
    assert_eq!(sent.operations[0].attempts, 1);
}

#[test]
fn their_branch_moving_after_approval_stops_the_push_before_it_is_sent_and_asks_again() {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    f.start_fix(&["src/export.rs"]).unwrap();
    f.fix_ready();
    let request = Fixes::request(RouteKind::PushToBranch);
    let preview = f.h.engine.preview_route(f.flow, &request).unwrap();

    // The author pushes after the preview, on the forge, before GitHub's API shows it: only the
    // read of the remote branch before pushing can see it.
    let theirs = common::pull_requests::contribute(
        &f.h.upstream,
        &f.h.first,
        &[("src/theirs.rs", "theirs\n")],
    );
    f.h.upstream
        .git(&["update-ref", &format!("refs/heads/{BRANCH}"), &theirs]);

    let approved =
        f.h.engine
            .approve_route(f.flow, &request, &preview.hash)
            .unwrap();
    let id = route_id(&approved);
    assert_eq!(f.settled(id), PublicationState::Stale);
    assert_eq!(
        f.h.their_branch().as_deref(),
        Some(theirs.as_str()),
        "nothing was pushed over them"
    );
    let route = f.db().route(id).unwrap().unwrap();
    assert_eq!(route.operations[0].state, OperationState::NeedsAttention);
    assert_eq!(route.operations[0].attempts, 0, "never sent");
    assert!(matches!(
        &route.operations[0].attention,
        Some(Attention::HeadMoved { approved, current })
            if *approved == f.h.first && *current == theirs
    ));
    let items = f.db().inbox(Some(f.flow), None).unwrap();
    assert!(
        items
            .iter()
            .any(|i| i.kind == InboxKind::Approval && i.title.contains("their branch moved")),
        "{items:?}"
    );
    assert!(
        f.h.engine
            .retry_route(id)
            .is_err_and(|e| e.message.contains("new approval")),
        "a retry cannot push past a move"
    );

    // Once GitHub shows the move too, the route is not even offered as a fast-forward.
    f.h.gh.pushed(&theirs);
    let refused = f.h.engine.preview_route(f.flow, &request).unwrap_err();
    assert!(
        refused.message.contains("fast-forward"),
        "{}",
        refused.message
    );
}

#[test]
fn a_push_approved_again_after_a_stop_is_sent_once_and_the_stopped_approval_leaves_no_empty_card() {
    // What the click-through did: the author pushed, the push stopped; you left it, the author
    // withdrew their commit, and you approved the same push again.
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    f.start_fix(&["src/export.rs"]).unwrap();
    let head = f
        .fix_ready()
        .fixes
        .last()
        .unwrap()
        .head_sha
        .clone()
        .unwrap();
    let request = Fixes::request(RouteKind::PushToBranch);
    let preview = f.h.engine.preview_route(f.flow, &request).unwrap();
    let theirs = common::pull_requests::contribute(
        &f.h.upstream,
        &f.h.first,
        &[("src/theirs.rs", "theirs\n")],
    );
    f.h.upstream
        .git(&["update-ref", &format!("refs/heads/{BRANCH}"), &theirs]);
    let stopped = route_id(
        &f.h.engine
            .approve_route(f.flow, &request, &preview.hash)
            .unwrap(),
    );
    assert_eq!(f.settled(stopped), PublicationState::Stale);
    f.h.engine.cancel_route(stopped).unwrap();

    f.h.upstream
        .git(&["update-ref", &format!("refs/heads/{BRANCH}"), &f.h.first]);
    let again = route_id(
        &f.h.engine
            .approve_route(f.flow, &request, &preview.hash)
            .unwrap(),
    );
    assert_eq!(f.settled(again), PublicationState::Published);
    assert_eq!(f.h.their_branch().as_deref(), Some(head.as_str()));

    let shown = f.h.view(f.flow).pull_request.unwrap().routes;
    assert_eq!(
        shown
            .iter()
            .map(|r| (r.route.approval.id, r.state))
            .collect::<Vec<_>>(),
        [(again, PublicationState::Published)],
        "the same push is one write, now the later approval's; the earlier one has nothing to show"
    );
}

#[test]
fn a_follow_up_pushes_a_branch_of_its_own_and_opens_a_pull_request_into_their_branch() {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    f.start_fix(&["src/export.rs"]).unwrap();
    let head = f
        .fix_ready()
        .fixes
        .last()
        .unwrap()
        .head_sha
        .clone()
        .unwrap();

    let id = route_id(&f.approve(RouteKind::FollowUp));
    assert_eq!(f.settled(id), PublicationState::Published);
    let branch = format!("wtm/pr-4031-fix-{}", &head[..7]);
    assert_eq!(
        f.h.upstream
            .git(&["rev-parse", &format!("refs/heads/{branch}")])
            .trim(),
        head
    );
    assert_eq!(
        f.h.their_branch().as_deref(),
        Some(f.h.first.as_str()),
        "their branch untouched"
    );
    let opened = f.pulls.opened.lock().clone();
    assert_eq!(opened.len(), 1);
    let (repo, from, into, title, body) = &opened[0];
    assert_eq!(repo, "acme/webapp");
    assert_eq!(from, &branch);
    assert_eq!(into, BRANCH);
    assert_eq!(title, "Fixes for #4031");
    assert!(body.starts_with("The off-by-one, fixed."));
    assert!(body.contains("<!-- wtm:"), "found again by its marker");
    let view = f.h.engine.route_view(id).unwrap();
    assert_eq!(
        view.url.as_deref(),
        Some("https://github.com/acme/webapp/pull/100")
    );

    // Approving exactly the same again sends nothing more.
    f.approve(RouteKind::FollowUp);
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(f.pulls.opened.lock().len(), 1);
}

#[test]
fn a_follow_up_of_a_forks_branch_goes_to_your_fork_and_is_opened_in_theirs_or_is_not_offered() {
    let without = Fixes::new(
        Shape::Fork {
            maintainer_edits: false,
        },
        fixes_export(),
    );
    without.start_fix(&["src/export.rs"]).unwrap();
    without.fix_ready();
    let options = without.h.engine.route_options(without.flow).unwrap();
    let follow = options
        .routes
        .iter()
        .find(|r| r.kind == RouteKind::FollowUp)
        .unwrap();
    assert!(
        follow
            .unavailable
            .as_deref()
            .is_some_and(|why| why.contains("dana-lee/webapp")),
        "{follow:?}"
    );

    let f = Fixes::build(
        Shape::Fork {
            maintainer_edits: false,
        },
        fixes_export(),
        true,
    );
    f.start_fix(&["src/export.rs"]).unwrap();
    let head = f
        .fix_ready()
        .fixes
        .last()
        .unwrap()
        .head_sha
        .clone()
        .unwrap();
    let id = route_id(&f.approve(RouteKind::FollowUp));
    assert_eq!(f.settled(id), PublicationState::Published);
    let branch = format!("wtm/pr-4031-fix-{}", &head[..7]);
    let yours = f.h.yours.as_ref().unwrap();
    assert_eq!(
        yours
            .git(&["rev-parse", &format!("refs/heads/{branch}")])
            .trim(),
        head,
        "pushed to your fork"
    );
    let (repo, from, into, _, _) = f.pulls.opened.lock()[0].clone();
    assert_eq!(repo, "sam-rivera/webapp", "opened where their branch is");
    assert_eq!(from, format!("dana-lee:{branch}"));
    assert_eq!(into, BRANCH);
    assert_eq!(
        f.h.their_branch().as_deref(),
        Some(f.h.first.as_str()),
        "nothing pushed to their fork"
    );
}

#[test]
fn a_push_to_a_forks_branch_needs_maintainer_edits_and_write_access_and_then_goes_by_url() {
    let closed = Fixes::new(
        Shape::Fork {
            maintainer_edits: false,
        },
        fixes_export(),
    );
    closed.start_fix(&["src/export.rs"]).unwrap();
    closed.fix_ready();
    let push = |f: &Fixes| {
        f.h.engine
            .route_options(f.flow)
            .unwrap()
            .routes
            .into_iter()
            .find(|r| r.kind == RouteKind::PushToBranch)
            .unwrap()
    };
    assert!(
        push(&closed)
            .unavailable
            .is_some_and(|why| why.contains("not allowed maintainers"))
    );
    assert!(
        closed
            .h
            .engine
            .preview_route(closed.flow, &Fixes::request(RouteKind::PushToBranch))
            .is_err()
    );

    let open = Fixes::new(
        Shape::Fork {
            maintainer_edits: true,
        },
        fixes_export(),
    );
    open.start_fix(&["src/export.rs"]).unwrap();
    let head = open
        .fix_ready()
        .fixes
        .last()
        .unwrap()
        .head_sha
        .clone()
        .unwrap();
    *open.h.gh.permission.lock() = "READ";
    assert!(
        push(&open)
            .unavailable
            .is_some_and(|why| why.contains("does not include pushing"))
    );
    *open.h.gh.permission.lock() = "WRITE";
    let preview = open
        .h
        .engine
        .preview_route(open.flow, &Fixes::request(RouteKind::PushToBranch))
        .unwrap();
    let plan = preview.plan.clone().unwrap();
    assert_eq!(plan.push.remote, None, "no remote is added to your config");
    assert_eq!(plan.push.url, THEIR_FORK_URL);
    let id = route_id(
        &open
            .h
            .engine
            .approve_route(
                open.flow,
                &Fixes::request(RouteKind::PushToBranch),
                &preview.hash,
            )
            .unwrap(),
    );
    assert_eq!(open.settled(id), PublicationState::Published);
    assert_eq!(open.h.their_branch().as_deref(), Some(head.as_str()));
}

#[test]
fn an_integration_branch_is_never_where_a_fix_is_pushed() {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    f.start_fix(&["src/export.rs"]).unwrap();
    f.fix_ready();
    for branch in ["main", "release/2.0"] {
        f.h.gh.pr.lock()["headRefName"] = json!(branch);
        let push =
            f.h.engine
                .route_options(f.flow)
                .unwrap()
                .routes
                .into_iter()
                .find(|r| r.kind == RouteKind::PushToBranch)
                .unwrap();
        assert!(
            push.unavailable
                .as_deref()
                .is_some_and(|why| why.contains("integration branch")),
            "{branch}: {push:?}"
        );
    }
    assert!(f.db().routes(f.flow).unwrap().is_empty());
}

#[test]
fn suggestions_post_each_hunk_as_a_suggestion_through_the_review_outbox_under_a_route_approval() {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    f.start_fix(&["src/export.rs"]).unwrap();
    let fix = f.fix_ready().fixes.last().unwrap().clone();

    let request = Fixes::request(RouteKind::Suggestions);
    let preview = f.h.engine.preview_route(f.flow, &request).unwrap();
    let shown = preview.suggestions.clone().unwrap();
    assert_eq!(shown.comments.len(), 1);
    assert_eq!(shown.comments[0].path, "src/export.rs");
    assert_eq!(shown.comments[0].line, 1);
    assert!(
        shown.comments[0]
            .body
            .contains("```suggestion\nfn export() { for i in 0..n {} }\n```"),
        "{}",
        shown.comments[0].body
    );

    let approved =
        f.h.engine
            .approve_route(f.flow, &request, &preview.hash)
            .unwrap();
    let ApprovedRoute::Suggestions { publication } = approved else {
        panic!("suggestions post as a review");
    };
    let id = publication.publication.approval.id;
    for _ in 0..4_000 {
        let now = f.h.engine.publication_view(id).unwrap();
        if now.state != PublicationState::Posting {
            assert_eq!(now.state, PublicationState::Published);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let held = f.reviews.held.lock().clone();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].0.state, RemoteReviewState::Commented);
    assert_eq!(held[0].0.commit_id.as_deref(), Some(f.h.first.as_str()));
    assert!(held[0].1[0].body.contains("```suggestion"));
    let stored = f.db().publication(id).unwrap().unwrap();
    assert_eq!(stored.approval.intent, ApprovalIntent::Route);
    assert_eq!(
        stored.approval.binding.0.get(&Pinned::Diff),
        fix.diff_hash.as_ref()
    );
    assert_eq!(
        stored
            .approval
            .binding
            .0
            .get(&Pinned::Ref)
            .map(String::as_str),
        Some("refs/pull/4031/head")
    );
    assert_eq!(
        f.h.their_branch().as_deref(),
        Some(f.h.first.as_str()),
        "nothing pushed"
    );
}

#[test]
fn a_push_cut_off_by_a_crash_is_settled_by_reading_the_remote_and_never_pushed_twice() {
    let f = Fixes::new(Shape::SameRepo, fixes_export());
    f.start_fix(&["src/export.rs"]).unwrap();
    let head = f
        .fix_ready()
        .fixes
        .last()
        .unwrap()
        .head_sha
        .clone()
        .unwrap();
    let request = Fixes::request(RouteKind::PushToBranch);
    let preview = f.h.engine.preview_route(f.flow, &request).unwrap();
    let plan = preview.plan.clone().unwrap();
    // What a crash mid-push leaves: the approval, the push marked executing, and the push itself
    // on the forge with its answer lost.
    let db = f.db();
    let approved = db
        .approve(
            &wtm_core::flow::NewApproval::route(f.flow, &plan).unwrap(),
            &plan.operations(f.flow),
            7,
        )
        .unwrap();
    let op = approved.operations[0].id;
    db.change_operation(op, &wtm_core::flow::OperationChange::sending(), 8)
        .unwrap();
    f.h.clone.git(&[
        "push",
        "-q",
        "origin",
        &format!("{head}:refs/heads/{BRANCH}"),
    ]);

    let checked = f.h.engine.recover_publications().unwrap();
    assert_eq!(checked, [approved.approval.id]);
    let settled = db.operation(op).unwrap().unwrap();
    assert_eq!(settled.state, OperationState::Done);
    assert_eq!(settled.attempts, 1, "recovery sends nothing");
    assert!(matches!(
        settled.result,
        Some(wtm_core::flow::OperationResult::Pushed { adopted: true, .. })
    ));
}

#[test]
fn a_writer_has_no_tool_that_pushes_and_asking_for_one_sends_nothing() {
    let f = Fixes::new(
        Shape::SameRepo,
        vec![
            Act::Write("src/export.rs", FIXED),
            Act::Checkpoint("Fix the loop"),
            Act::Handoff("Fixed."),
        ],
    );
    f.start_fix(&["src/export.rs"]).unwrap();
    f.fix_ready();
    // A writer's tools are the run tools: `flow_request` records that a push was asked for.
    let fixer = wtm_app_lib::app::FlowRole {
        flow: f.flow,
        role: "fixer".to_owned(),
        authority: wtm_agent::authority::Authority::Writer,
        browser: None,
        design: false,
    };
    let names: Vec<String> = wtm_app_lib::flow_tools::definitions(fixer.authority)
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.contains("push") || n.contains("route"))
    );
    assert!(f.db().routes(f.flow).unwrap().is_empty());
    assert!(f.db().publications(f.flow).unwrap().is_empty());
    assert_eq!(f.h.their_branch().as_deref(), Some(f.h.first.as_str()));
    let _ = finding_at;
}

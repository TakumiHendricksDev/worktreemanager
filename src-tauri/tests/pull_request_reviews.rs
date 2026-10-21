//! A teammate's pull request reviewed end to end: Run Review on its review worktree, a push by the
//! author, Re-review changes, and keeping the result private.
//!
//! What is real: the app, `GhForge` parsing `gh`'s own JSON, git on two repositories, the snapshot,
//! the SQLite store and the run tools. The forge is a second [`GitFixture`] playing the base
//! repository, whose pull request ref is made with `update-ref` and reached through
//! `url.<path>.insteadOf`, as `review_worktrees.rs` does it; `gh` is a runner that answers each
//! read with JSON in `gh` 2.100.0's shape, built from the fixtures' fork reply with the head the
//! test says the author has pushed. The agents are `common::agents`' scripts.
//!
//! Each test is one thing a reviewer would notice if it broke: the review compared with the wrong
//! base, a push the review never notices, a re-review that reruns everything or moves a worktree
//! with your edits in it, the earlier head's results gone, or a "private" outcome that reached
//! GitHub. Every `gh` call any test makes is checked to be a read.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{Value, json};
use wtm_app_lib::app::{App, WORK_VIEW_PREF};
use wtm_app_lib::flow::findings::Rerun;
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::{Engine, FlowView, Notice, Tuning, pull_request};
use wtm_app_lib::reviews::{self, ReviewWorktreeArgs};
use wtm_config::AppPaths;
use wtm_core::error::ExecError;
use wtm_core::flow::{EarlierFate, FlowId, FlowKind, OutcomeKind, Phase, PushToBranch, WorkItemId};
use wtm_core::model::ProjectId;
use wtm_core::ports::clock::Clock;
use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation, Output};
use wtm_core::ports::forge::Forge;
use wtm_core::ports::progress::NullProgress;
use wtm_core::usecase::ReviewCheckout;
use wtm_forge::GhForge;
use wtm_testkit::{FakeClock, GitFixture, NullPtySink};

mod common;
use common::agents::{Behaviour, FakeHost, Heard, Script};

const FORGE_URL: &str = "https://github.com/acme/webapp.git";
const PR_URL: &str = "https://github.com/acme/webapp/pull/4031";

// ═══════════════════════════════ a forge that speaks gh's JSON ═══════════════════════════════

/// `gh`, answering the reads `GhForge` makes with JSON in its own shape.
struct GhJson {
    pr: Mutex<Value>,
    permission: Mutex<&'static str>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl GhJson {
    fn new(head: &str, base: &str) -> Arc<Self> {
        let mut pr: Value = serde_json::from_str(include_str!(
            "../../crates/wtm-forge/tests/fixtures/pr_fork.json"
        ))
        .unwrap();
        pr["headRefName"] = json!("feature/csv-export");
        pr["headRefOid"] = json!(head);
        pr["baseRefOid"] = json!(base);
        pr["maintainerCanModify"] = json!(false);
        pr["title"] = json!("Export units as CSV");
        Arc::new(Self {
            pr: Mutex::new(pr),
            permission: Mutex::new("WRITE"),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// The author pushed: the forge reports `head` from now on.
    fn pushed(&self, head: &str) {
        self.pr.lock()["headRefOid"] = json!(head);
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().clone()
    }

    /// Every call so far was a read. See `crates/wtm-forge/tests/gh.rs` for the same list.
    fn assert_only_reads(&self) {
        for argv in self.calls() {
            let words: Vec<&str> = argv.iter().map(String::as_str).collect();
            let read = match words.as_slice() {
                ["gh", "--version"]
                | ["gh", "pr", "view" | "list", ..]
                | ["gh", "repo", "view", ..] => true,
                ["gh", "auth", "status", rest @ ..] => !rest.contains(&"--show-token"),
                ["gh", "api", rest @ ..] => rest.windows(2).any(|w| w == ["--method", "GET"]),
                _ => false,
            };
            assert!(read, "not a read: {}", argv.join(" "));
        }
    }
}

impl CommandRunner for GhJson {
    fn run(&self, inv: &Invocation, _cancel: &CancelToken) -> Result<Output, ExecError> {
        self.calls.lock().push(inv.argv.clone());
        let words: Vec<&str> = inv.argv.iter().map(String::as_str).collect();
        let stdout = match words.as_slice() {
            ["gh", "--version"] => "gh version 2.100.0 (2026-09-03)\n".to_owned(),
            ["gh", "auth", "status", ..] => {
                include_str!("../../crates/wtm-forge/tests/fixtures/auth_status.json").to_owned()
            }
            ["gh", "pr", "view", ..] => self.pr.lock().to_string(),
            ["gh", "repo", "view", ..] => {
                format!(r#"{{"viewerPermission":"{}"}}"#, self.permission.lock())
            }
            other => panic!("the fake gh was asked something it does not answer: {other:?}"),
        };
        Ok(Output {
            code: 0,
            stdout,
            stderr: String::new(),
            duration_ms: 1,
        })
    }

    fn run_allow_failure(
        &self,
        inv: &Invocation,
        cancel: &CancelToken,
    ) -> Result<Output, ExecError> {
        self.run(inv, cancel)
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/usr/bin").join(program))
    }

    fn resolved_path(&self) -> String {
        "/usr/bin".to_owned()
    }
}

// ═══════════════════════════════ the harness ═══════════════════════════════

/// Two roles with paths of their own, so a push to one directory reruns one of them. Neither takes
/// `findings` as an input: a re-review gives them their earlier findings anyway.
const WORKFLOW: &str = r#"
forge = "github"

[role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
inputs = ["diff"]
paths = ["src/**"]
timeout_ms = 60000

[role.docs]
objective = "Check the documentation the change touches"
provider = "claude"
inputs = ["diff"]
paths = ["docs/**"]
timeout_ms = 60000

[recipe.standard]
label = "Standard"

[[recipe.standard.stage]]
roles = ["correctness", "docs"]
"#;

struct Harness {
    app: Arc<App>,
    gh: Arc<GhJson>,
    host: Arc<FakeHost>,
    engine: Arc<Engine>,
    project: String,
    item: WorkItemId,
    /// The base repository on the forge.
    upstream: GitFixture,
    /// Your clone of it, the registered project.
    clone: GitFixture,
    /// The pull request's first head, which the review worktree is prepared at.
    first: String,
    worktree: PathBuf,
    _data: tempfile::TempDir,
}

fn finding_at(title: &str, path: &str) -> Value {
    json!({ "findings": [{
        "title": title,
        "severity": "notable",
        "class": "suggestion",
        "body": "Worth a look.",
        "path": path,
        "line": 1,
    }]})
}

/// The usual reviewers: `correctness` reports one thing in `src/` on its first attempt and nothing
/// after, `docs` one thing in `docs/` every time.
fn reviewers() -> Script {
    Box::new(|role, attempt| match (role, attempt) {
        ("correctness", 1) => Behaviour::Submit(vec![finding_at(
            "Off by one in the export loop",
            "src/export.rs",
        )]),
        ("correctness", _) => Behaviour::Submit(vec![json!({ "findings": [] })]),
        _ => Behaviour::Submit(vec![finding_at(
            "The guide names the old flag",
            "docs/guide.md",
        )]),
    })
}

impl Harness {
    /// A fork's pull request #4031 into acme/webapp's `main`, queued and with a review worktree
    /// prepared at its first head.
    fn new(script: Script) -> Self {
        let upstream = GitFixture::new();
        let clone = GitFixture::new();
        clone.git(&["remote", "set-url", "origin", FORGE_URL]);
        clone.git(&[
            "config",
            &format!("url.{}.insteadOf", upstream.root().display()),
            FORGE_URL,
        ]);
        // Something on the base that is not in the pull request, so comparing with the wrong base
        // would show in the review's changed files.
        upstream.commit("CHANGELOG.md", "nothing yet\n", "Start a changelog");
        let base = upstream.git(&["rev-parse", "main"]).trim().to_owned();
        let first = contribute(
            &upstream,
            &base,
            &[
                ("src/export.rs", "fn export() { for i in 0..=n {} }\n"),
                ("docs/guide.md", "Run with --old-flag.\n"),
            ],
        );

        let data = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let paths = AppPaths::rooted(data.path());
        let config_file = paths.config_file.clone();
        let gh = GhJson::new(&first, &base);
        let mut app = App::with_paths(paths).unwrap();
        app.forge =
            Arc::new(GhForge::new(Arc::clone(&gh) as Arc<dyn CommandRunner>)) as Arc<dyn Forge>;
        let root = app.register(clone.root()).unwrap();
        app.config.set_user_pref(WORK_VIEW_PREF, "on").unwrap();
        let mut text = std::fs::read_to_string(&config_file).unwrap_or_default();
        write!(
            text,
            "\n[projects.\"{}\".config.workflow]\n",
            root.display()
        )
        .unwrap();
        for line in WORKFLOW.lines() {
            // Each table under the project's own workflow, which is where `forge` may live.
            if let Some(table) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
                writeln!(
                    text,
                    "[[projects.\"{}\".config.workflow.{table}]]",
                    root.display()
                )
                .unwrap();
            } else if let Some(table) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                writeln!(
                    text,
                    "[projects.\"{}\".config.workflow.{table}]",
                    root.display()
                )
                .unwrap();
            } else {
                writeln!(text, "{line}").unwrap();
            }
        }
        std::fs::write(&config_file, text).unwrap();
        let project = ProjectId::from_root(&root).to_string();
        let app = Arc::new(app);

        let item = reviews::add(&app, &project, PR_URL).unwrap().item.id;
        reviews::prepare(
            &app,
            &project,
            &ReviewWorktreeArgs {
                item_id: item,
                expected_head: Some(first.clone()),
                checkout: ReviewCheckout::Detached,
                prepare_environment: false,
                acknowledged: Vec::new(),
                rows: 24,
                cols: 80,
            },
            &NullProgress,
            Arc::new(NullPtySink),
        )
        .unwrap();
        let worktree = clone.parent().join(format!(
            "{}-pr-4031",
            clone.root().file_name().unwrap().to_string_lossy()
        ));
        assert!(
            worktree.is_dir(),
            "no review worktree at {}",
            worktree.display()
        );

        let host = FakeHost::new(Arc::clone(&app), script);
        let engine = Engine::new(
            Arc::clone(&app),
            Arc::clone(&host) as Arc<dyn RoleHost>,
            Arc::new(FakeClock::new()) as Arc<dyn Clock>,
            Arc::new(Heard::default()) as Arc<dyn Notice>,
            Tuning {
                tick_ms: 5,
                skills_wait_ms: 30_000,
                settle_ms: 30_000,
                nudges: 1,
            },
        );
        *host.engine.lock() = Arc::downgrade(&engine);
        Self {
            app,
            gh,
            host,
            engine,
            project,
            item,
            upstream,
            clone,
            first,
            worktree,
            _data: data,
        }
    }

    fn start(&self) -> FlowId {
        pull_request::start(
            &self.app,
            &self.engine,
            &self.project,
            self.item,
            "standard",
            Some(&self.first),
        )
        .unwrap()
    }

    fn view(&self, flow: FlowId) -> FlowView {
        self.engine.view(flow).unwrap()
    }

    fn consolidated(&self, flow: FlowId) -> FlowView {
        for _ in 0..4_000 {
            let view = self.view(flow);
            if view.flow.state.phase == Phase::Consolidated && !view.running {
                return view;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("never consolidated: {:#?}", self.view(flow));
    }

    /// The author pushes another commit on top of `on`, changing `files`, and the forge says so.
    fn push(&self, on: &str, files: &[(&str, &str)]) -> String {
        let head = contribute(&self.upstream, on, files);
        self.gh.pushed(&head);
        head
    }

    /// The window coming back into focus: the pull request is read again.
    fn focus(&self) {
        reviews::refresh(&self.app, &self.project, self.item).unwrap();
    }

    fn head_of_review_worktree(&self) -> String {
        self.clone
            .git_in(&self.worktree, &["rev-parse", "HEAD"])
            .trim()
            .to_owned()
    }

    fn briefs(&self, role: &str) -> Vec<String> {
        self.host
            .opened
            .lock()
            .iter()
            .filter(|o| o.role == role)
            .map(|o| o.brief.clone())
            .collect()
    }
}

/// A commit on top of `on` in the forge's repository that only `refs/pull/4031/head` reaches, as a
/// fork's is.
fn contribute(upstream: &GitFixture, on: &str, files: &[(&str, &str)]) -> String {
    upstream.git(&["checkout", "-q", "-b", "contribution", on]);
    for (path, contents) in files {
        let at = upstream.root().join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, contents).unwrap();
    }
    upstream.git(&["add", "."]);
    upstream.git(&["commit", "-q", "-m", "A contribution"]);
    let sha = upstream.git(&["rev-parse", "HEAD"]).trim().to_owned();
    upstream.git(&["update-ref", "refs/pull/4031/head", &sha]);
    upstream.git(&["checkout", "-q", "main"]);
    upstream.git(&["branch", "-q", "-D", "contribution"]);
    sha
}

fn short(sha: &str) -> &str {
    &sha[..10]
}

// ═══════════════════════════════ the tests ═══════════════════════════════

#[test]
fn run_review_on_a_forks_pull_request_compares_it_with_the_pull_requests_base_and_says_whose_it_is()
{
    let h = Harness::new(reviewers());
    let flow = h.start();
    let view = h.consolidated(flow);

    assert_eq!(view.flow.state.kind, FlowKind::ReviewPr);
    assert_eq!(view.review.base, "origin/main");
    let snapshot = view.snapshot.as_ref().unwrap();
    assert_eq!(snapshot.head_sha, h.first);
    assert_eq!(
        snapshot.base_sha,
        h.upstream.git(&["rev-parse", "main"]).trim(),
        "measured from where the pull request left its base"
    );
    let brief = &h.briefs("correctness")[0];
    assert!(brief.contains("  - src/export.rs"), "{brief}");
    assert!(
        !brief.contains("CHANGELOG.md"),
        "a commit on the base was reviewed as part of the pull request: {brief}"
    );
    assert!(
        brief.contains("Pull request #4031") && brief.contains("by @sam-rivera"),
        "{brief}"
    );
    assert!(brief.contains("from a fork"), "{brief}");

    let pr = view.pull_request.as_ref().unwrap();
    assert_eq!(pr.number, 4031);
    assert_eq!(pr.reviewed_head.as_deref(), Some(h.first.as_str()));
    assert_eq!(pr.moved_to, None);
    assert_eq!(
        pr.push_to_branch,
        Some(PushToBranch::ForkWithoutMaintainerEdits),
        "a fork whose author did not allow maintainer edits takes no push, write access or not"
    );
    assert_eq!(view.findings.len(), 2);
    h.gh.assert_only_reads();
}

#[test]
fn a_push_by_the_author_makes_the_review_stale_on_the_next_read_and_rerun_affected_is_refused() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    h.consolidated(flow);
    let second = h.push(
        &h.first,
        &[("src/export.rs", "fn export() { for i in 0..n {} }\n")],
    );

    // Before the window reads the pull request again, nothing has said it moved.
    let before = h.engine.staleness(flow).unwrap();
    assert_eq!(before.pull_request_head, None);
    assert!(!before.changed);

    h.focus();
    let stale = h.engine.staleness(flow).unwrap();
    assert_eq!(stale.pull_request_head.as_deref(), Some(second.as_str()));
    assert!(
        !stale.pull_request_head_here,
        "the new commits have not been fetched"
    );
    assert!(stale.changed);
    assert!(
        !stale.complete,
        "what changed cannot be known before fetching"
    );
    assert_eq!(
        stale.stale_findings.len(),
        2,
        "with the new commits unknown, every finding is stale"
    );
    let view = h.view(flow);
    assert_eq!(
        view.pull_request.as_ref().unwrap().moved_to.as_deref(),
        Some(second.as_str())
    );

    let refused = h.engine.rerun(flow, Rerun::Affected).unwrap_err();
    assert!(
        refused.message.contains("Re-review changes"),
        "{}",
        refused.message
    );
    assert_eq!(
        h.head_of_review_worktree(),
        h.first,
        "a refused rerun moved the worktree"
    );
    h.gh.assert_only_reads();
}

#[test]
fn re_review_changes_moves_the_worktree_to_the_new_head_and_reruns_only_the_role_it_affects() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    let first_round = h.consolidated(flow);
    let earlier_ids: Vec<_> = first_round.findings.iter().map(|f| f.finding.id).collect();
    let second = h.push(
        &h.first,
        &[("src/export.rs", "fn export() { for i in 0..n {} }\n")],
    );
    h.focus();

    let rereviewed =
        pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&second)).unwrap();
    assert_eq!(rereviewed.roles, vec!["correctness".to_owned()]);
    let view = h.consolidated(flow);

    assert_eq!(h.head_of_review_worktree(), second);
    assert_eq!(
        h.clone
            .git_in(&h.worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
            .trim(),
        "HEAD",
        "still detached"
    );
    assert_eq!(h.briefs("correctness").len(), 2);
    assert_eq!(
        h.briefs("docs").len(),
        1,
        "docs/ did not change, so docs did not run again"
    );

    // The rerun role is asked to verify what it said before, though it takes no `findings`.
    let again = &h.briefs("correctness")[1];
    assert!(
        again.contains(&format!(
            "The author pushed new commits since you reviewed this pull request at {}",
            short(&h.first)
        )),
        "{again}"
    );
    assert!(
        again.contains("] Off by one in the export loop (src/export.rs:1)")
            && again.contains("`repeats` set to its id"),
        "{again}"
    );

    let pr = view.pull_request.as_ref().unwrap();
    assert_eq!(pr.reviewed_head.as_deref(), Some(second.as_str()));
    assert_eq!(pr.moved_to, None);
    let titles: Vec<_> = view
        .findings
        .iter()
        .map(|f| f.finding.title.clone())
        .collect();
    assert_eq!(titles, vec!["The guide names the old flag".to_owned()]);

    // The first head's results are kept, with what became of each.
    assert_eq!(pr.earlier.len(), 1);
    let earlier = &pr.earlier[0];
    assert_eq!(earlier.head.head.head_sha, h.first);
    assert_eq!(earlier.head.head.next_head, second);
    assert_eq!(
        earlier
            .findings
            .iter()
            .map(|f| f.record.finding.id)
            .collect::<Vec<_>>(),
        earlier_ids
    );
    let fate = |title: &str| {
        earlier
            .findings
            .iter()
            .find(|f| f.record.finding.title == title)
            .unwrap()
            .fate
    };
    assert_eq!(
        fate("Off by one in the export loop"),
        EarlierFate::NotReportedAgain
    );
    assert_eq!(fate("The guide names the old flag"), EarlierFate::Unchanged);

    // Nothing left to follow: the review is at the pull request's head.
    let current = h.engine.staleness(flow).unwrap();
    assert_eq!(current.pull_request_head, None);
    assert!(!current.changed);
    h.gh.assert_only_reads();
}

#[test]
fn re_review_changes_refuses_a_head_that_moved_again_since_it_was_shown_and_moves_nothing() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    h.consolidated(flow);
    let second = h.push(&h.first, &[("src/export.rs", "fn export() {}\n")]);
    h.focus();
    let third = h.push(&second, &[("src/export.rs", "fn export() { todo!() }\n")]);

    // The window showed the second head; the forge now says the third.
    let refused =
        pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&second)).unwrap_err();
    assert!(
        refused.message.contains("moved again"),
        "{}",
        refused.message
    );
    assert_eq!(h.head_of_review_worktree(), h.first);
    let pr = h.view(flow).pull_request.unwrap();
    assert!(
        pr.earlier.is_empty(),
        "a refused re-review recorded a head as left"
    );
    assert_eq!(pr.moved_to.as_deref(), Some(third.as_str()));
}

#[test]
fn uncommitted_changes_in_the_review_worktree_stop_a_re_review_before_anything_moves() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    h.consolidated(flow);
    let second = h.push(&h.first, &[("src/export.rs", "fn export() {}\n")]);
    h.focus();
    std::fs::write(h.worktree.join("src/export.rs"), "// trying something\n").unwrap();

    let refused =
        pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&second)).unwrap_err();
    assert!(
        refused.message.contains("not committed"),
        "{}",
        refused.message
    );
    assert_eq!(h.head_of_review_worktree(), h.first);
    assert_eq!(
        std::fs::read_to_string(h.worktree.join("src/export.rs")).unwrap(),
        "// trying something\n",
        "your edit was replaced"
    );
    assert!(h.view(flow).pull_request.unwrap().earlier.is_empty());
    assert_eq!(h.briefs("correctness").len(), 1, "a role ran again");
}

#[test]
fn keeping_a_review_private_records_notes_at_the_reviewed_head_and_asks_the_forge_nothing() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    let view = h.consolidated(flow);
    let calls = h.gh.calls().len();

    let kept = h
        .engine
        .keep_private(
            flow,
            "  Ask Sam whether the loop bound is meant to include n. ",
        )
        .unwrap();

    assert_eq!(kept.decision.kind, OutcomeKind::Private);
    assert_eq!(kept.decision.head_sha, h.first);
    assert_eq!(
        kept.decision.notes,
        "Ask Sam whether the loop bound is meant to include n."
    );
    assert_eq!(
        kept.decision.findings,
        view.findings
            .iter()
            .map(|f| f.finding.id)
            .collect::<Vec<_>>()
    );
    let outcomes = h.view(flow).pull_request.unwrap().outcomes;
    assert_eq!(outcomes.last(), Some(&kept));
    assert_eq!(h.gh.calls().len(), calls, "keeping notes private called gh");
}

#[test]
fn im_reviewing_this_is_kept_in_wtm_and_asks_the_forge_nothing() {
    let h = Harness::new(reviewers());
    let calls = h.gh.calls().len();

    let on = reviews::set_reviewing(&h.app, &h.project, h.item, true).unwrap();
    assert!(on.reviewing_since.is_some());
    let queued = reviews::queue(&h.app, &h.project).unwrap();
    assert_eq!(queued.items[0].reviewing_since, on.reviewing_since);
    let off = reviews::set_reviewing(&h.app, &h.project, h.item, false).unwrap();
    assert_eq!(off.reviewing_since, None);

    assert_eq!(h.gh.calls().len(), calls, "the flag reached gh");
}

#[test]
fn a_finding_the_rerun_role_names_in_repeats_is_still_reported_and_the_one_it_drops_is_not() {
    // The label the brief gives the second report, known once the first round has run.
    let label: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let named = Arc::clone(&label);
    let h = Harness::new(Box::new(move |role, attempt| match (role, attempt) {
        ("correctness", 1) => Behaviour::Submit(vec![json!({ "findings": [
            { "title": "Off by one in the export loop", "severity": "notable",
              "class": "suggestion", "body": "x", "path": "src/export.rs", "line": 1 },
            { "title": "Names are not escaped", "severity": "notable",
              "class": "suggestion", "body": "x", "path": "src/export.rs", "line": 5 },
        ]})]),
        // Run again: the escaping problem is still there, now lower down, and says so.
        ("correctness", 2) => Behaviour::Submit(vec![json!({ "findings": [
            { "title": "Names are still not escaped", "severity": "notable",
              "class": "suggestion", "body": "x", "path": "src/export.rs", "line": 9,
              "repeats": named.lock().clone() },
        ]})]),
        // And again, after a push that fixed it.
        ("correctness", _) => Behaviour::Submit(vec![json!({ "findings": [] })]),
        _ => Behaviour::Submit(vec![finding_at(
            "The guide names the old flag",
            "docs/guide.md",
        )]),
    }));
    let flow = h.start();
    let first_round = h.consolidated(flow);
    let step = first_round
        .steps
        .iter()
        .find(|s| s.role == "correctness")
        .unwrap()
        .id;
    let invocation = first_round
        .invocations
        .iter()
        .find(|i| i.step == step)
        .unwrap()
        .id;
    *label.lock() = format!("E{}.1", invocation.0);

    let second = h.push(
        &h.first,
        &[("src/export.rs", "fn export() { for i in 0..n {} }\n")],
    );
    h.focus();
    pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&second)).unwrap();
    let view = h.consolidated(flow);

    let brief = &h.briefs("correctness")[1];
    assert!(
        brief.contains(&format!(
            "- [E{}.1] Names are not escaped (src/export.rs:5)",
            invocation.0
        )),
        "{brief}"
    );
    let earlier = &view.pull_request.as_ref().unwrap().earlier[0];
    let fate = |title: &str| {
        earlier
            .findings
            .iter()
            .find(|f| f.record.finding.title == title)
            .unwrap()
            .fate
    };
    assert_eq!(fate("Names are not escaped"), EarlierFate::StillReported);
    assert_eq!(
        fate("Off by one in the export loop"),
        EarlierFate::NotReportedAgain
    );
    assert_eq!(fate("The guide names the old flag"), EarlierFate::Unchanged);

    // A third head that fixes it: the second head's report is not reported again, and the first
    // head still says what the second round found, not what the third did.
    let third = h.push(&second, &[("src/export.rs", "fn export() {}\n")]);
    h.focus();
    pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&third)).unwrap();
    let view = h.consolidated(flow);
    let heads = &view.pull_request.as_ref().unwrap().earlier;
    assert_eq!(heads.len(), 2);
    let fate_at = |head: usize, title: &str| {
        heads[head]
            .findings
            .iter()
            .find(|f| f.record.finding.title == title)
            .unwrap()
            .fate
    };
    assert_eq!(
        fate_at(0, "Names are not escaped"),
        EarlierFate::StillReported
    );
    assert_eq!(
        fate_at(1, "Names are still not escaped"),
        EarlierFate::NotReportedAgain
    );
}

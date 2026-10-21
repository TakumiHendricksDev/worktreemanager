//! A teammate's pull request on a fake forge, for the tests that review and publish one.
//!
//! The forge is a second [`GitFixture`] playing the base repository, whose pull request ref is made
//! with `update-ref` and reached through `url.<path>.insteadOf`; `gh` is a runner that answers each
//! read with JSON in `gh` 2.100.0's shape, built from the fixtures' fork reply with the head the test
//! says the author has pushed. Shared by `pull_request_reviews.rs` and `publish_review.rs`.

#![allow(dead_code, clippy::unwrap_used)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{Value, json};
use wtm_app_lib::app::{App, WORK_VIEW_PREF};
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::{Engine, FlowView, Notice, Tuning, pull_request};
use wtm_app_lib::reviews::{self, ReviewWorktreeArgs};
use wtm_config::AppPaths;
use wtm_core::error::ExecError;
use wtm_core::flow::{FlowId, Phase, WorkItemId};
use wtm_core::model::ProjectId;
use wtm_core::ports::clock::Clock;
use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation, Output};
use wtm_core::ports::forge::{Forge, ReviewWriter};
use wtm_core::ports::progress::NullProgress;
use wtm_core::usecase::ReviewCheckout;
use wtm_forge::GhForge;
use wtm_testkit::{FakeClock, GitFixture, NullPtySink};

use super::agents::{Behaviour, FakeHost, Heard, Script};

pub(crate) const FORGE_URL: &str = "https://github.com/acme/webapp.git";
pub(crate) const PR_URL: &str = "https://github.com/acme/webapp/pull/4031";

// ═══════════════════════════════ a forge that speaks gh's JSON ═══════════════════════════════

/// `gh`, answering the reads `GhForge` makes with JSON in its own shape.
pub(crate) struct GhJson {
    pub(crate) pr: Mutex<Value>,
    pub(crate) permission: Mutex<&'static str>,
    /// What `compare/{base}...{head}` answers: the files the change touches and their hunks.
    pub(crate) compare: Mutex<Value>,
    pub(crate) calls: Mutex<Vec<Vec<String>>>,
}

impl GhJson {
    pub(crate) fn new(head: &str, base: &str) -> Arc<Self> {
        let mut pr: Value = serde_json::from_str(include_str!(
            "../../../crates/wtm-forge/tests/fixtures/pr_fork.json"
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
            compare: Mutex::new(json!({ "files": [
                { "filename": "src/export.rs", "status": "added", "patch": "@@ -0,0 +1 @@\n+fn export() {}" },
                { "filename": "docs/guide.md", "status": "added", "patch": "@@ -0,0 +1 @@\n+Run it." },
            ]})),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// The author pushed: the forge reports `head` from now on.
    pub(crate) fn pushed(&self, head: &str) {
        self.pr.lock()["headRefOid"] = json!(head);
    }

    pub(crate) fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().clone()
    }

    /// Every call so far was a read. See `crates/wtm-forge/tests/gh.rs` for the same list.
    pub(crate) fn assert_only_reads(&self) {
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
                include_str!("../../../crates/wtm-forge/tests/fixtures/auth_status.json").to_owned()
            }
            ["gh", "pr", "view", ..] => self.pr.lock().to_string(),
            ["gh", "repo", "view", ..] => {
                format!(r#"{{"viewerPermission":"{}"}}"#, self.permission.lock())
            }
            ["gh", "api", "--method", "GET", "--hostname", _, path]
                if path.contains("/compare/") =>
            {
                self.compare.lock().to_string()
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
pub(crate) const WORKFLOW: &str = r#"
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

pub(crate) struct Harness {
    pub(crate) app: Arc<App>,
    pub(crate) gh: Arc<GhJson>,
    pub(crate) host: Arc<FakeHost>,
    pub(crate) engine: Arc<Engine>,
    pub(crate) project: String,
    pub(crate) item: WorkItemId,
    /// The base repository on the forge.
    pub(crate) upstream: GitFixture,
    /// Your clone of it, the registered project.
    pub(crate) clone: GitFixture,
    /// The pull request's first head, which the review worktree is prepared at.
    pub(crate) first: String,
    pub(crate) worktree: PathBuf,
    pub(crate) _data: tempfile::TempDir,
}

pub(crate) fn finding_at(title: &str, path: &str) -> Value {
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
pub(crate) fn reviewers() -> Script {
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
    pub(crate) fn new(script: Script) -> Self {
        Self::with_writer(script, None)
    }

    /// As [`Self::new`], with `writer` posting reviews in place of `gh`, for a test that publishes.
    pub(crate) fn with_writer(script: Script, writer: Option<Arc<dyn ReviewWriter>>) -> Self {
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
        if let Some(writer) = writer {
            app.review_writer = writer;
        }
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

    pub(crate) fn start(&self) -> FlowId {
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

    pub(crate) fn view(&self, flow: FlowId) -> FlowView {
        self.engine.view(flow).unwrap()
    }

    pub(crate) fn consolidated(&self, flow: FlowId) -> FlowView {
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
    pub(crate) fn push(&self, on: &str, files: &[(&str, &str)]) -> String {
        let head = contribute(&self.upstream, on, files);
        self.gh.pushed(&head);
        head
    }

    /// The window coming back into focus: the pull request is read again.
    pub(crate) fn focus(&self) {
        reviews::refresh(&self.app, &self.project, self.item).unwrap();
    }

    pub(crate) fn head_of_review_worktree(&self) -> String {
        self.clone
            .git_in(&self.worktree, &["rev-parse", "HEAD"])
            .trim()
            .to_owned()
    }

    pub(crate) fn briefs(&self, role: &str) -> Vec<String> {
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
pub(crate) fn contribute(upstream: &GitFixture, on: &str, files: &[(&str, &str)]) -> String {
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

pub(crate) fn short(sha: &str) -> &str {
    &sha[..10]
}

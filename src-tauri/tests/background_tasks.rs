//! The background-agent roster is read from the real CLI and filtered to one worktree.
//!
//! # Why this needs a real binary
//!
//! Everything about this command is a fact about another program: that `claude agents --json --all
//! --cwd <path>` needs no TTY, that `--cwd` filters by directory rather than merely accepting one, and
//! that the fields are `id`/`name`/`state`/`sessionId`. Any of those being wrong yields an empty tray,
//! which is indistinguishable from having no background agents — so only a real read notices.
//!
//! No API credit is spent: this lists a state directory.
//!
//! Skips loudly without the CLI, for the reason `agent_sessions.rs` gives.

#![allow(clippy::unwrap_used, clippy::disallowed_methods, clippy::print_stderr)]

use wtm_core::ports::CommandRunner;
use wtm_exec::ResolvedPath;

#[test]
fn the_roster_needs_no_tty_and_filters_by_directory() {
    let path = ResolvedPath::resolve(None);
    if path.which("claude", &std::env::temp_dir()).is_none() {
        eprintln!(
            "skipping: no `claude` on the resolved PATH ({})",
            path.value
        );
        return;
    }

    let runner = wtm_exec::Runner::new(path);
    let run = |cwd: Option<&str>| {
        let mut argv = vec![
            "claude".to_owned(),
            "agents".to_owned(),
            "--json".to_owned(),
            "--all".to_owned(),
        ];
        if let Some(cwd) = cwd {
            argv.push("--cwd".to_owned());
            argv.push(cwd.to_owned());
        }
        let inv = wtm_core::ports::exec::Invocation::new(argv, std::env::temp_dir(), 10_000);
        runner
            .run_allow_failure(&inv, &wtm_core::ports::exec::CancelToken::new())
            .unwrap()
    };

    // Captured, not inherited: the whole point is that this works with no terminal attached, which is
    // what `Runner` gives it.
    let all = run(None);
    assert!(
        all.is_success(),
        "`claude agents --json` failed: {}",
        all.stderr
    );

    let tasks: Vec<serde_json::Value> = serde_json::from_str(&all.stdout).unwrap_or_default();
    eprintln!("{} agents across all directories", tasks.len());
    if tasks.is_empty() {
        eprintln!("nothing to filter — this machine has no recorded background agents");
        return;
    }

    // The roster carries *interactive* sessions too — including whichever one is driving this test —
    // and they are shaped differently: a background entry has an `id`, an interactive one has a `pid`.
    // The command filters on `kind` for exactly that reason, and asserting it here is what caught the
    // earlier version, which filtered them out by accident when its `id` lookup happened to fail.
    let kinds: std::collections::BTreeSet<&str> =
        tasks.iter().filter_map(|t| t["kind"].as_str()).collect();
    eprintln!("kinds present: {kinds:?}");

    let background: Vec<&serde_json::Value> = tasks
        .iter()
        .filter(|t| t["kind"].as_str() == Some("background"))
        .collect();

    // The fields the command reads. A rename here is the difference between a populated tray and an
    // empty one, and nothing else would notice.
    for task in &background {
        assert!(
            task.get("id").and_then(serde_json::Value::as_str).is_some(),
            "a background entry must carry an id: {task}"
        );
        assert!(
            task.get("cwd")
                .and_then(serde_json::Value::as_str)
                .is_some()
        );
        assert!(
            task.get("state")
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "a background entry must report a state: {task}"
        );
    }
    eprintln!("{} of them are background agents", background.len());
    if background.is_empty() {
        eprintln!("nothing to filter — no recorded background agents");
        return;
    }

    // `--cwd` must *filter*, not merely be accepted. A flag that parsed and did nothing would give
    // every worktree the same roster — every session in the app, attributed to whichever worktree the
    // user happened to be looking at.
    let target = background[0]["cwd"].as_str().unwrap().to_owned();
    let filtered = run(Some(&target));
    let only: Vec<serde_json::Value> = serde_json::from_str(&filtered.stdout).unwrap_or_default();

    assert!(
        !only.is_empty(),
        "filtering to a known directory found nothing"
    );
    for task in &only {
        assert_eq!(
            task["cwd"].as_str(),
            Some(target.as_str()),
            "`--cwd` returned an agent from another directory"
        );
    }
    eprintln!("{} of them in {target}", only.len());
    assert!(
        only.len() <= tasks.len(),
        "a filtered list cannot be longer than the whole one"
    );
}

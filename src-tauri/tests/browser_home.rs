//! Browser defaults must follow the repository's display rules, including each worktree's
//! environment and local config overrides. Exercising the real config and display adapters
//! catches a default that accidentally uses the main checkout's port or a hidden link.

#![allow(clippy::unwrap_used)]

use wtm_app_lib::app::App;
use wtm_app_lib::view::WorktreeView;
use wtm_config::AppPaths;
use wtm_testkit::GitFixture;

const LINKS: &str = r#"
[[display.source]]
id = "runtime"
kind = "dotenv"
path = "{{ worktree.path }}/.env"

[[display.link]]
label = "Preview"
value = "http://127.0.0.1:{{ env.APP_PORT | default_if_empty('3000') }}/app"

[[display.link]]
label = "Docs"
value = "https://example.com/docs"
"#;

fn views(config: &str, local: &str, environments: &[&str]) -> Vec<WorktreeView> {
    let fixture = GitFixture::new();
    std::fs::write(fixture.root().join("wtm.toml"), config).unwrap();
    std::fs::write(fixture.root().join(".git/wtm.local.toml"), local).unwrap();
    for (index, env) in environments.iter().enumerate() {
        let path = if index == 0 {
            fixture.root().to_path_buf()
        } else {
            fixture.add_worktree(&format!("wt-{index}"), &format!("topic/{index}"))
        };
        std::fs::write(path.join(".env"), env).unwrap();
    }
    let config_dir = tempfile::tempdir().unwrap();
    let app = App::with_paths(AppPaths::rooted(config_dir.path())).unwrap();
    let project = app.project(fixture.root().to_str().unwrap()).unwrap();
    app.worktrees(&project).unwrap()
}

#[test]
fn the_first_web_link_uses_each_worktrees_port_and_the_repositorys_fallback() {
    let worktrees = views(LINKS, "", &["APP_PORT=4100", "APP_PORT=4200", ""]);
    let homes: Vec<_> = worktrees
        .iter()
        .map(|view| view.browser_home.as_deref())
        .collect();
    assert_eq!(
        homes,
        vec![
            Some("http://127.0.0.1:4100/app"),
            Some("http://127.0.0.1:4200/app"),
            Some("http://127.0.0.1:3000/app"),
        ]
    );
}

#[test]
fn an_explicit_home_overrides_links_and_the_local_config_overrides_the_repository() {
    let config = format!("{LINKS}\n[browser]\nhome = 'https://example.com/start'\n");
    let repo = views(&config, "", &["APP_PORT=4100"]);
    assert_eq!(
        repo[0].browser_home.as_deref(),
        Some("https://example.com/start")
    );

    let local = views(
        &config,
        "[browser]\nhome = 'http://localhost:{{ env.APP_PORT }}/local'\n",
        &["APP_PORT=4200"],
    );
    assert_eq!(
        local[0].browser_home.as_deref(),
        Some("http://localhost:4200/local")
    );
}

#[test]
fn an_empty_or_failed_home_template_falls_back_to_the_display_link() {
    for home in ["   ", "{{ env.APP_PORT | unknown_filter }}"] {
        let config = format!("{LINKS}\n[browser]\nhome = '{home}'\n");
        let worktrees = views(&config, "", &["APP_PORT=4100"]);
        assert_eq!(
            worktrees[0].browser_home.as_deref(),
            Some("http://127.0.0.1:4100/app")
        );
    }
}

#[test]
fn unavailable_links_are_skipped_without_reordering_the_remaining_links() {
    let config = r#"
[[display.link]]
label = "Hidden"
value = "https://example.com/hidden"
when = "false"

[[display.link]]
label = "Not openable"
value = "https://example.com/disabled"
open = false

[[display.link]]
label = "Missing port"
value = "http://localhost:{{ env.MISSING | default_if_empty('invalid') }}"

[[display.link]]
label = "Failed template"
value = "{{ env.MISSING | unknown_filter }}"

[[display.link]]
label = "Empty"
value = "   "

[[display.link]]
label = "Local file"
value = "file:///tmp/index.html"

[[display.link]]
label = "Blank"
value = "about:blank"

[[display.link]]
label = "First available"
value = "https://example.com/first"

[[display.link]]
label = "Second available"
value = "https://example.com/second"
"#;
    let worktrees = views(config, "", &[]);
    assert_eq!(
        worktrees[0].browser_home.as_deref(),
        Some("https://example.com/first")
    );
}

#[test]
fn a_repository_without_an_available_web_link_keeps_the_empty_browser_state() {
    for config in [
        "",
        "[[display.link]]\nlabel = 'Unavailable'\nvalue = 'https://example.com'\nwhen = 'false'\n",
        "[[display.link]]\nlabel = 'Unavailable'\nvalue = 'https://example.com'\nopen = false\n",
        "[[display.link]]\nlabel = 'Unavailable'\nvalue = 'http://localhost:invalid'\n",
    ] {
        assert!(views(config, "", &[])[0].browser_home.is_none());
    }
}

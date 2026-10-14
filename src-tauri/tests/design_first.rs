//! Conversation prototypes must preserve worktree context, immutable review identity and explicit
//! implementation approval without starting another provider or any design daemon.
#![allow(clippy::unwrap_used)]
use std::path::Path;
use wtm_app_lib::prototype_context::{self as context, SourceChoice};
use wtm_app_lib::prototypes::{self, Prototype};
use wtm_testkit::GitFixture;

fn prototype(directory: &Path) -> Prototype {
    Prototype {
        id: uuid::Uuid::new_v4().to_string(),
        project_id: "/repo".to_owned(),
        worktree_id: "/repo/topic".to_owned(),
        root: "/repo/topic".to_owned(),
        session: "chat-one".to_owned(),
        provider: "codex".to_owned(),
        conversation: "conversation-one".to_owned(),
        title: "Registration export".to_owned(),
        brief: "Prototype the export using our components".to_owned(),
        created: "2026-01-01".to_owned(),
        upstream: prototypes::UPSTREAM.to_owned(),
        sources: vec![],
        directory: directory
            .canonicalize()
            .unwrap_or_else(|_| directory.to_path_buf())
            .to_string_lossy()
            .into_owned(),
        revision: String::new(),
        phase: String::new(),
        feedback: vec![],
        review: None,
        implemented_review: None,
    }
}
const HTML: &str = "<!doctype html><html><body><button class='btn btn-primary project-action'>Export</button></body></html>";
#[test]
fn the_fixed_review_document_contains_only_escaped_snapshot_bytes_and_a_sandboxed_frame() {
    let document = prototypes::review_document("<html><script>parent.test = 1</script></html>");
    assert!(document.contains("sandbox=\"allow-scripts\""));
    assert!(document.contains("connect-src 'none'"));
    assert!(document.contains("data-wtm-prototype-preview"));
    assert!(document.contains("onload=\"this.dataset.loaded='true'\""));
    assert!(!document.contains("allow-same-origin"));
    assert!(!document.contains("<script>parent"));
    assert!(document.contains("&lt;script&gt;parent"));
}

#[test]
fn a_review_cannot_silently_depend_on_mutable_styles_images_or_network_responses() {
    for html in [
        r#"<link href="style.css">"#,
        r#"<img src="https://example.com/user.png">"#,
        "<style>@import 'theme.css';</style>",
        "<style>.x {background:url(image.png)}</style>",
        "<script>fetch('/live-data')</script>",
    ] {
        assert!(prototypes::validate_artifact(html).is_err());
    }
    assert!(prototypes::validate_artifact(r##"<a href="#settings">Settings</a><img src="data:image/png;base64,AA=="><style>.x{fill:url(#gradient)}</style>"##).is_ok());
}

#[test]
fn context_uses_local_worktree_edits_and_never_the_main_checkouts_components() {
    let fixture = GitFixture::new();
    let topic = fixture.add_worktree("design-topic", "topic/design");
    std::fs::write(
        fixture.root().join("Button.vue"),
        "<template><button>Main</button></template>",
    )
    .unwrap();
    std::fs::write(
        topic.join("Button.vue"),
        "<template><button class='btn btn-primary'>Local</button></template>",
    )
    .unwrap();
    std::fs::write(topic.join("tokens.css"), ":root { --accent: #734399; }").unwrap();
    let root = topic.canonicalize().unwrap();
    let names = vec!["Button.vue".to_owned(), "tokens.css".to_owned()];
    let discovery = context::discover(&root, &names, false);
    let choices = discovery
        .sources
        .iter()
        .map(|source| SourceChoice {
            path: source.path.clone(),
            fingerprint: source.fingerprint.clone(),
        })
        .collect::<Vec<_>>();
    let sources = context::capture(&root, &names, &choices).unwrap();
    let evidence = context::evidence(root.to_str().unwrap(), &sources);
    assert!(evidence.contains("btn btn-primary"));
    assert!(evidence.contains("--accent: #734399"));
    assert!(!evidence.contains(">Main<"));
    std::fs::write(topic.join("Button.vue"), "changed after discovery").unwrap();
    assert!(context::capture(&root, &names, &choices).is_err());
    assert!(context::capture(&root, &[], &choices).is_err());
}

#[test]
fn traversal_links_credentials_generated_files_and_oversize_sources_are_excluded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for path in [
        "../Button.vue",
        "/tmp/Button.vue",
        ".env",
        "data/Button.vue",
        "node_modules/button.css",
        "dist/tokens.css",
        "credentials.md",
        "secret.ts",
    ] {
        assert!(!context::allowed(path), "{path}");
    }
    std::os::unix::fs::symlink("/tmp", root.join("linked")).unwrap();
    assert!(context::read(&root, "linked/Button.vue").is_err());
    std::fs::write(root.join("tokens.css"), "api_key=do-not-export").unwrap();
    assert!(context::read(&root, "tokens.css").is_err());
    std::fs::write(root.join("tokens.css"), "x".repeat(context::MAX_FILE + 1)).unwrap();
    assert!(context::read(&root, "tokens.css").is_err());
    std::fs::write(
        root.join("package.json"),
        r#"{"scripts":{"deploy":"SECRET"},"dependencies":{"vue":"private-registry-token"}}"#,
    )
    .unwrap();
    let source = context::read(&root, "package.json").unwrap();
    assert!(source.text.contains("vue"));
    assert!(!source.text.contains("SECRET"));
    assert!(!source.text.contains("private-registry-token"));
}

#[test]
fn context_requires_distinct_reviewed_sources_with_a_total_size_budget() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut names = Vec::new();
    let mut choices = Vec::new();
    for n in 0..7 {
        let name = format!("tokens{n}.css");
        std::fs::write(root.join(&name), "x".repeat(context::MAX_FILE)).unwrap();
        let source = context::read(&root, &name).unwrap();
        choices.push(SourceChoice {
            path: name.clone(),
            fingerprint: source.fingerprint,
        });
        names.push(name);
    }
    assert!(context::capture(&root, &names, &choices).is_err());
    assert!(context::capture(&root, &names, &choices[..6]).is_ok());
    choices[1] = choices[0].clone();
    assert!(context::capture(&root, &names, &choices[..2]).is_err());
}

#[test]
fn a_project_cannot_select_another_repositorys_worktree() {
    let fixture = GitFixture::new();
    let other = GitFixture::new();
    let config = tempfile::tempdir().unwrap();
    let app =
        wtm_app_lib::app::App::with_paths(wtm_config::AppPaths::rooted(config.path())).unwrap();
    assert!(
        wtm_app_lib::prototype_commands::root_of(
            &app,
            fixture.root().to_str().unwrap(),
            other.root().to_str().unwrap()
        )
        .is_err()
    );
}

#[test]
fn generation_and_revision_cannot_approve_implementation_or_write_application_files() {
    let application = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let mut design = prototype(storage.path());
    design.root = application.path().to_string_lossy().into_owned();
    prototypes::begin(&mut design, "Use an inline label").unwrap();
    let first = design.revision.clone();
    assert!(prototypes::handoff(&design, &first).is_err());
    prototypes::publish(
        &mut design,
        &first,
        HTML,
        "Uses Button.vue; HTML approximation",
    )
    .unwrap();
    assert!(prototypes::handoff(&design, &first).is_err());
    design.review.as_mut().unwrap().opened = true;
    let prompt = prototypes::handoff(&design, &first).unwrap();
    assert!(prompt.contains(&format!("reviews/{first}/prototype.html")));
    assert!(prompt.contains("Use an inline label"));
    assert!(design.implemented_review.is_none());
    design.implemented_review = Some(first.clone());
    assert!(prototypes::handoff(&design, &first).is_err());
    prototypes::begin(&mut design, "Make the layout simpler").unwrap();
    assert!(design.implemented_review.is_none());
    assert!(design.review.is_none());
    assert!(prototypes::handoff(&design, &first).is_err());
    assert!(prototypes::publish(&mut design, &first, HTML, "late result").is_err());
    assert_eq!(std::fs::read_dir(application.path()).unwrap().count(), 0);
}

#[test]
fn cancellation_invalidates_late_results_and_retry_requires_a_new_generation() {
    let storage = tempfile::tempdir().unwrap();
    let mut design = prototype(storage.path());
    prototypes::begin(&mut design, "initial").unwrap();
    let revision = design.revision.clone();
    prototypes::cancel(&mut design).unwrap();
    assert!(prototypes::publish(&mut design, &revision, HTML, "cancelled").is_err());
    prototypes::begin(&mut design, "retry").unwrap();
    let revision = design.revision.clone();
    prototypes::publish(&mut design, &revision, HTML, "current").unwrap();
}

#[test]
fn changed_html_or_reuse_notes_cannot_inherit_a_review() {
    for file in ["prototype.html", "REUSE.md"] {
        let storage = tempfile::tempdir().unwrap();
        let mut design = prototype(storage.path());
        prototypes::begin(&mut design, "initial").unwrap();
        let revision = design.revision.clone();
        prototypes::publish(&mut design, &revision, HTML, "Uses Button.vue").unwrap();
        design.review.as_mut().unwrap().opened = true;
        std::fs::write(
            storage.path().join("reviews").join(&revision).join(file),
            "changed",
        )
        .unwrap();
        assert!(prototypes::handoff(&design, &revision).is_err());
    }
}

#[test]
fn restoration_only_reads_records_and_keeps_conversation_and_worktree_ownership() {
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path().canonicalize().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut design = prototype(&root.join(&id));
    design.id = id.clone();
    prototypes::begin(&mut design, "initial").unwrap();
    let revision = design.revision.clone();
    prototypes::publish(&mut design, &revision, HTML, "Uses Button.vue").unwrap();
    let before = std::fs::read(root.join(&id).join("prototype.json")).unwrap();
    let restored = prototypes::load(&root, &id, "/repo", "/repo/topic").unwrap();
    assert_eq!(
        before,
        std::fs::read(root.join(&id).join("prototype.json")).unwrap()
    );
    assert!(restored.implemented_review.is_none());
    assert!(prototypes::load(&root, &id, "/repo", "/repo/other").is_err());
    let mut owner = wtm_app_lib::app::AgentSessionFacts {
        session: "resumed-session".to_owned(),
        scope: wtm_app_lib::app::SessionScope::worktree("/repo", "/repo/topic"),
        provider: "codex".to_owned(),
        provider_session: "conversation-one".to_owned(),
        ephemeral: false,
    };
    assert!(wtm_app_lib::prototype_commands::belongs(&restored, &owner));
    owner.provider_session = "other-conversation".to_owned();
    assert!(!wtm_app_lib::prototype_commands::belongs(&restored, &owner));
}

#[test]
fn automatic_context_and_open_design_token_evidence_preserve_real_source_values() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("tokens.css"), ":root { --accent: #734399; --radius: 6px; --space: 12px; }\n.btn { font-family: system-ui; }").unwrap();
    std::fs::write(
        root.join("Button.vue"),
        "<button class='btn btn-primary project-action'>Export</button>",
    )
    .unwrap();
    let discovery = context::discover(
        &root,
        &["tokens.css".to_owned(), "Button.vue".to_owned()],
        false,
    );
    let sources = context::select(&discovery, "registration export");
    let tokens = wtm_app_lib::design_evidence::collect(&sources);
    assert!(tokens.iter().any(|t| t.name == "--accent"
        && t.value == "#734399"
        && t.source == "tokens.css"
        && t.line == 1));
    assert!(
        tokens
            .iter()
            .any(|t| t.kind == "radius" && t.value == "6px")
    );
    assert!(sources.iter().any(|s| s.text.contains("project-action")));
    assert!(prototypes::CHARTER.contains("Keep the Design System Bound on Every Turn"));
}

#[test]
fn first_use_is_bundled_and_storage_failure_does_not_require_a_cli_or_login() {
    let root = tempfile::tempdir().unwrap();
    let mut design = prototype(&root.path().join("first-use"));
    prototypes::begin(&mut design, "initial").unwrap();
    assert!(
        Path::new(&design.directory)
            .join("prototype.json")
            .is_file()
    );
    let blocked = root.path().join("blocked");
    std::fs::write(&blocked, "file").unwrap();
    let mut other = prototype(&blocked);
    assert!(prototypes::begin(&mut other, "initial").is_err());
    assert!(wtm_app_lib::prototype_tools::INSTRUCTIONS.contains("current session"));
    assert!(
        !wtm_app_lib::prototype_tools::definitions()
            .iter()
            .any(|t| t["name"].as_str().unwrap().contains("implement"))
    );
}

//! Conversation prototypes must preserve worktree context, immutable review identity and explicit
//! implementation approval without starting another provider or any design daemon.
#![allow(clippy::unwrap_used)]
use std::path::Path;
use wtm_app_lib::prototype_commands::{preview_belongs, send_implementation};
use wtm_app_lib::prototype_context::{self as context, SourceChoice};
use wtm_app_lib::prototypes::{self, Prototype};
use wtm_app_lib::view::BrowserView;
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
        dismissed: false,
        reviews: vec![],
        flow: None,
    }
}
const HTML: &str = "<!doctype html><html><body><button class='btn btn-primary project-action'>Export</button></body></html>";

fn browser(id: &str, design: &Prototype, identity: Option<String>) -> BrowserView {
    BrowserView {
        id: id.to_owned(),
        project: design.project_id.clone(),
        worktree: design.worktree_id.clone(),
        prototype_id: identity,
        url: "about:blank".to_owned(),
        title: "Prototype review".to_owned(),
        loading: false,
        load_error: None,
        can_go_back: false,
        can_go_forward: false,
        agent_access: true,
        opened_by: Some(design.session.clone()),
        comment_mode: false,
        agent_driving: None,
    }
}

#[test]
fn successful_implementation_closes_only_that_designs_previews_after_saving_and_sending() {
    let storage = tempfile::tempdir().unwrap();
    let mut design = prototype(storage.path());
    prototypes::begin(&mut design, "initial").unwrap();
    let revision = design.revision.clone();
    prototypes::publish(&mut design, &revision, HTML, "Uses Button.vue").unwrap();
    design.review.as_mut().unwrap().opened = true;
    let preview = browser(
        "current-preview",
        &design,
        Some(format!("{}/{revision}", design.id)),
    );
    let mut previous = browser(
        "previous-preview",
        &design,
        Some(format!("{}/previous-review", design.id)),
    );
    previous.opened_by = Some("before-chat-was-resumed".to_owned());
    let live = browser("live-app", &design, None);
    let other = browser(
        "other-design",
        &design,
        Some(format!("{}-other/{revision}", design.id)),
    );
    let mut other_worktree = preview.clone();
    other_worktree.id = "other-worktree".to_owned();
    other_worktree.worktree = "/repo/other".to_owned();
    let mut other_project = preview.clone();
    other_project.id = "other-project".to_owned();
    other_project.project = "/other-repo".to_owned();
    let browsers = [
        preview,
        previous,
        live,
        other,
        other_worktree,
        other_project,
    ];
    let saved = Path::new(&design.directory).join("prototype.json");
    let sent = std::cell::Cell::new(false);
    let mut closed = Vec::new();
    send_implementation(
        &mut design,
        &revision,
        |prompt| {
            let persisted: Prototype =
                serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
            assert_eq!(
                persisted.implemented_review.as_deref(),
                Some(revision.as_str())
            );
            assert!(prompt.contains(&format!("review {revision}")));
            sent.set(true);
            Ok(())
        },
        |approved| {
            assert!(sent.get());
            closed.extend(
                browsers
                    .iter()
                    .filter(|view| preview_belongs(approved, view))
                    .map(|view| view.id.clone()),
            );
        },
    )
    .unwrap();
    assert_eq!(closed, ["current-preview", "previous-preview"]);
    assert_eq!(design.phase, "implementation approved");
    // Dismissing a pane does not consume or alter the saved artifact used by Show preview.
    assert_eq!(prototypes::artifact(&design, &revision).unwrap().0, HTML);
}

#[test]
fn a_failed_implementation_send_keeps_the_preview_open_and_allows_retry() {
    let storage = tempfile::tempdir().unwrap();
    let mut design = prototype(storage.path());
    prototypes::begin(&mut design, "initial").unwrap();
    let revision = design.revision.clone();
    prototypes::publish(&mut design, &revision, HTML, "Uses Button.vue").unwrap();
    design.review.as_mut().unwrap().opened = true;
    let error = send_implementation(
        &mut design,
        &revision,
        |_| Err("Chat is unavailable".to_owned()),
        |_| panic!("A failed send must not dismiss the preview"),
    )
    .unwrap_err();
    assert!(error.message.contains("You can retry"));
    let persisted: Prototype = serde_json::from_slice(
        &std::fs::read(Path::new(&design.directory).join("prototype.json")).unwrap(),
    )
    .unwrap();
    assert!(persisted.implemented_review.is_none());
    assert_eq!(persisted.phase, "ready for review");
    assert!(prototypes::handoff(&persisted, &revision).is_ok());
    assert!(
        send_implementation(
            &mut design,
            "unreviewed-revision",
            |_| panic!("An invalid review must not send implementation"),
            |_| panic!("An invalid review must not dismiss the preview"),
        )
        .is_err()
    );
}

#[test]
fn dismissing_a_card_persists_only_its_visibility_and_keeps_generation_and_review_available() {
    for published in [false, true] {
        let storage = tempfile::tempdir().unwrap();
        let root = storage.path().canonicalize().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut design = prototype(&root.join(&id));
        design.id = id.clone();
        prototypes::begin(&mut design, "initial").unwrap();
        let revision = design.revision.clone();
        if published {
            prototypes::publish(&mut design, &revision, HTML, "Uses Button.vue").unwrap();
            design.review.as_mut().unwrap().opened = true;
            prototypes::save(&design).unwrap();
        }
        let mut expected = serde_json::to_value(&design).unwrap();
        expected["dismissed"] = serde_json::json!(true);
        prototypes::dismiss(&mut design).unwrap();
        let mut restored = prototypes::load(&root, &id, "/repo", "/repo/topic").unwrap();
        assert_eq!(serde_json::to_value(&restored).unwrap(), expected);
        if published {
            assert!(prototypes::handoff(&restored, &revision).is_ok());
        } else {
            prototypes::publish(&mut restored, &revision, HTML, "Uses Button.vue").unwrap();
            let restored = prototypes::load(&root, &id, "/repo", "/repo/topic").unwrap();
            assert!(restored.dismissed);
        }
        assert_eq!(prototypes::artifact(&restored, &revision).unwrap().0, HTML);
        assert!(restored.implemented_review.is_none());
    }
}

#[test]
fn existing_saved_records_default_to_an_undismissed_card_without_losing_approval() {
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path().canonicalize().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut design = prototype(&root.join(&id));
    design.id = id.clone();
    prototypes::begin(&mut design, "initial").unwrap();
    let revision = design.revision.clone();
    prototypes::publish(&mut design, &revision, HTML, "Uses Button.vue").unwrap();
    design.review.as_mut().unwrap().opened = true;
    send_implementation(&mut design, &revision, |_| Ok(()), |_| {}).unwrap();
    let mut legacy = serde_json::to_value(&design).unwrap();
    legacy.as_object_mut().unwrap().remove("dismissed");
    std::fs::write(
        root.join(&id).join("prototype.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let restored = prototypes::load(&root, &id, "/repo", "/repo/topic").unwrap();
    assert!(!restored.dismissed);
    assert_eq!(
        restored.implemented_review.as_deref(),
        Some(revision.as_str())
    );
    assert_eq!(prototypes::artifact(&restored, &revision).unwrap().0, HTML);
}

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
fn every_published_revision_stays_addressable_but_only_the_newest_can_be_implemented() {
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path().canonicalize().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut design = prototype(&root.join(&id));
    design.id = id.clone();
    prototypes::begin(&mut design, "initial").unwrap();
    let first = design.revision.clone();
    prototypes::publish(&mut design, &first, HTML, "Uses Button.vue").unwrap();
    prototypes::begin(&mut design, "Header: use PageHeader").unwrap();
    let second = design.revision.clone();
    let revised = HTML.replace("Export", "Export units");
    prototypes::publish(&mut design, &second, &revised, "Uses PageHeader.vue").unwrap();
    prototypes::begin(&mut design, "A third look").unwrap();

    // Read back from disk, as a later launch would: both revisions, in order, with their briefs.
    let restored = prototypes::load(&root, &id, "/repo", "/repo/topic").unwrap();
    let history: Vec<(&str, &str)> = restored
        .history()
        .into_iter()
        .map(|r| (r.id.as_str(), r.brief.as_str()))
        .collect();
    assert_eq!(
        history,
        [
            (first.as_str(), "initial"),
            (second.as_str(), "Header: use PageHeader")
        ]
    );
    // The first is still there to preview after the second, and after a third was begun.
    assert_eq!(prototypes::artifact(&restored, &first).unwrap().0, HTML);
    assert_eq!(
        prototypes::artifact(&restored, &second).unwrap(),
        (revised, "Uses PageHeader.vue".to_owned())
    );
    // Still checked against the fingerprint it was published with.
    std::fs::write(
        root.join(&id).join("reviews").join(&first).join("REUSE.md"),
        "edited later",
    )
    .unwrap();
    assert!(prototypes::artifact(&restored, &first).is_err());
    assert!(prototypes::artifact(&restored, "never-published").is_err());

    // Approval is the newest revision's only.
    let mut current = restored.clone();
    current.review = current.reviews.last().cloned();
    if let Some(review) = current.review.as_mut() {
        review.opened = true;
    }
    assert!(prototypes::handoff(&current, &first).is_err());
    assert!(prototypes::handoff(&current, &second).is_ok());
}

#[test]
fn a_record_from_before_history_was_kept_starts_its_history_with_the_review_it_had() {
    let storage = tempfile::tempdir().unwrap();
    let mut design = prototype(storage.path());
    prototypes::begin(&mut design, "initial").unwrap();
    let first = design.revision.clone();
    prototypes::publish(&mut design, &first, HTML, "Uses Button.vue").unwrap();
    // As an older build wrote it: one review, no history.
    let mut legacy = serde_json::to_value(&design).unwrap();
    legacy.as_object_mut().unwrap().remove("reviews");
    let mut legacy: Prototype = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.history().len(), 1);
    prototypes::begin(&mut legacy, "again").unwrap();
    let second = legacy.revision.clone();
    prototypes::publish(&mut legacy, &second, HTML, "Uses Button.vue").unwrap();
    assert_eq!(
        legacy
            .history()
            .into_iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>(),
        [first.clone(), second]
    );
    assert_eq!(prototypes::artifact(&legacy, &first).unwrap().0, HTML);
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

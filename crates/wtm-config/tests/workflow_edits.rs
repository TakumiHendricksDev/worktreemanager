//! Settings › Recipes writing recipes and roles back into a file people also edit by hand.
//!
//! The file is the record, so what these protect is that a save changes what the editor changed and
//! nothing else: comments, key order and unrelated tables come through byte for byte, an override
//! of your defaults stays an override, and a save that cannot be said in the file it would write is
//! refused rather than written as something else. And that the local layer's trust approval follows
//! an edit made here, and only one made here.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wtm_config::{
    AppPaths, FileConfigStore, WorkflowDraft, WorkflowFile, WorkflowHome, WorkflowSaveError,
};
use wtm_core::error::ConfigError;
use wtm_core::flow::{Fallback, RoleProvider, StageSpec};
use wtm_core::ports::config::{ConfigStore, TrustDecision};
use wtm_render::Engine;
use wtm_testkit::{FakeClock, FakeGit};

struct Harness {
    store: FileConfigStore,
    repo: PathBuf,
    _config: tempfile::TempDir,
    _repo: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let config = tempfile::tempdir().unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().to_path_buf();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let store = FileConfigStore::new(
            AppPaths::rooted(config.path()),
            Arc::new(FakeGit::with_main(repo.clone(), "main")),
            Arc::new(Engine::new()),
            Arc::new(FakeClock::new()),
        );
        Self {
            store,
            repo,
            _config: config,
            _repo: repo_dir,
        }
    }

    fn local(&self) -> PathBuf {
        self.repo.join(".git/wtm.local.toml")
    }

    fn user(&self) -> PathBuf {
        self.store.paths().config_file.clone()
    }

    fn root_key(&self) -> String {
        self.repo.to_string_lossy().into_owned()
    }

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    /// The editor's starting point: the merged recipes and roles, and where they would be saved.
    fn draft(&self) -> (WorkflowDraft, WorkflowFile) {
        let workflow = self.store.load(&self.repo).unwrap().workflow.unwrap();
        (
            WorkflowDraft {
                role: workflow.role,
                recipe: workflow.recipe,
            },
            self.store.workflow_file(&self.repo).unwrap(),
        )
    }

    fn save(
        &self,
        draft: &WorkflowDraft,
        file: &WorkflowFile,
    ) -> Result<WorkflowFile, WorkflowSaveError> {
        self.store
            .save_workflow(&self.repo, draft, &file.revision, None)
    }
}

/// The lines of `after` that are not in `before`, and the reverse: what a person would see in a diff.
fn changed_lines(before: &str, after: &str) -> (Vec<String>, Vec<String>) {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let added = new
        .iter()
        .filter(|l| !old.contains(l))
        .map(|l| (*l).to_owned())
        .collect();
    let removed = old
        .iter()
        .filter(|l| !new.contains(l))
        .map(|l| (*l).to_owned())
        .collect();
    (added, removed)
}

/// A local layer as a person writes one: comments, its own key order, and tables the editor never
/// touches, one of which runs a command and so is behind the trust prompt.
const LOCAL: &str = r#"# This checkout's own config. Kept out of git on purpose.

[setup]
# Install before anything else.
run = ["just", "setup"]

[[guards.forbid]]
argv_matches = "rm -rf /"
reason       = "never"

# ── Review ──────────────────────────────────────────────

[workflow.role.correctness]
# Behaviour and edge cases.
objective = "Find bugs the change introduces"
provider  = "codex"
model     = "gpt-6"   # the fast one
inputs    = ["diff", "rules"]
retries   = 1

[workflow.role.conventions]
objective = "Check the change against the repository's conventions"
provider = "claude"
inputs = ["diff", "instructions"]

[[workflow.recipe.standard.stage]]
# Both at once.
roles = ["correctness", "conventions"]

[[environment.resource]]
# Nothing here is the editor's.
id = "db"
isolation = "shared"
"#;

#[test]
fn changing_one_roles_model_changes_that_line_alone_and_keeps_every_comment_and_unrelated_table() {
    let h = Harness::new();
    Harness::write(&h.local(), LOCAL);
    h.store
        .set_trust(&h.local(), TrustDecision::Approve)
        .unwrap();

    let (mut draft, file) = h.draft();
    assert_eq!(file.home, Some(WorkflowHome::Local));
    draft.role.get_mut("correctness").unwrap().model = Some("gpt-6-high".to_owned());
    h.save(&draft, &file).unwrap();

    let after = Harness::read(&h.local());
    let (added, removed) = changed_lines(LOCAL, &after);
    assert_eq!(
        added,
        vec![r#"model     = "gpt-6-high"   # the fast one"#],
        "the value changes in place, keeping its alignment and its comment:\n{after}"
    );
    assert_eq!(removed, vec![r#"model     = "gpt-6"   # the fast one"#]);
    assert_eq!(
        after.replace("gpt-6-high", "gpt-6"),
        LOCAL,
        "everything else comes through byte for byte"
    );
}

#[test]
fn saving_what_was_loaded_writes_nothing_new() {
    let h = Harness::new();
    Harness::write(&h.local(), LOCAL);
    h.store
        .set_trust(&h.local(), TrustDecision::Approve)
        .unwrap();
    let (draft, file) = h.draft();
    h.save(&draft, &file).unwrap();
    assert_eq!(Harness::read(&h.local()), LOCAL);
}

#[test]
fn a_new_role_in_a_new_stage_is_written_beside_the_other_roles_and_loads_back_as_shown() {
    let h = Harness::new();
    Harness::write(&h.local(), LOCAL);
    h.store
        .set_trust(&h.local(), TrustDecision::Approve)
        .unwrap();
    let (mut draft, file) = h.draft();
    let mut summary = draft.role["conventions"].clone();
    summary.objective = "Summarise what the first stage found".to_owned();
    summary.skills = vec!["/team-summary".to_owned()];
    draft.role.insert("summary".to_owned(), summary);
    draft
        .recipe
        .get_mut("standard")
        .unwrap()
        .stages
        .push(StageSpec {
            roles: vec!["summary".to_owned()],
        });
    h.save(&draft, &file).unwrap();

    let after = Harness::read(&h.local());
    let role_at = after.find("[workflow.role.summary]").unwrap();
    assert!(
        after.find("[workflow.role.conventions]").unwrap() < role_at
            && role_at < after.find("[[workflow.recipe.standard.stage]]").unwrap(),
        "a new role sits after the last role, not at the end of the file:\n{after}"
    );
    let stages: Vec<usize> = after
        .match_indices("[[workflow.recipe.standard.stage]]")
        .map(|(at, _)| at)
        .collect();
    assert_eq!(stages.len(), 2, "{after}");
    assert!(
        stages[1] < after.find("[[environment.resource]]").unwrap(),
        "the new stage follows the first:\n{after}"
    );
    assert!(after.contains("# Both at once."), "{after}");
    assert_eq!(
        h.store
            .load(&h.repo)
            .unwrap()
            .workflow
            .unwrap()
            .role
            .get("summary"),
        draft.role.get("summary")
    );
}

#[test]
fn removing_a_role_from_a_stage_and_deleting_it_leaves_the_other_tables_as_they_were() {
    let h = Harness::new();
    Harness::write(&h.local(), LOCAL);
    h.store
        .set_trust(&h.local(), TrustDecision::Approve)
        .unwrap();
    let (mut draft, file) = h.draft();
    draft.role.remove("conventions");
    draft.recipe.get_mut("standard").unwrap().stages[0].roles = vec!["correctness".to_owned()];
    h.save(&draft, &file).unwrap();

    let after = Harness::read(&h.local());
    assert!(!after.contains("conventions"), "{after}");
    assert!(after.contains("# Behaviour and edge cases."), "{after}");
    assert!(after.contains("[[guards.forbid]]"), "{after}");
    assert!(after.contains("# Nothing here is the editor's."), "{after}");
}

/// Your own config holds every project's entry. Only this one's workflow may change.
const USER: &str = r#"# My wtm config.

[ui]
theme = "dark"

[projects."/elsewhere/other"]
name = "other"

[projects."/elsewhere/other".config.workflow.role.reviewer]
objective = "Another project's reviewer"
provider = "claude"
inputs = ["diff"]
"#;

fn user_with_this_project(h: &Harness) -> String {
    format!(
        r#"{USER}
[projects."{root}"]
name = "this"

# This project's own review.
[projects."{root}".config.workflow.role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
inputs = ["diff"]

[[projects."{root}".config.workflow.recipe.quick.stage]]
roles = ["correctness"]
"#,
        root = h.root_key()
    )
}

#[test]
fn the_project_entry_in_your_own_config_is_written_in_place_and_other_projects_are_untouched() {
    let h = Harness::new();
    let before = user_with_this_project(&h);
    Harness::write(&h.user(), &before);

    let (mut draft, file) = h.draft();
    assert_eq!(file.home, Some(WorkflowHome::Project));
    let role = draft.role.get_mut("correctness").unwrap();
    role.effort = Some("high".to_owned());
    role.retries = 2;
    h.save(&draft, &file).unwrap();

    let after = Harness::read(&h.user());
    let (added, removed) = changed_lines(&before, &after);
    assert_eq!(added, vec!["effort = \"high\"", "retries = 2"], "{after}");
    assert!(removed.is_empty(), "{after}");
    assert!(
        after.starts_with(USER),
        "the other project and the preferences are untouched:\n{after}"
    );
    assert!(!h.local().exists(), "nothing is written to the local layer");
}

#[test]
fn an_override_of_your_defaults_stays_an_override() {
    let h = Harness::new();
    let root = h.root_key();
    let before = format!(
        r#"[defaults.workflow.role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
model = "gpt-6"
inputs = ["diff"]

[[defaults.workflow.recipe.quick.stage]]
roles = ["correctness"]

[projects."{root}"]
name = "this"

[projects."{root}".config.workflow.role.correctness]
model = "gpt-6-high"
"#
    );
    Harness::write(&h.user(), &before);

    let (mut draft, file) = h.draft();
    assert_eq!(file.home, Some(WorkflowHome::Project));
    assert_eq!(
        file.declaring,
        vec![WorkflowHome::Defaults, WorkflowHome::Project]
    );
    draft.role.get_mut("correctness").unwrap().effort = Some("low".to_owned());
    h.save(&draft, &file).unwrap();

    let after = Harness::read(&h.user());
    let (added, _) = changed_lines(&before, &after);
    assert_eq!(added, vec!["effort = \"low\""], "{after}");
    let project_entry = &after[after.find("[projects.").unwrap()..];
    assert!(
        !project_entry.contains("objective"),
        "what the defaults give is not copied into the override:\n{after}"
    );

    // Back to the defaults' model: the override's line goes, rather than restating the default.
    let (mut draft, file) = h.draft();
    draft.role.get_mut("correctness").unwrap().model = Some("gpt-6".to_owned());
    h.save(&draft, &file).unwrap();
    let after = Harness::read(&h.user());
    assert!(!after.contains("gpt-6-high"), "{after}");
    assert_eq!(
        after.matches("model =").count(),
        1,
        "only the defaults say the model now:\n{after}"
    );
}

#[test]
fn what_your_defaults_declare_cannot_be_removed_from_one_project_and_the_refusal_names_where() {
    let h = Harness::new();
    let root = h.root_key();
    Harness::write(
        &h.user(),
        &format!(
            r#"[defaults.workflow.role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
model = "gpt-6"
inputs = ["diff"]

[defaults.workflow.role.spare]
objective = "Unused"
provider = "claude"
inputs = ["diff"]

[[defaults.workflow.recipe.quick.stage]]
roles = ["correctness"]

[projects."{root}".config.workflow.role.correctness]
effort = "low"
"#
        ),
    );
    let before = Harness::read(&h.user());

    let (mut draft, file) = h.draft();
    draft.role.remove("spare");
    match h.save(&draft, &file).unwrap_err() {
        WorkflowSaveError::Edit(wtm_config::EditError::Unremovable { key, from }) => {
            assert_eq!(key, "role.spare");
            assert!(from.contains("[defaults.workflow]"), "{from}");
        }
        other => panic!("expected the removal to be refused, got {other:?}"),
    }

    let (mut draft, file) = h.draft();
    draft.role.get_mut("correctness").unwrap().model = None;
    match h.save(&draft, &file).unwrap_err() {
        WorkflowSaveError::Edit(wtm_config::EditError::Unremovable { key, .. }) => {
            assert_eq!(key, "role.correctness.model");
        }
        other => panic!("a model the defaults set cannot be cleared here: {other:?}"),
    }
    assert_eq!(Harness::read(&h.user()), before, "nothing was written");

    // A list can be emptied over the defaults, since an array replaces rather than merges.
    let (mut draft, file) = h.draft();
    draft.role.get_mut("correctness").unwrap().fallback = Fallback::Role("spare".to_owned());
    draft.role.get_mut("spare").unwrap().provider = RoleProvider::Codex;
    h.save(&draft, &file).unwrap();
    assert_eq!(
        h.store.load(&h.repo).unwrap().workflow.unwrap().role["spare"].provider,
        RoleProvider::Codex
    );
}

#[test]
fn with_no_workflow_anywhere_a_save_asks_where_and_the_local_layer_can_take_it() {
    let h = Harness::new();
    Harness::write(&h.user(), USER);
    let file = h.store.workflow_file(&h.repo).unwrap();
    assert_eq!(file.home, None);

    let mut draft = WorkflowDraft::default();
    let mut role: wtm_core::flow::RoleSpec =
        toml::from_str("objective = \"Find bugs\"\nprovider = \"codex\"\ninputs = [\"diff\"]\n")
            .unwrap();
    role.retries = 1;
    draft.role.insert("correctness".to_owned(), role);
    draft.recipe.insert(
        "quick".to_owned(),
        toml::from_str("[[stage]]\nroles = [\"correctness\"]\n").unwrap(),
    );

    assert_eq!(
        h.store
            .save_workflow(&h.repo, &draft, &file.revision, None)
            .unwrap_err(),
        WorkflowSaveError::NoHome
    );
    let saved = h
        .store
        .save_workflow(&h.repo, &draft, &file.revision, Some(WorkflowHome::Local))
        .unwrap();
    assert_eq!(saved.home, Some(WorkflowHome::Local));
    assert_eq!(
        Harness::read(&h.user()),
        USER,
        "your own config is untouched"
    );
    assert_eq!(
        Harness::read(&h.local()),
        "[workflow.role.correctness]\nobjective = \"Find bugs\"\nprovider = \"codex\"\n\
         inputs = [\"diff\"]\nretries = 1\n\n[[workflow.recipe.quick.stage]]\n\
         roles = [\"correctness\"]\n"
    );
}

#[test]
fn a_validation_error_names_the_key_it_is_about_and_nothing_is_written() {
    let h = Harness::new();
    Harness::write(&h.local(), LOCAL);
    h.store
        .set_trust(&h.local(), TrustDecision::Approve)
        .unwrap();

    let (mut draft, file) = h.draft();
    draft.role.get_mut("correctness").unwrap().retries = 9;
    assert_eq!(
        h.store.check_workflow(&h.repo, &draft, None).unwrap_err(),
        WorkflowSaveError::Invalid {
            key: Some("role.correctness.retries".to_owned()),
            message: "at most 3: each retry is a whole turn against your usage window".to_owned(),
        }
    );

    let (mut draft, _) = h.draft();
    draft.recipe.get_mut("standard").unwrap().stages[0]
        .roles
        .push("nobody".to_owned());
    match h.save(&draft, &file).unwrap_err() {
        WorkflowSaveError::Invalid { key, message } => {
            assert_eq!(key.as_deref(), Some("recipe.standard.stage[0].roles"));
            assert!(message.contains("there is no role `nobody`"), "{message}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
    assert_eq!(Harness::read(&h.local()), LOCAL, "nothing was written");
}

#[test]
fn a_save_made_from_a_reading_older_than_the_file_is_refused() {
    let h = Harness::new();
    Harness::write(&h.local(), LOCAL);
    h.store
        .set_trust(&h.local(), TrustDecision::Approve)
        .unwrap();
    let (mut draft, file) = h.draft();

    let by_hand = LOCAL.replace("retries   = 1", "retries   = 2");
    Harness::write(&h.local(), &by_hand);
    draft.role.get_mut("correctness").unwrap().model = Some("gpt-7".to_owned());
    assert_eq!(h.save(&draft, &file).unwrap_err(), WorkflowSaveError::Stale);
    assert_eq!(Harness::read(&h.local()), by_hand, "the hand edit stands");
}

#[test]
fn an_edit_saved_here_keeps_the_local_layers_approval_and_an_edit_made_elsewhere_prompts_again() {
    let h = Harness::new();
    Harness::write(&h.local(), LOCAL);
    h.store
        .set_trust(&h.local(), TrustDecision::Approve)
        .unwrap();

    let (mut draft, file) = h.draft();
    draft.role.get_mut("conventions").unwrap().effort = Some("high".to_owned());
    h.save(&draft, &file).unwrap();
    assert!(
        h.store.is_trusted(&h.local()).unwrap(),
        "you made this edit, in the app, and it runs nothing new"
    );
    h.store.load(&h.repo).unwrap();

    // The same kind of change, made outside the app, is not the app's to vouch for.
    let by_hand = Harness::read(&h.local()).replace("effort = \"high\"", "effort = \"low\"");
    Harness::write(&h.local(), &by_hand);
    assert!(matches!(
        h.store.load(&h.repo),
        Err(ConfigError::Untrusted { .. })
    ));
}

#[test]
fn a_local_layer_whose_commands_you_never_approved_is_not_written() {
    let h = Harness::new();
    Harness::write(&h.local(), LOCAL);
    // Read while approved, as the editor would have been opened.
    h.store
        .set_trust(&h.local(), TrustDecision::Approve)
        .unwrap();
    let (mut draft, _) = h.draft();
    // Then rejected, which the editor did not see.
    h.store
        .set_trust(&h.local(), TrustDecision::Reject)
        .unwrap();
    let file = h.store.workflow_file(&h.repo).unwrap();

    draft.role.get_mut("conventions").unwrap().effort = Some("high".to_owned());
    match h.save(&draft, &file).unwrap_err() {
        WorkflowSaveError::Untrusted { path } => assert_eq!(path, h.local()),
        other => panic!("expected the write to be refused, got {other:?}"),
    }
    assert_eq!(Harness::read(&h.local()), LOCAL);
    assert!(!h.store.is_trusted(&h.local()).unwrap(), "still rejected");
}

#[test]
fn a_local_layer_that_declares_no_command_needs_no_approval_before_or_after() {
    let h = Harness::new();
    let plain = LOCAL.replace(
        "[setup]\n# Install before anything else.\nrun = [\"just\", \"setup\"]\n",
        "",
    );
    Harness::write(&h.local(), &plain);
    let (mut draft, file) = h.draft();
    draft.role.get_mut("conventions").unwrap().effort = Some("high".to_owned());
    h.save(&draft, &file).unwrap();
    assert!(
        !h.store.is_trusted(&h.local()).unwrap(),
        "nothing to approve, so nothing recorded"
    );
    h.store.load(&h.repo).unwrap();
}

#[test]
fn going_back_to_a_default_your_defaults_spell_out_drops_the_override_rather_than_restating_it() {
    let h = Harness::new();
    let root = h.root_key();
    let before = format!(
        r#"[defaults.workflow.role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
inputs = ["diff"]
fallback = "ask"

[defaults.workflow.role.spare]
objective = "Find bugs, elsewhere"
provider = "codex"
inputs = ["diff"]

[[defaults.workflow.recipe.quick.stage]]
roles = ["correctness"]

[projects."{root}".config.workflow.role.correctness]
fallback = "spare"
"#
    );
    Harness::write(&h.user(), &before);
    let (mut draft, file) = h.draft();
    draft.role.get_mut("correctness").unwrap().fallback = Fallback::Ask;
    h.save(&draft, &file).unwrap();
    let after = Harness::read(&h.user());
    assert!(
        !after.contains("\"spare\"\n\n") && !after.contains("fallback = \"spare\""),
        "{after}"
    );
    assert_eq!(after.matches("fallback").count(), 1, "{after}");
}

/// Your defaults are every project's. Deleting one from them, from this project's Settings, must
/// not quietly break another project whose recipe names it.
#[test]
fn an_edit_to_your_defaults_that_would_break_another_project_is_refused_and_names_it() {
    let h = Harness::new();
    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(other.path().join(".git")).unwrap();
    let before = format!(
        r#"[defaults.workflow.role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
inputs = ["diff"]

[defaults.workflow.role.spare]
objective = "Find bugs, elsewhere"
provider = "claude"
inputs = ["diff"]

[[defaults.workflow.recipe.quick.stage]]
roles = ["correctness"]

[projects."{this}"]
name = "this"

[projects."{other}"]
name = "other"

[[projects."{other}".config.workflow.recipe.full.stage]]
roles = ["correctness", "spare"]
"#,
        this = h.root_key(),
        other = other.path().display()
    );
    Harness::write(&h.user(), &before);

    let (mut draft, file) = h.draft();
    assert_eq!(file.home, Some(WorkflowHome::Defaults));
    draft.role.remove("spare");
    match h.save(&draft, &file).unwrap_err() {
        WorkflowSaveError::Config { message } => {
            assert!(
                message.contains(&other.path().display().to_string())
                    && message.contains("there is no role `spare`"),
                "{message}"
            );
        }
        other => panic!("expected the edit to be refused for the other project, got {other:?}"),
    }
    assert_eq!(Harness::read(&h.user()), before);

    // The same edit to a role nothing else names is fine.
    let (mut draft, file) = h.draft();
    draft.role.get_mut("spare").unwrap().effort = Some("low".to_owned());
    h.save(&draft, &file).unwrap();
}

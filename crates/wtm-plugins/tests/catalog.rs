//! The operation catalog is the line between what a package may declare and what only the host
//! or the user may do.
//!
//! These pin that every native-equivalent operation sits on the host's side of it, that every
//! public one says how the host keeps it inside a workspace, and that each says why in its own
//! words: a reason shared by a hundred entries was not a classification anyone had made.
#![allow(clippy::unwrap_used)]
use std::collections::BTreeMap;
use wtm_plugins::catalog::{Exposure, Scope, catalog};

#[test]
fn operations_that_act_as_native_code_cannot_be_declared_by_a_package() {
    for id in [
        "pty_write",
        "pty_kill",
        "open_terminal",
        "close_terminal",
        "open_native_cli",
        "send_native_message",
        "run_action",
        "start_recipe",
        "run_setup",
        "create_worktree",
        "preview_worktree",
        "field_options",
        "prepare_shell_run",
        "remove_worktree",
        "register_project",
        "unregister_project",
        "set_pref",
        "save_recipe",
        "delete_recipe",
        "set_recipe_offers",
        "run_database_query",
        "database_table_page",
        "connect_database",
        "send_turn",
        "steer_turn",
        "open_agent_session",
        "open_agent_side_session",
        "open_home_session",
        "close_agent_session",
        "probe_agent_skills",
        "open_url",
        "open_in",
        "open_browser",
        "browser_navigate",
        "prepare_agent_attachment",
        "stage_agent_attachment",
        "start_dictation",
        "prepare_update",
        "pop_out_pane",
        "open_prototype_preview",
        "ui.chat.configure",
    ] {
        let operation = catalog()
            .operation(id)
            .unwrap_or_else(|| panic!("{id} is missing"));
        assert_eq!(operation.exposure, Exposure::Protected, "{id}");
        assert!(catalog().public(id).is_none(), "{id}");
    }
}

#[test]
fn every_public_operation_says_how_it_stays_inside_a_workspace() {
    for operation in catalog().operations() {
        if operation.exposure != Exposure::Public {
            assert!(operation.scope.is_none(), "{} is not public", operation.id);
            continue;
        }
        let scope = operation
            .scope
            .unwrap_or_else(|| panic!("{} has no scope", operation.id));
        assert_eq!(
            scope == Scope::Arguments,
            !operation.resources.is_empty(),
            "{} lists resources exactly when its arguments bind it",
            operation.id
        );
    }
}

#[test]
fn every_operation_gives_a_reason_of_its_own() {
    const OLD: &[&str] = &[
        "Uses the same validated operation as built-in WTM UI.",
        "Host-owned trust, permission, secret or package boundary.",
        "Host-owned operation on existing workbench state.",
    ];
    let mut uses: BTreeMap<&str, usize> = BTreeMap::new();
    for operation in catalog().operations() {
        assert!(!operation.reason.trim().is_empty(), "{}", operation.id);
        assert!(
            !OLD.contains(&operation.reason.as_str()),
            "{}",
            operation.id
        );
        *uses.entry(&operation.reason).or_default() += 1;
    }
    // A sentence can fit a pair of twins (navigate and open, two window handshakes), not a family.
    for (reason, count) in uses {
        assert!(count <= 3, "{count} operations share: {reason}");
    }
}

/// Argument names that name something a scoped plugin must not reach outside its worktrees.
const RESOURCE_NAMES: &[&str] = &[
    "worktreeId",
    "projectId",
    "session",
    "parentSession",
    "browser",
    "paneId",
    "path",
    "directory",
    "id",
];

/// Resource-like arguments that are deliberately not declared, each with the reason it cannot
/// leave the caller's worktree. Adding to this list is a security decision, so it says why.
const CONTAINED: &[(&str, &str, &str)] = &[
    (
        "cancel_prototype",
        "id",
        "a design id, looked up inside the declared session",
    ),
    (
        "dismiss_prototype",
        "id",
        "a design id, looked up inside the declared session",
    ),
    (
        "code_add_comment",
        "comment.path",
        "a file path inside the declared worktree",
    ),
    (
        "code_base_version",
        "path",
        "a file path inside the declared worktree",
    ),
    (
        "code_file_diff",
        "path",
        "a file path inside the declared worktree",
    ),
    (
        "code_read_file",
        "path",
        "a file path inside the declared worktree",
    ),
    (
        "code_remove_comment",
        "id",
        "a comment id inside the declared worktree",
    ),
    (
        "code_resolve_comment",
        "id",
        "a comment id inside the declared worktree",
    ),
    (
        "code_update_comment",
        "id",
        "a comment id inside the declared worktree",
    ),
    (
        "post_notification",
        "paneId",
        "only where a click on the notice lands",
    ),
    (
        "remove_brief",
        "id",
        "a brief id inside the declared project",
    ),
    (
        "ui.chat.edit",
        "paneId",
        "the workbench resolves it in the caller's worktree",
    ),
    (
        "ui.chat.snapshot",
        "paneId",
        "the workbench resolves it in the caller's worktree",
    ),
    (
        "ui.composer.read",
        "paneId",
        "the workbench resolves it in the caller's worktree",
    ),
    (
        "ui.composer.write",
        "paneId",
        "the workbench resolves it in the caller's worktree",
    ),
    (
        "ui.panels.close",
        "paneId",
        "the workbench resolves it in the caller's worktree",
    ),
    (
        "ui.panels.focus",
        "paneId",
        "the workbench resolves it in the caller's worktree",
    ),
    (
        "ui.panels.move",
        "paneId",
        "the workbench resolves it in the caller's worktree",
    ),
    (
        "ui.panels.move",
        "target.paneId",
        "the workbench refuses a destination elsewhere",
    ),
    (
        "ui.panels.resize",
        "path",
        "a split path in the caller's own layout, not a file",
    ),
];

fn argument_paths(
    schema: &serde_json::Value,
    defs: &serde_json::Value,
    prefix: &str,
) -> Vec<String> {
    let mut schema = schema;
    while let Some(reference) = schema.get("$ref").and_then(|r| r.as_str()) {
        schema = &defs[reference.rsplit('/').next().unwrap()];
    }
    let mut paths = Vec::new();
    for key in ["anyOf", "oneOf"] {
        for alternative in schema[key].as_array().into_iter().flatten() {
            paths.extend(argument_paths(alternative, defs, prefix));
        }
    }
    for (name, property) in schema["properties"].as_object().into_iter().flatten() {
        let path = format!("{prefix}{name}");
        paths.extend(argument_paths(property, defs, &format!("{path}.")));
        paths.push(path);
    }
    paths
}

#[test]
fn every_argument_that_names_a_resource_is_resolved_or_explained() {
    // Scope is resolved from the catalog's declared resources and nowhere else, so an id argument
    // missing from them would reach whatever it names without the scope check. Global operations
    // are refused to every scoped plugin outright and so need no per-argument resolution.
    let contracts: serde_json::Value =
        serde_json::from_str(include_str!("../contracts/operations.json")).unwrap();
    let defs = &contracts["$defs"];
    let mut unexplained = Vec::new();
    for operation in catalog().operations() {
        if operation.exposure != Exposure::Public || operation.scope == Some(Scope::Global) {
            continue;
        }
        let Some(signature) = contracts["operations"].get(&operation.id) else {
            continue;
        };
        for path in argument_paths(&signature["arguments"], defs, "") {
            let name = path.rsplit('.').next().unwrap();
            let explained = operation.resources.contains_key(&path)
                || CONTAINED
                    .iter()
                    .any(|(id, argument, _)| *id == operation.id && *argument == path);
            if RESOURCE_NAMES.contains(&name) && !explained {
                unexplained.push(format!("{} `{path}`", operation.id));
            }
        }
    }
    assert!(
        unexplained.is_empty(),
        "declare these as resources, or add each to CONTAINED with why it stays in its worktree: {unexplained:?}"
    );
}

#[test]
fn every_contained_exception_still_names_a_real_argument() {
    // A stale exception would silently cover a different argument added later under that name.
    let contracts: serde_json::Value =
        serde_json::from_str(include_str!("../contracts/operations.json")).unwrap();
    for (operation, argument, _) in CONTAINED {
        let signature = &contracts["operations"][operation];
        let paths = argument_paths(&signature["arguments"], &contracts["$defs"], "");
        assert!(
            paths.iter().any(|p| p == argument),
            "{operation} no longer takes `{argument}`; remove it from CONTAINED"
        );
    }
}

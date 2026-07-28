//! Rendering a worktree into something the UI can show.
//!
//! # Why this is in Rust
//!
//! The display templates, the config, the `.env` files and the template engine all live
//! here. Sending raw facts and re-implementing `[display]` in TypeScript would mean two
//! renderers that eventually disagree about what a worktree is called — and the sidebar
//! and the detail pane would show different titles for the same thing.
//!
//! # The subtlety worth naming
//!
//! A `[[display.port_table]]` treats an **absent** key as "the defaults source's value is
//! in effect". That is how compose-style `${VAR:-base}` fallbacks actually behave, and it
//! is invisible unless the UI says so — hence [`TableRowView::inherited`].

use std::collections::BTreeMap;
use std::path::Path;

use wtm_core::model::{
    CommandSpec, DisplaySourceKind, Project, TokenScope, WorkingTreeStatus, Worktree,
};
use wtm_core::ports::fs::FileStore;
use wtm_core::ports::template::{Context, TemplateEngine};

use crate::view::{
    ActionView, BadgeView, EnvEntryView, LinkView, TableRowView, WorktreeView, classify_env,
    extract_issue_key,
};

/// Ambient tokens that do not depend on a worktree.
#[must_use]
pub fn base_context(project: &Project, os_tokens: &BTreeMap<String, String>) -> Context {
    let mut ctx = Context::new();

    for (key, value) in &project.meta.vars {
        ctx.insert(format!("vars.{key}"), value.clone());
    }

    ctx.insert(
        "repo.root".to_owned(),
        project.root.to_string_lossy().into_owned(),
    );
    ctx.insert(
        "repo.parent".to_owned(),
        project
            .root
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    ctx.insert(
        "repo.name".to_owned(),
        project
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );

    ctx.extend(os_tokens.clone());
    ctx
}

/// Add `worktree.*` tokens.
pub fn add_worktree_tokens(ctx: &mut Context, worktree: &Worktree) {
    ctx.insert(
        "worktree.path".to_owned(),
        worktree.path.to_string_lossy().into_owned(),
    );
    ctx.insert("worktree.dirname".to_owned(), worktree.dirname().to_owned());
    ctx.insert(
        "worktree.branch".to_owned(),
        worktree
            .branch()
            .map(|b| b.as_str().to_owned())
            .unwrap_or_default(),
    );
    ctx.insert(
        "worktree.head".to_owned(),
        worktree
            .head
            .as_ref()
            .map(|h| h.as_str().to_owned())
            .unwrap_or_default(),
    );
}

/// Read the project's declared display sources for one worktree.
///
/// Every source is optional by default and a missing file is not an error: a worktree
/// that has not been set up yet legitimately has no `.env`, and refusing to display it
/// would hide exactly the worktree the user needs to fix.
#[must_use]
pub fn read_sources(
    project: &Project,
    files: &dyn FileStore,
    engine: &dyn TemplateEngine,
    ctx: &Context,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out = BTreeMap::new();

    for source in &project.display.sources {
        let key = format!("display.source.{}", source.id);
        let Ok(rendered) = engine.render(&key, &source.path, ctx) else {
            tracing::debug!(source = %source.id, "display source path failed to render");
            continue;
        };
        let path = Path::new(&rendered);

        let values = match source.kind {
            DisplaySourceKind::Dotenv => files.read_dotenv(path).ok(),
            DisplaySourceKind::Json => files.read_to_string(path).ok().and_then(|text| {
                serde_json::from_str::<BTreeMap<String, serde_json::Value>>(&text)
                    .ok()
                    .map(|map| {
                        map.into_iter()
                            .map(|(k, v)| {
                                let rendered =
                                    v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                                (k, rendered)
                            })
                            .collect()
                    })
            }),
        };

        match values {
            Some(values) => {
                out.insert(source.id.clone(), values);
            }
            None if source.optional => {
                tracing::debug!(source = %source.id, path = %rendered, "optional display source absent");
            }
            None => {
                tracing::warn!(source = %source.id, path = %rendered, "display source unreadable");
            }
        }
    }

    out
}

/// Merge source values into the context under both `env.*` and `<source-id>.*`.
///
/// `env.*` aliases the *first* declared source, which is what makes
/// `{{ env.COMPOSE_PROJECT_NAME }}` work without a config having to name its own source
/// in every template.
pub fn add_source_tokens(
    ctx: &mut Context,
    project: &Project,
    sources: &BTreeMap<String, BTreeMap<String, String>>,
) {
    for (id, values) in sources {
        for (key, value) in values {
            ctx.insert(format!("{id}.{key}"), value.clone());
        }
    }

    if let Some(first) = project.display.sources.first()
        && let Some(values) = sources.get(&first.id)
    {
        for (key, value) in values {
            ctx.insert(format!("env.{key}"), value.clone());
        }
    }

    // A few process values are always available, so a config can pass the resolved
    // shell or PATH through to a command it spawns.
    for name in ["SHELL", "HOME", "USER", "TERM", "LOGIN_PATH"] {
        if let Ok(value) = std::env::var(name) {
            ctx.entry(format!("env.{name}")).or_insert(value);
        }
    }
}

/// Build the view for one worktree.
///
/// `favorite` is passed in rather than looked up: this module renders from the project
/// config and git, and knows nothing about the app config where stars are stored. Taking
/// it as an argument keeps the function total — there is no half-built view for a caller
/// to remember to patch afterwards.
pub fn worktree_view(
    project: &Project,
    worktree: &Worktree,
    status: WorkingTreeStatus,
    favorite: bool,
    files: &dyn FileStore,
    engine: &dyn TemplateEngine,
    os_tokens: &BTreeMap<String, String>,
) -> WorktreeView {
    let mut ctx = base_context(project, os_tokens);
    add_worktree_tokens(&mut ctx, worktree);

    let sources = read_sources(project, files, engine, &ctx);
    add_source_tokens(&mut ctx, project, &sources);

    // A failed display template must never hide a worktree — fall back to the plain
    // fact rather than dropping the row.
    let render = |key: &str, template: &Option<String>, fallback: &str| -> String {
        template
            .as_ref()
            .and_then(|t| engine.render(key, t, &ctx).ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| fallback.to_owned())
    };

    let branch_label = worktree
        .branch()
        .map_or_else(|| "(detached)".to_owned(), |b| b.as_str().to_owned());

    let title = render("display.title", &project.display.title, worktree.dirname());
    let subtitle = render("display.subtitle", &project.display.subtitle, &branch_label);

    let visible = |when: &Option<String>, key: &str| -> bool {
        when.as_ref()
            .is_none_or(|expression| engine.eval_bool(key, expression, &ctx).unwrap_or(false))
    };

    let badges = project
        .display
        .badges
        .iter()
        .filter(|badge| visible(&badge.when, "display.badge.when"))
        .filter_map(|badge| {
            let value = engine
                .render("display.badge.value", &badge.value, &ctx)
                .ok()?;
            (!value.trim().is_empty()).then(|| BadgeView {
                label: badge.label.clone(),
                value,
            })
        })
        .collect();

    let links = project
        .display
        .links
        .iter()
        .filter(|link| visible(&link.when, "display.link.when"))
        .filter_map(|link| {
            let url = engine
                .render("display.link.value", &link.value, &ctx)
                .ok()?;
            (!url.trim().is_empty()).then(|| LinkView {
                label: link.label.clone(),
                url,
            })
        })
        .collect();

    let table = build_tables(project, &sources, engine, &ctx);

    // The Env tab shows the first source, which is the one `env.*` aliases.
    //
    // Sensitive values are dropped here, before the view is serialized, so a secret never
    // crosses the IPC boundary unless the user asks for that one key by name.
    let env: Vec<EnvEntryView> = project
        .display
        .sources
        .first()
        .and_then(|source| sources.get(&source.id))
        .map(classify_env)
        .unwrap_or_default();

    WorktreeView {
        id: worktree.id.as_str().to_owned(),
        title,
        subtitle,
        path: worktree.path.to_string_lossy().into_owned(),
        dirname: worktree.dirname().to_owned(),
        branch: worktree.branch().map(|b| b.as_str().to_owned()),
        head: worktree.head.as_ref().map(|h| h.short().to_owned()),
        is_main: worktree.is_main,
        is_bare: worktree.is_bare,
        locked: worktree.locked.clone(),
        prunable: worktree.prunable.clone(),
        dirty: status.dirty_tracked,
        untracked: status.untracked,
        staged: status.staged,
        ahead: status.ahead,
        behind: status.behind,
        issue_key: extract_issue_key(worktree),
        favorite,
        badges,
        links,
        table,
        env,
    }
}

/// Expand every `[[display.port_table]]` into rows.
fn build_tables(
    project: &Project,
    sources: &BTreeMap<String, BTreeMap<String, String>>,
    engine: &dyn TemplateEngine,
    ctx: &Context,
) -> Vec<TableRowView> {
    let mut rows = Vec::new();

    for table in &project.display.tables {
        let values = sources.get(&table.from);
        let defaults = table.defaults.as_ref().and_then(|id| sources.get(id));

        // Union of both key sets: a key present only in defaults is still a real row,
        // because the base value *is* what is in effect.
        let mut keys: Vec<&String> = values
            .into_iter()
            .flat_map(|v| v.keys())
            .chain(defaults.into_iter().flat_map(|d| d.keys()))
            .filter(|key| key.starts_with(&table.prefix))
            .collect();
        keys.sort_unstable();
        keys.dedup();

        for key in keys {
            let own = values.and_then(|v| v.get(key));
            let inherited_value = defaults.and_then(|d| d.get(key));
            let Some(value) = own.or(inherited_value) else {
                continue;
            };

            let mut label = key.clone();
            for transform in &table.label_transform {
                label = apply_label_transform(&label, transform, &table.prefix, engine);
            }

            let url = table
                .link_template
                .as_ref()
                .filter(|_| table.link_for.iter().any(|allowed| allowed == key))
                .and_then(|template| {
                    let mut row_ctx = ctx.clone();
                    row_ctx.insert("value".to_owned(), value.clone());
                    engine
                        .render("display.port_table.link_template", template, &row_ctx)
                        .ok()
                });

            rows.push(TableRowView {
                label,
                value: value.clone(),
                inherited: own.is_none(),
                url,
            });
        }
    }

    rows
}

/// Apply one label transform.
///
/// `strip_prefix('…')`-style calls are honoured with their literal argument; a bare name
/// is passed to the engine's filter set. Routing through the engine keeps the vocabulary
/// identical to the one templates use, rather than inventing a second, smaller one here.
fn apply_label_transform(
    label: &str,
    transform: &str,
    table_prefix: &str,
    engine: &dyn TemplateEngine,
) -> String {
    if let Some(argument) = transform
        .strip_prefix("strip_prefix(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let prefix = argument.trim().trim_matches(['\'', '"']);
        return label.strip_prefix(prefix).unwrap_or(label).to_owned();
    }
    // The common case: strip the table's own prefix without repeating it.
    if transform == "strip_prefix" {
        return label.strip_prefix(table_prefix).unwrap_or(label).to_owned();
    }
    if let Some(argument) = transform
        .strip_prefix("replace(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let parts: Vec<&str> = argument
            .split(',')
            .map(|p| p.trim().trim_matches(['\'', '"']))
            .collect();
        if let [from, to] = parts.as_slice() {
            return label.replace(from, to);
        }
        return label.to_owned();
    }

    engine
        .apply_filter(transform, label)
        .unwrap_or_else(|_| label.to_owned())
}

/// The actions a project offers on a worktree.
#[must_use]
pub fn action_views(project: &Project) -> Vec<ActionView> {
    project
        .actions
        .iter()
        .map(|action| ActionView {
            id: action.id.clone(),
            label: action.label.clone(),
            pty: action.command.pty,
        })
        .collect()
}

/// Render a command's argv and resolve its working directory.
///
/// Returns the argv with `args_when` clauses applied, so what runs is exactly what a
/// preview would have shown.
pub fn render_command(
    command: &CommandSpec,
    engine: &dyn TemplateEngine,
    ctx: &Context,
    key: &str,
) -> Result<Vec<String>, wtm_core::error::RenderError> {
    let mut argv = Vec::with_capacity(command.run.len());
    for (index, template) in command.run.iter().enumerate() {
        argv.push(engine.render(&format!("{key}[{index}]"), template, ctx)?);
    }

    for conditional in &command.args_when {
        if engine.eval_bool(&format!("{key}.args_when"), &conditional.when, ctx)? {
            for template in &conditional.push {
                argv.push(engine.render(&format!("{key}.args_when"), template, ctx)?);
            }
        }
    }

    Ok(argv)
}

/// Resolve a [`CwdBase`](wtm_core::model::CwdBase) into a real directory.
pub fn resolve_cwd(
    base: &wtm_core::model::CwdBase,
    project: &Project,
    worktree: Option<&Worktree>,
    engine: &dyn TemplateEngine,
    ctx: &Context,
) -> std::path::PathBuf {
    use wtm_core::model::CwdBase;

    match base {
        CwdBase::RepoRoot | CwdBase::MainWorktree => project.root.clone(),
        // Falling back to the repo root rather than failing: a config that asks for the
        // worktree in a context without one is a validation error, caught at load time.
        CwdBase::Worktree => worktree.map_or_else(|| project.root.clone(), |w| w.path.clone()),
        CwdBase::Custom(template) => engine
            .render("cwd", template, ctx)
            .map_or_else(|_| project.root.clone(), std::path::PathBuf::from),
    }
}

/// The token scope for anything running against an existing worktree.
#[must_use]
pub fn worktree_scope(position: &str) -> TokenScope {
    TokenScope::worktree_command(position)
}

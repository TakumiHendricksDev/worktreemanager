//! The Tauri command surface.
//!
//! # Every command is `async` and every command body is `spawn_blocking`
//!
//! Tauri runs a synchronous command on the webview thread, so any real work there freezes
//! the UI. Meanwhile the ports are deliberately synchronous — `git`, pty reads and
//! `child.wait()` are blocking syscalls, and making the traits async would spread
//! `Send + 'static` bounds through the whole domain for nothing. The resolution is here,
//! at the edge: `async fn` that immediately hands the blocking work to a thread pool.
//!
//! # Errors cross the boundary as `ErrorView`
//!
//! Never as a formatted string. The frontend has to distinguish "needs trust approval"
//! and "these fields are invalid" from "something failed", and `ErrorView::detail` carries
//! the structured payload for those cases.

use std::path::PathBuf;
use std::sync::Arc;

use regex::Regex;

use tauri::State;
use wtm_core::model::ProjectId;
use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::pty::PtyHost;

use crate::app::App;
use crate::display;
use crate::openers;
use crate::view::{
    ActionView, DoctorView, ErrorView, FieldView, FormView, OpenersView, PaletteView, ProjectView,
    RegisteredView, WorktreeView,
};

/// Shared application state.
pub type AppState<'a> = State<'a, Arc<App>>;

/// Where the chosen "Open in …" tool is remembered.
///
/// A plain `ui.*` preference, so it needs no schema in `wtm-config` at all — unknown keys
/// under that prefix round-trip through `UiPrefs::extra`, the same route `ui.sidebar_width`
/// takes. Every read goes through [`openers::preferred`] so that making this per-project
/// later is one function rather than a silent reset of everyone's setting.
const OPENER_PREF: &str = "ui.opener";

type Reply<T> = Result<T, ErrorView>;

/// Run blocking work off the webview thread.
///
/// The one helper every command goes through, so no command can accidentally block the
/// UI thread by forgetting.
async fn blocking<T, F>(work: F) -> Reply<T>
where
    T: Send + 'static,
    F: FnOnce() -> Reply<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .unwrap_or_else(|e| {
            // A panic inside a command must surface as an error, not silently hang the UI
            // waiting for a reply that will never come.
            tracing::error!(error = %e, "a command panicked");
            Err(ErrorView::new(
                "panic",
                "the operation crashed; see the log for details",
            ))
        })
}

// ─────────────────────────────── projects ───────────────────────────────

/// Registered projects, each annotated with whether it is usable.
#[tauri::command]
pub async fn list_projects(app: AppState<'_>) -> Reply<Vec<ProjectView>> {
    let app = Arc::clone(&app);
    blocking(move || app.projects().map_err(Into::into)).await
}

/// Register a repository. Accepts any path inside it; resolves to the root.
///
/// Returns the resolved id as well as the list, because the caller wants to select what it
/// just added and cannot work out which entry that is — see [`RegisteredView`].
#[tauri::command]
pub async fn register_project(app: AppState<'_>, path: String) -> Reply<RegisteredView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = app.register(&PathBuf::from(path))?;
        Ok(RegisteredView {
            // Built with the same function `project_view` uses, so the id can never drift
            // from the one in `projects` and silently stop matching.
            id: ProjectId::from_root(&root).to_string(),
            projects: app.projects()?,
        })
    })
    .await
}

/// Unregister a repository.
///
/// Takes the project's **root**, not any path inside it: the store removes an exact key
/// (`UserConfig::unregister`), so a subdirectory would quietly remove nothing.
#[tauri::command]
pub async fn unregister_project(app: AppState<'_>, path: String) -> Reply<Vec<ProjectView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.config.unregister_project(&PathBuf::from(path))?;
        app.projects().map_err(Into::into)
    })
    .await
}

// ─────────────────────────────── worktrees ───────────────────────────────

/// A project's worktrees, rendered for display.
#[tauri::command]
pub async fn list_worktrees(app: AppState<'_>, project_id: String) -> Reply<Vec<WorktreeView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        app.worktrees(&project).map_err(Into::into)
    })
    .await
}

/// Star or unstar a worktree, persisting to the app config.
///
/// Returns nothing on purpose. Unlike a create or a remove, this changes no git state and
/// nothing else can contradict it, so the frontend flips its own star and calls this
/// behind the click — the same shape as a theme change. Re-listing every worktree to
/// confirm one boolean would cost several `git` invocations per click.
#[tauri::command]
pub async fn set_worktree_favorite(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    favorite: bool,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        app.set_favorite(&project, &worktree_id, favorite)
            .map_err(Into::into)
    })
    .await
}

// ─────────────────────────────── the form ───────────────────────────────

/// The New Worktree form for a project, straight from its config.
///
/// This is what makes the dialog project-defined: the frontend renders whatever comes
/// back, and adding a field to a `wtm.toml` changes the UI with no code change.
#[tauri::command]
pub async fn worktree_form(app: AppState<'_>, project_id: String) -> Reply<FormView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;

        let fields = project
            .fields
            .iter()
            .map(|field| {
                let (has_dynamic_options, options) = match &field.options {
                    Some(wtm_core::model::OptionsSource::Static { values }) => {
                        (false, values.clone())
                    }
                    Some(wtm_core::model::OptionsSource::Command { .. }) => (true, Vec::new()),
                    None => (false, Vec::new()),
                };

                FieldView {
                    key: field.key.clone(),
                    label: field.label.clone(),
                    kind: field.kind.clone(),
                    required: field.required,
                    // Collapsed to a string for the UI: the form binds text inputs, and a
                    // checkbox reads `default === "true"`.
                    default: field
                        .default
                        .as_ref()
                        .map(wtm_core::model::FieldDefault::as_string),
                    placeholder: field.placeholder.clone(),
                    help: field.help.clone(),
                    allow_custom: field.allow_custom,
                    has_dynamic_options,
                    options,
                    pattern: field.pattern.clone(),
                    pattern_message: field.pattern_message.clone(),
                }
            })
            .collect();

        Ok(FormView {
            project_id: project.id.as_str().to_owned(),
            fields,
            actions: display::action_views(&project),
        })
    })
    .await
}

/// Run a select field's options command and return the choices.
///
/// This is the "pull the options from bash" capability. Separate from
/// [`worktree_form`] so the form paints immediately and each dropdown fills in as its
/// command finishes, rather than the whole dialog waiting on the slowest one.
#[tauri::command]
pub async fn field_options(
    app: AppState<'_>,
    project_id: String,
    field_key: String,
) -> Reply<Vec<String>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let field = project.field(&field_key).ok_or_else(|| {
            ErrorView::new("unknownField", format!("no field named `{field_key}`"))
        })?;

        let Some(wtm_core::model::OptionsSource::Command {
            command,
            parse,
            exclude,
            cache_ttl_ms: _,
        }) = &field.options
        else {
            return Ok(Vec::new());
        };

        let ctx = display::base_context(&project, app.os_tokens());
        let key = format!("field.{field_key}.options.run");
        let argv = display::render_command(command, app.engine.as_ref(), &ctx, &key)
            .map_err(|e| ErrorView::new("render", e.to_string()))?;

        // Defence in depth: the guard rules were checked when the config loaded, but the
        // argv is only fully known once its templates are rendered.
        wtm_config::check_forbidden(&project, &argv)?;

        let cwd = display::resolve_cwd(&command.cwd, &project, None, app.engine.as_ref(), &ctx);
        let mut inv =
            wtm_core::ports::exec::Invocation::new(argv, cwd, command.timeout_ms.unwrap_or(10_000));
        inv.env = render_env(command, app.engine.as_ref(), &ctx, &key);

        let output = app
            .runner
            .run(&inv, &wtm_core::ports::exec::CancelToken::new())
            .map_err(|e| ErrorView::new("exec", e.to_string()))?;

        let mut values = match parse {
            wtm_core::model::OptionsParse::Lines => output.lines(),
            wtm_core::model::OptionsParse::Nul => output
                .stdout
                .split('\0')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            wtm_core::model::OptionsParse::Json => {
                serde_json::from_str::<Vec<String>>(&output.stdout).map_err(|e| {
                    ErrorView::new("parse", format!("options command did not return JSON: {e}"))
                })?
            }
        };

        if let Some(pattern) = exclude
            && let Ok(regex) = regex_lite(pattern)
        {
            values.retain(|value| !regex.is_match(value));
        }

        Ok(values)
    })
    .await
}

/// Compile an exclude pattern.
///
/// Config validation does not check this one, so a bad pattern must degrade to "exclude
/// nothing" rather than emptying the dropdown.
fn regex_lite(pattern: &str) -> Result<Regex, ()> {
    Regex::new(pattern).map_err(|e| {
        tracing::warn!(pattern, error = %e, "invalid options exclude pattern; ignoring");
    })
}

/// Render a command's environment overrides.
fn render_env(
    command: &wtm_core::model::CommandSpec,
    engine: &dyn wtm_core::ports::template::TemplateEngine,
    ctx: &wtm_core::ports::template::Context,
    key: &str,
) -> std::collections::BTreeMap<String, String> {
    command
        .env
        .iter()
        .filter_map(|(name, template)| {
            engine
                .render(&format!("{key}.env.{name}"), template, ctx)
                .ok()
                .map(|value| (name.clone(), value))
        })
        .collect()
}

// ─────────────────────────────── trust ───────────────────────────────

/// Record a trust decision for a config file.
///
/// `approve` binds to the file's *current* contents, so a later edit re-arms the prompt.
#[tauri::command]
pub async fn set_config_trust(
    app: AppState<'_>,
    path: String,
    approve: bool,
) -> Reply<Vec<ProjectView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let decision = if approve {
            wtm_core::ports::config::TrustDecision::Approve
        } else {
            wtm_core::ports::config::TrustDecision::Reject
        };
        app.config.set_trust(&PathBuf::from(path), decision)?;
        app.projects().map_err(Into::into)
    })
    .await
}

// ─────────────────────────────── preferences ───────────────────────────────

#[tauri::command]
pub async fn get_pref(app: AppState<'_>, key: String) -> Reply<Option<String>> {
    let app = Arc::clone(&app);
    blocking(move || app.config.user_pref(&key).map_err(Into::into)).await
}

#[tauri::command]
pub async fn set_pref(app: AppState<'_>, key: String, value: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || app.config.set_user_pref(&key, &value).map_err(Into::into)).await
}

// ─────────────────────────────── diagnostics ───────────────────────────────

/// The resolved `PATH` and which project tools are reachable.
///
/// Exists because of the app's most likely production failure: a bundled `.app` inherits
/// `launchd`'s minimal `PATH` and cannot see Homebrew, which looks like "the config is
/// broken" until you can see the PATH the app is actually using.
#[tauri::command]
pub async fn doctor(app: AppState<'_>) -> Reply<DoctorView> {
    let app = Arc::clone(&app);
    blocking(move || Ok(app.doctor())).await
}

/// The palettes declared in `[ui.palettes]`, for the Settings picker.
///
/// Only the user's own. The six that ship with the app are compiled into the stylesheet and
/// Rust has never heard of them — which is deliberate: a built-in palette is a set of CSS
/// custom properties, and routing them through IPC so the frontend could list what it
/// already contains would be a contract to keep in step for no gain.
///
/// Unusable entries come back with `error` set rather than being dropped. See
/// [`PaletteView`](crate::view::PaletteView).
#[tauri::command]
pub async fn list_palettes(app: AppState<'_>) -> Reply<Vec<PaletteView>> {
    let app = Arc::clone(&app);
    blocking(move || Ok(app.palettes())).await
}

/// Return one environment value that was withheld from the worktree listing.
///
/// *No* value travels with `list_worktrees` — see [`EnvKeys`](crate::view::EnvKeys). This
/// fetches exactly one, by key, when the user clicks reveal. Three properties follow, and
/// they are the reason this is a separate command rather than a flag on the listing:
///
/// * a screenshot or screen-share of the app cannot leak a value that was never sent;
/// * only the key the user asked for enters the webview, not all fifty;
/// * the value is read fresh from disk each time, so nothing is cached in the frontend.
///
/// The value is deliberately **not** logged, at any level.
#[tauri::command]
pub async fn reveal_env_value(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    key: String,
) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        app.env_value(&project, &worktree_id, &key)
            .map_err(Into::into)
    })
    .await
}

/// Open an `http`/`https` URL in the user's browser.
///
/// Routed through a command rather than a plugin so the scheme is validated here. The
/// URLs come from `[[display.link]]` templates, which come from a config file inside a
/// repository — so without this check a config could hand the OS a `file://` path or a
/// custom scheme registered by some other application and have it opened on demand.
///
/// The two checks below are what make handing a string to a shell-out safe, and the
/// scheme check is doing more work than it looks: a URL that begins with `http://` or
/// `https://` cannot begin with `-`, so neither opener can parse it as a flag.
#[tauri::command]
pub async fn open_url(app: AppState<'_>, url: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ErrorView::new(
                "badUrl",
                format!("refusing to open `{url}`: only http and https are allowed"),
            ));
        }
        // A URL containing whitespace or a control character is malformed and could be
        // an attempt to smuggle a second argument past the opener. This matters more on
        // Linux, not less: `xdg-open` is a shell script that dispatches to a
        // desktop-specific opener, so although we pass one argv element and never a
        // command line, it is defence that now guards two paths.
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(ErrorView::new(
                "badUrl",
                "refusing to open a URL containing whitespace",
            ));
        }

        let inv = wtm_core::ports::exec::Invocation::new(
            vec![openers::OPENER.to_owned(), url],
            std::env::temp_dir(),
            10_000,
        );
        // Deliberately the captured runner, not `launch_detached`: the platform opener
        // returns within milliseconds and its exit code is real signal — `xdg-open` exits
        // 3 when no handler is registered, which is worth surfacing. See
        // `Runner::launch_detached` for why `open_in` needs the opposite.
        app.runner
            .run(&inv, &wtm_core::ports::exec::CancelToken::new())
            .map(|_| ())
            .map_err(|e| ErrorView::new("exec", e.to_string()))
    })
    .await
}

// ─────────────────────────────── open in ───────────────────────────────

/// Every tool wtm knows how to open a worktree in, resolved against this machine.
///
/// The whole catalogue comes back, not just what is installed — see
/// [`openers::resolve_all`] for why, and for why nothing here is cached.
#[tauri::command]
pub async fn list_openers(app: AppState<'_>) -> Reply<OpenersView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let resolved = openers::resolve_all(&app.probe());
        let stored = app.config.user_pref(OPENER_PREF)?;
        let preferred = openers::preferred(&resolved, stored.as_deref()).map(|a| a.id.to_owned());

        Ok(OpenersView {
            openers: resolved.iter().map(Into::into).collect(),
            preferred,
        })
    })
    .await
}

/// Hand a worktree's directory to an external tool.
///
/// Spawned through [`wtm_exec::Runner::launch_detached`], which has no deadline. That is
/// the whole reason it exists: the captured runner terminates the process group on expiry,
/// and a shim like `code` or the JetBrains launcher stays in the foreground for the
/// editor's lifetime — so a deadline would kill the application seconds after opening it.
#[tauri::command]
pub async fn open_in(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    opener_id: String,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        let opener = openers::find(&opener_id).ok_or_else(|| {
            ErrorView::new(
                "unknownOpener",
                format!("wtm has no opener called `{opener_id}`"),
            )
        })?;

        let project = app.project(&project_id)?;
        let worktree = app.worktree(&project, &worktree_id)?;

        // Re-probed rather than trusted from whatever `list_openers` last reported: the
        // user may have uninstalled the tool since the picker was drawn, and the message
        // below is better than the bare spawn failure that would otherwise surface.
        let probe = app.probe();
        let resolved = openers::resolve_all(&probe);
        let launch = resolved
            .iter()
            .find(|a| a.id == opener.id)
            .and_then(|a| a.launch)
            .ok_or_else(|| {
                ErrorView::new(
                    "openerUnavailable",
                    format!(
                        "{} is not installed, or wtm cannot see it — {}",
                        opener.label_macos,
                        resolved
                            .iter()
                            .find(|a| a.id == opener.id)
                            .and_then(|a| a.detail.clone())
                            .unwrap_or_default()
                    ),
                )
                .with_detail(serde_json::json!({
                    "openerId": opener.id,
                    "searched": app.runner.resolved_path(),
                }))
            })?;

        // A worktree whose directory has been deleted is still listed by git (as
        // prunable). Launching an editor at a path that is not there produces a confusing
        // empty window rather than an error, so catch it here.
        if !app.files.exists(&worktree.path) {
            return Err(ErrorView::new(
                "exec",
                format!(
                    "`{}` no longer exists on disk — the worktree may need pruning",
                    worktree.path.display()
                ),
            ));
        }

        let inv = openers::invocation_for(launch, &worktree.path, &probe)
            .map_err(|e| ErrorView::new("badPath", e.to_string()))?;

        // Not a defence against a hostile argv: every element is either a literal from the
        // catalogue or a path git reported, so there is nothing here for a config to
        // inject. It is called because `[guards].forbid` is the project's stated list of
        // what must not be spawned in its worktrees, and a spawn site that quietly exempts
        // itself is how a guard stops meaning what it says. Empty for almost every
        // project, so it costs an empty `collect`.
        wtm_config::check_forbidden(&project, &inv.argv)?;

        app.launcher
            .launch_detached(&inv)
            .map_err(|e| ErrorView::new("exec", e.to_string()))
    })
    .await
}

/// The actions a project offers on a worktree.
#[tauri::command]
pub async fn list_actions(app: AppState<'_>, project_id: String) -> Reply<Vec<ActionView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        Ok(display::action_views(&project))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_exclude_pattern_degrades_to_excluding_nothing() {
        // Emptying a dropdown because of a config typo would be far worse than ignoring
        // the filter, and this pattern is not covered by load-time validation.
        assert!(regex_lite("([unclosed").is_err());
        assert!(regex_lite("^origin/HEAD$").is_ok());
    }

    /// A panic must reach the UI as an error, not take the process with it.
    ///
    /// This boundary existed already but was unreachable in a release build: the profile set
    /// `panic = "abort"`, so the process died before the handler below could run — which is
    /// exactly what "the application crashed" looks like from the outside. The companion guard
    /// is `the_release_profile_does_not_abort_on_panic`; both are needed, because either one
    /// alone lets a silent death back in.
    #[test]
    fn a_panicking_command_becomes_an_error() {
        let reply: Reply<()> = tauri::async_runtime::block_on(blocking(|| panic!("boom")));
        let error = reply.expect_err("a panic must not be reported as success");
        assert_eq!(error.kind, "panic");
    }

    #[test]
    fn the_release_profile_does_not_abort_on_panic() {
        let manifest = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml"),
        )
        .expect("read the workspace manifest");
        let doc: toml::Table = toml::from_str(&manifest).expect("parse the workspace manifest");

        let release = doc
            .get("profile")
            .and_then(|p| p.get("release"))
            .and_then(toml::Value::as_table)
            .expect("[profile.release]");

        assert!(
            release.get("panic").is_none(),
            "setting `panic` in [profile.release] would make every caught panic a silent \
             process death; got {:?}",
            release.get("panic")
        );
    }
}

// ═══════════════════════════ create / remove ═══════════════════════════

/// Build the pipeline's request from what the frontend sent.
fn create_request(
    app: &App,
    project_id: &str,
    values: &std::collections::BTreeMap<String, String>,
    adopt_branch: Option<String>,
    acknowledged: Vec<String>,
    rows: u16,
    cols: u16,
) -> Result<wtm_core::usecase::CreateRequest, ErrorView> {
    let project = app.project(project_id)?;

    // Everything arrives as a string from the form; `FieldValue::from` maps "" to Empty so
    // `required` behaves, and a bool field's "true"/"false" is decoded by kind.
    let mut raw = std::collections::BTreeMap::new();
    for field in &project.fields {
        let text = values.get(&field.key).cloned().unwrap_or_default();
        let value = match field.kind {
            wtm_core::model::FieldKind::Bool => wtm_core::model::FieldValue::Bool(text == "true"),
            wtm_core::model::FieldKind::Number => text.parse::<f64>().map_or(
                wtm_core::model::FieldValue::Empty,
                wtm_core::model::FieldValue::Number,
            ),
            _ => wtm_core::model::FieldValue::from(text),
        };
        raw.insert(field.key.clone(), value);
    }

    let mut ambient = display::base_context(&project, app.os_tokens());
    ambient.insert("now.date".to_owned(), app.clock.today());
    ambient.insert("now.iso".to_owned(), app.clock.now_iso());
    ambient.insert("now.unix".to_owned(), app.clock.now_unix_ms().to_string());
    // A config may pass the resolved shell or PATH through to a command it spawns.
    for name in ["SHELL", "HOME", "USER", "LOGIN_PATH"] {
        if let Ok(value) = std::env::var(name) {
            ambient.entry(format!("env.{name}")).or_insert(value);
        }
    }
    ambient.insert("env.LOGIN_PATH".to_owned(), app.runner.resolved_path());

    Ok(wtm_core::usecase::CreateRequest {
        project,
        values: wtm_core::model::FormValues::new(raw),
        ambient,
        adopt_branch,
        acknowledged,
        rows,
        cols,
    })
}

/// Stages 1–6b: what will happen, before anything happens.
///
/// Safe to call on every form change — the pipeline's central invariant is that these stages
/// mutate nothing, so the review screen can be live.
#[tauri::command]
pub async fn preview_worktree(
    app: AppState<'_>,
    project_id: String,
    values: std::collections::BTreeMap<String, String>,
    adopt_branch: Option<String>,
) -> Reply<crate::view::PreviewView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let req = create_request(
            &app,
            &project_id,
            &values,
            adopt_branch,
            Vec::new(),
            24,
            100,
        )?;
        let pipeline = app.create_pipeline();
        let preview = pipeline.preview(
            &req,
            &wtm_core::ports::progress::NullProgress,
            &wtm_core::ports::exec::CancelToken::new(),
        )?;
        Ok(crate::view::preview_view(&preview, &req.values))
    })
    .await
}

/// Create the worktree: fetch, `git worktree add`, then the project's setup command in a PTY.
///
/// Re-runs the planning stages first rather than trusting the preview the caller holds — the
/// repository can change between the review screen and the click, and re-planning is free
/// because it mutates nothing.
#[tauri::command]
pub async fn create_worktree(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    values: std::collections::BTreeMap<String, String>,
    adopt_branch: Option<String>,
    acknowledged: Vec<String>,
    rows: u16,
    cols: u16,
) -> Reply<wtm_core::model::CreateOutcome> {
    let app = Arc::clone(&app);
    blocking(move || {
        let req = create_request(
            &app,
            &project_id,
            &values,
            adopt_branch,
            acknowledged,
            rows,
            cols,
        )?;
        let progress = crate::pty_bridge::ProgressBridge::new(handle.clone());
        let sink = crate::pty_bridge::EventSink::new(handle);

        app.create_pipeline()
            .execute(
                &req,
                &progress,
                sink,
                &wtm_core::ports::exec::CancelToken::new(),
            )
            .map_err(Into::into)
    })
    .await
}

/// What the remove dialog needs to warn about, without touching anything.
#[tauri::command]
pub async fn remove_preflight(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    delete_branch: bool,
) -> Reply<Vec<crate::view::PreflightView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let req = remove_request(
            &app,
            &project_id,
            &worktree_id,
            delete_branch,
            false,
            Vec::new(),
        )?;
        let items = app.remove_pipeline().preflight(&req)?;
        Ok(items.iter().map(crate::view::preflight_view).collect())
    })
    .await
}

fn remove_request(
    app: &App,
    project_id: &str,
    worktree_id: &str,
    delete_branch: bool,
    force: bool,
    acknowledged: Vec<String>,
) -> Result<wtm_core::usecase::RemoveRequest, ErrorView> {
    let project = app.project(project_id)?;
    let worktree = app.worktree(&project, worktree_id)?;

    let mut ambient = display::base_context(&project, app.os_tokens());
    // Teardown steps are commonly gated on `env.COMPOSE_PROJECT_NAME != ''`, so the
    // worktree's own env has to be in scope or they would all be skipped.
    let mut source_ctx = ambient.clone();
    display::add_worktree_tokens(&mut source_ctx, &worktree);
    let sources = display::read_sources(
        &project,
        app.files.as_ref(),
        app.engine.as_ref(),
        &source_ctx,
    );
    display::add_source_tokens(&mut ambient, &project, &sources);

    Ok(wtm_core::usecase::RemoveRequest {
        project,
        worktree,
        ambient,
        delete_branch,
        force,
        acknowledged,
    })
}

/// Remove a worktree: the project's teardown steps, then `git worktree remove`, then
/// optionally the branch.
#[tauri::command]
pub async fn remove_worktree(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    delete_branch: bool,
    force: bool,
    acknowledged: Vec<String>,
) -> Reply<wtm_core::usecase::RemoveOutcome> {
    let app = Arc::clone(&app);
    blocking(move || {
        let req = remove_request(
            &app,
            &project_id,
            &worktree_id,
            delete_branch,
            force,
            acknowledged,
        )?;
        let progress = crate::pty_bridge::ProgressBridge::new(handle.clone());
        let sink = crate::pty_bridge::EventSink::new(handle);

        let outcome = app.remove_pipeline().execute(
            &req,
            &progress,
            &(sink as Arc<dyn wtm_core::ports::pty::PtySink>),
            &wtm_core::ports::exec::CancelToken::new(),
        )?;

        // Drop the star along with the worktree. Without this the app config accumulates
        // paths that no longer exist, and a later worktree created at the same path would
        // come back mysteriously starred.
        if matches!(outcome, wtm_core::usecase::RemoveOutcome::Removed { .. })
            && let Err(err) = app.set_favorite(&req.project, &worktree_id, false)
        {
            // The worktree is already gone; a leftover entry is untidy, not broken.
            tracing::warn!(error = %err, "could not clear the favorite for a removed worktree");
        }

        Ok(outcome)
    })
    .await
}

/// Re-run a project's setup against an existing worktree.
///
/// Both the `RetrySetup` remedy after a failed create and the way to adopt a worktree made
/// outside the app. One code path, two entry points.
#[tauri::command]
pub async fn run_setup(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    extra_args: Vec<String>,
    rows: u16,
    cols: u16,
) -> Reply<crate::view::SetupResultView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let worktree = app.worktree(&project, &worktree_id)?;

        let mut ambient = display::base_context(&project, app.os_tokens());
        ambient.insert("env.LOGIN_PATH".to_owned(), app.runner.resolved_path());

        let progress = crate::pty_bridge::ProgressBridge::new(handle.clone());
        let sink = crate::pty_bridge::EventSink::new(handle);
        let (session, outcome) = app.create_pipeline().run_setup(
            &wtm_core::usecase::SetupRequest {
                project: &project,
                worktree: &worktree,
                ambient: &ambient,
                extra_args: &extra_args,
                rows,
                cols,
            },
            sink,
            &progress,
        )?;

        Ok(crate::view::SetupResultView {
            session: session.as_str().to_owned(),
            success: outcome.is_success(),
            summary: outcome.describe(),
        })
    })
    .await
}

// ═══════════════════════════ pty session control ═══════════════════════════

/// Forward keystrokes to a running session, so a prompt can be answered.
#[tauri::command]
pub async fn pty_write(app: AppState<'_>, session: String, data_base64: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        let bytes = crate::pty_bridge::base64_decode(&data_base64)
            .ok_or_else(|| ErrorView::new("badInput", "terminal input was not valid base64"))?;
        app.pty
            .write(&wtm_core::model::SessionId::new(session), &bytes)
            .map_err(|e| ErrorView::new("exec", e.to_string()))
    })
    .await
}

/// Tell a session its window changed, so full-screen output reflows.
#[tauri::command]
pub async fn pty_resize(app: AppState<'_>, session: String, rows: u16, cols: u16) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.pty
            .resize(&wtm_core::model::SessionId::new(session), rows, cols)
            .map_err(|e| ErrorView::new("exec", e.to_string()))
    })
    .await
}

/// Kill a session's process group.
#[tauri::command]
pub async fn pty_kill(app: AppState<'_>, session: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.pty
            .kill(&wtm_core::model::SessionId::new(session))
            .map_err(|e| ErrorView::new("exec", e.to_string()))
    })
    .await
}

/// Run one of a project's declared `[[action]]`s in a PTY.
#[tauri::command]
pub async fn run_action(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    action_id: String,
    rows: u16,
    cols: u16,
) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let worktree = app.worktree(&project, &worktree_id)?;
        let action = project.action(&action_id).ok_or_else(|| {
            ErrorView::new("unknownAction", format!("no action named `{action_id}`"))
        })?;

        let mut ctx = display::base_context(&project, app.os_tokens());
        display::add_worktree_tokens(&mut ctx, &worktree);
        let sources =
            display::read_sources(&project, app.files.as_ref(), app.engine.as_ref(), &ctx);
        display::add_source_tokens(&mut ctx, &project, &sources);
        ctx.insert("env.LOGIN_PATH".to_owned(), app.runner.resolved_path());

        let key = format!("action.{action_id}.run");
        let argv = display::render_command(&action.command, app.engine.as_ref(), &ctx, &key)
            .map_err(|e| ErrorView::new("render", e.to_string()))?;

        // Defence in depth: guards were checked at config load, but an argv is only fully
        // known once its templates are rendered.
        wtm_config::check_forbidden(&project, &argv)?;

        let cwd = display::resolve_cwd(
            &action.command.cwd,
            &project,
            Some(&worktree),
            app.engine.as_ref(),
            &ctx,
        );
        let mut inv = wtm_core::ports::exec::Invocation::new(
            argv,
            cwd,
            action.command.timeout_ms.unwrap_or(1_800_000),
        );
        inv.env = render_env(&action.command, app.engine.as_ref(), &ctx, &key);

        let sink = crate::pty_bridge::EventSink::new(handle);
        let spawned = app
            .pty
            .spawn(&inv, rows, cols, Some(worktree.id.as_str()), sink)
            .map_err(|e| ErrorView::new("exec", e.to_string()))?;

        // Return as soon as it is running: the terminal pane streams the rest, and an
        // interactive shell would otherwise block this command forever.
        Ok(spawned.session.as_str().to_owned())
    })
    .await
}

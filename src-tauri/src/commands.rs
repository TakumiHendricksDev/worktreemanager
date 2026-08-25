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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use regex::Regex;

use tauri::State;
use wtm_core::model::ProjectId;
use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::pty::PtyHost;

use crate::app::App;
use crate::display;
use crate::handoff;
use crate::openers;
use crate::view::{
    ActionView, AgentOptionView, AgentSessionView, BackgroundTaskView, BriefView, CapabilityView,
    DoctorView, ErrorView, FieldView, FormView, OpenersView, PaletteView, ProjectView,
    RegisteredView, ResumableView, TerminalSessionView, WorktreeView,
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

// ─────────────────────────────── dictation ───────────────────────────────

/// Whether dictation can be offered, and what is missing if not.
#[tauri::command]
pub async fn dictation_status(app: AppState<'_>) -> Reply<crate::dictate::DictationStatus> {
    let app = Arc::clone(&app);
    blocking(move || Ok(crate::dictate::status(&app))).await
}

/// Store the transcription key.
///
/// One-way, deliberately: there is no command that reads it back. The frontend can learn that a key
/// is set — `DictationStatus::key_set` — and never what it is, which is the same discipline
/// `reveal_env_value` and `EnvKeys` keep for environment values. An empty string clears it, so the
/// Settings field needs no separate Clear button.
#[tauri::command]
pub async fn set_dictation_key(app: AppState<'_>, key: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || crate::dictate::set_key(&app, &key).map_err(|e| ErrorView::new("dictate", e)))
        .await
}

/// Start recording from the microphone.
#[tauri::command]
pub async fn start_dictation(app: AppState<'_>) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || crate::dictate::start(&app).map_err(|e| ErrorView::new("dictate", e))).await
}

/// Stop recording and return what was said.
#[tauri::command]
pub async fn stop_dictation(app: AppState<'_>) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || crate::dictate::stop(&app).map_err(|e| ErrorView::new("dictate", e))).await
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

// ────────────────────────────── notifications ──────────────────────────────

/// Post a notification whose click navigates back to the pane it is about.
///
/// The decision to post at all is the webview's — it knows the selection and the window focus,
/// and Rust does not; see `attention.svelte.ts`. This side attaches the payload the click needs
/// and reports the one failure worth acting on: the OS refusing delivery.
#[tauri::command]
pub async fn post_notification(
    handle: tauri::AppHandle,
    title: String,
    body: String,
    project_id: String,
    worktree_id: String,
    pane_id: String,
) -> Reply<()> {
    blocking(move || {
        use tauri::Manager;
        let notifier = handle.state::<Arc<crate::notifier::Notifier>>();
        notifier.post(
            &handle,
            &title,
            &body,
            &wtm_notify::ClickPayload {
                project_id,
                worktree_id,
                pane_id,
            },
        )
    })
    .await
}

/// What the OS says about delivering notifications: `granted`, `denied` or `prompt`.
#[tauri::command]
pub async fn notification_permission(handle: tauri::AppHandle) -> Reply<String> {
    blocking(move || {
        use tauri::Manager;
        let notifier = handle.state::<Arc<crate::notifier::Notifier>>();
        Ok(notifier.permission().to_owned())
    })
    .await
}

/// Ask the OS for notification permission. Blocks until the user answers the prompt, which is
/// why it goes through `blocking()` like everything else that waits.
#[tauri::command]
pub async fn request_notification_permission(handle: tauri::AppHandle) -> Reply<bool> {
    blocking(move || {
        use tauri::Manager;
        let notifier = handle.state::<Arc<crate::notifier::Notifier>>();
        Ok(notifier.request_permission())
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

    /// A stale `$SHELL` must not be the reason a terminal refuses to open.
    ///
    /// The candidate and the predicate are injected because a test cannot set `$SHELL`:
    /// `std::env::set_var` is `unsafe` in Rust 2024 and this workspace forbids `unsafe_code`.
    #[test]
    fn the_login_shell_falls_back_to_a_posix_shell_when_the_environment_names_one_that_is_not_there()
     {
        let installed = |path: &str| path == "/bin/zsh";

        assert_eq!(
            login_shell(Some("/bin/zsh".to_owned()), installed),
            ["/bin/zsh", "-l"]
        );
        // The `brew uninstall fish` case: still in the environment, gone from disk.
        assert_eq!(
            login_shell(Some("/opt/homebrew/bin/fish".to_owned()), installed),
            ["/bin/sh", "-l"]
        );
        assert_eq!(login_shell(None, installed), ["/bin/sh", "-l"]);
        // An empty or whitespace `SHELL` is set-but-useless, which `filter` on emptiness alone
        // would let through as a program named " ".
        assert_eq!(
            login_shell(Some(String::new()), installed),
            ["/bin/sh", "-l"]
        );
        assert_eq!(
            login_shell(Some("   ".to_owned()), installed),
            ["/bin/sh", "-l"]
        );
    }

    /// A pane measured before it has a box must not spawn a zero-sized pty.
    ///
    /// The dock is closed by default, so this is the ordinary path rather than an edge case.
    /// `openpty` accepts 0×0 without complaint and then every full-screen program in the
    /// session draws into nothing, which reads as a broken terminal rather than a bad
    /// measurement.
    #[test]
    fn a_zero_sized_terminal_is_given_a_usable_default_geometry() {
        assert_eq!(usable_geometry(0, 0), (24, 80));
        assert_eq!(usable_geometry(30, 0), (30, 80));
        assert_eq!(usable_geometry(0, 120), (24, 120));
        // A real measurement passes through untouched.
        assert_eq!(usable_geometry(41, 173), (41, 173));
    }

    /// The shell deadline must stay inside what an `Instant` can represent.
    ///
    /// `PtyHost::wait` computes `started + Duration::from_millis(timeout_ms)`, and
    /// `Instant + Duration` panics on overflow — so `u64::MAX`, the obvious spelling of "no
    /// deadline", is a trap armed for whoever first calls `wait` on a dock shell. Asserted
    /// against the nanosecond range rather than by constructing a real `Instant`, which
    /// `clippy.toml` disallows.
    #[test]
    fn the_shell_deadline_stays_inside_what_an_instant_can_represent() {
        let nanos = std::time::Duration::from_millis(SHELL_TIMEOUT_MS).as_nanos();
        assert!(
            nanos < u128::from(u64::MAX),
            "{SHELL_TIMEOUT_MS} ms is {nanos} ns, which would overflow an Instant"
        );
        // A week, so it also cannot be mistaken for a deadline anyone meant to enforce. Not
        // asserted — clippy rejects an assertion whose value is a compile-time constant, and it
        // is right that `7 * 24 * 60 * 60 * 1_000` says this at the definition already.
    }

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
        // End every dock shell in the worktree before teardown runs.
        //
        // Two reasons, and the second is the one that bites. A shell sitting in a directory
        // that is about to be deleted keeps running with an unlinked cwd, which is confusing
        // but survivable. What is not survivable is what the shell *started*: a dev server
        // writing into `node_modules` is exactly the untracked churn that makes
        // `git worktree remove` refuse, so a removal that ought to work fails for a reason
        // nothing in the dialog mentions.
        //
        // All of them, not one: a worktree can hold several, and the odds that the one running a
        // dev server is the one left behind are exactly as bad as they sound.
        for session in app.shells_in(&worktree_id) {
            app.close_shell(&session);
        }

        // And every agent session, for the same reason with more force. An agent mid-turn is
        // *writing* into the directory git is about to refuse to delete, and unlike a shell it may
        // also be holding a model connection open with nothing left to talk to.
        for session in app.agents_in(&worktree_id) {
            app.close_agent(&session);
        }

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

        // And the resume entries, for a sharper reason than tidiness: every one names this
        // worktree's absolute path, so each would fail on click with an error about a missing
        // directory — offering to resume something that cannot be resumed.
        if matches!(outcome, wtm_core::usecase::RemoveOutcome::Removed { .. }) {
            app.forget_worktree_sessions(&worktree_id);
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

// ═══════════════════════════ the terminal dock ═══════════════════════════

/// The deadline recorded on an interactive shell session.
///
/// Nothing enforces it, and saying that out loud is better than papering over it.
/// `Invocation::timeout_ms` is mandatory because `CommandRunner::run` and `PtyHost::wait` are
/// the callers that enforce it, and the reason it is mandatory is a project script that
/// prompts in a loop and never sees EOF. A dock shell is the mirror image: prompting forever
/// *is* the feature, the user is sitting in front of it, and no code path waits on it —
/// [`run_action`] already spawns without waiting for the same reason. So this is data nobody
/// reads, recorded honestly rather than pretended into meaning.
///
/// A week rather than `u64::MAX`, for one concrete reason. `PtyHost::wait` computes
/// `started + Duration::from_millis(timeout_ms)`, and `Instant + Duration` **panics** on
/// overflow — `u64::MAX` milliseconds is far past what a 64-bit nanosecond clock can hold.
/// That would be a trap armed for whoever first calls `wait` on one of these.
pub(crate) const SHELL_TIMEOUT_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

/// The shell to run in the terminal dock, as an argv.
///
/// `$SHELL` is what the user chose; `/bin/sh` is what POSIX guarantees exists. The fallback
/// is not paranoia — `$SHELL` routinely names a binary that has been uninstalled since login
/// (a `brew uninstall fish`, a Homebrew prefix that moved), and `PtyHostImpl::spawn` reports
/// that as `ProgramNotFound`. A terminal that refuses to open because of a stale environment
/// variable is worse than one that opens in `sh`.
///
/// `-l` so the shell reads the user's profile, matching the "Open shell" action in
/// `examples/webapp.wtm.toml`. Without it the prompt, the aliases, `nvm`, `direnv` and the
/// `PATH` the rest of this app worked hard to resolve are all missing, and the pane is a
/// shell nobody recognises as theirs.
///
/// No platform `#[cfg]` is needed: `$SHELL` is set by the login machinery on both platforms
/// and `/bin/sh` exists on both. (Spelled without the attribute's argument on purpose —
/// `tests/platform_seams.rs` scans for the literal token, so writing it out in prose counts as
/// an undeclared seam. `platform_plugin` in `lib.rs` phrases its own version the same way.)
///
/// The candidate and the executability test are parameters rather than read in here, because
/// otherwise this is untestable: `std::env::set_var` is `unsafe` in Rust 2024 and this
/// workspace *forbids* `unsafe_code`, so no test can set `$SHELL` at all.
fn login_shell(candidate: Option<String>, executable: impl Fn(&str) -> bool) -> Vec<String> {
    const POSIX_SHELL: &str = "/bin/sh";

    let shell = candidate
        .filter(|value| !value.trim().is_empty())
        .filter(|value| executable(value))
        .unwrap_or_else(|| POSIX_SHELL.to_owned());

    vec![shell, "-l".to_owned()]
}

/// Terminal geometry, with an empty measurement replaced by something usable.
///
/// The dock is closed by default, so a pane can be measured before it has a box and report
/// 0×0. `openpty` accepts that, and then every full-screen program in the session draws into
/// a zero-width window — which looks like a broken terminal rather than a bad measurement.
/// 24×80 is the classic default, and any real measurement replaces it a frame later over
/// [`pty_resize`].
fn usable_geometry(rows: u16, cols: u16) -> (u16, u16) {
    (
        if rows == 0 { 24 } else { rows },
        if cols == 0 { 80 } else { cols },
    )
}

/// Open a long-lived interactive shell in one worktree.
///
/// # Why this is not `run_action`
///
/// [`run_action`] resolves a project-declared `[[action]]` by id, and this has to work in any
/// repository, including one with no `wtm.toml` at all. Generalising would mean either
/// synthesising a fake action or making `action_id` optional — a no-config special case
/// inside the path whose entire contract is "run what the project declared". The deadline
/// differs too. Two divergences is two functions.
///
/// # Every call opens a shell
///
/// This used to return the running session for a worktree that already had one. It no longer
/// does: a worktree can hold several shells, so asking twice gets two, exactly as
/// [`open_agent_session`] does. Re-attaching after a webview reload is [`list_terminals`]' job,
/// and the frontend is what decides whether ⌘J should focus an existing shell or open another —
/// that is a question about panes, which this layer knows nothing about.
///
/// # Why guards are *not* checked here
///
/// Every other spawn site calls `wtm_config::check_forbidden`, and this one deliberately does
/// not. Those three sites all spawn an argv rendered from the project's config, which is what
/// guards constrain; [`open_in`] — the only other user-initiated spawn of a program the config
/// did not name — does not check them either. Decisively, `GuardSpec`'s own doc says guards
/// exist for scripts that "genuinely cannot be run from a GUI: they `exec` a login shell and
/// never return, or they prompt with a read loop that spins forever on EOF stdin", and the
/// reference config's first guard blocks a script *because* it ends in `exec "$SHELL" -l`.
/// Those are the failure modes an interactive terminal fixes. Checking guards here would
/// invert their purpose and produce "your project forbids `bash`, so you cannot open a
/// terminal".
///
/// # An untrusted project is still refused
///
/// [`App::project`] fails for a config awaiting trust, and this command inherits that. A bare
/// shell needs nothing from the config, so exempting it is tempting — but a project is
/// untrusted precisely because nobody has read what it would run, and the trust prompt is one
/// click away.
#[tauri::command]
pub async fn open_terminal(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    rows: u16,
    cols: u16,
) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let worktree = app.worktree(&project, &worktree_id)?;

        // A worktree whose directory was deleted by hand is *prunable*, not absent, and
        // `App::worktree` does not prune — so it is still found here. A shell whose cwd is an
        // unlinked inode is one where `getcwd` fails and every command misbehaves for no
        // visible reason, which is much harder to diagnose than this sentence.
        if !app.files.exists(&worktree.path) {
            return Err(ErrorView::new(
                "exec",
                format!(
                    "`{}` no longer exists on disk — the worktree may need pruning",
                    worktree.path.display()
                ),
            ));
        }

        let argv = login_shell(std::env::var("SHELL").ok(), |path| {
            app.runner.which(path).is_some()
        });

        let (rows, cols) = usable_geometry(rows, cols);
        let sink = crate::pty_bridge::EventSink::new(handle);
        let session = app
            .open_shell(&worktree, &project_id, argv, rows, cols, sink)
            .map_err(|e| ErrorView::new("exec", e.to_string()))?;

        // Return as soon as it is running, exactly as `run_action` does: the dock streams the
        // rest over `pty:output`, and waiting on an interactive shell would block this command
        // until the user typed `exit`.
        Ok(session.as_str().to_owned())
    })
    .await
}

/// Every live dock shell, one row each.
///
/// The re-attach path, and the reason it has to exist in Rust: a webview reload throws away
/// the frontend's pane-to-session map while the shells keep running in process sessions of
/// their own, so without this they are unreachable until the app quits. It does **not**
/// restore a transcript — no output is buffered anywhere but in the pane that received it.
///
/// Deliberately *not* derived from `PtyHost::sessions` directly. That also reports the sessions
/// of running actions and of setup, which carry the same worktree id and must never be adopted
/// as the dock's shell — see `App::shells`.
///
/// Every project's shells, not one project's: the map is global, the dock filters by what is in
/// the current listing, and a project switch then needs no second round-trip.
#[tauri::command]
pub async fn list_terminals(app: AppState<'_>) -> Reply<Vec<TerminalSessionView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        Ok(app
            .live_shells()
            .into_iter()
            .map(|shell| TerminalSessionView {
                session: shell.session,
                worktree: shell.worktree,
                project: shell.project,
            })
            .collect())
    })
    .await
}

/// Kill one dock shell and forget it.
///
/// The dock's Kill control, and the first half of its Restart control — restart is this
/// followed by [`open_terminal`], with no wait in between. See `App::close_shell` for why
/// forgetting the entry is what removes the race.
///
/// Takes the session id rather than the worktree, because a worktree may have several shells and
/// closing the wrong one would kill somebody's dev server. There is no config or git to consult,
/// so no project id is needed either; the id is one this app itself put in the map.
#[tauri::command]
pub async fn close_terminal(app: AppState<'_>, session: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.close_shell(&session);
        Ok(())
    })
    .await
}

// ═══════════════════════════ agent sessions ═══════════════════════════

/// What a new session asks for beyond which agent and where.
///
/// A struct rather than four more parameters, and not only because clippy caps a command at eight:
/// every one of these is optional and provider-specific, so they belong together and the set will
/// grow. `Default` matters — a caller that wants the provider's own choices sends `{}`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The provider's own spelling of an approval or permission mode.
    pub mode: Option<String>,
    /// A conversation to pick up, by the id its provider knows it by.
    pub resume: Option<String>,
    /// Start in the provider's high-speed mode. Only Claude has one.
    pub fast: Option<bool>,
}

/// Every agent this build can drive, and whether this machine can.
///
/// The whole catalogue, including what is not installed, with the reason — the same contract
/// [`list_openers`] keeps, and for the same reason: a greyed row saying which program was looked
/// for doubles as the diagnosis of a GUI-launched app that cannot see the user's `PATH`.
///
/// Nothing is cached, so a CLI installed since the app started shows up without a restart.
///
/// # Why it takes a project
///
/// Because a repository can decline an agent, and this list did not know that — `offers_agent` was
/// enforced at spawn, in `handoff::open_pane`, and in the handoff tool's own `enum`, but the
/// launcher offered every installed agent regardless and failed with a config error once one was
/// clicked. `None` reports the whole catalogue as offered, for the startup call that happens before
/// a project is selected.
#[tauri::command]
pub async fn list_agents(
    app: AppState<'_>,
    project_id: Option<String>,
) -> Reply<Vec<AgentOptionView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        // Resolved once rather than per entry, and silently: a project id that no longer loads is
        // not this command's failure to report, and the honest degradation is the catalogue-wide
        // answer this had before.
        let project = project_id.and_then(|id| app.project(&id).ok());

        Ok(wtm_agent::CATALOGUE
            .iter()
            .map(|entry| {
                let program = entry.provider.program();
                let executable = app.agent_executable(entry);
                let found = executable.is_some();
                let offered = project.as_ref().is_none_or(|p| p.offers_agent(entry.id));
                AgentOptionView {
                    id: entry.id.to_owned(),
                    label: entry.label.to_owned(),
                    blurb: entry.blurb.to_owned(),
                    available: found,
                    offered,
                    // Not installed first, because that is the one the user can act on from here.
                    detail: if !found {
                        Some(if entry.id == wtm_agent::cursor::ID {
                            "no Cursor Agent CLI (`cursor-agent` or `agent`) on wtm's PATH or in Cursor.app"
                                .to_owned()
                        } else {
                            format!("no `{program}` on wtm's PATH")
                        })
                    } else if !offered {
                        Some(format!(
                            "this repository's `wtm.toml` does not offer `{}`",
                            entry.id
                        ))
                    } else {
                        None
                    },
                }
            })
            .collect())
    })
    .await
}

/// Start an agent session in a worktree. Returns the session id to attach to.
///
/// Returns as soon as the CLI is running, exactly as [`open_terminal`] and `run_action` do. The
/// handshake completes asynchronously and announces itself with `agent:ready`; waiting for it here
/// would block this command on a network round trip and present as a frozen window.
///
/// Not idempotent per worktree, which is the deliberate difference from [`open_terminal`]: several
/// sessions in one worktree is the feature, so asking twice starts two.
///
/// `resume` picks up a conversation by the id its provider knows it by — the value
/// [`list_resumable`] returns. Passing it means the CLI reloads its own transcript rather than
/// starting fresh; passing `None` starts a new one.
#[tauri::command]
pub async fn open_agent_session(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    agent_id: String,
    options: SessionOptions,
) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || {
        open_agent_process(
            handle,
            &app,
            &project_id,
            &worktree_id,
            &agent_id,
            options,
            None,
        )
    })
    .await
}

/// Fork a live conversation for one ephemeral side question.
///
/// The provider conversation id stays behind this boundary. A webview cannot accidentally fork a
/// stale saved id or claim another project's conversation; it names the live session it already
/// owns and the app resolves the rest.
#[tauri::command]
pub async fn open_agent_side_session(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    parent_session: String,
    options: SessionOptions,
) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || {
        let source = app.agent_fork_source(&parent_session).ok_or_else(|| {
            ErrorView::new(
                "exec",
                "the parent conversation is still starting, so it cannot be forked yet",
            )
        })?;
        open_agent_process(
            handle,
            &app,
            &source.project,
            &source.worktree,
            &source.provider,
            options,
            Some(source.provider_session),
        )
    })
    .await
}

#[allow(clippy::too_many_arguments)]
fn open_agent_process(
    handle: tauri::AppHandle,
    app: &Arc<App>,
    project_id: &str,
    worktree_id: &str,
    agent_id: &str,
    options: SessionOptions,
    fork: Option<String>,
) -> Reply<String> {
    let project = app.project(project_id)?;
    let worktree = app.worktree(&project, worktree_id)?;

    // Same check `open_terminal` makes, and the same reasoning: a worktree whose directory was
    // deleted by hand is *prunable*, not absent, so it is still found here — and a CLI whose
    // cwd is an unlinked inode misbehaves in ways much harder to diagnose than this sentence.
    if !app.files.exists(&worktree.path) {
        return Err(ErrorView::new(
            "exec",
            format!(
                "`{}` no longer exists on disk — the worktree may need pruning",
                worktree.path.display()
            ),
        ));
    }

    let entry = wtm_agent::entry(agent_id).ok_or_else(|| {
        ErrorView::new(
            "exec",
            format!("`{agent_id}` is not an agent this build of wtm knows how to drive"),
        )
    })?;

    // A repository may refuse an agent. Checked here rather than only by hiding it in the
    // launcher, because the launcher is not the only route in — a resume entry or a handoff can
    // ask for one too, and a refusal that only exists in the UI is not a refusal.
    if !project.offers_agent(agent_id) {
        return Err(ErrorView::new(
            "config",
            format!("this repository's `wtm.toml` does not offer `{agent_id}`"),
        ));
    }

    let spec = project.agent_spec(agent_id);
    let mut req = session_request_for(
        app,
        &project,
        &spec,
        entry,
        &worktree,
        Some(options),
        // Nothing to inherit: this route has a picker, and it already spoke.
        None,
    )?;
    req.fork = fork;
    req.ephemeral = req.fork.is_some();
    if req.ephemeral {
        const SIDE_INSTRUCTIONS: &str = "This is an ephemeral side question. Answer only from the \
            conversation context already available. Do not call tools or change files.";
        req.instructions = Some(match req.instructions {
            Some(existing) => format!("{existing}\n\n{SIDE_INSTRUCTIONS}"),
            None => SIDE_INSTRUCTIONS.to_owned(),
        });
    }

    let sink: Arc<dyn wtm_agent::session::AgentSink> =
        crate::agent_bridge::AgentEventSink::new(handle);
    let session = app
        .open_agent(entry, &req, &worktree, project_id, &sink)
        .map_err(|e| ErrorView::new("exec", e.to_string()))?;

    Ok(session.as_str().to_owned())
}

/// Send one turn to a session.
///
/// A turn submitted before the handshake finishes is queued by the provider rather than refused —
/// the composer is live the moment a pane opens, so that is the ordinary case on a slow start, not
/// an edge case, and dropping it would lose the user's first prompt.
#[tauri::command]
pub async fn send_turn(
    app: AppState<'_>,
    session: String,
    text: String,
    attachments: Vec<wtm_core::model::AgentAttachment>,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.with_agent(&session, |agent| agent.send_turn(&text, &attachments))
            .map_err(|e| ErrorView::new("exec", e.to_string()))
    })
    .await
}

const MAX_AGENT_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

/// Read a file the user picked or dropped and prepare it for both agent transports.
#[tauri::command]
pub async fn prepare_agent_attachment(path: String) -> Reply<wtm_core::model::AgentAttachment> {
    blocking(move || attachment_from_path(std::path::Path::new(&path))).await
}

/// Persist a clipboard file long enough for Codex app-server to consume it by local path.
#[tauri::command]
pub async fn stage_agent_attachment(
    name: String,
    mime: String,
    data_base64: String,
) -> Reply<wtm_core::model::AgentAttachment> {
    blocking(move || {
        let bytes = crate::pty_bridge::base64_decode(&data_base64)
            .ok_or_else(|| ErrorView::new("badInput", "the pasted file was not valid base64"))?;
        if bytes.len() > MAX_AGENT_ATTACHMENT_BYTES {
            return Err(ErrorView::new(
                "badInput",
                "attachments must be 20 MB or smaller",
            ));
        }
        let safe_name = std::path::Path::new(&name)
            .file_name()
            .and_then(|part| part.to_str())
            .filter(|part| !part.is_empty())
            .unwrap_or("pasted-file");
        let directory = std::env::temp_dir().join("wtm-agent-attachments");
        std::fs::create_dir_all(&directory)
            .map_err(|e| ErrorView::new("io", format!("create attachment directory: {e}")))?;
        let path = directory.join(format!("{}-{safe_name}", uuid::Uuid::new_v4()));
        std::fs::write(&path, &bytes)
            .map_err(|e| ErrorView::new("io", format!("stage pasted attachment: {e}")))?;
        Ok(wtm_core::model::AgentAttachment {
            name: safe_name.to_owned(),
            path: path.to_string_lossy().into_owned(),
            mime: if mime.trim().is_empty() {
                mime_for(&path).to_owned()
            } else {
                mime
            },
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            data_base64,
        })
    })
    .await
}

fn attachment_from_path(path: &std::path::Path) -> Reply<wtm_core::model::AgentAttachment> {
    if !path.is_file() {
        return Err(ErrorView::new(
            "badInput",
            format!("`{}` is not a file", path.display()),
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| ErrorView::new("io", format!("read `{}`: {e}", path.display())))?;
    if bytes.len() > MAX_AGENT_ATTACHMENT_BYTES {
        return Err(ErrorView::new(
            "badInput",
            format!(
                "`{}` is larger than the 20 MB attachment limit",
                path.display()
            ),
        ));
    }
    Ok(wtm_core::model::AgentAttachment {
        name: path
            .file_name()
            .and_then(|part| part.to_str())
            .unwrap_or("attachment")
            .to_owned(),
        path: path.to_string_lossy().into_owned(),
        mime: mime_for(path).to_owned(),
        size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        data_base64: crate::pty_bridge::base64_encode(&bytes),
    })
}

fn mime_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "md" | "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        _ => "application/octet-stream",
    }
}

/// Answer an outstanding approval.
///
/// The first answer wins: the provider removes the request when it replies, so a second call for
/// the same id finds nothing and succeeds silently. That is deliberate rather than an oversight —
/// two panes, or a click racing a keystroke, must not both reply and desynchronise the server's
/// view of the turn, and an error here would surface a race the user did not cause as a failure
/// they have to read. The card collapses on `approval_resolved` either way.
#[tauri::command]
pub async fn answer_approval(
    app: AppState<'_>,
    session: String,
    request_id: String,
    answer: wtm_core::model::ApprovalAnswer,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.with_agent(&session, |agent| agent.answer(&request_id, &answer))
            .map_err(|e| ErrorView::new("exec", e.to_string()))
    })
    .await
}

/// Change what a running session uses, without restarting it.
///
/// `None` for either argument means "leave that one alone", so the picker can change one without
/// asserting a stale value for the other.
///
/// Codex applies effort on its next turn and Cursor uses its advertised ACP config option. Claude
/// has no live effort control, so the frontend does not send that argument for Claude and marks the
/// selection for restart instead.
///
/// `fast` is Claude's alone and goes the other way: it *is* live, and the only one of these whose
/// real value comes back from the CLI rather than being assumed applied.
#[tauri::command]
pub async fn configure_session(
    app: AppState<'_>,
    session: String,
    model: Option<String>,
    effort: Option<String>,
    mode: Option<String>,
    fast: Option<bool>,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.with_agent(&session, |agent| {
            agent.reconfigure(model.as_deref(), effort.as_deref(), mode.as_deref(), fast)
        })
        .map_err(|e| ErrorView::new("exec", e.to_string()))?;
        if let Some(effort) = effort.as_deref() {
            app.handoff.update_effort(&session, effort);
        }
        Ok(())
    })
    .await
}

/// Every file in a worktree that is worth offering in the composer's `@` list.
///
/// `git ls-files --cached --others --exclude-standard`: everything tracked, plus everything
/// untracked that is not ignored. That set is the point — a plain directory walk would offer
/// `node_modules` and `target`, which on this repo alone is tens of thousands of paths nobody
/// wants to reference and enough to make a typeahead feel broken.
///
/// It is also why this is `git` rather than a `walkdir` dependency: the ignore rules are already
/// implemented, correctly, by the tool that owns them, and re-implementing `.gitignore` semantics
/// well enough to agree with the repository is not a small job.
///
/// Paths come back relative to the worktree root and are handed to the model that way. Absolute
/// paths would be correct too, but they are long, they bury the part that identifies the file, and
/// they leak the user's home directory into every prompt.
#[tauri::command]
pub async fn list_worktree_files(app: AppState<'_>, worktree_id: String) -> Reply<Vec<String>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let inv = wtm_core::ports::exec::Invocation::new(
            vec![
                "git".to_owned(),
                "ls-files".to_owned(),
                "--cached".to_owned(),
                "--others".to_owned(),
                "--exclude-standard".to_owned(),
                // NUL-separated, because a filename may legally contain a newline and splitting on
                // one would turn a single odd path into two paths that do not exist.
                "-z".to_owned(),
            ],
            std::path::PathBuf::from(&worktree_id),
            FILES_TIMEOUT_MS,
        );

        // `run_allow_failure`: a worktree that has been removed from disk, or a directory that is
        // not a repository, is an empty list rather than a banner. This backs a convenience, and
        // the composer still works without it — the user types the path.
        let output = app
            .runner
            .run_allow_failure(&inv, &wtm_core::ports::exec::CancelToken::new())
            .map_err(|e| ErrorView::new("exec", e.to_string()))?;
        if !output.is_success() {
            tracing::debug!(stderr = %output.stderr, "could not list worktree files");
            return Ok(Vec::new());
        }

        Ok(output
            .stdout
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect())
    })
    .await
}

/// How long to wait for `git ls-files`. Generous for a cold cache on a large repository, short
/// enough that a hung git does not leave the `@` list spinning.
const FILES_TIMEOUT_MS: u64 = 5_000;

/// Ask a session to stop the turn it is running.
#[tauri::command]
pub async fn interrupt_turn(app: AppState<'_>, session: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.with_agent(&session, wtm_agent::AgentSession::interrupt)
            .map_err(|e| ErrorView::new("exec", e.to_string()))
    })
    .await
}

/// Conversations that can be picked up again in a worktree, newest first.
///
/// Excludes anything already running: an entry for a live session would offer to resume a
/// conversation that is on screen two inches away, and accepting would hand the CLI two clients for
/// one thread.
#[tauri::command]
pub async fn list_resumable(app: AppState<'_>, worktree_id: String) -> Reply<Vec<ResumableView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        Ok(app
            .resumable(&worktree_id)
            .into_iter()
            .map(|record| ResumableView {
                provider: record.provider,
                provider_session: record.provider_session,
                title: record.title,
                model: record.model,
                effort: record.effort,
                updated: record.updated,
            })
            .collect())
    })
    .await
}

/// Forget a conversation, so it stops being offered.
#[tauri::command]
pub async fn forget_session(
    app: AppState<'_>,
    provider: String,
    provider_session: String,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.forget_session(&provider, &provider_session);
        Ok(())
    })
    .await
}

/// Which agent sessions are live. Every project's, not one's.
///
/// The re-attach path after a webview reload, and it exists for the same reason
/// [`list_terminals`] does: a reload throws away the frontend's pane-to-session map while the CLIs
/// keep running, so without this they are unreachable until the app quits. The transcript that goes
/// with each one comes from [`agent_replay`].
#[tauri::command]
pub async fn list_agent_sessions(app: AppState<'_>) -> Reply<Vec<AgentSessionView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        Ok(app
            .live_agents()
            .into_iter()
            .filter(|facts| !facts.ephemeral)
            .map(|facts| AgentSessionView {
                session: facts.session,
                worktree: facts.worktree,
                project: facts.project,
                provider: facts.provider,
                provider_session: facts.provider_session,
            })
            .collect())
    })
    .await
}

/// Everything a live session has already said, so a re-attached pane is not blank.
///
/// The other half of [`list_agent_sessions`]. Before this existed a reload adopted a running CLI
/// into an empty pane, which is the "my sessions changed when I refreshed the app" report: the
/// session was fine and only the window had forgotten it.
///
/// Returns an empty list for a session that has gone, rather than an error. A pane asking about a
/// session that just exited is an ordinary race, not a fault, and the `agent:exit` event is what
/// tells it the truth.
#[tauri::command]
pub async fn agent_replay(app: AppState<'_>, session: String) -> Reply<Vec<crate::app::SeqEvent>> {
    let app = Arc::clone(&app);
    blocking(move || Ok(app.agent_replay(&session))).await
}

/// End a session and forget it.
///
/// Takes only the session id, like [`close_terminal`]: there is no config or git to consult, and
/// the id is a key this app itself minted.
///
/// Closing a pane is **not** the same as forgetting the conversation, and this deliberately keeps
/// the resume entry. Closing a pane is how you tidy the screen; the CLI still has the transcript,
/// and the commonest thing anyone wants next is it back. `forget_session` is the explicit discard.
#[tauri::command]
pub async fn close_agent_session(app: AppState<'_>, session: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.close_agent(&session);
        Ok(())
    })
    .await
}

/// How long to wait for a provider to answer a capability query, in milliseconds.
///
/// The app server starts its configured MCP servers before it will answer, so this is not
/// instantaneous — six were observed starting on one machine. Six seconds is generous against that
/// and still short enough that a picker which cannot be filled says so rather than hanging.
///
/// Measured with `Clock::monotonic_ms` rather than `Instant`: `Instant::now` is banned outside the
/// clock adapter so time enters through the port, and the wall clock is the wrong one here because
/// it can jump backwards.
const CAPABILITY_TIMEOUT_MS: u64 = 6_000;

/// Cursor.app's managed CLI cold-starts through its agent-worker extension before ACP answers.
///
/// On the machine that exposed the bundled-path bug, `initialize` alone took just under seven
/// seconds. Cursor then has two more local handshake round trips, so sharing Codex's six-second
/// deadline made a correctly discovered CLI look incompatible immediately afterwards.
const CURSOR_CAPABILITY_TIMEOUT_MS: u64 = 20_000;

/// What an agent can do on this machine: its models, and the effort ladder each one supports.
///
/// Two providers, two honest answers. Claude Code advertises nothing — no `model/list`, and its
/// efforts are a fixed five — so its answer is a table compiled into this build, and
/// `modelsAreLive` is false so the UI can say "as of this wtm build" rather than presenting it as
/// the CLI's word. Codex advertises everything, so this spawns a throwaway app server, asks, and
/// kills it.
///
/// A short-lived process for a picker is worth it: the ladders genuinely differ between models of
/// the same provider — `gpt-5.6-sol` reaches `ultra` where `gpt-5.5` stops at `xhigh` — so a
/// hardcoded list would offer rungs the selected model rejects.
#[tauri::command]
pub async fn agent_capability(app: AppState<'_>, agent_id: String) -> Reply<CapabilityView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let entry = wtm_agent::entry(&agent_id).ok_or_else(|| {
            ErrorView::new(
                "exec",
                format!("`{agent_id}` is not an agent this build of wtm knows how to drive"),
            )
        })?;

        let mut capability = match agent_id.as_str() {
            wtm_agent::claude::ID => wtm_agent::claude_capability(),
            wtm_agent::codex::ID => {
                let mut probed = probe_codex(&app).map_err(|e| ErrorView::new("exec", e))?;
                // Compiled in rather than queried, and the query is only for the part that moves.
                // `permissionProfile/list` returns user profiles, not the protocol vocabulary.
                probed.modes = wtm_agent::codex_modes();
                probed
            }
            wtm_agent::cursor::ID => probe_cursor(&app).map_err(|e| ErrorView::new("exec", e))?,
            _ => unreachable!("the catalogue lookup above accepted only known providers"),
        };

        // One call for both providers, deliberately: this is the seed the picker shows, and the
        // spawn path reaches the same preference by its own route through `session_request_for`. If
        // the two disagreed, a fresh pane would say one rung and run at another.
        wtm_agent::prefer_effort(&mut capability);

        let _ = entry;
        Ok(CapabilityView::from(capability))
    })
    .await
}

/// Drive a throwaway app server for its model catalogue.
///
/// Synchronous and blocking, which is why it is only reachable from inside `blocking()`. The
/// collected lines are scanned rather than assumed to arrive in order: the server interleaves
/// notifications — MCP startup statuses, remote-control status — with its replies.
/// [`probe_codex`], reachable from an integration test.
///
/// The probe itself stays private: it is an implementation detail of one command, and the only reason
/// to expose it is that the property worth testing — *does a real app server answer* — cannot be
/// reached through `#[tauri::command]` without a running Tauri runtime.
///
/// # Errors
///
/// If the CLI cannot be spawned, or does not answer within the deadline.
pub fn probe_codex_for_test(app: &Arc<App>) -> Result<wtm_core::model::AgentCapability, String> {
    probe_codex(app)
}

fn probe_codex(app: &Arc<App>) -> Result<wtm_core::model::AgentCapability, String> {
    use wtm_core::ports::pipe::{PipeHost, PipeSink};

    #[derive(Default)]
    struct Collect {
        lines: parking_lot::Mutex<Vec<String>>,
    }
    impl PipeSink for Collect {
        fn on_line(&self, _s: &wtm_core::model::SessionId, line: &str) {
            self.lines.lock().push(line.to_owned());
        }
        fn on_stderr(&self, _s: &wtm_core::model::SessionId, line: &str) {
            tracing::debug!(line, "codex capability probe stderr");
        }
        fn on_exit(&self, _s: &wtm_core::model::SessionId, _o: &wtm_core::model::ExitOutcome) {}
    }

    let entry = wtm_agent::entry(wtm_agent::codex::ID).ok_or("codex is not in the catalogue")?;
    let argv = entry.provider.argv(&wtm_agent::SessionRequest::default());
    let inv = wtm_core::ports::exec::Invocation::new(argv, std::env::temp_dir(), SHELL_TIMEOUT_MS);

    let sink = Arc::new(Collect::default());
    let spawned = app
        .pipe
        .spawn(&inv, None, Arc::clone(&sink) as Arc<dyn PipeSink>)
        .map_err(|e| e.to_string())?;

    for frame in wtm_agent::codex::model_list_frames() {
        app.pipe
            .write_line(&spawned.session, &frame)
            .map_err(|e| e.to_string())?;
    }

    let deadline = app.clock.monotonic_ms() + CAPABILITY_TIMEOUT_MS;
    let mut models = Vec::new();
    loop {
        let found = sink.lines.lock().iter().find_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            (value.get("id").and_then(serde_json::Value::as_i64)
                == Some(wtm_agent::codex::MODEL_LIST_ID))
            .then(|| wtm_agent::codex::parse_models(&value))
        });
        if let Some(parsed) = found {
            models = parsed;
            break;
        }
        if app.clock.monotonic_ms() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Killed either way. A probe that leaked an app server per picker open would be worse than a
    // picker that comes back empty.
    let _ = app.pipe.kill(&spawned.session);

    if models.is_empty() {
        return Err(
            "codex did not answer `model/list` — check that `codex` is logged in".to_owned(),
        );
    }
    Ok(wtm_core::model::AgentCapability {
        models,
        // Filled in by the caller, which is the only place that knows the modes are a compiled
        // table while the models beside them are live.
        modes: Vec::new(),
        models_are_live: true,
        supports_fast: false,
    })
}

/// Ask Cursor's ACP session configuration for its live model, thought-level and mode selectors.
///
/// Exposed only so an integration test can prove that machine discovery and the real ACP
/// handshake agree on the executable. The Tauri command reaches this through `agent_capability`.
pub fn probe_cursor_for_test(app: &Arc<App>) -> Result<wtm_core::model::AgentCapability, String> {
    probe_cursor(app)
}

fn probe_cursor(app: &Arc<App>) -> Result<wtm_core::model::AgentCapability, String> {
    use wtm_core::ports::pipe::{PipeHost, PipeSink};

    #[derive(Default)]
    struct Collect {
        lines: parking_lot::Mutex<Vec<String>>,
    }
    impl PipeSink for Collect {
        fn on_line(&self, _session: &wtm_core::model::SessionId, line: &str) {
            self.lines.lock().push(line.to_owned());
        }
        fn on_stderr(&self, _session: &wtm_core::model::SessionId, line: &str) {
            tracing::debug!(line, "cursor capability probe stderr");
        }
        fn on_exit(
            &self,
            _session: &wtm_core::model::SessionId,
            _outcome: &wtm_core::model::ExitOutcome,
        ) {
        }
    }

    let entry = wtm_agent::entry(wtm_agent::cursor::ID).ok_or("cursor is not in the catalogue")?;
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    let request = wtm_agent::SessionRequest {
        cwd: cwd.clone(),
        executable: app
            .agent_executable(entry)
            .map(|path| path.to_string_lossy().into_owned()),
        ..wtm_agent::SessionRequest::default()
    };
    let inv = wtm_core::ports::exec::Invocation::new(
        entry.provider.argv(&request),
        std::env::temp_dir(),
        SHELL_TIMEOUT_MS,
    );
    let sink = Arc::new(Collect::default());
    let spawned = app
        .pipe
        .spawn(&inv, None, Arc::clone(&sink) as Arc<dyn PipeSink>)
        .map_err(|error| error.to_string())?;
    let deadline = app.clock.monotonic_ms() + CURSOR_CAPABILITY_TIMEOUT_MS;

    let wait_for = |id: i64| -> Result<serde_json::Value, String> {
        loop {
            let found = sink.lines.lock().iter().find_map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).ok()?;
                (value.get("id").and_then(serde_json::Value::as_i64) == Some(id)).then_some(value)
            });
            if let Some(reply) = found {
                if let Some(error) = reply.get("error") {
                    return Err(error
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Cursor rejected the capability probe")
                        .to_owned());
                }
                return Ok(reply);
            }
            if app.clock.monotonic_ms() >= deadline {
                return Err("Cursor did not answer its ACP capability probe".to_owned());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    };

    let outcome = (|| {
        app.pipe
            .write_line(&spawned.session, &wtm_agent::cursor::initialize_frame(1))
            .map_err(|error| error.to_string())?;
        let initialized = wait_for(1)?;
        app.pipe
            .write_line(&spawned.session, &wtm_agent::cursor::authenticate_frame(2))
            .map_err(|error| error.to_string())?;
        wait_for(2)?;
        app.pipe
            .write_line(
                &spawned.session,
                &wtm_agent::cursor::new_session_frame(3, &cwd),
            )
            .map_err(|error| error.to_string())?;
        let reply = wait_for(3)?;
        let capability = wtm_agent::cursor::parse_capability(&reply);
        if capability.models.is_empty() {
            return Err(
                "Cursor's ACP session advertised no models — check that the Cursor Agent CLI is logged in"
                    .to_owned(),
            );
        }

        // The probe's only purpose was its selectors. Delete the empty conversation rather than
        // leaving one in Cursor's session list every time the picker opens.
        if initialized
            .pointer("/result/agentCapabilities/sessionCapabilities/delete")
            .is_some()
            && let Some(session) = reply
                .pointer("/result/sessionId")
                .and_then(serde_json::Value::as_str)
        {
            let frame = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "session/delete",
                "params": { "sessionId": session },
            })
            .to_string();
            let _ = app.pipe.write_line(&spawned.session, &frame);
        }
        Ok(capability)
    })();

    let _ = app.pipe.kill(&spawned.session);
    outcome
}

/// Everything a session needs to start, resolved from config and the caller's choices.
///
/// Extracted because there are now **two** routes to a session and they must agree. A handoff opens
/// one from the socket thread, and if it built its request by hand then a repository's `extra_args`,
/// its `[agent.*.mcp.*]` servers, or its mode default would apply to a pane the user opened and not
/// to one an agent opened — a difference nothing in the UI would explain.
///
/// `options` is `None` for a handoff, which has no picker to express a preference with. `inherited`
/// is how that route still gets an effort: the calling session's own, already translated into this
/// target's vocabulary by [`wtm_agent::carried_effort`].
///
/// # Errors
///
/// If a template in an MCP server's argv fails to render, or a rendered argv is forbidden by a guard.
pub fn session_request_for(
    app: &Arc<App>,
    project: &wtm_core::model::Project,
    spec: &wtm_core::model::AgentSpec,
    entry: &'static wtm_agent::ProviderEntry,
    worktree: &wtm_core::model::Worktree,
    options: Option<SessionOptions>,
    inherited: Option<&str>,
) -> Result<wtm_agent::SessionRequest, ErrorView> {
    let SessionOptions {
        model,
        effort,
        mode,
        resume,
        fast,
    } = options.unwrap_or_default();

    /*
     * Four layers, and the order between the middle two is the only interesting decision.
     *
     * 1. The picker, or a resume record — a value the user chose *for this pane*.
     * 2. `[agent.<id>] effort` in the repository's `wtm.toml`.
     * 3. The effort a handoff caller was running at.
     * 4. `ProviderEntry::default_effort`, which is `PREFERRED_EFFORT`.
     *
     * Config outranks the inherited value because the repo line was typed by a human *for this
     * agent*, where the inherited one was typed for a different one — a repository that pins Codex
     * to `low` because its test suite is slow means that whoever opened the pane. It stays below the
     * picker for the reason the layers below already state: a value chosen for this pane wins.
     */
    let effort = effort
        .or_else(|| spec.effort.clone())
        .or_else(|| inherited.map(str::to_owned))
        .or_else(|| entry.default_effort.map(str::to_owned));

    // The caller's choice wins, then the repository's, then the provider's own. Three
    // layers rather than two because a picker change must not be overridden by config, and
    // a repo's default must not be overridden by a compiled one.
    let model = model.or_else(|| spec.model.clone());

    // A model can imply a mode — see `AgentModel::implied_mode`. Between the explicit layers
    // and the compiled default, so a picker or a repo that says otherwise wins: the coupling
    // is an assist, not a lock. Skipped on resume, where the conversation may already be past
    // its plan and re-entering plan mode would rewrite a state the CLI owns.
    let implied = if resume.is_none() {
        model
            .as_deref()
            .and_then(|m| wtm_agent::implied_mode(entry.id, m))
    } else {
        None
    };

    Ok(wtm_agent::SessionRequest {
        cwd: worktree.path.to_string_lossy().into_owned(),
        // Machine discovery belongs to `App::open_agent`, immediately before spawn, so a CLI
        // installed while this picker was open is usable without rebuilding this request.
        executable: None,
        model,
        // Ask before running anything, unless something asked for otherwise. A worktree is
        // disposable and git is the undo, so a permissive default is defensible — but it is a
        // decision the user should make deliberately rather than discover, and the safe
        // direction is the one where the first surprising command is a card rather than a
        // `git status` you cannot explain. The spelling is the provider's own; see
        // `ProviderEntry::default_mode`.
        mode: mode
            .or_else(|| spec.mode.clone())
            .or(implied)
            .or_else(|| entry.default_mode.map(str::to_owned)),
        resume,
        fork: None,
        ephemeral: false,
        extra_args: spec.extra_args.clone(),
        // The resolved effort, not the caller's — so a session's handoff token offers the rung it is
        // actually running at. See `handoff::Caller::effort`.
        mcp: mcp_servers_for(app, project, spec, worktree, entry.id, effort.as_deref())?,
        instructions: handoff_instructions(project, entry.id),
        effort,
        // Two layers rather than the model's three: there is no compiled default to fall back to,
        // because a mode that spends usage credits faster is not one this build gets to opt anybody
        // into. Absent means off, and only the user or their repository can say otherwise.
        fast: fast.or(spec.fast),
    })
}

/// What to tell a session about the handoff tool, or `None` when there is nobody to hand to.
///
/// # Why this is needed at all, when the tool already describes itself
///
/// Because the tool's description is not the only thing competing for "let Codex review this". A
/// user's global skills and slash commands are in the same context, and one of them is very likely
/// to be *named* after an agent — a `codex` skill wrapping the `codex` CLI is a popular thing to
/// have, and its trigger phrases are the obvious ones: "codex review", "second opinion". Against a
/// name that direct, a tool called `ask_agent` loses, and the observed failure is exactly that: a
/// handoff request answered by shelling out to a CLI in a subprocess nobody can see.
///
/// That is not a bug in the skill. Both are reasonable readings of the request, and the CLI has no
/// way to prefer one, because the deciding fact is about the *environment*: this session is a pane in
/// a window somebody is watching, so an agent started any other way is invisible to them. Nothing in
/// the session can know that unless wtm says it.
///
/// # Why it names the alternatives explicitly
///
/// "Prefer the tool" is not actionable if the model does not realise it is choosing. Naming the two
/// routes it would otherwise take — a skill, or a CLI through the shell — is what turns this from a
/// preference into a tie-break it can apply.
///
/// `None` rather than a paragraph nobody can act on when the repository offers no other agent: a
/// session told to hand off, with nothing to hand off to, would reach for the tool and be refused.
///
/// # Why it describes the lifecycle and not just the choice
///
/// The first version answered "which tool" and stopped, on the assumption that a delegation is a
/// function call. It is not: a child keeps its process and its conversation after it answers, which
/// is deliberate — the user asked to *watch* it, and closing the pane the moment the caller had its
/// answer would destroy the transcript they wanted. The cost of that decision is that somebody has
/// to clean up, and a session that thinks it made a function call never will. Twenty children from
/// one `spawn_agents` then sit there holding twenty CLIs.
///
/// So the paragraph names the three facts a model cannot infer from a tool schema — that children
/// outlive the call, that they share this one checkout, and that `close_agents` exists — and it
/// names them here rather than only in the tool descriptions because a description is read when a
/// tool is being *chosen*, and the obligation lands after it has been used.
///
/// The read-only steer is repeated from `spawn_agents`' own description on purpose. Two places
/// saying the same thing is the lesser problem; the greater one is a swarm of writers in a single
/// worktree, which is a class of conflict no amount of retrying resolves.
fn handoff_instructions(project: &wtm_core::model::Project, provider: &str) -> Option<String> {
    let others: Vec<&str> = wtm_agent::CATALOGUE
        .iter()
        .filter(|entry| entry.id != provider && project.offers_agent(entry.id))
        .map(|entry| entry.label)
        .collect();
    if others.is_empty() {
        return None;
    }

    let roster = others.join(" and ");
    Some(format!(
        "You are running inside Worktree Manager (wtm). This session is a live pane in a window the \
         user is watching, alongside the other panes for this worktree.\n\n\
         {roster} {} available here through the `mcp__wtm__ask_agent` and \
         `mcp__wtm__spawn_agents` tools. Use `ask_agent` for one child and `spawn_agents` when the \
         user explicitly asks for several independent reviewers or delegated tasks. When \
         the user asks you to involve another agent — \"let Codex review this\", \"pass this to \
         Codex\", \"ask Claude what it thinks\", \"get a second opinion\", \"have the other model \
         check this\" — use that tool.\n\n\
         Prefer it over every other way of reaching another agent: over running that agent's CLI \
         yourself with Bash, and over any skill, slash command, or wrapper you have that does. Those \
         all work, but they run the other agent inside a subprocess that is invisible to the user. \
         The tool opens it as a real pane beside this one, so they can watch it think, answer its \
         approval prompts, and read its transcript afterwards — which is the reason they asked you \
         here rather than in a terminal.\n\n\
         The tool blocks until the other agent finishes, and it does not share your conversation, so \
         put everything it needs in the prompt rather than referring to what is above.\n\n\
         What you are creating, and what you owe afterwards:\n\
         - A child opens in **this** worktree and appears in the Agents rail above the panes, \
         grouped as one run. It does not take a pane of its own until the user clicks it, so the \
         rail and its All agents list are how they reach it.\n\
         - A child keeps its process and its conversation **after it answers you**. It is an \
         ordinary interactive session the user can carry on with, not a function call that returns \
         and disappears.\n\
         - So close them: call `mcp__wtm__close_agents` once you have read a run's results and are \
         done with it. It takes no arguments, closes only the children you started, and leaves any \
         that are still working. Do not call it if the user is reading a child's transcript or has \
         started talking to one.\n\
         - All the children share this worktree and this checkout. Concurrent writers conflict, so \
         give a parallel run read-only work — review, analysis, search — and do the editing \
         yourself, or run writers one at a time.\n\
         - They inherit your effort level but never fast mode, and their model and approval mode \
         come from this repository unless you name them.\n\
         - `spawn_agents` runs its tasks in waves of `concurrency` (4 by default) and each child \
         has ten minutes of its own, so a long run can block you for a while. Ask for the number \
         of agents the user asked for rather than rounding up.",
        if others.len() == 1 { "is" } else { "are" }
    ))
}

/// The MCP servers a session is handed: wtm's own bridge, plus whatever the repository declared.
///
/// # Why this is rendered here and serialized elsewhere
///
/// Each server's argv is a template, like every other argv in `wtm.toml`, so `{{ repo.root }}` works
/// in one, and each is checked against `[[guards.forbid]]` **after** rendering — for exactly the
/// reason the setup command is: an argv is only fully known once its templates have run, so a guard
/// checked any earlier can be evaded by a template. Rendering and guarding are this crate's job
/// because it owns the template engine.
///
/// *Serializing* is not, and used to be. This returned pre-baked `--mcp-config` JSON, which meant
/// Codex — which has no such flag — silently received nothing. Handing over a structured map lets
/// each provider spell it its own way. See `wtm_agent::SessionRequest::mcp`.
fn mcp_servers_for(
    app: &Arc<App>,
    project: &wtm_core::model::Project,
    spec: &wtm_core::model::AgentSpec,
    worktree: &wtm_core::model::Worktree,
    provider: &str,
    effort: Option<&str>,
) -> Result<BTreeMap<String, wtm_agent::McpServer>, ErrorView> {
    let mut servers = BTreeMap::new();

    if let Some(bridge) = handoff_server(app, project, worktree, provider, effort) {
        servers.insert(handoff::SERVER_NAME.to_owned(), bridge);
    }

    for (name, server) in &spec.mcp {
        // A repository cannot shadow the bridge, and this is checked before anything else in the loop
        // so the message is about the name rather than about whatever the argv happened to do. Without
        // it, `[agent.claude.mcp.wtm]` would `insert` over the handoff tool — a name the model already
        // trusts, pointed at an argv of the repository's choosing. It is the one key in this map that
        // is wtm's rather than the repository's, so it is the one that must not be overridable.
        if name == handoff::SERVER_NAME {
            return Err(ErrorView::new(
                "config",
                format!(
                    "`{name}` is reserved for wtm's own handoff tool — rename `[agent.*.mcp.{name}]`"
                ),
            ));
        }

        // Rendered through the same engine and the same base context every other argv in this file
        // goes through, so `{{ repo.root }}` works in an MCP server's arguments too.
        let ctx = display::base_context(project, app.os_tokens());
        let key = format!("agent.mcp.{name}.command");
        let argv = server
            .argv()
            .iter()
            .map(|part| app.engine.render(&key, part, &ctx))
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| ErrorView::new("render", e.to_string()))?;

        // Defence in depth, on the *rendered* argv — the same reason `field_options` and `open_in`
        // do it: guards were checked when the config loaded, but an argv is only fully known once
        // its templates have run, so a guard checked any earlier can be evaded by a template. This
        // is also what lets a repository that may not send its source to a model forbid the server
        // that would, with the guard's own `reason` as the message.
        wtm_config::check_forbidden(project, &argv)?;

        let (program, arguments) = argv.split_first().ok_or_else(|| {
            ErrorView::new(
                "config",
                format!("`[agent.*.mcp.{name}]` has an empty `command`"),
            )
        })?;

        servers.insert(
            name.clone(),
            wtm_agent::McpServer {
                command: program.clone(),
                args: arguments.to_vec(),
                env: server.env.clone(),
            },
        );
    }

    Ok(servers)
}

/// wtm's own bridge, as an MCP server this session can call.
///
/// `None` when the socket path cannot be resolved — the same degradation `bridge::listen` takes, and
/// for the same reason: a session that cannot hand off is worth strictly more than no session.
///
/// Every session gets one, and that default is deliberate. The blast radius is not new: the target
/// comes from the compiled catalogue rather than from config, it runs in the *caller's own* worktree,
/// it is refused unless the repository offers it, and its own approval mode applies exactly as it
/// would to a hand-opened pane. An agent that can already run `bash` in this directory is not
/// meaningfully further constrained by being unable to open a sibling pane — and a handoff is visible
/// on screen, which a subprocess is not.
fn handoff_server(
    app: &Arc<App>,
    project: &wtm_core::model::Project,
    worktree: &wtm_core::model::Worktree,
    provider: &str,
    effort: Option<&str>,
) -> Option<wtm_agent::McpServer> {
    let socket = crate::bridge::socket_path().ok()?;
    // `current_exe` rather than a bundled sidecar. See `main.rs`.
    let program = std::env::current_exe().ok()?;

    // Only the agents this repository actually offers, so the tool's `enum` cannot suggest one that
    // would be refused the moment it was chosen.
    let roster = wtm_agent::CATALOGUE
        .iter()
        .filter(|entry| project.offers_agent(entry.id))
        .map(|entry| format!("{}:{}", entry.id, entry.label))
        .collect::<Vec<_>>()
        .join(",");

    let token = app.handoff.issue(handoff::Caller {
        project: project.id.as_str().to_owned(),
        worktree: worktree.id.as_str().to_owned(),
        provider: provider.to_owned(),
        effort: effort.map(str::to_owned),
        session: None,
    });

    let mut env = BTreeMap::new();
    env.insert(handoff::TOKEN_ENV.to_owned(), token);
    env.insert(
        handoff::SOCKET_ENV.to_owned(),
        socket.to_string_lossy().into_owned(),
    );
    env.insert(handoff::AGENTS_ENV.to_owned(), roster);

    Some(wtm_agent::McpServer {
        command: program.to_string_lossy().into_owned(),
        args: vec![crate::bridge::ARGV_FLAG.to_owned()],
        env,
    })
}

// ═══════════════════════════ plans and background work ═══════════════════════════

/// Store a plan, so it outlives the session that wrote it.
///
/// Called when a plan approval is allowed, which is the moment a plan stops moving — before that it is
/// a live step list and a progress widget, and storing every revision would fill the list with the
/// same plan a dozen times with one line different.
///
/// **Nothing is written into the worktree.** `~/.config/wtm/plans/<project>/` instead, because wtm
/// writes nothing into a repository and that property is worth more than the convenience of a plan
/// sitting beside the code — a file in `git status` the user did not create is how a tool becomes
/// untrustworthy.
#[tauri::command]
pub async fn save_brief(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    provider: String,
    markdown: String,
    model: Option<String>,
    provider_session: Option<String>,
    provider_path: Option<String>,
) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || {
        let meta = wtm_config::BriefMeta {
            title: wtm_config::briefs::title_of(&markdown),
            provider,
            worktree: worktree_id,
            model,
            provider_session,
            provider_path,
            created: app.clock.now_iso(),
            extra: std::collections::BTreeMap::new(),
        };
        app.save_brief(&project_id, &meta, &markdown)
            .map_err(|e| ErrorView::new("config", e.to_string()))
    })
    .await
}

/// Every stored plan for a worktree, newest first.
#[tauri::command]
pub async fn list_briefs(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
) -> Reply<Vec<BriefView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        Ok(wtm_config::briefs::list(&app.brief_dir(&project_id))
            .into_iter()
            .filter(|brief| brief.meta.worktree == worktree_id)
            .map(|brief| BriefView {
                id: brief.id,
                title: brief.meta.title,
                provider: brief.meta.provider,
                created: brief.meta.created,
                markdown: brief.markdown,
            })
            .collect())
    })
    .await
}

/// Forget a plan.
#[tauri::command]
pub async fn remove_brief(app: AppState<'_>, project_id: String, id: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        wtm_config::briefs::remove(&app.brief_dir(&project_id), &id);
        Ok(())
    })
    .await
}

/// How long to allow for the background-task roster.
///
/// A local read of a state directory, so this is generous rather than tuned. Short enough that a chip
/// which cannot be filled says nothing instead of hanging the refresh it rides on.
const AGENTS_TIMEOUT_MS: u64 = 4_000;

/// Background agents running in a worktree.
///
/// `claude agents --json --all --cwd <path>` — verified to filter exactly by directory, and to need no
/// TTY. Claude Code only: Codex has no equivalent roster, and its long-running work is another live
/// thread that already shows as a pane.
///
/// # Why this is a command and not a subscription
///
/// **There is no event when a background task finishes.** So this is read on demand and on window
/// focus, the same triggers `refreshWorktrees` uses — polling is banned, and a chip that reads "3
/// running" for a few seconds after one finished is worth saying in the copy rather than hiding
/// behind a timer.
#[tauri::command]
pub async fn list_background_tasks(
    app: AppState<'_>,
    worktree_id: String,
) -> Reply<Vec<BackgroundTaskView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        // Absent CLI is an empty list, not an error: this is an auxiliary read, and a banner about a
        // missing `claude` on a machine that never had one would be noise.
        if app.runner.which("claude").is_none() {
            return Ok(Vec::new());
        }

        let inv = wtm_core::ports::exec::Invocation::new(
            vec![
                "claude".to_owned(),
                "agents".to_owned(),
                "--json".to_owned(),
                "--all".to_owned(),
                "--cwd".to_owned(),
                worktree_id,
            ],
            std::env::temp_dir(),
            AGENTS_TIMEOUT_MS,
        );

        // `run_allow_failure`, because a non-zero exit here means "could not list" rather than
        // "something is wrong with wtm" — an old CLI without the flag, or one not logged in.
        let output = app
            .runner
            .run_allow_failure(&inv, &wtm_core::ports::exec::CancelToken::new())
            .map_err(|e| ErrorView::new("exec", e.to_string()))?;
        if !output.is_success() {
            tracing::debug!(stderr = %output.stderr, "could not list background agents");
            return Ok(Vec::new());
        }

        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&output.stdout).unwrap_or_default();
        Ok(parsed
            .iter()
            // `kind == "background"`, stated rather than implied. The roster also carries
            // *interactive* sessions — including whichever one is driving wtm itself — and those have
            // a `pid` where a background one has an `id`, so an earlier version filtered them out by
            // accident when its `id` lookup failed. An accident is not a filter: the day the CLI gives
            // interactive entries an `id`, this would have started listing the user's own terminal
            // sessions as background work.
            .filter(|task| {
                task.get("kind").and_then(serde_json::Value::as_str) == Some("background")
            })
            .filter_map(|task| {
                let text = |key: &str| task.get(key).and_then(serde_json::Value::as_str);
                Some(BackgroundTaskView {
                    id: text("id").or_else(|| text("sessionId"))?.to_owned(),
                    // `(backgrounded)` is what the CLI calls an unnamed one, which is not a name.
                    name: text("name")
                        .filter(|n| !n.starts_with('('))
                        .unwrap_or("Untitled")
                        .to_owned(),
                    state: text("state").unwrap_or("running").to_owned(),
                    session: text("sessionId").map(str::to_owned),
                })
            })
            .collect())
    })
    .await
}

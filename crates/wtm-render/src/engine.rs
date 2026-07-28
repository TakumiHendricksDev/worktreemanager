//! The `TemplateEngine` implementation.
//!
//! # Sandboxed by construction
//!
//! `minijinja` is built with `default-features = false`, so there is no template
//! loader: `{% include %}` and `{% extends %}` cannot resolve anything. Combined with
//! the fixed filter set in [`crate::filters`], a `wtm.toml` template can transform
//! the values it was given and nothing else. That matters because these templates
//! come from a file inside a repository.
//!
//! # Validation is the point
//!
//! [`Engine::validate`] is what makes a mistyped token a *load-time* error with a
//! file and a line, instead of an empty render at create time. The failure being
//! prevented is concrete: `naming.branch` referring to `worktree.path` — which cannot
//! exist yet, because the branch name is computed in order to decide the path —
//! renders to nothing and yields a branch called `experiment/ACME-0000-`. Nothing
//! crashes, so only a check catches it.

use std::collections::BTreeSet;

use minijinja::{Environment, UndefinedBehavior, Value};

use wtm_core::error::RenderError;
use wtm_core::model::{TokenScope, TokenSet, namespace_of};
use wtm_core::ports::template::{Context, TemplateEngine};

use crate::context::NestedContext;
use crate::filters;

/// minijinja-backed template engine.
pub struct Engine {
    env: Environment<'static>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    #[must_use]
    pub fn new() -> Self {
        let mut env = Environment::new();

        // Missing tokens render as empty rather than raising. Deliberate: a lookup
        // that failed under its `warn` policy leaves its tokens absent, and the
        // config's `default_if_empty` chain is how that is meant to be handled.
        // Correctness comes from `validate`, not from runtime explosions.
        env.set_undefined_behavior(UndefinedBehavior::Chainable);

        // Every filter takes `Option<String>` and treats absence as the empty string.
        //
        // Necessary, not stylistic: with `Chainable` undefined behaviour a missing
        // token is `undefined`, which does not coerce to `&str`. Without this,
        // `{{ lookup.jira.type | default_if_empty('experiment') }}` — the exact
        // pattern a config uses to survive a failed lookup — would raise "value is
        // not a string" instead of falling back. The filter bodies stay pure `&str`
        // functions so they remain directly testable.
        env.add_filter("slugify", |v: Option<String>| {
            filters::filter_slugify(&v.unwrap_or_default())
        });
        env.add_filter(
            "truncate",
            |v: Option<String>, length: usize, suffix: Option<String>| {
                filters::filter_truncate(&v.unwrap_or_default(), length, suffix)
            },
        );
        env.add_filter(
            "default_if_empty",
            |v: Option<String>, fallback: Option<Value>| {
                filters::filter_default_if_empty(&v.unwrap_or_default(), fallback.as_ref())
            },
        );
        env.add_filter(
            "re_replace",
            |v: Option<String>, pattern: String, to: String| {
                filters::filter_re_replace(&v.unwrap_or_default(), &pattern, &to)
            },
        );
        env.add_filter("matches", |v: Option<String>, pattern: String| {
            filters::filter_matches(&v.unwrap_or_default(), &pattern)
        });
        env.add_filter("strip_prefix", |v: Option<String>, prefix: String| {
            filters::filter_strip_prefix(&v.unwrap_or_default(), &prefix)
        });
        env.add_filter("strip_suffix", |v: Option<String>, suffix: String| {
            filters::filter_strip_suffix(&v.unwrap_or_default(), &suffix)
        });
        env.add_filter("after", |v: Option<String>, separator: String| {
            filters::filter_after(&v.unwrap_or_default(), &separator)
        });
        env.add_filter("before", |v: Option<String>, separator: String| {
            filters::filter_before(&v.unwrap_or_default(), &separator)
        });
        // `lower`, `upper`, `trim`, `replace`, `default`, `length` etc. come from
        // minijinja's `builtins` feature.

        Self { env }
    }

    fn syntax_error(key: &str, err: &minijinja::Error) -> RenderError {
        RenderError::Syntax {
            key: key.to_owned(),
            message: format_error(err),
        }
    }

    fn eval_error(key: &str, err: &minijinja::Error) -> RenderError {
        RenderError::Eval {
            key: key.to_owned(),
            message: format_error(err),
        }
    }
}

/// Truthiness for a stringly-typed config value.
///
/// TOML has real booleans and the form has real checkboxes, but everything reaches a template
/// as a string, so `"false"` has to mean false. The accepted spellings are the ones a config
/// author or a serializer actually produces.
#[must_use]
pub fn is_truthy_config_string(text: &str) -> bool {
    !matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "" | "false" | "0" | "no" | "off" | "null" | "none" | "undefined"
    )
}

/// Flatten a minijinja error chain into one message.
///
/// The interesting detail is usually in the `source`, not the top-level error — a
/// bare "invalid operation" is useless without "unknown filter: slugfy".
fn format_error(err: &minijinja::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    if let Some(line) = err.line() {
        parts.push(format!("(line {line})"));
    }
    parts.join(": ")
}

impl TemplateEngine for Engine {
    fn render(&self, key: &str, template: &str, ctx: &Context) -> Result<String, RenderError> {
        let nested = NestedContext::build(ctx);
        let compiled = self
            .env
            .template_from_str(template)
            .map_err(|e| Self::syntax_error(key, &e))?;
        compiled
            .render(nested.value())
            .map_err(|e| Self::eval_error(key, &e))
    }

    fn eval_bool(&self, key: &str, template: &str, ctx: &Context) -> Result<bool, RenderError> {
        let nested = NestedContext::build(ctx);
        // `compile_expression` rather than rendering: a `when` clause is written as a
        // bare expression (`issue != vars.placeholder`), not as `{{ … }}`.
        let expr = self
            .env
            .compile_expression(template)
            .map_err(|e| Self::syntax_error(key, &e))?;
        let value: Value = expr
            .eval(nested.value())
            .map_err(|e| Self::eval_error(key, &e))?;

        // A bare token evaluates to a *string*, because the context is stringly-typed by
        // construction — every value arrives as `String`. jinja then says any non-empty
        // string is true, which makes `when = "skip_db"` fire when the checkbox is
        // **unticked** and its value is the string "false".
        //
        // That is not a theoretical worry. It shipped: `--no-db` was appended to a project's
        // setup command on every single run, so the database clone the user expected was
        // silently skipped every time, and ticking "restore from dump" produced
        // `--load-dump --no-db` together.
        //
        // So a string result gets *config* truthiness rather than jinja's. An expression that
        // yields a real bool (`issue != vars.placeholder`) is unaffected.
        if let Some(text) = value.as_str() {
            return Ok(is_truthy_config_string(text));
        }

        Ok(value.is_true())
    }

    fn referenced_tokens(&self, key: &str, template: &str) -> Result<Vec<String>, RenderError> {
        let compiled = self
            .env
            .template_from_str(template)
            .map_err(|e| Self::syntax_error(key, &e))?;
        // `nested = true` yields dotted paths (`lookup.jira.summary`) rather than
        // just the root name, which is exactly the granularity the scope check needs.
        let mut tokens: Vec<String> = compiled.undeclared_variables(true).into_iter().collect();
        tokens.sort();
        Ok(tokens)
    }

    fn validate(&self, key: &str, template: &str, scope: &TokenScope) -> Result<(), RenderError> {
        let tokens = self.referenced_tokens(key, template)?;

        let mut offenders: Vec<(String, TokenSet)> = Vec::new();
        let mut seen: BTreeSet<TokenSet> = BTreeSet::new();
        for token in &tokens {
            let set = namespace_of(token);
            if !scope.allows(set) && seen.insert(set) {
                offenders.push((token.clone(), set));
            }
        }

        if let Some((token, set)) = offenders.first() {
            return Err(RenderError::Unusable {
                key: key.to_owned(),
                rendered: token.clone(),
                message: scope.reason_for(*set),
            });
        }

        Ok(())
    }

    fn apply_filter(&self, filter: &str, value: &str) -> Result<String, RenderError> {
        // Route through the engine rather than matching on names here, so a lookup
        // transform and a template filter cannot diverge. `value` is passed as a
        // variable, never interpolated into the template source, so a summary
        // containing `{{` cannot become template code.
        let template = format!("{{{{ subject | {filter} }}}}");
        let compiled = self
            .env
            .template_from_str(&template)
            .map_err(|e| Self::syntax_error(filter, &e))?;
        compiled
            .render(minijinja::context! { subject => value })
            .map_err(|e| Self::eval_error(filter, &e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(pairs: &[(&str, &str)]) -> Context {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn engine() -> Engine {
        Engine::new()
    }

    // ── rendering ─────────────────────────────────────────────────────────────

    #[test]
    fn renders_the_reference_projects_branch_template() {
        // The end-to-end case: Jira metadata → branch name.
        let engine = engine();
        let context = ctx(&[
            ("issue", "ACME-1234"),
            ("computed.issue_type", "task"),
            ("computed.slug", "extend-report-templates-and-exports"),
        ]);
        assert_eq!(
            engine
                .render(
                    "naming.branch",
                    "{{ computed.issue_type }}/{{ issue }}-{{ computed.slug }}",
                    &context
                )
                .unwrap(),
            "task/ACME-1234-extend-report-templates-and-exports"
        );
    }

    #[test]
    fn renders_the_slug_precedence_chain() {
        // A typed title beats the tracker's summary; blank falls through.
        let engine = engine();
        let template = "{{ title | default_if_empty(lookup.jira.summary) | slugify }}";

        let with_title = ctx(&[
            ("title", "My Override"),
            ("lookup.jira.summary", "Stretch: Action Plans"),
        ]);
        assert_eq!(
            engine
                .render("computed.slug", template, &with_title)
                .unwrap(),
            "my-override"
        );

        let without_title = ctx(&[
            ("title", ""),
            ("lookup.jira.summary", "Stretch: Action Plans"),
        ]);
        assert_eq!(
            engine
                .render("computed.slug", template, &without_title)
                .unwrap(),
            "stretch-action-plans"
        );
    }

    #[test]
    fn renders_the_bare_number_auto_prefix_rule() {
        let engine = engine();
        let template =
            "{{ issue | trim | re_replace('^([0-9]+)$', vars.jira_slug ~ '-$1') | upper }}";

        for (input, expected) in [
            ("1234", "ACME-1234"),
            (" 1234 ", "ACME-1234"),
            ("ACME-1234", "ACME-1234"),
            ("acme-99", "ACME-99"),
        ] {
            let context = ctx(&[("issue", input), ("vars.jira_slug", "ACME")]);
            assert_eq!(
                engine
                    .render("field.issue.normalize", template, &context)
                    .unwrap(),
                expected,
                "input {input:?}"
            );
        }
    }

    #[test]
    fn a_missing_token_renders_empty_rather_than_failing() {
        // A lookup that failed under `on_error = "warn"` leaves its tokens absent;
        // the config's fallback chain is how that is meant to be handled.
        let engine = engine();
        assert_eq!(
            engine
                .render("k", "[{{ lookup.jira.summary }}]", &Context::new())
                .unwrap(),
            "[]"
        );
        assert_eq!(
            engine
                .render(
                    "k",
                    "{{ lookup.jira.type | default_if_empty('experiment') }}",
                    &Context::new()
                )
                .unwrap(),
            "experiment"
        );
    }

    #[test]
    fn the_directory_template_for_an_adopted_branch_strips_the_type_prefix() {
        let engine = engine();
        let context = ctx(&[("matched_branch", "experiment/ACME-0000-move-settings")]);
        assert_eq!(
            engine
                .render(
                    "create.existing_branch_match.directory",
                    "{{ matched_branch | re_replace('^[^/]+/', '') }}",
                    &context
                )
                .unwrap(),
            "ACME-0000-move-settings"
        );
    }

    #[test]
    fn a_syntax_error_names_the_config_key() {
        let engine = engine();
        let err = engine
            .render("naming.branch", "{{ unclosed", &Context::new())
            .unwrap_err();
        match err {
            RenderError::Syntax { key, .. } => assert_eq!(key, "naming.branch"),
            other => panic!("expected Syntax, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_filter_is_reported_with_its_name() {
        // A typo like `slugfy` must be actionable.
        let engine = engine();
        let err = engine
            .render("k", "{{ a | slugfy }}", &ctx(&[("a", "x")]))
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("slugfy"), "unhelpful message: {message}");
    }

    // ── sandbox ───────────────────────────────────────────────────────────────

    #[test]
    fn template_includes_cannot_reach_the_filesystem() {
        // The sandbox guarantee: these templates come from a file inside a repo.
        let engine = engine();
        for hostile in [
            "{% include '/etc/passwd' %}",
            "{% extends 'base.html' %}",
            "{% import '/etc/hosts' as h %}",
        ] {
            let result = engine.render("k", hostile, &Context::new());
            assert!(result.is_err(), "{hostile} should not resolve");
        }
    }

    #[test]
    fn a_value_containing_template_syntax_is_not_evaluated() {
        // A Jira summary is untrusted text. It must never become code.
        let engine = engine();
        let context = ctx(&[("title", "{{ 7 * 7 }}")]);
        let rendered = engine.render("k", "{{ title }}", &context).unwrap();
        assert_eq!(rendered, "{{ 7 * 7 }}", "values must not be re-evaluated");
        assert!(!rendered.contains("49"));
    }

    #[test]
    fn apply_filter_does_not_interpolate_the_value_into_template_source() {
        let engine = engine();
        assert_eq!(
            engine.apply_filter("slugify", "{{ 7 * 7 }}").unwrap(),
            "7-7"
        );
    }

    // ── boolean expressions ───────────────────────────────────────────────────

    #[test]
    fn eval_bool_handles_the_required_when_rule() {
        let engine = engine();
        let expr = "issue == vars.placeholder";

        let placeholder = ctx(&[("issue", "ACME-0000"), ("vars.placeholder", "ACME-0000")]);
        assert!(
            engine
                .eval_bool("required_when", expr, &placeholder)
                .unwrap()
        );

        let real = ctx(&[("issue", "ACME-1234"), ("vars.placeholder", "ACME-0000")]);
        assert!(!engine.eval_bool("required_when", expr, &real).unwrap());
    }

    #[test]
    fn eval_bool_treats_a_bare_token_as_truthiness() {
        // `when = "load_dump"` must work without comparing against the string "true".
        let engine = engine();
        assert!(
            engine
                .eval_bool("when", "load_dump", &ctx(&[("load_dump", "true")]))
                .unwrap()
        );
        assert!(
            !engine
                .eval_bool("when", "load_dump", &ctx(&[("load_dump", "")]))
                .unwrap()
        );
        assert!(
            !engine
                .eval_bool("when", "load_dump", &Context::new())
                .unwrap()
        );
    }

    #[test]
    fn eval_bool_supports_comparison_against_an_empty_string() {
        // The `when` guard on a display badge.
        let engine = engine();
        let expr = "lookup.jira.status != ''";
        assert!(
            engine
                .eval_bool("when", expr, &ctx(&[("lookup.jira.status", "In Progress")]))
                .unwrap()
        );
        assert!(
            !engine
                .eval_bool("when", expr, &ctx(&[("lookup.jira.status", "")]))
                .unwrap()
        );
    }

    /// **The regression that shipped.** A bool field's `"false"` must be falsy.
    #[test]
    fn a_string_false_is_falsy_so_an_unticked_checkbox_does_not_fire() {
        let engine = engine();
        // Every spelling a checkbox or a serializer can produce.
        for falsy in ["false", "False", "FALSE", "0", "no", "off", "", "  false  "] {
            assert!(
                !engine
                    .eval_bool("when", "skip_db", &ctx(&[("skip_db", falsy)]))
                    .unwrap(),
                "{falsy:?} must be falsy — otherwise --no-db is appended on every run"
            );
        }
        for truthy in ["true", "True", "1", "yes", "on"] {
            assert!(
                engine
                    .eval_bool("when", "skip_db", &ctx(&[("skip_db", truthy)]))
                    .unwrap(),
                "{truthy:?} must be truthy"
            );
        }
    }

    #[test]
    fn config_truthiness_does_not_disturb_a_real_comparison() {
        // An expression yielding a genuine bool must be untouched by the string coercion.
        let engine = engine();
        let context = ctx(&[("issue", "ACME-0000"), ("vars.placeholder", "ACME-0000")]);
        assert!(
            engine
                .eval_bool("w", "issue == vars.placeholder", &context)
                .unwrap()
        );
        assert!(
            !engine
                .eval_bool("w", "issue != vars.placeholder", &context)
                .unwrap()
        );
        // And a string that merely *contains* "false" is still truthy.
        assert!(
            engine
                .eval_bool("w", "name", &ctx(&[("name", "falsehood")]))
                .unwrap()
        );
    }

    /// The trap: an undefined token is not equal to `''`.
    ///
    /// A `when = "env.FOO != ''"` guard therefore fires when `FOO` is *absent*, which is the
    /// opposite of what it reads like — found in the wild when a Docker teardown step ran
    /// against a worktree that had no environment file at all. Pinned here so the documented
    /// idiom cannot quietly stop working.
    #[test]
    fn an_undefined_token_is_not_equal_to_the_empty_string() {
        let engine = engine();
        assert!(
            engine
                .eval_bool("when", "env.X != ''", &Context::new())
                .unwrap(),
            "this is the surprising behaviour, and it is jinja's, not ours"
        );
        assert!(
            !engine
                .eval_bool(
                    "when",
                    "env.X | default_if_empty('') != ''",
                    &Context::new()
                )
                .unwrap(),
            "the documented idiom must collapse undefined to empty"
        );
        // And it must still be true for a value that is genuinely set.
        assert!(
            engine
                .eval_bool(
                    "when",
                    "env.X | default_if_empty('') != ''",
                    &ctx(&[("env.X", "acme-1234")])
                )
                .unwrap()
        );
    }

    #[test]
    fn eval_bool_reports_a_bad_expression() {
        let engine = engine();
        assert!(
            engine
                .eval_bool("when", "issue ==", &Context::new())
                .is_err()
        );
    }

    // ── token extraction ──────────────────────────────────────────────────────

    #[test]
    fn referenced_tokens_returns_dotted_paths() {
        let engine = engine();
        let tokens = engine
            .referenced_tokens(
                "naming.branch",
                "{{ computed.issue_type }}/{{ issue }}-{{ computed.slug }}",
            )
            .unwrap();
        assert!(tokens.contains(&"issue".to_owned()), "got {tokens:?}");
        assert!(
            tokens.contains(&"computed.issue_type".to_owned()),
            "got {tokens:?}"
        );
        assert!(
            tokens.contains(&"computed.slug".to_owned()),
            "got {tokens:?}"
        );
    }

    #[test]
    fn referenced_tokens_sees_through_filter_arguments() {
        // `vars.jira_slug` appears only as a filter argument, but a config typo there
        // is just as damaging.
        let engine = engine();
        let tokens = engine
            .referenced_tokens(
                "k",
                "{{ issue | re_replace('^x$', vars.jira_slug ~ '-y') }}",
            )
            .unwrap();
        assert!(
            tokens.contains(&"vars.jira_slug".to_owned()),
            "got {tokens:?}"
        );
    }

    // ── scope validation ──────────────────────────────────────────────────────

    /// The load-time check that prevents an empty render producing a corrupt branch.
    #[test]
    fn naming_may_not_reference_the_worktree() {
        let engine = engine();
        let err = engine
            .validate(
                "naming.branch",
                "{{ computed.issue_type }}/{{ worktree.dirname }}",
                &TokenScope::naming("branch"),
            )
            .unwrap_err();

        match err {
            RenderError::Unusable {
                key,
                rendered,
                message,
            } => {
                assert_eq!(key, "naming.branch");
                assert_eq!(rendered, "worktree.dirname");
                assert!(
                    message.contains("does not exist yet"),
                    "the message must explain the ordering: {message}"
                );
            }
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[test]
    fn naming_may_not_reference_env_either() {
        let engine = engine();
        assert!(
            engine
                .validate(
                    "naming.branch",
                    "{{ env.HOST_PORT_WEB }}",
                    &TokenScope::naming("branch")
                )
                .is_err(),
            "env files are read from a worktree that does not exist yet"
        );
    }

    #[test]
    fn a_valid_naming_template_passes() {
        let engine = engine();
        engine
            .validate(
                "naming.branch",
                "{{ computed.issue_type }}/{{ issue }}-{{ computed.slug }}",
                &TokenScope::naming("branch"),
            )
            .unwrap();
    }

    #[test]
    fn normalize_may_not_reference_a_lookup_because_it_produces_the_lookup_key() {
        let engine = engine();
        assert!(
            engine
                .validate(
                    "field.issue.normalize",
                    "{{ lookup.jira.type }}",
                    &TokenScope::normalize("issue")
                )
                .is_err(),
            "that would be circular"
        );
    }

    #[test]
    fn lookups_may_not_chain() {
        let engine = engine();
        assert!(
            engine
                .validate("lookup.b", "{{ lookup.a.value }}", &TokenScope::lookup("b"))
                .is_err()
        );
    }

    #[test]
    fn a_worktree_command_may_reference_everything() {
        let engine = engine();
        let scope = TokenScope::worktree_command("setup");
        engine
            .validate(
                "setup.run",
                "{{ worktree.path }} {{ env.COMPOSE_PROJECT_NAME }} {{ computed.slug }} \
                 {{ lookup.jira.type }} {{ vars.remote }} {{ repo.root }} {{ os.uid }} {{ now.date }}",
                &scope,
            )
            .unwrap();
    }

    #[test]
    fn matched_branch_is_only_in_scope_for_an_adopted_directory() {
        let engine = engine();
        engine
            .validate(
                "d",
                "{{ matched_branch }}",
                &TokenScope::matched_directory(),
            )
            .unwrap();
        assert!(
            engine
                .validate(
                    "d",
                    "{{ matched_branch }}",
                    &TokenScope::naming("directory")
                )
                .is_err()
        );
    }

    #[test]
    fn validation_reports_the_first_offender_deterministically() {
        // Two out-of-scope namespaces; the error must be stable across runs so a
        // config author does not chase a moving message.
        let engine = engine();
        let template = "{{ worktree.path }} {{ env.X }}";
        let first = engine
            .validate("k", template, &TokenScope::naming("branch"))
            .unwrap_err();
        let second = engine
            .validate("k", template, &TokenScope::naming("branch"))
            .unwrap_err();
        assert_eq!(first.to_string(), second.to_string());
    }

    #[test]
    fn a_syntax_error_surfaces_from_validate_too() {
        let engine = engine();
        assert!(matches!(
            engine
                .validate("k", "{{ oops", &TokenScope::naming("branch"))
                .unwrap_err(),
            RenderError::Syntax { .. }
        ));
    }

    // ── filters via the port ──────────────────────────────────────────────────

    #[test]
    fn apply_filter_covers_the_lookup_transform_vocabulary() {
        let engine = engine();
        assert_eq!(
            engine.apply_filter("lower", "Sub-Task").unwrap(),
            "sub-task"
        );
        assert_eq!(engine.apply_filter("upper", "task").unwrap(), "TASK");
        assert_eq!(engine.apply_filter("trim", "  x  ").unwrap(), "x");
        assert_eq!(
            engine.apply_filter("slugify", "Stretch: Updates").unwrap(),
            "stretch-updates"
        );
    }

    #[test]
    fn apply_filter_rejects_an_unknown_filter() {
        let engine = engine();
        assert!(engine.apply_filter("definitely_not_a_filter", "x").is_err());
    }

    #[test]
    fn every_documented_filter_actually_exists() {
        // Keeps the port's FILTERS list — which is part of the config contract —
        // honest about what the engine provides.
        let engine = engine();
        for filter in wtm_core::ports::template::FILTERS {
            let template = match *filter {
                "truncate" => "{{ subject | truncate(3, '') }}".to_owned(),
                "default_if_empty" => "{{ subject | default_if_empty('x') }}".to_owned(),
                "default" => "{{ subject | default('x') }}".to_owned(),
                "re_replace" => "{{ subject | re_replace('a', 'b') }}".to_owned(),
                "replace" => "{{ subject | replace('a', 'b') }}".to_owned(),
                "strip_prefix" => "{{ subject | strip_prefix('a') }}".to_owned(),
                other => format!("{{{{ subject | {other} }}}}"),
            };
            let result = engine.render("contract", &template, &ctx(&[("subject", "abc")]));
            assert!(
                result.is_ok(),
                "documented filter `{filter}` is missing: {result:?}"
            );
        }
    }
}

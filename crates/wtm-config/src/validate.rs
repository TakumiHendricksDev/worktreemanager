//! Semantic validation of a merged project config.
//!
//! # What this is for
//!
//! Deserialization already rejects unknown keys and wrong types. This catches the
//! mistakes that are *type-correct but wrong*, and it catches them at load time so the
//! error names a file and a key instead of surfacing as a strange worktree hours later:
//!
//! - a template referencing a token that cannot exist at that position — the failure
//!   that yields a branch called `experiment/ACME-0000-`;
//! - a field key shadowing a reserved namespace, which makes `{{ repo.root }}` resolve
//!   against a form field;
//! - duplicate keys, dangling references (`base_field`, `display.from`), and
//!   `[[computed]]` entries that reference a later sibling;
//! - a `[[guards.forbid]]` rule matching one of the config's own commands, which means
//!   the config declares something it also forbids.
//!
//! It takes `&dyn TemplateEngine` rather than importing the engine, so this crate
//! stays dependent on the domain alone.

use std::collections::BTreeSet;

use wtm_core::error::{ConfigError, ConfigLayer};
use wtm_core::model::{
    CommandSpec, CwdBase, DirBase, OptionsSource, Project, SUPPORTED_SCHEMA_VERSION, TokenScope,
    shadows_reserved_prefix,
};
use wtm_core::ports::template::TemplateEngine;

/// Where an error is reported against when no more specific file is known.
#[derive(Debug, Clone)]
pub struct Origin {
    pub path: std::path::PathBuf,
    pub layer: ConfigLayer,
}

impl Origin {
    fn invalid(&self, key: &str, message: impl Into<String>) -> ConfigError {
        ConfigError::Invalid {
            path: self.path.clone(),
            layer: self.layer,
            line: None,
            column: None,
            key: Some(key.to_owned()),
            message: message.into(),
        }
    }
}

/// Validate `project`, returning the first problem found.
///
/// Fail-fast rather than collecting: config errors cascade (a bad field key makes every
/// template referencing it look wrong too), so a list would mostly be noise following
/// one real cause.
pub fn validate(
    project: &Project,
    engine: &dyn TemplateEngine,
    origin: &Origin,
) -> Result<(), ConfigError> {
    check_schema_version(project, origin)?;
    check_field_keys(project, origin)?;
    check_computed_keys(project, origin)?;
    check_lookup_ids(project, origin)?;
    check_references(project, origin)?;
    check_commands(project, origin)?;
    check_templates(project, engine, origin)?;
    check_guards(project, origin)?;
    Ok(())
}

fn check_schema_version(project: &Project, origin: &Origin) -> Result<(), ConfigError> {
    if project.schema_version > SUPPORTED_SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedSchema {
            path: origin.path.clone(),
            found: project.schema_version,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn check_field_keys(project: &Project, origin: &Origin) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();

    for field in &project.fields {
        if field.key.is_empty() {
            return Err(origin.invalid("field.key", "a field key must not be empty"));
        }
        if !field
            .key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(origin.invalid(
                &format!("field.{}", field.key),
                "a field key may contain only letters, digits and underscores, so it can be \
                 referenced from a template as a bare name",
            ));
        }
        if let Some(prefix) = shadows_reserved_prefix(&field.key) {
            return Err(origin.invalid(
                &format!("field.{}", field.key),
                format!(
                    "`{prefix}` is a reserved template namespace. A field with this key would \
                     make `{{{{ {prefix}.… }}}}` resolve against the field's value instead, which \
                     fails as an empty render rather than an error. Rename the field."
                ),
            ));
        }
        if !seen.insert(field.key.clone()) {
            return Err(origin.invalid(
                &format!("field.{}", field.key),
                "duplicate field key: the later definition would silently win",
            ));
        }

        // A select with no options is a dead control.
        if matches!(
            field.kind,
            wtm_core::model::FieldKind::Select | wtm_core::model::FieldKind::Multiselect
        ) && field.options.is_none()
        {
            return Err(origin.invalid(
                &format!("field.{}", field.key),
                "a select field needs an `[field.options]` table",
            ));
        }

        if let Some(pattern) = &field.pattern {
            compile_regex(pattern, &format!("field.{}.pattern", field.key), origin)?;
        }
    }

    Ok(())
}

fn check_computed_keys(project: &Project, origin: &Origin) -> Result<(), ConfigError> {
    let field_keys: BTreeSet<&str> = project.fields.iter().map(|f| f.key.as_str()).collect();
    let mut seen = BTreeSet::new();

    for computed in &project.computed {
        if computed.key.is_empty() {
            return Err(origin.invalid("computed.key", "a computed key must not be empty"));
        }
        if !seen.insert(computed.key.clone()) {
            return Err(origin.invalid(
                &format!("computed.{}", computed.key),
                "duplicate computed key",
            ));
        }
        if field_keys.contains(computed.key.as_str()) {
            // Not fatal for rendering — they live in different namespaces — but it
            // guarantees a reader will eventually confuse `slug` with `computed.slug`.
            return Err(origin.invalid(
                &format!("computed.{}", computed.key),
                "a computed value and a field share this name; rename one so templates \
                 are unambiguous to read",
            ));
        }
    }

    Ok(())
}

fn check_lookup_ids(project: &Project, origin: &Origin) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();

    for lookup in &project.lookups {
        if lookup.id.is_empty() {
            return Err(origin.invalid("lookup.id", "a lookup id must not be empty"));
        }
        if !lookup
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(origin.invalid(
                &format!("lookup.{}", lookup.id),
                "a lookup id may contain only letters, digits and underscores",
            ));
        }
        if !seen.insert(lookup.id.clone()) {
            return Err(origin.invalid(&format!("lookup.{}", lookup.id), "duplicate lookup id"));
        }
    }

    Ok(())
}

fn check_references(project: &Project, origin: &Origin) -> Result<(), ConfigError> {
    // `create.base_field` must name a real field, or the base is silently empty.
    if !project.create.base_field.is_empty() && project.field(&project.create.base_field).is_none()
    {
        return Err(origin.invalid(
            "create.base_field",
            format!(
                "`{}` is not a declared field. Add a field with that key, or point \
                 base_field at an existing one.",
                project.create.base_field
            ),
        ));
    }

    // Display tables must reference declared sources.
    let source_ids: BTreeSet<&str> = project
        .display
        .sources
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    for table in &project.display.tables {
        if !source_ids.contains(table.from.as_str()) {
            return Err(origin.invalid(
                "display.port_table.from",
                format!("`{}` is not a declared `[[display.source]]`", table.from),
            ));
        }
        if let Some(defaults) = &table.defaults {
            if !source_ids.contains(defaults.as_str()) {
                return Err(origin.invalid(
                    "display.port_table.defaults",
                    format!("`{defaults}` is not a declared `[[display.source]]`"),
                ));
            }
        }
    }

    let mut source_seen = BTreeSet::new();
    for source in &project.display.sources {
        if !source_seen.insert(source.id.clone()) {
            return Err(origin.invalid(
                &format!("display.source.{}", source.id),
                "duplicate display source id",
            ));
        }
    }

    let mut action_seen = BTreeSet::new();
    for action in &project.actions {
        if action.id.is_empty() {
            return Err(origin.invalid("action.id", "an action id must not be empty"));
        }
        if !action_seen.insert(action.id.clone()) {
            return Err(origin.invalid(&format!("action.{}", action.id), "duplicate action id"));
        }
    }

    Ok(())
}

/// Every command site, paired with the config key that owns it.
fn commands_of(project: &Project) -> Vec<(String, &CommandSpec)> {
    let mut out: Vec<(String, &CommandSpec)> = Vec::new();

    for field in &project.fields {
        if let Some(OptionsSource::Command { command, .. }) = &field.options {
            out.push((format!("field.{}.options.run", field.key), command));
        }
    }
    for lookup in &project.lookups {
        out.push((format!("lookup.{}.run", lookup.id), &lookup.command));
    }
    if let Some(setup) = &project.setup {
        out.push(("setup.run".to_owned(), &setup.command));
    }
    for (index, pre) in project.remove.pre.iter().enumerate() {
        out.push((format!("remove.pre[{index}].run"), pre));
    }
    if let Some(command) = &project.remove.command {
        out.push(("remove.command.run".to_owned(), command));
    }
    for action in &project.actions {
        out.push((format!("action.{}.run", action.id), &action.command));
    }

    out
}

fn check_commands(project: &Project, origin: &Origin) -> Result<(), ConfigError> {
    for (key, command) in commands_of(project) {
        if command.run.is_empty() {
            return Err(origin.invalid(&key, "a command must have at least a program"));
        }
        if command.run.iter().any(String::is_empty) {
            return Err(origin.invalid(
                &key,
                "a command argument is empty; an empty argv element is passed through to the \
                 program and almost never intended",
            ));
        }
        if command.timeout_ms == Some(0) {
            return Err(origin.invalid(&key, "timeout_ms must be greater than zero"));
        }
        // `cwd = "worktree"` cannot be resolved before a worktree exists, and form
        // options and lookups both run while the dialog is still open.
        let runs_before_a_worktree_exists = key.starts_with("field.") || key.starts_with("lookup.");
        if runs_before_a_worktree_exists && command.cwd == CwdBase::Worktree {
            return Err(origin.invalid(
                &key,
                "`cwd = \"worktree\"` is not available here: form options and lookups are \
                 evaluated before any worktree exists. Use \"repo_root\".",
            ));
        }
    }

    Ok(())
}

fn check_templates(
    project: &Project,
    engine: &dyn TemplateEngine,
    origin: &Origin,
) -> Result<(), ConfigError> {
    let render_err =
        |key: &str, err: wtm_core::error::RenderError| origin.invalid(key, err.to_string());

    // Naming: the position where an out-of-scope token does the most damage.
    engine
        .validate(
            "naming.branch",
            &project.naming.branch,
            &TokenScope::naming("branch"),
        )
        .map_err(|e| render_err("naming.branch", e))?;
    engine
        .validate(
            "naming.directory",
            &project.naming.directory,
            &TokenScope::naming("directory"),
        )
        .map_err(|e| render_err("naming.directory", e))?;

    if let Some(pattern) = &project.naming.branch_must_match {
        compile_regex(pattern, "naming.branch_must_match", origin)?;
    }
    if let DirBase::Custom(template) = &project.naming.dir_base {
        engine
            .validate("naming.dir_base", template, &TokenScope::naming("dir_base"))
            .map_err(|e| render_err("naming.dir_base", e))?;
    }

    for field in &project.fields {
        let key = format!("field.{}.normalize", field.key);
        if let Some(template) = &field.normalize {
            engine
                .validate(&key, template, &TokenScope::normalize(&field.key))
                .map_err(|e| render_err(&key, e))?;
        }
        let key = format!("field.{}.required_when", field.key);
        if let Some(expression) = &field.required_when {
            // Wrapped so the same scope check applies to a bare expression.
            engine
                .validate(
                    &key,
                    &format!("{{{{ ({expression}) }}}}"),
                    &TokenScope::normalize(&field.key),
                )
                .map_err(|e| render_err(&key, e))?;
        }
    }

    for lookup in &project.lookups {
        let scope = TokenScope::lookup(&lookup.id);
        for (index, argument) in lookup.command.run.iter().enumerate() {
            let key = format!("lookup.{}.run[{index}]", lookup.id);
            engine
                .validate(&key, argument, &scope)
                .map_err(|e| render_err(&key, e))?;
        }
        if let Some(when) = &lookup.command.when {
            let key = format!("lookup.{}.when", lookup.id);
            engine
                .validate(&key, &format!("{{{{ ({when}) }}}}"), &scope)
                .map_err(|e| render_err(&key, e))?;
        }
    }

    // `[computed]` is order-dependent: each entry sees only the ones before it.
    let mut available: BTreeSet<String> = BTreeSet::new();
    for computed in &project.computed {
        let key = format!("computed.{}", computed.key);
        let scope = TokenScope::computed(&computed.key);
        engine
            .validate(&key, &computed.template, &scope)
            .map_err(|e| render_err(&key, e))?;

        let referenced = engine
            .referenced_tokens(&key, &computed.template)
            .map_err(|e| render_err(&key, e))?;
        for token in referenced {
            if let Some(name) = token.strip_prefix("computed.") {
                let name = name.split('.').next().unwrap_or(name);
                if name == computed.key {
                    return Err(origin.invalid(&key, "a computed value cannot reference itself"));
                }
                if !available.contains(name) {
                    return Err(origin.invalid(
                        &key,
                        format!(
                            "references `computed.{name}`, which is declared later. \
                             `[[computed]]` entries are evaluated in order, so move it above."
                        ),
                    ));
                }
            }
        }
        available.insert(computed.key.clone());
    }

    // Anything running against an existing worktree sees the full scope.
    for (key, command) in commands_of(project) {
        if key.starts_with("lookup.") {
            continue; // already checked under the narrower lookup scope
        }
        let scope = TokenScope::worktree_command(&key);
        for (index, argument) in command.run.iter().enumerate() {
            let arg_key = format!("{key}[{index}]");
            engine
                .validate(&arg_key, argument, &scope)
                .map_err(|e| render_err(&arg_key, e))?;
        }
        for (name, value) in &command.env {
            let env_key = format!("{key}.env.{name}");
            engine
                .validate(&env_key, value, &scope)
                .map_err(|e| render_err(&env_key, e))?;
        }
    }

    Ok(())
}

/// A guard rule with both of its patterns compiled.
struct CompiledRule<'a> {
    matches: regex::Regex,
    unless: Option<regex::Regex>,
    reason: &'a str,
}

impl CompiledRule<'_> {
    fn fires_on(&self, argv: &str) -> bool {
        self.matches.is_match(argv)
            && !self
                .unless
                .as_ref()
                .is_some_and(|unless| unless.is_match(argv))
    }
}

fn compile_rules<'a>(
    project: &'a Project,
    origin: &Origin,
) -> Result<Vec<CompiledRule<'a>>, ConfigError> {
    project
        .guards
        .forbid
        .iter()
        .map(|rule| {
            let matches = compile_regex(&rule.argv_matches, "guards.forbid.argv_matches", origin)?;
            let unless = rule
                .unless_matches
                .as_deref()
                .map(|pattern| compile_regex(pattern, "guards.forbid.unless_matches", origin))
                .transpose()?;
            Ok(CompiledRule {
                matches,
                unless,
                reason: rule.reason.as_str(),
            })
        })
        .collect()
}

fn check_guards(project: &Project, origin: &Origin) -> Result<(), ConfigError> {
    let rules = compile_rules(project, origin)?;

    // A config that declares a command it also forbids is contradicting itself, and the
    // spawn-time check would reject it anyway — better to say so now, with the key.
    for (key, command) in commands_of(project) {
        let argv = command.display_argv();
        for rule in &rules {
            if rule.fires_on(&argv) {
                return Err(ConfigError::Forbidden {
                    argv: format!("{key}: {argv}"),
                    reason: rule.reason.to_owned(),
                });
            }
        }
    }

    Ok(())
}

fn compile_regex(pattern: &str, key: &str, origin: &Origin) -> Result<regex::Regex, ConfigError> {
    regex::Regex::new(pattern)
        .map_err(|e| origin.invalid(key, format!("invalid regex `{pattern}`: {e}")))
}

/// Whether `argv` is forbidden by `project`'s guards.
///
/// Used at spawn time as well as at load time — defence in depth, since a command's
/// argv is only fully known once its templates are rendered.
///
/// # Errors
///
/// If a guard's regex does not compile. Load-time validation catches that first, so a
/// failure here means the config was constructed programmatically.
pub fn check_forbidden(project: &Project, argv: &[String]) -> Result<(), ConfigError> {
    let origin = Origin {
        path: project.root.clone(),
        layer: ConfigLayer::Repo,
    };
    let joined = argv.join(" ");

    // Same compilation and same matching rule as load-time validation, so the two
    // enforcement points cannot disagree about what is forbidden.
    for rule in compile_rules(project, &origin)? {
        if rule.fires_on(&joined) {
            return Err(ConfigError::Forbidden {
                argv: joined,
                reason: rule.reason.to_owned(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use wtm_render::Engine;

    use super::*;

    fn origin() -> Origin {
        Origin {
            path: PathBuf::from("/repo/wtm.toml"),
            layer: ConfigLayer::Repo,
        }
    }

    /// Deserialize a config fragment on top of the built-in defaults, the way loading
    /// really works, so a fragment does not have to restate everything.
    fn project_from(toml_text: &str) -> Project {
        let merged = crate::layers::merge(
            crate::layers::document(crate::layers::BUILT_IN_DEFAULTS).unwrap(),
            crate::layers::document(toml_text).unwrap(),
        );
        let mut project: Project = merged.try_into().expect("fragment should deserialize");
        project.root = PathBuf::from("/repo");
        project
    }

    fn check(toml_text: &str) -> Result<(), ConfigError> {
        validate(&project_from(toml_text), &Engine::new(), &origin())
    }

    #[test]
    fn the_built_in_defaults_are_valid() {
        // If the shipped defaults do not validate, every zero-config repo is broken.
        check("").expect("built-in defaults must validate");
    }

    #[test]
    fn a_field_shadowing_a_reserved_namespace_is_rejected() {
        let err = check("[[field]]\nkey = 'repo'\nlabel = 'Repo'\nkind = 'text'\n").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("reserved"), "unhelpful message: {message}");
    }

    #[test]
    fn duplicate_field_keys_are_rejected() {
        let err = check(
            "[[field]]\nkey = 'a'\nlabel = 'A'\nkind = 'text'\n\n\
             [[field]]\nkey = 'a'\nlabel = 'A again'\nkind = 'text'\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "got {err}");
    }

    #[test]
    fn a_field_key_with_a_dash_is_rejected_because_a_template_cannot_reference_it() {
        assert!(check("[[field]]\nkey = 'my-field'\nlabel = 'X'\nkind = 'text'\n").is_err());
    }

    #[test]
    fn a_select_without_options_is_rejected() {
        let err = check("[[field]]\nkey = 'pick'\nlabel = 'Pick'\nkind = 'select'\n").unwrap_err();
        assert!(err.to_string().contains("options"), "got {err}");
    }

    #[test]
    fn a_base_field_pointing_at_nothing_is_rejected() {
        // Otherwise the base ref is silently empty and every create fails oddly.
        let err = check("[create]\nbase_field = 'nonexistent'\n").unwrap_err();
        assert!(err.to_string().contains("nonexistent"), "got {err}");
    }

    /// The check that prevents `experiment/ACME-0000-`.
    #[test]
    fn naming_referencing_the_worktree_is_rejected_at_load_time() {
        let err = check("[naming]\nbranch = '{{ worktree.dirname }}'\n").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("does not exist yet"), "got {message}");
    }

    #[test]
    fn a_naming_template_with_a_syntax_error_is_rejected() {
        assert!(check("[naming]\nbranch = '{{ unclosed'\n").is_err());
    }

    #[test]
    fn an_invalid_regex_is_rejected_with_the_pattern() {
        let err = check("[naming]\nbranch_must_match = '([unclosed'\n").unwrap_err();
        assert!(err.to_string().contains("([unclosed"), "got {err}");
    }

    #[test]
    fn a_computed_value_referencing_a_later_sibling_is_rejected() {
        // `[[computed]]` is evaluated in declaration order, so this would render empty.
        let err = check(
            "[[computed]]\nkey = 'first'\ntemplate = '{{ computed.second }}'\n\n\
             [[computed]]\nkey = 'second'\ntemplate = 'x'\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("declared later"), "got {err}");
    }

    #[test]
    fn a_computed_value_referencing_an_earlier_sibling_is_fine() {
        check(
            "[[computed]]\nkey = 'first'\ntemplate = 'x'\n\n\
             [[computed]]\nkey = 'second'\ntemplate = '{{ computed.first }}-y'\n",
        )
        .unwrap();
    }

    #[test]
    fn a_self_referential_computed_value_is_rejected() {
        let err =
            check("[[computed]]\nkey = 'loop'\ntemplate = '{{ computed.loop }}'\n").unwrap_err();
        assert!(err.to_string().contains("itself"), "got {err}");
    }

    #[test]
    fn a_lookup_referencing_another_lookup_is_rejected() {
        let err =
            check("[[lookup]]\nid = 'b'\nrun = ['echo', '{{ lookup.a.value }}']\n").unwrap_err();
        assert!(err.to_string().contains("lookups"), "got {err}");
    }

    #[test]
    fn a_lookup_with_cwd_worktree_is_rejected() {
        // A lookup runs while the form is open; there is no worktree yet.
        let err = check("[[lookup]]\nid = 'a'\nrun = ['echo']\ncwd = 'worktree'\n").unwrap_err();
        assert!(
            err.to_string().contains("before any worktree exists"),
            "got {err}"
        );
    }

    #[test]
    fn setup_may_use_the_full_token_scope() {
        check(
            "[setup]\nrun = ['./bin/setup.sh', '{{ worktree.path }}']\ncwd = 'repo_root'\n\n\
             [setup.env]\nPATH = '{{ env.LOGIN_PATH }}'\n",
        )
        .unwrap();
    }

    #[test]
    fn an_empty_command_is_rejected() {
        assert!(check("[setup]\nrun = []\n").is_err());
        assert!(check("[setup]\nrun = ['sh', '']\n").is_err());
    }

    #[test]
    fn a_zero_timeout_is_rejected() {
        let err = check("[setup]\nrun = ['true']\ntimeout_ms = 0\n").unwrap_err();
        assert!(err.to_string().contains("greater than zero"), "got {err}");
    }

    #[test]
    fn a_newer_schema_version_is_rejected_clearly() {
        let err = check("schema_version = 99\n").unwrap_err();
        assert!(
            matches!(err, ConfigError::UnsupportedSchema { found: 99, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn a_config_that_declares_a_command_it_also_forbids_is_rejected() {
        let err = check(
            "[setup]\nrun = ['just', 'worktree_create', '8267']\n\n\
             [[guards.forbid]]\nargv_matches = 'just\\s+worktree_create'\nreason = 'never returns'\n",
        )
        .unwrap_err();
        match err {
            ConfigError::Forbidden { reason, .. } => assert!(reason.contains("never returns")),
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    #[test]
    fn a_display_table_referencing_an_undeclared_source_is_rejected() {
        let err =
            check("[[display.port_table]]\nprefix = 'HOST_PORT_'\nfrom = 'nope'\n").unwrap_err();
        assert!(err.to_string().contains("nope"), "got {err}");
    }

    #[test]
    fn duplicate_action_ids_are_rejected() {
        // The defaults already declare `shell`; redeclaring it in the same array is a
        // mistake, and arrays replace, so this array is the whole set.
        let err = check(
            "[[action]]\nid = 'x'\nlabel = 'X'\nrun = ['true']\n\n\
             [[action]]\nid = 'x'\nlabel = 'X2'\nrun = ['true']\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "got {err}");
    }

    // ── the spawn-time guard ──────────────────────────────────────────────────

    #[test]
    fn check_forbidden_matches_at_spawn_time() {
        let project = project_from(
            "[[guards.forbid]]\nargv_matches = 'worktree\\.sh\\s+create'\n\
             reason = 'never returns: it execs a login shell'\n",
        );

        let allowed = [
            "./bin/worktree.sh".to_owned(),
            "init".to_owned(),
            "/wt/a".to_owned(),
        ];
        check_forbidden(&project, &allowed).unwrap();

        let blocked = [
            "./bin/worktree.sh".to_owned(),
            "create".to_owned(),
            "8267".to_owned(),
        ];
        match check_forbidden(&project, &blocked).unwrap_err() {
            ConfigError::Forbidden { reason, .. } => {
                assert!(
                    reason.contains("execs a login shell"),
                    "the reason must reach the user"
                );
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    #[test]
    fn the_default_guards_block_unparsable_worktree_listing() {
        let project = project_from("");
        let bad = ["git".to_owned(), "worktree".to_owned(), "list".to_owned()];
        assert!(
            check_forbidden(&project, &bad).is_err(),
            "the human-readable form is unparsable"
        );

        let good = [
            "git".to_owned(),
            "worktree".to_owned(),
            "list".to_owned(),
            "--porcelain".to_owned(),
            "-z".to_owned(),
        ];
        check_forbidden(&project, &good).unwrap();
    }
}

/// The examples shipped in this repository must always be valid.
///
/// A broken example is worse than no example: someone installs it, gets a validation
/// error naming a file they did not write, and has no way to tell whether the bug is in
/// their setup or in ours. These tests parse each one exactly the way loading does.
#[cfg(test)]
mod example_tests {
    use std::path::PathBuf;

    use wtm_core::error::ConfigLayer;
    use wtm_render::Engine;

    use super::*;

    fn validate_example(relative: &str) -> Project {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

        // Merged onto the built-in defaults, exactly as `FileConfigStore::load` does.
        let merged = crate::layers::merge(
            crate::layers::document(crate::layers::BUILT_IN_DEFAULTS).unwrap(),
            crate::layers::document(&source)
                .unwrap_or_else(|e| panic!("{} is not valid TOML: {e}", path.display())),
        );

        let mut project: Project = merged.try_into().unwrap_or_else(|e: toml::de::Error| {
            panic!(
                "{} does not match the schema: {}",
                path.display(),
                e.message()
            )
        });
        project.root = PathBuf::from("/Users/dev/code/webapp");

        let origin = Origin {
            path: path.clone(),
            layer: ConfigLayer::Local,
        };
        validate(&project, &Engine::new(), &origin)
            .unwrap_or_else(|e| panic!("{} failed validation: {e}", path.display()));

        project
    }

    #[test]
    fn the_bundled_example_is_valid() {
        let project = validate_example("examples/webapp.wtm.toml");

        // Spot-check that it actually describes what it claims to, so a future edit that
        // guts the file still fails rather than passing an empty config.
        assert_eq!(project.display_name(), "ACME");
        assert!(
            project.field("issue").is_some(),
            "the Jira issue field must exist"
        );
        assert!(project.field("base").is_some());
        assert_eq!(project.fields.len(), 6, "six form fields");
        assert_eq!(project.lookups.len(), 1, "the acli lookup");
        assert_eq!(project.computed.len(), 2, "issue_type and slug");
        assert!(project.setup.is_some(), "worktree.sh init");
        assert_eq!(
            project.remove.pre.len(),
            2,
            "docker teardown plus the chown fixup"
        );
        assert_eq!(
            project.display.sources.len(),
            2,
            ".env and the port defaults"
        );
        assert_eq!(project.actions.len(), 3);
        assert_eq!(
            project.guards.forbid.len(),
            5,
            "the five unrunnable commands"
        );
    }

    /// The guards in the example must actually block the commands they name. A guard with a
    /// subtly wrong regex is worse than no guard, because it reads as protection.
    #[test]
    fn the_example_guards_block_every_hazardous_command() {
        let project = validate_example("examples/webapp.wtm.toml");

        for argv in [
            vec!["./bin/worktree.sh", "create", "1234"],
            vec!["just", "worktree_create", "1234"],
            vec!["just", "worktree_remove", "1234"],
            vec!["just", "worktree_list"],
            vec!["./bin/worktree.sh", "branch-create", "1234"],
        ] {
            let owned: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
            let result = check_forbidden(&project, &owned);
            assert!(result.is_err(), "{argv:?} must be blocked");
            // Every rejection has to explain itself, or the next person deletes the guard.
            match result.unwrap_err() {
                ConfigError::Forbidden { reason, .. } => {
                    assert!(
                        reason.len() > 20,
                        "the reason for {argv:?} is too thin: {reason}"
                    );
                }
                other => panic!("expected Forbidden, got {other:?}"),
            }
        }
    }

    /// And they must not block the commands the config itself relies on.
    #[test]
    fn the_example_guards_permit_the_safe_entry_points() {
        let project = validate_example("examples/webapp.wtm.toml");

        for argv in [
            vec!["./bin/worktree.sh", "init", "/Users/dev/code/ACME-1-x"],
            vec!["./bin/worktree.sh", "init", "/x", "--force"],
            vec!["just", "start"],
            vec!["acli", "jira", "workitem", "view", "ACME-1234", "--json"],
            vec![
                "git",
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads",
            ],
            vec!["git", "worktree", "list", "--porcelain", "-z"],
            vec!["docker", "compose", "down", "-v", "-t", "0"],
        ] {
            let owned: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
            check_forbidden(&project, &owned)
                .unwrap_or_else(|e| panic!("{argv:?} must be allowed, got {e}"));
        }
    }
}

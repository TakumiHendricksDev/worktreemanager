//! Writing recipes and roles back to the file that declares them, for Settings › Recipes.
//!
//! # Which file, and which table in it
//!
//! Recipes and roles may be declared in three places, never in a repository's `wtm.toml`
//! (`store::refuse_repository_roles` says why): `[defaults.workflow]` and
//! `[projects."<root>".config.workflow]` in your own config, and `[workflow]` in the local layer.
//! An edit is written to the most specific of those that already declares recipes or roles, so it
//! lands where you would have made it by hand. With none, the most specific that declares
//! `[workflow]` at all, since that is where this project's workflow already lives. With none of
//! those either, nothing is guessed: the editor asks, and offers the local layer first.
//!
//! # Only what changed, and everything else byte for byte
//!
//! The file is edited with `toml_edit`, never re-serialized, so its comments, key order and every
//! unrelated table (`[setup]`, `[guards]`, `[environment]`, other projects' entries) come through
//! untouched. The editor sends the whole merged spec it showed, and [`edit`] compares it, key by
//! key, with what the file and the layers beneath it already give: a key whose effective value is
//! unchanged is not touched, even when the file spells it differently. That is also what keeps a
//! project's override small. A project entry that changes one role's model over your defaults
//! stays one line, because the other keys still come from the defaults.
//!
//! Layers merge tables per key and replace arrays (`layers::merge`), so most edits can be said in
//! the file being written. One cannot: a layer can override a value from a weaker layer but not
//! remove it, and an optional key such as `model` has no value meaning "none". Clearing one that
//! the defaults set, or deleting a role they declare, is refused with the file and table to change
//! instead, rather than written as something that would not mean what the editor showed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use toml::Value;
use toml_edit::{ArrayOfTables, DocumentMut, Item, TableLike};
use wtm_core::flow::{RecipeSpec, RoleSpec};

/// The places a project's recipes and roles can be written, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowHome {
    /// `[defaults.workflow]` in your own config, which every project reads.
    Defaults,
    /// `[projects."<root>".config.workflow]` in your own config: this project's.
    Project,
    /// `[workflow]` in `<git-common-dir>/wtm.local.toml`: this repository's, untracked.
    Local,
}

impl WorkflowHome {
    pub const ALL: [Self; 3] = [Self::Defaults, Self::Project, Self::Local];

    /// The table path of this home's `workflow` within its file.
    #[must_use]
    pub fn section(self, root_key: &str) -> Vec<String> {
        match self {
            Self::Defaults => vec!["defaults".to_owned(), "workflow".to_owned()],
            Self::Project => vec![
                "projects".to_owned(),
                root_key.to_owned(),
                "config".to_owned(),
                "workflow".to_owned(),
            ],
            Self::Local => vec!["workflow".to_owned()],
        }
    }

    /// The table header a person would look for, such as `[defaults.workflow]`.
    #[must_use]
    pub fn header(self, root_key: &str) -> String {
        match self {
            Self::Defaults => "[defaults.workflow]".to_owned(),
            Self::Project => format!("[projects.\"{root_key}\".config.workflow]"),
            Self::Local => "[workflow]".to_owned(),
        }
    }
}

/// The recipes and roles Settings › Recipes edits: the whole of both maps, as the merged config
/// gives them, with the editor's changes applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDraft {
    #[serde(default)]
    pub role: BTreeMap<String, RoleSpec>,
    #[serde(default)]
    pub recipe: BTreeMap<String, RecipeSpec>,
}

/// Why an edit could not be written into the file, before validation looks at it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditError {
    /// The file is not TOML, so nothing in it can be kept.
    #[error("{message}")]
    Unreadable { message: String },
    /// A table the edit has to go inside is a value of another type in this file.
    #[error("`{key}` is not a table in this file, so nothing can be written under it")]
    NotATable { key: String },
    /// The layers beneath declare what the edit removes, and this file cannot take it away.
    #[error(
        "`{key}` comes from {from}, and a more specific file can change it but not remove it; \
         change it there"
    )]
    Unremovable {
        /// Relative to `workflow`, such as `role.conventions.model`.
        key: String,
        /// The table that declares it, such as `[defaults.workflow]`.
        from: String,
    },
    /// A role or recipe in the merged config is not one this build can read.
    #[error("`{key}` cannot be read: {message}")]
    Unparsed { key: String, message: String },
}

/// Whether a raw `[workflow]` table declares recipes or roles, or names a role to run by itself.
#[must_use]
pub fn declares_recipes(workflow: &Value) -> bool {
    ["role", "recipe", "planner", "designer", "implement"]
        .iter()
        .any(|key| workflow.get(key).is_some())
}

/// `source` with `draft` written into the `workflow` table at `section`.
///
/// `base` is the `workflow` table the layers beneath this one merge to, and `from` names where it
/// comes from, for the sentence an unremovable key is refused with.
///
/// # Errors
///
/// When the file cannot be parsed, a table in the path is a value, or the edit removes something
/// only `base` declares. Validation is the caller's: this only writes what was asked.
pub fn edit(
    source: &str,
    section: &[String],
    base: Option<&Value>,
    from: &str,
    draft: &WorkflowDraft,
) -> Result<String, EditError> {
    let mut doc: DocumentMut =
        source
            .parse()
            .map_err(|e: toml_edit::TomlError| EditError::Unreadable {
                message: e.message().to_owned(),
            })?;
    // What the file itself says, through `toml`, so it compares with `base` and the draft.
    let file: Value = crate::layers::document(source).map_err(|e| EditError::Unreadable {
        message: e.message().to_owned(),
    })?;
    let current = section
        .iter()
        .try_fold(&file, |value, key| value.get(key))
        .cloned();

    let ctx = Ctx { from };
    let table = section_mut(doc.as_table_mut(), section)?;
    ctx.keyed(
        table,
        "role",
        base.and_then(|b| b.get("role")),
        current.as_ref().and_then(|c| c.get("role")),
        &draft
            .role
            .iter()
            .map(|(id, spec)| {
                let key = format!("role.{id}");
                Ok((id.clone(), (role_table(spec, &key)?, field_order(spec))))
            })
            .collect::<Result<BTreeMap<_, _>, EditError>>()?,
        canonical_role,
    )?;
    ctx.keyed(
        table,
        "recipe",
        base.and_then(|b| b.get("recipe")),
        current.as_ref().and_then(|c| c.get("recipe")),
        &draft
            .recipe
            .iter()
            .map(|(id, spec)| {
                let key = format!("recipe.{id}");
                Ok((id.clone(), (recipe_table(spec, &key)?, field_order(spec))))
            })
            .collect::<Result<BTreeMap<_, _>, EditError>>()?,
        canonical_recipe,
    )?;
    Ok(doc.to_string())
}

/// What the editor's spelling of a key is compared against: its effective value, defaults left out.
type Canonical = fn(&Value, &str) -> Result<toml::Table, EditError>;

struct Ctx<'a> {
    from: &'a str,
}

impl Ctx<'_> {
    /// One keyed map, `role` or `recipe`: tables added, removed, and changed key by key.
    fn keyed(
        &self,
        section: &mut dyn TableLike,
        map: &str,
        base: Option<&Value>,
        current: Option<&Value>,
        want: &BTreeMap<String, (toml::Table, Vec<String>)>,
        canonical: Canonical,
    ) -> Result<(), EditError> {
        let ids = |value: Option<&Value>| -> Vec<String> {
            value
                .and_then(Value::as_table)
                .map(|t| t.keys().cloned().collect())
                .unwrap_or_default()
        };
        let mut known: Vec<String> = ids(base);
        known.extend(ids(current));
        known.sort();
        known.dedup();

        for id in known.iter().filter(|id| !want.contains_key(*id)) {
            if base.and_then(|b| b.get(id)).is_some() {
                return Err(EditError::Unremovable {
                    key: format!("{map}.{id}"),
                    from: self.from.to_owned(),
                });
            }
            if let Some(tables) = section.get_mut(map).and_then(Item::as_table_like_mut) {
                tables.remove(id);
            }
        }

        for (id, (wanted, order)) in want {
            let key = format!("{map}.{id}");
            let below = base.and_then(|b| b.get(id));
            let here = current.and_then(|c| c.get(id));
            let merged = match (below, here) {
                (None, None) => None,
                (Some(v), None) | (None, Some(v)) => Some(v.clone()),
                (Some(b), Some(h)) => Some(crate::layers::merge(b.clone(), h.clone())),
            };
            let merged = merged.map(|m| canonical(&m, &key)).transpose()?;
            if merged.as_ref() == Some(wanted) {
                continue;
            }
            let tables = child_mut(section, map, &key)?;
            let target = child_mut(tables, id, &key)?;
            self.keys(
                target,
                &key,
                below.and_then(Value::as_table),
                merged.as_ref(),
                wanted,
                order,
            )?;
        }

        // A map left empty by removals would be an empty header in the file.
        if section
            .get(map)
            .and_then(Item::as_table_like)
            .is_some_and(TableLike::is_empty)
            && current.is_some()
        {
            section.remove(map);
        }
        Ok(())
    }

    /// One table's keys: `wanted` is what the editor shows, `merged` what the file and the layers
    /// beneath give now, and `below` what the layers beneath give alone. A key the table lacks is
    /// added in `order`, the struct's own, so a new role reads objective first, as one written by
    /// hand would.
    #[allow(clippy::too_many_arguments)]
    fn keys(
        &self,
        target: &mut dyn TableLike,
        key: &str,
        below: Option<&toml::Table>,
        merged: Option<&toml::Table>,
        wanted: &toml::Table,
        order: &[String],
    ) -> Result<(), EditError> {
        let empty = toml::Table::new();
        let merged = merged.unwrap_or(&empty);
        let mut names: Vec<&String> = wanted.keys().collect();
        names.extend(merged.keys().filter(|k| !wanted.contains_key(*k)));
        names.sort_by_key(|name| order.iter().position(|o| o == *name).unwrap_or(usize::MAX));

        for name in names {
            let want = wanted.get(name);
            let have = merged.get(name);
            if want == have {
                continue;
            }
            let path = format!("{key}.{name}");
            let beneath = below.and_then(|b| b.get(name));
            // A nested table merges per key, like the role around it, so it is edited the same way.
            if name == "knowledge" {
                let child = child_mut(target, name, &path)?;
                self.keys(
                    child,
                    &path,
                    beneath.and_then(Value::as_table),
                    have.and_then(Value::as_table),
                    want.and_then(Value::as_table).unwrap_or(&empty),
                    &[],
                )?;
                if child.is_empty() {
                    target.remove(name);
                }
                continue;
            }
            match want {
                Some(value) if beneath == Some(value) => {
                    // The layers beneath already say it; this file only has to stop disagreeing.
                    target.remove(name);
                }
                Some(Value::Array(stages)) if name == "stage" => {
                    write_stages(target, stages)?;
                }
                Some(value) => set(target, name, value),
                // Nothing beneath, or the default beneath: this file only has to stop overriding it.
                None if beneath.is_none() || beneath == explicit_default(name).as_ref() => {
                    target.remove(name);
                }
                None => match explicit_default(name) {
                    Some(value) => set(target, name, &value),
                    None => {
                        return Err(EditError::Unremovable {
                            key: path,
                            from: self.from.to_owned(),
                        });
                    }
                },
            }
        }
        Ok(())
    }
}

/// The value that means "the default" for a key that has one a layer can write. An optional key
/// such as `model` has none, which is why clearing it over a weaker layer is refused.
fn explicit_default(key: &str) -> Option<Value> {
    match key {
        "retries" => Some(Value::Integer(0)),
        "output" => Some(Value::String("findings".to_owned())),
        "profile" => Some(Value::String("reviewer".to_owned())),
        "fallback" => Some(Value::String("ask".to_owned())),
        "kind" => Some(Value::String("static".to_owned())),
        "allow_credit_billing" => Some(Value::Boolean(false)),
        // Arrays replace rather than merge, so an empty one clears a weaker layer's.
        "skills" | "mcp" | "plugins" | "paths" | "checks" | "areas" => {
            Some(Value::Array(Vec::new()))
        }
        _ => None,
    }
}

/// The keys `RoleSpec` writes even at their defaults. Left out of the comparison, so a role that
/// never said `retries = 0` and one that did are the same role.
const ROLE_DEFAULTS: [(&str, fn() -> Value); 4] = [
    ("retries", || Value::Integer(0)),
    ("output", || Value::String("findings".to_owned())),
    ("profile", || Value::String("reviewer".to_owned())),
    ("fallback", || Value::String("ask".to_owned())),
];

fn role_table(spec: &RoleSpec, key: &str) -> Result<toml::Table, EditError> {
    let mut table = to_table(spec, key)?;
    for (name, default) in ROLE_DEFAULTS {
        if table.get(name) == Some(&default()) {
            table.remove(name);
        }
    }
    Ok(table)
}

/// The struct's keys in its declaration order, which `toml::Table`, sorted, does not keep.
fn field_order<T: Serialize>(spec: &T) -> Vec<String> {
    toml::to_string(spec)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .map(|doc| doc.iter().map(|(key, _)| key.to_owned()).collect())
        .unwrap_or_default()
}

fn recipe_table(spec: &RecipeSpec, key: &str) -> Result<toml::Table, EditError> {
    to_table(spec, key)
}

fn canonical_role(value: &Value, key: &str) -> Result<toml::Table, EditError> {
    role_table(&from_value(value, key)?, key)
}

fn canonical_recipe(value: &Value, key: &str) -> Result<toml::Table, EditError> {
    recipe_table(&from_value(value, key)?, key)
}

fn from_value<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Result<T, EditError> {
    value
        .clone()
        .try_into()
        .map_err(|e: toml::de::Error| EditError::Unparsed {
            key: key.to_owned(),
            message: e.message().to_owned(),
        })
}

fn to_table<T: Serialize>(spec: &T, key: &str) -> Result<toml::Table, EditError> {
    match Value::try_from(spec) {
        Ok(Value::Table(table)) => Ok(table),
        Ok(_) => Err(EditError::Unparsed {
            key: key.to_owned(),
            message: "it is not a table".to_owned(),
        }),
        Err(e) => Err(EditError::Unparsed {
            key: key.to_owned(),
            message: e.to_string(),
        }),
    }
}

/// The table at `path` under `table`, made (implicit, so it draws no header of its own) where the
/// file has none.
fn section_mut<'a>(
    table: &'a mut dyn TableLike,
    path: &[String],
) -> Result<&'a mut dyn TableLike, EditError> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(table);
    };
    let next = child_mut(table, first, first)?;
    section_mut(next, rest)
}

/// `table[name]` as a table, made implicit when absent.
fn child_mut<'a>(
    table: &'a mut dyn TableLike,
    name: &str,
    key: &str,
) -> Result<&'a mut dyn TableLike, EditError> {
    if table.get(name).is_none() {
        let mut child = toml_edit::Table::new();
        child.set_implicit(true);
        table.insert(name, Item::Table(child));
    }
    table
        .get_mut(name)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| EditError::NotATable {
            key: key.to_owned(),
        })
}

/// Write `value` at `name`, keeping the spacing and trailing comment the old value had.
fn set(table: &mut dyn TableLike, name: &str, value: &Value) {
    let mut fresh = edit_value(value);
    match table.get_mut(name) {
        Some(Item::Value(old)) => {
            let decor = old.decor().clone();
            *fresh.decor_mut() = decor;
            *old = fresh;
        }
        Some(item) => *item = Item::Value(fresh),
        None => {
            table.insert(name, Item::Value(fresh));
        }
    }
}

/// A `toml` value as `toml_edit` writes one fresh: TOML's own inline spelling.
fn edit_value(value: &Value) -> toml_edit::Value {
    // Through text, because `toml` already spells every value correctly — strings escaped, tables
    // inline — and a second hand-written spelling of it here is how the two would drift.
    format!("v = {value}")
        .parse::<DocumentMut>()
        .ok()
        .and_then(|doc| doc.get("v").and_then(Item::as_value).cloned())
        .unwrap_or_else(|| toml_edit::Value::from(value.to_string()))
}

/// Whether a value in the file says `want`, however it is spelled.
fn same_value(item: Option<&Item>, want: &Value) -> bool {
    item.and_then(Item::as_value)
        .and_then(|value| crate::layers::document(&format!("v = {value}")).ok())
        .and_then(|doc| doc.get("v").cloned())
        .as_ref()
        == Some(want)
}

/// A recipe's `[[…stage]]` array, rewritten stage by stage so an unchanged stage keeps its comments.
fn write_stages(recipe: &mut dyn TableLike, stages: &[Value]) -> Result<(), EditError> {
    let roles = |stage: &Value| stage.get("roles").cloned().unwrap_or(Value::Array(vec![]));
    match recipe.get_mut("stage") {
        Some(Item::ArrayOfTables(written)) => {
            for (index, stage) in stages.iter().enumerate() {
                let want = roles(stage);
                if let Some(table) = written.get_mut(index) {
                    if !same_value(table.get("roles"), &want) {
                        set(table, "roles", &want);
                    }
                } else {
                    let mut table = toml_edit::Table::new();
                    table.insert("roles", Item::Value(edit_value(&want)));
                    written.push(table);
                }
            }
            while written.len() > stages.len() {
                written.remove(written.len() - 1);
            }
        }
        Some(Item::Value(_)) => set(recipe, "stage", &Value::Array(stages.to_vec())),
        Some(_) => {
            return Err(EditError::NotATable {
                key: "stage".to_owned(),
            });
        }
        None => {
            let mut written = ArrayOfTables::new();
            for stage in stages {
                let mut table = toml_edit::Table::new();
                table.insert("roles", Item::Value(edit_value(&roles(stage))));
                written.push(table);
            }
            recipe.insert("stage", Item::ArrayOfTables(written));
        }
    }
    Ok(())
}

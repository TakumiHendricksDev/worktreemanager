//! Turning the flat token map into something a template can walk.
//!
//! # Two representations, one source
//!
//! The domain's [`Context`] is a flat map of dotted keys — `lookup.jira.summary`,
//! `computed.slug`, `repo.root`. That shape is right for the scope check (which works
//! on prefixes) and for snapshot tests (a flat map diffs readably).
//!
//! But a template writes `{{ lookup.jira.summary }}`, which jinja parses as
//! attribute access: `lookup` → `jira` → `summary`. So the flat map has to be nested
//! before rendering. Doing that here, once, is what lets the rest of the codebase
//! keep the simpler representation.
//!
//! # Collisions
//!
//! A flat map can express something a nested one cannot: both `repo` and `repo.root`
//! as values. Nesting resolves that in favour of the deeper path, because
//! `repo.root` is the useful one — but silently is not good enough, so the collision
//! is *returned* and `wtm-config` rejects it at load time with the offending key.
//! Field keys are bare, every other namespace is prefixed, so this only happens when
//! a config names a field after a reserved namespace.

use std::collections::{BTreeMap, BTreeSet};

use minijinja::Value;
use minijinja::value::Object;

use wtm_core::ports::template::Context;

/// Namespaces a field key must not shadow.
///
/// Re-exported from the domain rather than redefined: config validation and token
/// classification must agree on this list, and two copies would drift.
pub use wtm_core::model::RESERVED_PREFIXES;

/// A nested tree built from dotted keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Node {
    #[default]
    Empty,
    Leaf(String),
    Branch(BTreeMap<String, Node>),
}

impl Node {
    /// Insert `value` at `path`, recording any key rendered unreachable.
    ///
    /// `prefix` tracks the dotted path walked so far, so a collision is reported
    /// against the *shallow* key that got shadowed — `repo`, not `repo.root`. That is
    /// the one a config author has to rename.
    fn insert(
        &mut self,
        prefix: &mut Vec<String>,
        path: &[&str],
        value: String,
        collisions: &mut BTreeSet<String>,
    ) {
        if matches!(self, Self::Empty) {
            *self = Self::Branch(BTreeMap::new());
        }
        let Self::Branch(children) = self else {
            // A leaf reached as an intermediate node: the caller promotes before
            // descending, so this is unreachable in practice.
            return;
        };
        let Some((head, rest)) = path.split_first() else {
            return;
        };

        let child = children.entry((*head).to_owned()).or_default();
        prefix.push((*head).to_owned());

        if rest.is_empty() {
            match child {
                // A branch already occupies this name, so our scalar cannot be
                // addressed. The deeper path wins; report this key.
                Self::Branch(_) => {
                    collisions.insert(prefix.join("."));
                }
                _ => *child = Self::Leaf(value),
            }
        } else {
            if matches!(child, Self::Leaf(_)) {
                // An existing shallow scalar is about to be shadowed by this deeper
                // path. Same collision, discovered from the other direction.
                collisions.insert(prefix.join("."));
                *child = Self::Branch(BTreeMap::new());
            }
            child.insert(prefix, rest, value, collisions);
        }

        prefix.pop();
    }

    fn to_value(&self) -> Value {
        match self {
            // Undefined rather than empty-string, so `default`/`default_if_empty`
            // behave the way a template author expects.
            Self::Empty => Value::UNDEFINED,
            Self::Leaf(text) => Value::from(text.clone()),
            Self::Branch(children) => Value::from_object(NodeMap {
                entries: children
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect(),
            }),
        }
    }
}

/// Minimal map object so nested tokens resolve by attribute access.
#[derive(Debug)]
struct NodeMap {
    entries: BTreeMap<String, Value>,
}

impl Object for NodeMap {
    fn get_value(self: &std::sync::Arc<Self>, key: &Value) -> Option<Value> {
        self.entries.get(key.as_str()?).cloned()
    }

    fn enumerate(self: &std::sync::Arc<Self>) -> minijinja::value::Enumerator {
        minijinja::value::Enumerator::Str(
            // Leaked once per render of a stable key set; the alternative is
            // threading lifetimes through minijinja's object API for no benefit.
            self.entries
                .keys()
                .map(|k| Box::leak(k.clone().into_boxed_str()) as &'static str)
                .collect::<Vec<_>>()
                .leak(),
        )
    }
}

/// The result of nesting a flat context.
#[derive(Debug, Clone)]
pub struct NestedContext {
    root: Node,
    /// Dotted keys whose scalar value was shadowed by a deeper path.
    collisions: BTreeSet<String>,
}

impl NestedContext {
    /// Nest `context`'s dotted keys.
    ///
    /// Scalars are inserted shortest-path-first so a collision is always reported
    /// from the shallow key's perspective, whatever order the map iterates in.
    #[must_use]
    pub fn build(context: &Context) -> Self {
        let mut keys: Vec<&String> = context.keys().collect();
        keys.sort_by_key(|k| (k.matches('.').count(), (*k).clone()));

        // Always a branch, even when empty, so `value().get_attr(..)` is valid rather
        // than an error on an undefined root.
        let mut root = Node::Branch(BTreeMap::new());
        let mut collisions = BTreeSet::new();
        let mut prefix = Vec::new();

        for key in keys {
            let path: Vec<&str> = key.split('.').collect();
            if path.iter().any(|segment| segment.is_empty()) {
                // `a..b` or a leading/trailing dot: not addressable by a template.
                collisions.insert(key.clone());
                continue;
            }
            root.insert(&mut prefix, &path, context[key].clone(), &mut collisions);
        }

        Self { root, collisions }
    }

    /// The value handed to minijinja.
    #[must_use]
    pub fn value(&self) -> Value {
        self.root.to_value()
    }

    /// Keys whose value a template cannot reach.
    #[must_use]
    pub fn collisions(&self) -> &BTreeSet<String> {
        &self.collisions
    }
}

pub use wtm_core::model::shadows_reserved_prefix;

#[cfg(test)]
mod tests {
    use super::*;

    fn context(pairs: &[(&str, &str)]) -> Context {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn nests_dotted_keys_into_a_walkable_tree() {
        let nested = NestedContext::build(&context(&[
            ("issue", "ACME-1234"),
            ("lookup.jira.summary", "Stretch: updates"),
            ("lookup.jira.type", "task"),
            ("computed.slug", "stretch-updates"),
        ]));
        assert!(nested.collisions().is_empty());

        let value = nested.value();
        assert_eq!(value.get_attr("issue").unwrap().to_string(), "ACME-1234");
        let jira = value.get_attr("lookup").unwrap().get_attr("jira").unwrap();
        assert_eq!(jira.get_attr("type").unwrap().to_string(), "task");
        assert_eq!(
            jira.get_attr("summary").unwrap().to_string(),
            "Stretch: updates"
        );
    }

    #[test]
    fn a_missing_key_is_undefined_not_the_string_undefined() {
        // So `| default(...)` and `| default_if_empty(...)` work.
        let nested = NestedContext::build(&context(&[("a", "1")]));
        let missing = nested.value().get_attr("nope").unwrap();
        assert!(missing.is_undefined());
    }

    #[test]
    fn a_deeper_path_wins_over_a_scalar_and_the_collision_is_reported() {
        // The case config validation must reject: a field named after a namespace.
        let nested = NestedContext::build(&context(&[("repo", "shadow"), ("repo.root", "/x")]));
        assert!(
            nested.collisions().contains("repo"),
            "the shadowed scalar must be reported, got {:?}",
            nested.collisions()
        );
        assert_eq!(
            nested
                .value()
                .get_attr("repo")
                .unwrap()
                .get_attr("root")
                .unwrap()
                .to_string(),
            "/x",
            "the useful, deeper value must survive"
        );
    }

    #[test]
    fn collision_is_reported_regardless_of_insertion_order() {
        let forward = NestedContext::build(&context(&[("a.b", "deep"), ("a", "shallow")]));
        let backward = NestedContext::build(&context(&[("a", "shallow"), ("a.b", "deep")]));
        assert_eq!(forward.collisions(), backward.collisions());
        assert!(forward.collisions().contains("a"));
    }

    #[test]
    fn malformed_keys_are_reported_rather_than_silently_dropped() {
        let nested =
            NestedContext::build(&context(&[("a..b", "x"), (".lead", "y"), ("trail.", "z")]));
        assert_eq!(
            nested.collisions().len(),
            3,
            "got {:?}",
            nested.collisions()
        );
    }

    #[test]
    fn reserved_prefixes_are_detected_for_field_keys() {
        assert_eq!(shadows_reserved_prefix("repo"), Some("repo"));
        assert_eq!(shadows_reserved_prefix("lookup"), Some("lookup"));
        assert_eq!(shadows_reserved_prefix("worktree"), Some("worktree"));
        assert_eq!(shadows_reserved_prefix("issue"), None);
        assert_eq!(shadows_reserved_prefix("base"), None);
        // Only the first segment matters.
        assert_eq!(shadows_reserved_prefix("my.repo"), None);
    }

    #[test]
    fn an_empty_context_yields_an_addressable_but_empty_value() {
        let nested = NestedContext::build(&Context::new());
        assert!(nested.value().get_attr("anything").unwrap().is_undefined());
    }
}

//! The versioned host contract, parsed once per process.
//!
//! Manifest validation, every host call and every activation read it. Parsing the JSON on each of
//! those was a measurable cost on the hottest path a plugin has, and it bought nothing: the file
//! is compiled in, so it cannot change while the app runs.
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

/// The contract as authors read it. The host serves this text, so it is the same bytes the
/// typed view below was parsed from.
pub const SOURCE: &str = include_str!("../contracts/catalog.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exposure {
    /// A package may declare it in `extensions.access`.
    Public,
    /// Native-equivalent, or a host decision; no declaration can reach it.
    Protected,
    /// Window and process plumbing that is not an operation at all.
    Internal,
}

/// How a public operation is kept inside a workspace-scoped plugin's workspaces.
///
/// Declared per operation rather than inferred from argument names, because a name proves
/// nothing: `worktreeId` is a path the command trusts, `id` is sometimes a browser and sometimes
/// a comment. An operation the host cannot bind to a workspace says so, and a scoped plugin is
/// refused it — the alternative, passing anything the resolver did not recognise, is how
/// another workspace's database session once slipped through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Touches nothing a workspace owns.
    None,
    /// The host's dispatcher acts only inside the caller's validated workspace context.
    Context,
    /// Each argument listed in `resources` is resolved by the host and must be in scope.
    Arguments,
    /// Reads or changes state across workspaces, or names something the host cannot attribute
    /// to one. Refused to a plugin limited to chosen workspaces.
    Global,
}

/// What an argument listed in `resources` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    /// A registered project's id.
    Project,
    /// A worktree id. Worktree ids are paths, so an unresolved one is any directory on disk.
    Worktree,
    /// A live terminal, agent, native session or browser pane, each of which belongs to one
    /// worktree or to Home.
    Session,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Operation {
    pub id: String,
    pub exposure: Exposure,
    pub reason: String,
    #[serde(default)]
    pub scope: Option<Scope>,
    /// Argument path (dot-separated for nested objects) to the kind of resource it names.
    #[serde(default)]
    pub resources: BTreeMap<String, Resource>,
}

#[derive(Debug, Deserialize)]
struct Surface {
    id: String,
    #[serde(default)]
    kinds: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Event {
    id: String,
}

#[derive(Debug, Deserialize)]
struct Raw {
    operations: Vec<Operation>,
    surfaces: Vec<Surface>,
    events: Vec<Event>,
}

#[derive(Debug)]
pub struct Catalog {
    operations: BTreeMap<String, Operation>,
    surfaces: BTreeMap<String, BTreeSet<String>>,
    events: BTreeSet<String>,
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let raw: Raw = serde_json::from_str(SOURCE).expect("the compiled-in contract catalog parses");
    Catalog {
        operations: raw
            .operations
            .into_iter()
            .map(|o| (o.id.clone(), o))
            .collect(),
        surfaces: raw
            .surfaces
            .into_iter()
            .map(|s| (s.id, s.kinds.into_iter().collect()))
            .collect(),
        events: raw.events.into_iter().map(|e| e.id).collect(),
    }
});

pub fn catalog() -> &'static Catalog {
    &CATALOG
}

impl Catalog {
    pub fn operation(&self, id: &str) -> Option<&Operation> {
        self.operations.get(id)
    }
    /// The operation, if a package may declare it.
    pub fn public(&self, id: &str) -> Option<&Operation> {
        self.operation(id)
            .filter(|o| o.exposure == Exposure::Public)
    }
    pub fn operations(&self) -> impl Iterator<Item = &Operation> {
        self.operations.values()
    }
    /// Whether `id` is a public surface accepting contributions of `kind`.
    pub fn surface(&self, id: &str, kind: &str) -> bool {
        self.surfaces.get(id).is_some_and(|k| k.contains(kind))
    }
    pub fn event(&self, id: &str) -> bool {
        self.events.contains(id)
    }
}

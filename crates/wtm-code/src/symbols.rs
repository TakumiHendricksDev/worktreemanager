//! Go to Class and Go to Symbol: where things are defined, found by reading definitions' shapes.
//!
//! # Syntax, not semantics
//!
//! A language server would know what `EventList` *refers to*; this knows only where a line says
//! `class EventList`. That is the difference between go-to-definition and go-to-a-definition-named,
//! and for finding your way around a codebase it is most of the value at none of the cost — no
//! interpreter, no virtualenv, no `node_modules`, nothing per project to configure. Two classes
//! with one name are both offered, and the user picks.
//!
//! # A table of shapes per language
//!
//! Each language is a list of rules: a regex for a definition's line, the kind it defines, and
//! whether what it defines can contain others. Containment is by indentation — a `def` indented
//! under a `class` is its method — which is exact for Python and right for any code that is
//! formatted, which is all code an agent or a formatter has touched. Adding a language is a row.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

use regex::Regex;

use crate::{MAX_SEARCH_BYTES, resolve};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Class,
    Function,
    Method,
    Interface,
    Type,
    Enum,
    Struct,
    Trait,
    Module,
    Constant,
}

impl SymbolKind {
    /// What Go to Class offers: the things that are types rather than code.
    #[must_use]
    pub fn is_type(self) -> bool {
        matches!(
            self,
            Self::Class | Self::Interface | Self::Type | Self::Enum | Self::Struct | Self::Trait
        )
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Function => "function",
            Self::Method => "method",
            Self::Interface => "interface",
            Self::Type => "type",
            Self::Enum => "enum",
            Self::Struct => "struct",
            Self::Trait => "trait",
            Self::Module => "module",
            Self::Constant => "constant",
        }
    }
}

/// One definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// The class, impl or function it is defined inside, when it is.
    pub container: Option<String>,
    pub path: String,
    /// 1-based.
    pub line: u32,
}

/// What a rule's match defines.
#[derive(Clone, Copy)]
enum Defines {
    /// A symbol of this kind, which may contain others when `scope` is set.
    Symbol { kind: SymbolKind, scope: bool },
    /// A scope that is not itself a symbol: Rust's `impl Foo`, whose methods belong to `Foo`.
    Scope,
}

struct Rule {
    regex: Regex,
    defines: Defines,
}

fn rule(pattern: &str, defines: Defines) -> Rule {
    Rule {
        // The patterns are literals in this file, so a bad one is a bug that the tests catch.
        regex: Regex::new(pattern).unwrap_or_else(|e| panic!("symbol rule `{pattern}`: {e}")),
        defines,
    }
}

const fn symbol(kind: SymbolKind, scope: bool) -> Defines {
    Defines::Symbol { kind, scope }
}

/// Group 1 is always the indentation and group 2 the name.
static PYTHON: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule(
            r"^(\s*)class\s+([A-Za-z_]\w*)",
            symbol(SymbolKind::Class, true),
        ),
        rule(
            r"^(\s*)(?:async\s+)?def\s+([A-Za-z_]\w*)",
            symbol(SymbolKind::Function, true),
        ),
        rule(
            r"^()([A-Z][A-Z0-9_]+)\s*(?::[^=]+)?=[^=]",
            symbol(SymbolKind::Constant, false),
        ),
    ]
});

static SCRIPT: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let modifiers = r"(?:(?:export|default|declare|abstract)\s+)*";
    vec![
        rule(
            &format!(r"^(\s*){modifiers}class\s+([A-Za-z_$][\w$]*)"),
            symbol(SymbolKind::Class, true),
        ),
        rule(
            &format!(r"^(\s*){modifiers}(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)"),
            symbol(SymbolKind::Function, false),
        ),
        rule(
            &format!(
                r"^(\s*){modifiers}(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|(?:<[^>]*>\s*)?\([^)]*\)\s*(?::[^=]+)?=>|[A-Za-z_$][\w$]*\s*=>)"
            ),
            symbol(SymbolKind::Function, false),
        ),
        rule(
            &format!(r"^(\s*){modifiers}interface\s+([A-Za-z_$][\w$]*)"),
            symbol(SymbolKind::Interface, true),
        ),
        rule(
            &format!(r"^(\s*){modifiers}type\s+([A-Za-z_$][\w$]*)\s*(?:<[^=]*>)?\s*="),
            symbol(SymbolKind::Type, false),
        ),
        rule(
            &format!(r"^(\s*){modifiers}(?:const\s+)?enum\s+([A-Za-z_$][\w$]*)"),
            symbol(SymbolKind::Enum, false),
        ),
        // A method: an indented name, its parameters, and an opening brace — after any of the
        // modifiers a class member can carry. Keywords that share the shape are refused below.
        rule(
            r"^(\s+)(?:(?:public|private|protected|static|readonly|abstract|override|async|get|set)\s+)*\*?([A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\s*\([^)]*\)\s*(?::\s*[^{=]+)?\{\s*$",
            symbol(SymbolKind::Method, false),
        ),
    ]
});

static RUST: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let visibility = r"(?:pub(?:\([^)]*\))?\s+)?";
    vec![
        rule(
            &format!(
                r"^(\s*){visibility}(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+\S+\s+)?fn\s+([A-Za-z_]\w*)"
            ),
            symbol(SymbolKind::Function, false),
        ),
        rule(
            &format!(r"^(\s*){visibility}struct\s+([A-Za-z_]\w*)"),
            symbol(SymbolKind::Struct, false),
        ),
        rule(
            &format!(r"^(\s*){visibility}enum\s+([A-Za-z_]\w*)"),
            symbol(SymbolKind::Enum, false),
        ),
        rule(
            &format!(r"^(\s*){visibility}(?:unsafe\s+)?trait\s+([A-Za-z_]\w*)"),
            symbol(SymbolKind::Trait, true),
        ),
        rule(
            &format!(r"^(\s*){visibility}type\s+([A-Za-z_]\w*)"),
            symbol(SymbolKind::Type, false),
        ),
        rule(
            &format!(r"^(\s*){visibility}mod\s+([A-Za-z_]\w*)"),
            symbol(SymbolKind::Module, true),
        ),
        rule(
            &format!(r"^(\s*){visibility}(?:const|static)\s+(?:mut\s+)?([A-Z][A-Z0-9_]*)\s*:"),
            symbol(SymbolKind::Constant, false),
        ),
        rule(
            r"^(\s*)macro_rules!\s*([A-Za-z_]\w*)",
            symbol(SymbolKind::Function, false),
        ),
        // `impl Trait for Type` and `impl Type`: the methods inside belong to the type.
        rule(
            r"^(\s*)(?:unsafe\s+)?impl\b(?:<[^>]*>)?\s+(?:[\w:]+(?:<[^>]*>)?\s+for\s+)?(?:&?\w+::)*([A-Za-z_]\w*)",
            Defines::Scope,
        ),
    ]
});

/// Names that look like a method's in a script's grammar and are statements.
const SCRIPT_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "catch", "function", "return", "with", "else", "do", "try",
];

fn rules_for(path: &str) -> Option<(&'static [Rule], Syntax)> {
    let extension = path.rsplit('.').next().unwrap_or("");
    match extension {
        "py" | "pyi" => Some((&PYTHON, Syntax::Python)),
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "vue" | "svelte" => {
            Some((&SCRIPT, Syntax::Script))
        }
        "rs" => Some((&RUST, Syntax::Rust)),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Syntax {
    Python,
    Script,
    Rust,
}

/// Every definition in one file's text.
#[must_use]
pub fn symbols_in(path: &str, text: &str) -> Vec<Symbol> {
    let Some((rules, syntax)) = rules_for(path) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    // Scopes a definition may be inside: (indentation, name, whether it is a type).
    let mut scopes: Vec<(usize, String, bool)> = Vec::new();
    let mut in_docstring = false;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if syntax == Syntax::Python {
            // A triple-quoted string can hold lines that look exactly like a `def`; count its
            // delimiters and skip what is inside, so documentation is not indexed as code.
            let quotes = trimmed.matches("\"\"\"").count() + trimmed.matches("'''").count();
            let was = in_docstring;
            if quotes % 2 == 1 {
                in_docstring = !in_docstring;
            }
            if was {
                continue;
            }
        }
        if trimmed.is_empty() || is_comment(trimmed, syntax) {
            continue;
        }
        for rule in rules {
            let Some(caps) = rule.regex.captures(line) else {
                continue;
            };
            let indent = caps.get(1).map_or(0, |m| m.as_str().len());
            let Some(name) = caps.get(2).map(|m| m.as_str()) else {
                continue;
            };
            if syntax == Syntax::Script && SCRIPT_KEYWORDS.contains(&name) {
                continue;
            }
            while scopes.last().is_some_and(|(depth, _, _)| *depth >= indent) {
                scopes.pop();
            }
            let container = scopes.last().map(|(_, name, _)| name.clone());
            let in_type = scopes.last().is_some_and(|(_, _, is_type)| *is_type);
            match rule.defines {
                Defines::Symbol { kind, scope } => {
                    let kind = if kind == SymbolKind::Function && in_type {
                        SymbolKind::Method
                    } else {
                        kind
                    };
                    // A method-shaped line outside any type is a call with a block, not a method.
                    if kind == SymbolKind::Method && !in_type {
                        break;
                    }
                    found.push(Symbol {
                        name: name.to_owned(),
                        kind,
                        container,
                        path: path.to_owned(),
                        line: u32::try_from(index + 1).unwrap_or(u32::MAX),
                    });
                    if scope {
                        scopes.push((indent, name.to_owned(), kind.is_type()));
                    }
                }
                Defines::Scope => scopes.push((indent, name.to_owned(), true)),
            }
            break;
        }
    }
    found
}

fn is_comment(trimmed: &str, syntax: Syntax) -> bool {
    match syntax {
        Syntax::Python => trimmed.starts_with('#'),
        Syntax::Script | Syntax::Rust => {
            trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*')
        }
    }
}

/// Every definition in a worktree, kept per file and re-read only for files that changed.
#[derive(Debug, Default)]
pub struct SymbolIndex {
    files: HashMap<String, (Option<u64>, u64, Vec<Symbol>)>,
}

impl SymbolIndex {
    /// Bring the index up to date with a file list: read new and changed files, drop gone ones.
    ///
    /// A file is compared by its modification time and size, which is one `stat` for each file
    /// that did not change — the cost of a refresh on a repository nobody has touched.
    pub fn update(&mut self, root: &Path, paths: &[String]) {
        let mut seen = HashSet::with_capacity(paths.len());
        for path in paths {
            if rules_for(path).is_none() {
                continue;
            }
            let Ok(full) = resolve(root, path) else {
                continue;
            };
            let Ok(meta) = fs::metadata(&full) else {
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_SEARCH_BYTES {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| u64::try_from(d.as_millis()).ok());
            let size = meta.len();
            let fresh = self
                .files
                .get(path)
                .is_some_and(|(m, s, _)| *m == mtime && *s == size);
            if !fresh {
                let symbols = fs::read(&full)
                    .map(|bytes| symbols_in(path, &String::from_utf8_lossy(&bytes)))
                    .unwrap_or_default();
                self.files.insert(path.clone(), (mtime, size, symbols));
            }
            seen.insert(path.clone());
        }
        self.files.retain(|path, _| seen.contains(path));
    }

    /// The best `limit` definitions for a query, best first. `types_only` is Go to Class.
    ///
    /// A query with a dot — `EventList.get` — looks for `get` inside something named like
    /// `EventList`.
    #[must_use]
    pub fn query(&self, query: &str, types_only: bool, limit: usize) -> Vec<Symbol> {
        let query = query.trim();
        let (container_query, name_query) = match query.rsplit_once('.') {
            Some((container, name)) => (Some(container), name),
            None => (None, query),
        };
        let mut hits: Vec<(i32, &Symbol)> = self
            .files
            .values()
            .flat_map(|(_, _, symbols)| symbols)
            .filter(|symbol| !types_only || symbol.kind.is_type())
            .filter_map(|symbol| {
                let rank = score(&symbol.name, name_query)?;
                if let Some(wanted) = container_query {
                    score(symbol.container.as_deref()?, wanted)?;
                }
                Some((rank, symbol))
            })
            .collect();
        hits.sort_by(|(a, x), (b, y)| {
            b.cmp(a)
                .then(x.name.len().cmp(&y.name.len()))
                .then(x.kind.is_type().cmp(&y.kind.is_type()).reverse())
                .then(x.path.cmp(&y.path))
                .then(x.line.cmp(&y.line))
        });
        hits.into_iter()
            .take(limit)
            .map(|(_, symbol)| symbol.clone())
            .collect()
    }

    /// Every definition with exactly this name: what ⌘-click on a name offers.
    #[must_use]
    pub fn definitions(&self, name: &str) -> Vec<Symbol> {
        let mut found: Vec<Symbol> = self
            .files
            .values()
            .flat_map(|(_, _, symbols)| symbols)
            .filter(|symbol| symbol.name == name)
            .cloned()
            .collect();
        found.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        found
    }
}

/// How well a name matches a query, or `None` when it does not. Higher is better.
///
/// The tiers of the composer's file scorer, with camel humps between prefix and substring: an exact
/// name, then a prefix, then the query spelling the starts of the name's words (`EvLi` →
/// `EventList`, `gq` → `get_queryset`), then a substring, then a loose subsequence, tightest first.
fn score(name: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let lower = name.to_lowercase();
    let q = query.to_lowercase();
    if lower == q {
        return Some(5000);
    }
    if lower.starts_with(&q) {
        return Some(4000);
    }
    if humps(name, query) {
        return Some(3000);
    }
    if lower.contains(&q) {
        return Some(2000);
    }
    let mut at = 0;
    let mut first = None;
    for c in q.chars() {
        let found = lower[at..].find(c)? + at;
        first.get_or_insert(found);
        at = found + c.len_utf8();
    }
    let span = i32::try_from(at - first.unwrap_or(0)).unwrap_or(i32::MAX);
    Some((1000 - span).max(1))
}

/// Whether the query is a run of prefixes of the name's words, in order, each starting a word.
fn humps(name: &str, query: &str) -> bool {
    let words = words(name);
    let query: Vec<char> = query.to_lowercase().chars().filter(|c| *c != '_').collect();
    // The first character must start the first word, or `ist` would hump-match `EventList`.
    match (words.first(), query.first()) {
        (Some(word), Some(c)) if word.first() == Some(c) => hump_walk(&words, &query),
        _ => false,
    }
}

/// Take one to all of this word's leading characters and continue with the next word, or skip it.
fn hump_walk(words: &[Vec<char>], query: &[char]) -> bool {
    if query.is_empty() {
        return true;
    }
    let Some((word, rest)) = words.split_first() else {
        return false;
    };
    for take in (1..=word.len().min(query.len())).rev() {
        if word[..take] == query[..take] && hump_walk(rest, &query[take..]) {
            return true;
        }
    }
    hump_walk(rest, query)
}

/// A name's words, lowercased: split at underscores, and where lower case turns upper.
fn words(name: &str) -> Vec<Vec<char>> {
    let mut words: Vec<Vec<char>> = Vec::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if c == '_' || c == '$' {
            previous_lower = false;
            words.push(Vec::new());
            continue;
        }
        if words.is_empty() || (c.is_uppercase() && previous_lower) {
            words.push(Vec::new());
        }
        if let Some(word) = words.last_mut() {
            word.extend(c.to_lowercase());
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
    }
    words.retain(|w| !w.is_empty());
    words
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn names(symbols: &[Symbol]) -> Vec<(String, SymbolKind, Option<String>, u32)> {
        symbols
            .iter()
            .map(|s| (s.name.clone(), s.kind, s.container.clone(), s.line))
            .collect()
    }

    #[test]
    fn python_methods_belong_to_their_class_and_docstrings_are_not_code() {
        let text = "\
MAX_EVENTS = 25


class EventList(ListView):
    \"\"\"Lists events.

    def example():  # documentation, not a definition
    \"\"\"

    def get_queryset(self):
        def key(event):
            return event.starts_at
        return sorted(super().get_queryset(), key=key)

    async def refresh(self):
        pass


def event_detail(request, pk):
    x = \"def fake():\"
";
        let found = names(&symbols_in("apps/events/views.py", text));
        assert_eq!(
            found,
            [
                ("MAX_EVENTS".to_owned(), SymbolKind::Constant, None, 1),
                ("EventList".to_owned(), SymbolKind::Class, None, 4),
                (
                    "get_queryset".to_owned(),
                    SymbolKind::Method,
                    Some("EventList".to_owned()),
                    10
                ),
                (
                    "key".to_owned(),
                    SymbolKind::Function,
                    Some("get_queryset".to_owned()),
                    11
                ),
                (
                    "refresh".to_owned(),
                    SymbolKind::Method,
                    Some("EventList".to_owned()),
                    15
                ),
                ("event_detail".to_owned(), SymbolKind::Function, None, 19),
            ]
        );
    }

    #[test]
    fn script_definitions_include_arrow_functions_types_and_class_methods_but_not_statements() {
        let text = "\
export interface Props { id: number }
export type Id = string;
export enum Mode { A, B }
export default class EventTable extends Base {
  private count = 0;
  async load(id: string): Promise<void> {
    if (id) {
      this.count += 1;
    }
  }
  get size() {
    return this.count;
  }
}
export const format = (value: number): string => String(value);
const handler = async (event) => {};
function helper() {}
foo(bar) {
";
        let found = names(&symbols_in("src/table.ts", text));
        assert_eq!(
            found,
            [
                ("Props".to_owned(), SymbolKind::Interface, None, 1),
                ("Id".to_owned(), SymbolKind::Type, None, 2),
                ("Mode".to_owned(), SymbolKind::Enum, None, 3),
                ("EventTable".to_owned(), SymbolKind::Class, None, 4),
                (
                    "load".to_owned(),
                    SymbolKind::Method,
                    Some("EventTable".to_owned()),
                    6
                ),
                (
                    "size".to_owned(),
                    SymbolKind::Method,
                    Some("EventTable".to_owned()),
                    11
                ),
                ("format".to_owned(), SymbolKind::Function, None, 15),
                ("handler".to_owned(), SymbolKind::Function, None, 16),
                ("helper".to_owned(), SymbolKind::Function, None, 17),
            ]
        );
    }

    #[test]
    fn rust_methods_in_an_impl_belong_to_the_type_it_is_for() {
        let text = "\
pub struct Cache {
    items: Vec<u8>,
}

impl<T> Display for Cache {
    fn fmt(&self) {}
}

impl Cache {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }
}

pub(crate) trait Store {
    fn get(&self);
}

const LIMIT: usize = 3;
macro_rules! bail { () => {} }
pub async fn run() {}
";
        let found = names(&symbols_in("src/cache.rs", text));
        assert_eq!(
            found,
            [
                ("Cache".to_owned(), SymbolKind::Struct, None, 1),
                (
                    "fmt".to_owned(),
                    SymbolKind::Method,
                    Some("Cache".to_owned()),
                    6
                ),
                (
                    "new".to_owned(),
                    SymbolKind::Method,
                    Some("Cache".to_owned()),
                    10
                ),
                ("Store".to_owned(), SymbolKind::Trait, None, 15),
                (
                    "get".to_owned(),
                    SymbolKind::Method,
                    Some("Store".to_owned()),
                    16
                ),
                ("LIMIT".to_owned(), SymbolKind::Constant, None, 19),
                ("bail".to_owned(), SymbolKind::Function, None, 20),
                ("run".to_owned(), SymbolKind::Function, None, 21),
            ]
        );
    }

    #[test]
    fn a_file_in_a_language_with_no_rules_has_no_symbols() {
        assert!(symbols_in("notes.md", "class Nothing:\n").is_empty());
    }

    fn index(files: &[(&str, &str)]) -> (tempfile::TempDir, SymbolIndex) {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        let mut index = SymbolIndex::default();
        let paths: Vec<String> = files.iter().map(|(p, _)| (*p).to_owned()).collect();
        index.update(dir.path(), &paths);
        (dir, index)
    }

    #[test]
    fn camel_humps_and_snake_humps_find_a_name_from_its_word_starts() {
        let (_dir, index) = index(&[(
            "a.py",
            "class EventList:\n    def get_queryset(self):\n        pass\nclass Listing:\n    pass\n",
        )]);

        let top = |q: &str| index.query(q, false, 5).first().map(|s| s.name.clone());
        assert_eq!(top("EvLi").as_deref(), Some("EventList"));
        assert_eq!(top("gq").as_deref(), Some("get_queryset"));
        assert_eq!(top("getq").as_deref(), Some("get_queryset"));
        // `ist` is inside both names but starts neither's word, so it is only a substring match.
        assert!(!humps("EventList", "ist"));
    }

    #[test]
    fn go_to_class_offers_only_types_and_a_dotted_query_searches_inside_one() {
        let (_dir, index) = index(&[(
            "a.py",
            "class EventList:\n    def get(self):\n        pass\nclass Other:\n    def get(self):\n        pass\ndef event_list():\n    pass\n",
        )]);

        let classes: Vec<String> = index
            .query("event", true, 10)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(classes, ["EventList"]);

        let inside = index.query("EventList.get", false, 10);
        assert_eq!(inside.len(), 1);
        assert_eq!(inside[0].container.as_deref(), Some("EventList"));
    }

    #[test]
    fn definitions_are_every_exact_match_and_an_update_rereads_only_what_changed() {
        let (dir, mut index) = index(&[
            ("a.py", "def run():\n    pass\n"),
            ("b.py", "def run():\n    pass\ndef running():\n    pass\n"),
        ]);
        assert_eq!(index.definitions("run").len(), 2);

        std::fs::write(dir.path().join("a.py"), "def walk():\n    pass\n").unwrap();
        // Make sure the change is visible to a coarse clock by changing the size too.
        index.update(dir.path(), &["a.py".to_owned(), "b.py".to_owned()]);
        assert_eq!(index.definitions("run").len(), 1);
        assert_eq!(index.definitions("walk").len(), 1);

        index.update(dir.path(), &["a.py".to_owned()]);
        assert!(
            index.definitions("running").is_empty(),
            "b.py left the list"
        );
    }
}

//! Template token scopes.
//!
//! # Why scopes are a type
//!
//! Not every token exists at every point in the pipeline. `worktree.path` cannot
//! be referenced by `naming.branch`, because the branch name is computed in order
//! to *decide* the path — the worktree does not exist yet. Likewise `lookup.*` is
//! meaningless before the enrich stage has run.
//!
//! Left unchecked, that class of mistake surfaces as a template silently rendering
//! an empty string, and you get a branch called `experiment/ACME-0000-` at 6pm.
//! So each template position declares a [`TokenScope`], and config validation
//! rejects an out-of-scope token *at load time*, with the file and line — see
//! [`crate::error::ConfigError::TokenOutOfScope`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// A namespace of template tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenSet {
    /// Bare field keys, post-normalization.
    Fields,
    /// `computed.<key>`.
    Computed,
    /// `lookup.<id>.<key>`.
    Lookup,
    /// `vars.<key>` from `[project.vars]`.
    Vars,
    /// `repo.{root,parent,name,git_common_dir,main_branch}`.
    Repo,
    /// `worktree.{path,dirname,branch,head}` — only after a worktree exists.
    Worktree,
    /// `env.<KEY>` from configured display sources plus a few process values.
    Env,
    /// `os.{uid,gid,home,platform}`.
    Os,
    /// `now.{date,iso,unix}`, from the injected clock.
    Now,
    /// `matched_branch`, only while resolving an adopted existing branch.
    MatchedBranch,
}

impl TokenSet {
    /// The token prefix, or `None` for bare field names.
    #[must_use]
    pub const fn prefix(self) -> Option<&'static str> {
        match self {
            Self::Fields => None,
            Self::Computed => Some("computed"),
            Self::Lookup => Some("lookup"),
            Self::Vars => Some("vars"),
            Self::Repo => Some("repo"),
            Self::Worktree => Some("worktree"),
            Self::Env => Some("env"),
            Self::Os => Some("os"),
            Self::Now => Some("now"),
            Self::MatchedBranch => Some("matched_branch"),
        }
    }
}

/// Every namespace prefix, in declaration order.
pub const RESERVED_PREFIXES: &[&str] = &[
    "computed",
    "lookup",
    "vars",
    "repo",
    "worktree",
    "env",
    "os",
    "now",
    "matched_branch",
];

/// Which namespace a dotted token belongs to.
///
/// A bare name is a field reference; a recognized first segment names its namespace.
/// Single definition, used by the template adapter to classify tokens and by config
/// validation to reject a field key that would shadow a namespace.
#[must_use]
pub fn namespace_of(token: &str) -> TokenSet {
    match token.split('.').next().unwrap_or(token) {
        "computed" => TokenSet::Computed,
        "lookup" => TokenSet::Lookup,
        "vars" => TokenSet::Vars,
        "repo" => TokenSet::Repo,
        "worktree" => TokenSet::Worktree,
        "env" => TokenSet::Env,
        "os" => TokenSet::Os,
        "now" => TokenSet::Now,
        "matched_branch" => TokenSet::MatchedBranch,
        _ => TokenSet::Fields,
    }
}

/// The reserved namespace `key` would shadow, if any.
///
/// A field named `repo` makes `{{ repo.root }}` resolve against that field's string
/// instead of the repository facts — which fails as an *empty render*, not an error.
/// For something that names branches, that is the worst possible failure mode, so
/// config validation rejects it at load time.
#[must_use]
pub fn shadows_reserved_prefix(key: &str) -> Option<&'static str> {
    let head = key.split('.').next().unwrap_or(key);
    RESERVED_PREFIXES
        .iter()
        .copied()
        .find(|prefix| *prefix == head)
}

/// Which token sets a template position may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenScope {
    /// Human-readable position, e.g. `naming.branch`, used in error messages.
    pub position: String,
    pub allowed: BTreeSet<TokenSet>,
}

impl TokenScope {
    pub fn new(position: impl Into<String>, allowed: impl IntoIterator<Item = TokenSet>) -> Self {
        Self {
            position: position.into(),
            allowed: allowed.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn allows(&self, set: TokenSet) -> bool {
        self.allowed.contains(&set)
    }

    /// Always-available sets: constants, repo facts, the OS and the clock.
    fn ambient() -> [TokenSet; 4] {
        [TokenSet::Vars, TokenSet::Repo, TokenSet::Os, TokenSet::Now]
    }

    /// A field's `normalize`: raw input only. No lookups — normalization is what
    /// *produces* the key a lookup will query, so depending on one would be
    /// circular.
    #[must_use]
    pub fn normalize(field_key: &str) -> Self {
        let mut allowed: BTreeSet<_> = Self::ambient().into_iter().collect();
        allowed.insert(TokenSet::Fields);
        Self {
            position: format!("field.{field_key}.normalize"),
            allowed,
        }
    }

    /// A `[[lookup]]`'s argv and `when`: fields are normalized by now, but no
    /// other lookup's results are visible (lookups do not chain).
    #[must_use]
    pub fn lookup(id: &str) -> Self {
        let mut allowed: BTreeSet<_> = Self::ambient().into_iter().collect();
        allowed.insert(TokenSet::Fields);
        Self {
            position: format!("lookup.{id}"),
            allowed,
        }
    }

    /// A `[computed]` template: fields and lookups, plus earlier computed values.
    #[must_use]
    pub fn computed(key: &str) -> Self {
        let mut allowed: BTreeSet<_> = Self::ambient().into_iter().collect();
        allowed.extend([TokenSet::Fields, TokenSet::Lookup, TokenSet::Computed]);
        Self {
            position: format!("computed.{key}"),
            allowed,
        }
    }

    /// `naming.branch` / `naming.directory`: everything except the worktree, which
    /// does not exist yet, and `env`, which is read from a worktree that does not
    /// exist yet either.
    #[must_use]
    pub fn naming(which: &str) -> Self {
        let mut allowed: BTreeSet<_> = Self::ambient().into_iter().collect();
        allowed.extend([TokenSet::Fields, TokenSet::Lookup, TokenSet::Computed]);
        Self {
            position: format!("naming.{which}"),
            allowed,
        }
    }

    /// The directory template for an adopted branch, which additionally sees
    /// `matched_branch`.
    #[must_use]
    pub fn matched_directory() -> Self {
        let mut scope = Self::naming("existing_branch_match.directory");
        scope.allowed.insert(TokenSet::MatchedBranch);
        scope
    }

    /// Anything that runs against an existing worktree: setup, actions, removal
    /// steps, display. Everything is in scope here.
    #[must_use]
    pub fn worktree_command(position: impl Into<String>) -> Self {
        let mut allowed: BTreeSet<_> = Self::ambient().into_iter().collect();
        allowed.extend([
            TokenSet::Fields,
            TokenSet::Lookup,
            TokenSet::Computed,
            TokenSet::Worktree,
            TokenSet::Env,
        ]);
        Self {
            position: position.into(),
            allowed,
        }
    }

    /// Why a token is unavailable here — the text that lands in the error, so it
    /// has to explain the ordering rather than just restate the rule.
    #[must_use]
    pub fn reason_for(&self, set: TokenSet) -> String {
        match set {
            TokenSet::Worktree => format!(
                "the worktree does not exist yet at {}; it is created after naming is resolved",
                self.position
            ),
            TokenSet::Env => format!(
                "environment files are read from a worktree, which does not exist yet at {}",
                self.position
            ),
            TokenSet::Lookup => format!(
                "lookups have not run yet at {}, and lookups cannot reference each other",
                self.position
            ),
            TokenSet::Computed => {
                format!("computed values are evaluated after {}", self.position)
            }
            TokenSet::MatchedBranch => {
                "`matched_branch` exists only while adopting an existing branch".to_owned()
            }
            _ => format!("not available at {}", self.position),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_cannot_reference_the_worktree() {
        // The bug this prevents: naming.branch referencing worktree.path renders
        // empty and produces a corrupt branch name.
        let scope = TokenScope::naming("branch");
        assert!(!scope.allows(TokenSet::Worktree));
        assert!(!scope.allows(TokenSet::Env));
        assert!(scope.allows(TokenSet::Computed));
        assert!(scope.allows(TokenSet::Lookup));
        assert!(scope.allows(TokenSet::Fields));
    }

    #[test]
    fn normalize_cannot_reference_lookups() {
        // Would be circular: normalize produces the key the lookup queries.
        let scope = TokenScope::normalize("issue");
        assert!(!scope.allows(TokenSet::Lookup));
        assert!(scope.allows(TokenSet::Fields));
        assert!(scope.allows(TokenSet::Vars));
    }

    #[test]
    fn lookups_cannot_chain() {
        assert!(!TokenScope::lookup("jira").allows(TokenSet::Lookup));
    }

    #[test]
    fn worktree_commands_see_everything() {
        let scope = TokenScope::worktree_command("setup");
        for set in [
            TokenSet::Fields,
            TokenSet::Computed,
            TokenSet::Lookup,
            TokenSet::Vars,
            TokenSet::Repo,
            TokenSet::Worktree,
            TokenSet::Env,
            TokenSet::Os,
            TokenSet::Now,
        ] {
            assert!(
                scope.allows(set),
                "{set:?} should be in scope for a worktree command"
            );
        }
    }

    #[test]
    fn matched_directory_adds_only_the_matched_branch() {
        let scope = TokenScope::matched_directory();
        assert!(scope.allows(TokenSet::MatchedBranch));
        assert!(!scope.allows(TokenSet::Worktree));
    }

    #[test]
    fn reasons_explain_ordering_rather_than_restating_the_rule() {
        let scope = TokenScope::naming("branch");
        let reason = scope.reason_for(TokenSet::Worktree);
        assert!(
            reason.contains("does not exist yet"),
            "unhelpful reason: {reason}"
        );
    }
}

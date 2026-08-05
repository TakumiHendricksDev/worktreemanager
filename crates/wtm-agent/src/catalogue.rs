//! Every agent wtm knows how to drive.
//!
//! Compiled in, and the shape is copied from `src-tauri/src/openers.rs` on purpose: a static
//! slice, entries tried in order, availability probed against the machine rather than declared.
//! ARCHITECTURE already records the reasoning for that choice there — *"openers are built in and
//! identical in every repository, so they need no `wtm.toml` entry and trigger no trust
//! prompt"* — and a provider is the same kind of fact.
//!
//! What a repository *can* say is which of these it offers and how they start: model, effort,
//! mode, extra argv, env, MCP servers. What it cannot say is the program name, because that is
//! what makes the pane say "Codex", and a label a branch can set is not a label.
//!
//! **Display order is this slice's order**, not alphabetical and not config's choice. One less
//! thing for a repo to get wrong, and it means the picker does not reshuffle when a project is
//! switched.

use crate::codex::{self, Codex};
use crate::provider::Provider;

/// One agent, and the metadata the UI needs before a session exists.
pub struct ProviderEntry {
    pub id: &'static str,
    pub label: &'static str,
    /// One line for the launcher menu. Present tense, no trailing period — matching how
    /// `openers.rs` writes its labels.
    pub blurb: &'static str,
    pub provider: &'static (dyn Provider + Sync),
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderEntry")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// The catalogue, in display order.
pub const CATALOGUE: &[ProviderEntry] = &[ProviderEntry {
    id: codex::ID,
    label: "Codex",
    blurb: "OpenAI Codex, over its app server",
    provider: &Codex,
}];

/// Look one up by id.
///
/// Returns `None` for an unknown id rather than falling back to the first entry. A config naming
/// an agent this build does not have should say so, not quietly start a different one.
#[must_use]
pub fn entry(id: &str) -> Option<&'static ProviderEntry> {
    CATALOGUE.iter().find(|e| e.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_is_reachable_by_its_own_id() {
        for candidate in CATALOGUE {
            let found = entry(candidate.id).expect("an entry must be findable by its own id");
            assert_eq!(found.id, candidate.id);
        }
    }

    #[test]
    fn an_unknown_id_is_refused_rather_than_falling_back() {
        // A config naming an agent this build does not ship must not silently start another.
        assert!(entry("no-such-agent").is_none());
    }

    #[test]
    fn an_entry_id_matches_the_provider_it_points_at() {
        // The one way this slice can lie: an entry whose id says `codex` holding a provider that
        // reports something else, which would make every lookup route to the wrong protocol.
        for candidate in CATALOGUE {
            assert_eq!(
                candidate.provider.id().as_str(),
                candidate.id,
                "entry `{}` points at a provider that calls itself `{}`",
                candidate.id,
                candidate.provider.id()
            );
        }
    }
}

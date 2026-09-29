//! How a project's worktrees are arranged in the sidebar: their order, the groups they are
//! filed under, and which groups are folded away.
//!
//! # Favorites is a group, not a flag
//!
//! Starring used to be a boolean that floated a row to the top. Once rows can be dragged into
//! any order and filed into groups, a flag has nothing left to do: a star that re-sorts fights
//! the order the user just set by hand, and one that does not re-sort changes nothing on
//! screen. So the star became membership of one built-in group, [`FAVORITES`], which is always
//! first — and every worktree is in exactly one group. The alternative of showing a starred
//! worktree both under Favorites *and* under its own group was rejected because it puts two
//! rows in the list for one worktree: two selected rows, and arrow keys visiting it twice.
//!
//! The rows that are in no group live in the other built-in, [`UNGROUPED`], which is always
//! last. Making it a group rather than a separate list is what lets the whole layout be one
//! ordered list of the same shape, and lets the ungrouped section fold like any other.
//!
//! # Why every field name is one word
//!
//! The same struct is written to `config.toml` and — converted to a view in `src-tauri` — sent
//! to the frontend. One-word names spell the same in TOML's snake case and the frontend's camel
//! case, so a hand-edited file and the payload never disagree about what a key is called.
//!
//! # What is *not* checked here
//!
//! That the worktree paths exist. A layout is read without listing git, and worktrees come and
//! go behind the app's back, so a stale path is inert rather than an error: the sidebar renders
//! what git lists, placed by this layout where it can be, and skips what the layout names but
//! git does not. Removing a worktree through the app does tidy its entry away —
//! [`SidebarLayout::forget`] — but nothing depends on that having happened.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The built-in group a starred worktree lives in. Always first.
pub const FAVORITES: &str = "favorites";

/// The built-in group for worktrees filed nowhere else. Always last.
pub const UNGROUPED: &str = "ungrouped";

/// The longest name a group keeps, in characters. A sidebar row truncates long before this;
/// the cap only stops a pasted paragraph from being written into the config.
pub const MAX_NAME: usize = 80;

/// The arrangement for one project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarLayout {
    /// Every group, in display order. After [`normalized`](Self::normalized) this always
    /// starts with [`FAVORITES`] and ends with [`UNGROUPED`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<SidebarGroup>,
    /// For each starred worktree, the custom group it was starred *from*.
    ///
    /// This is what lets unstarring put a worktree back where it was rather than dumping it
    /// in the ungrouped section. Absent means "ungrouped", which is also the fallback when
    /// the group it names has since been deleted.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub origins: BTreeMap<String, String>,
}

/// One group of worktrees.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarGroup {
    /// Stable across renames. Chosen by the frontend when it creates the group, since it
    /// has to render the group before any round trip could name it.
    pub id: String,
    /// What the header says. Always empty for the two built-ins, whose labels are the
    /// frontend's copy rather than something stored.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub collapsed: bool,
    /// Worktree ids — absolute paths, the same strings the listing reports — in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worktrees: Vec<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference.
const fn is_false(value: &bool) -> bool {
    !*value
}

impl SidebarGroup {
    fn builtin(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            ..Self::default()
        }
    }

    fn is_builtin(&self) -> bool {
        self.id == FAVORITES || self.id == UNGROUPED
    }
}

impl SidebarLayout {
    /// The layout with its invariants restored.
    ///
    /// This is the one place those invariants are enforced, and it is deliberately forgiving:
    /// the input is either a hand-edited file or a frontend that applied an edit optimistically,
    /// and neither should be able to produce an error the user has to resolve. So each repair
    /// picks the reading that loses the least:
    ///
    /// - [`FAVORITES`] exists and is first; [`UNGROUPED`] exists and is last; custom groups keep
    ///   their relative order between them.
    /// - A group id seen twice keeps its first group. A group with no id is dropped, and its
    ///   worktrees fall through to ungrouped by being in no group.
    /// - A worktree listed twice stays in the first place it appears, reading favorites first —
    ///   a star is the more deliberate act. Empty ids are dropped.
    /// - Names are trimmed and capped at [`MAX_NAME`] characters; the built-ins' are cleared.
    /// - An origin survives only for a worktree that is starred and a custom group that exists.
    #[must_use]
    pub fn normalized(&self) -> Self {
        let mut seen_ids = BTreeSet::new();
        let mut favorites = None;
        let mut ungrouped = None;
        let mut custom = Vec::new();

        for group in &self.groups {
            let id = group.id.trim();
            if id.is_empty() || !seen_ids.insert(id.to_owned()) {
                continue;
            }
            let mut group = group.clone();
            id.clone_into(&mut group.id);
            match group.id.as_str() {
                FAVORITES => favorites = Some(group),
                UNGROUPED => ungrouped = Some(group),
                _ => custom.push(group),
            }
        }

        let mut groups = Vec::with_capacity(custom.len() + 2);
        groups.push(favorites.unwrap_or_else(|| SidebarGroup::builtin(FAVORITES)));
        groups.extend(custom);
        groups.push(ungrouped.unwrap_or_else(|| SidebarGroup::builtin(UNGROUPED)));

        let mut placed = BTreeSet::new();
        for group in &mut groups {
            if group.is_builtin() {
                group.name.clear();
            } else {
                group.name = group.name.trim().chars().take(MAX_NAME).collect();
            }
            group
                .worktrees
                .retain(|worktree| !worktree.is_empty() && placed.insert(worktree.clone()));
        }

        let starred: BTreeSet<&str> = groups[0].worktrees.iter().map(String::as_str).collect();
        let custom_ids: BTreeSet<&str> = groups
            .iter()
            .filter(|group| !group.is_builtin())
            .map(|group| group.id.as_str())
            .collect();
        let origins = self
            .origins
            .iter()
            .filter(|(worktree, group)| {
                starred.contains(worktree.as_str()) && custom_ids.contains(group.as_str())
            })
            .map(|(worktree, group)| (worktree.clone(), group.clone()))
            .collect();

        Self { groups, origins }
    }

    /// The normalized layout with the built-ins dropped wherever they carry nothing.
    ///
    /// For writing, not for reading: their positions are fixed, so an empty, open built-in says
    /// nothing a [`normalized`](Self::normalized) read would not put back. Without this a project
    /// that has never been touched would still grow a `sidebar` table the first time any layout
    /// was saved for it.
    #[must_use]
    pub fn compacted(&self) -> Self {
        let mut layout = self.normalized();
        layout
            .groups
            .retain(|group| !group.is_builtin() || group.collapsed || !group.worktrees.is_empty());
        layout
    }

    /// True when there is nothing to write — the sidebar would look exactly as it does with
    /// no layout at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.compacted().groups.is_empty()
    }

    /// Append `worktrees` to the Favorites group, for a config written before favorites were a
    /// group. Existing members keep their places; the legacy list is appended in the order it
    /// was stored, which is sorted by path.
    #[must_use]
    pub fn with_legacy_favorites(&self, worktrees: &[String]) -> Self {
        if worktrees.is_empty() {
            return self.normalized();
        }
        let mut layout = self.normalized();
        layout.groups[0].worktrees.extend(worktrees.iter().cloned());
        layout.normalized()
    }

    /// Take a worktree out of every group and origin. Returns whether anything changed.
    pub fn forget(&mut self, worktree: &str) -> bool {
        let mut changed = self.origins.remove(worktree).is_some();
        for group in &mut self.groups {
            let before = group.worktrees.len();
            group.worktrees.retain(|w| w != worktree);
            changed |= group.worktrees.len() != before;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: &str, worktrees: &[&str]) -> SidebarGroup {
        SidebarGroup {
            id: id.to_owned(),
            name: format!("{id} name"),
            collapsed: false,
            worktrees: worktrees.iter().map(|&w| w.to_owned()).collect(),
        }
    }

    fn ids(layout: &SidebarLayout) -> Vec<&str> {
        layout.groups.iter().map(|g| g.id.as_str()).collect()
    }

    fn members<'a>(layout: &'a SidebarLayout, id: &str) -> Vec<&'a str> {
        layout
            .groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.worktrees.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    #[test]
    fn an_empty_layout_normalizes_to_the_two_builtins_and_nothing_else() {
        let layout = SidebarLayout::default().normalized();
        assert_eq!(ids(&layout), [FAVORITES, UNGROUPED]);
        assert!(layout.groups.iter().all(|g| g.worktrees.is_empty()));
    }

    #[test]
    fn favorites_is_moved_first_and_ungrouped_last_whatever_order_they_were_stored_in() {
        let layout = SidebarLayout {
            groups: vec![
                group(UNGROUPED, &["/u"]),
                group("reviews", &["/r"]),
                group(FAVORITES, &["/f"]),
                group("spikes", &["/s"]),
            ],
            ..SidebarLayout::default()
        }
        .normalized();

        assert_eq!(ids(&layout), [FAVORITES, "reviews", "spikes", UNGROUPED]);
        assert_eq!(members(&layout, FAVORITES), ["/f"]);
    }

    #[test]
    fn a_worktree_listed_twice_stays_in_favorites_over_any_other_group() {
        // Favorites is read first on purpose: of the two places, starring is the deliberate one.
        let layout = SidebarLayout {
            groups: vec![group("reviews", &["/a", "/b"]), group(FAVORITES, &["/a"])],
            ..SidebarLayout::default()
        }
        .normalized();

        assert_eq!(members(&layout, FAVORITES), ["/a"]);
        assert_eq!(members(&layout, "reviews"), ["/b"]);
    }

    #[test]
    fn a_worktree_listed_twice_in_one_group_keeps_only_its_first_place() {
        let layout = SidebarLayout {
            groups: vec![group("reviews", &["/a", "/b", "/a"])],
            ..SidebarLayout::default()
        }
        .normalized();
        assert_eq!(members(&layout, "reviews"), ["/a", "/b"]);
    }

    #[test]
    fn a_duplicated_group_id_keeps_the_first_group_and_its_worktrees_fall_to_ungrouped() {
        // The second group's rows are in no group once it is dropped, and "in no group" is what
        // ungrouped means — so nothing is lost from the sidebar, only from the second heading.
        let layout = SidebarLayout {
            groups: vec![group("g", &["/a"]), group("g", &["/b"])],
            ..SidebarLayout::default()
        }
        .normalized();

        assert_eq!(ids(&layout), [FAVORITES, "g", UNGROUPED]);
        assert_eq!(members(&layout, "g"), ["/a"]);
    }

    #[test]
    fn a_group_with_no_id_is_dropped_rather_than_becoming_unaddressable() {
        let layout = SidebarLayout {
            groups: vec![group("  ", &["/a"])],
            ..SidebarLayout::default()
        }
        .normalized();
        assert_eq!(ids(&layout), [FAVORITES, UNGROUPED]);
    }

    #[test]
    fn names_are_trimmed_and_capped_and_the_builtins_carry_none() {
        let mut long = group("g", &[]);
        long.name = format!("  {}  ", "é".repeat(MAX_NAME + 20));
        let mut favorites = group(FAVORITES, &[]);
        favorites.name = "Renamed by hand".to_owned();

        let layout = SidebarLayout {
            groups: vec![favorites, long],
            ..SidebarLayout::default()
        }
        .normalized();

        assert_eq!(
            layout.groups[0].name, "",
            "a built-in's label is not stored"
        );
        // Characters rather than bytes: a byte cap would split `é` and fail to be a string.
        assert_eq!(layout.groups[1].name.chars().count(), MAX_NAME);
        assert!(!layout.groups[1].name.starts_with(' '));
    }

    #[test]
    fn an_origin_survives_only_for_a_starred_worktree_and_a_group_that_still_exists() {
        let layout = SidebarLayout {
            groups: vec![group(FAVORITES, &["/a", "/b"]), group("reviews", &["/c"])],
            origins: BTreeMap::from([
                ("/a".to_owned(), "reviews".to_owned()),
                ("/b".to_owned(), "deleted".to_owned()),
                ("/c".to_owned(), "reviews".to_owned()),
                ("/a-too".to_owned(), UNGROUPED.to_owned()),
            ]),
        }
        .normalized();

        assert_eq!(
            layout.origins,
            BTreeMap::from([("/a".to_owned(), "reviews".to_owned())]),
            "a deleted group, an unstarred worktree and a built-in are all dropped"
        );
    }

    #[test]
    fn normalizing_twice_changes_nothing_the_first_pass_did_not() {
        let layout = SidebarLayout {
            groups: vec![
                group("g", &["/a", "/a"]),
                group(UNGROUPED, &["/b"]),
                group(FAVORITES, &["/b"]),
            ],
            origins: BTreeMap::from([("/b".to_owned(), "g".to_owned())]),
        };
        let once = layout.normalized();
        assert_eq!(once.normalized(), once);
    }

    #[test]
    fn compacting_drops_the_builtins_only_when_they_are_empty_and_open() {
        let mut ungrouped = group(UNGROUPED, &[]);
        ungrouped.collapsed = true;
        let layout = SidebarLayout {
            groups: vec![group(FAVORITES, &[]), group("g", &[]), ungrouped],
            ..SidebarLayout::default()
        };

        let compact = layout.compacted();
        assert_eq!(
            ids(&compact),
            ["g", UNGROUPED],
            "an empty custom group is the user's and stays; a folded ungrouped section is a choice"
        );
        assert_eq!(
            compact.normalized(),
            layout.normalized(),
            "compacting must lose nothing a read puts back"
        );
    }

    #[test]
    fn a_layout_that_says_nothing_is_empty() {
        assert!(SidebarLayout::default().is_empty());
        assert!(SidebarLayout::default().normalized().is_empty());
        assert!(
            !SidebarLayout {
                groups: vec![group(UNGROUPED, &["/a"])],
                ..SidebarLayout::default()
            }
            .is_empty(),
            "a hand-set order of ungrouped rows is worth keeping"
        );
    }

    #[test]
    fn legacy_favorites_join_the_favorites_group_after_its_existing_members() {
        let layout = SidebarLayout {
            groups: vec![group(FAVORITES, &["/b"]), group("g", &["/a"])],
            ..SidebarLayout::default()
        }
        .with_legacy_favorites(&["/a".to_owned(), "/b".to_owned(), "/c".to_owned()]);

        assert_eq!(members(&layout, FAVORITES), ["/b", "/a", "/c"]);
        assert!(
            members(&layout, "g").is_empty(),
            "a legacy star wins over a group, like any other duplicate"
        );
    }

    #[test]
    fn forgetting_a_worktree_takes_it_out_of_its_group_and_its_origin() {
        let mut layout = SidebarLayout {
            groups: vec![group(FAVORITES, &["/a"]), group("g", &["/b"])],
            origins: BTreeMap::from([("/a".to_owned(), "g".to_owned())]),
        };
        assert!(layout.forget("/a"));
        assert!(layout.origins.is_empty());
        assert!(layout.groups[0].worktrees.is_empty());
        assert!(!layout.forget("/a"), "forgetting twice is not a change");
    }
}

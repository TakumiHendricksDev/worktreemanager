//! Built-ins and external views compete for named regions, never DOM selectors.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Workbench,
    Titlebar,
    Sidebar,
    WorktreeBar,
    Home,
    Sessions,
    Code,
    Database,
    NewWorktree,
    SettingsAppearance,
    SettingsGeneral,
    SettingsNotifications,
    SettingsAdvanced,
    SettingsRecipes,
    SettingsActions,
}

#[derive(Debug, Clone, Serialize)]
pub struct Builtin {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub slots: &'static [Slot],
}

const WORKBENCH: Builtin = Builtin {
    id: "wtm.workbench",
    name: "Workbench",
    description: "The complete workbench layout. Plugins can replace it while Settings and recovery stay available.",
    slots: &[Slot::Workbench],
};
const TITLEBAR: Builtin = Builtin {
    id: "wtm.titlebar",
    name: "Navigation",
    description: "Project navigation, Home shortcut and window toolbar.",
    slots: &[Slot::Titlebar],
};
const SIDEBAR: Builtin = Builtin {
    id: "wtm.sidebar",
    name: "Sidebar",
    description: "Worktree navigation and Home's session tree.",
    slots: &[Slot::Sidebar],
};
const WORKTREE_BAR: Builtin = Builtin {
    id: "wtm.worktree-bar",
    name: "Worktree toolbar",
    description: "Worktree actions and view navigation.",
    slots: &[Slot::WorktreeBar],
};
const HOME: Builtin = Builtin {
    id: "wtm.home",
    name: "Home",
    description: "Your session tree, parent agent, and approvals inbox.",
    slots: &[Slot::Home],
};
const SESSIONS: Builtin = Builtin {
    id: "wtm.sessions",
    name: "Sessions",
    description: "The session workbench, pane arrangement and session navigation.",
    slots: &[Slot::Sessions],
};
const CODE: Builtin = Builtin {
    id: "wtm.code",
    name: "Code",
    description: "The built-in file browser, editor and code review.",
    slots: &[Slot::Code],
};
const DATABASE: Builtin = Builtin {
    id: "wtm.database",
    name: "Database",
    description: "The built-in database browser and query interface.",
    slots: &[Slot::Database],
};
const NEW_WORKTREE: Builtin = Builtin {
    id: "wtm.new-worktree",
    name: "New worktree",
    description: "The form for creating a worktree.",
    slots: &[Slot::NewWorktree],
};
const CHAT: Builtin = Builtin {
    id: "wtm.chat",
    name: "WTM Chat",
    description: "Structured chat panes with tool results and inline approvals. Home needs it. Turning it off stops new chat panes from opening; recipes and agent delegation keep running.",
    slots: &[],
};
const NATIVE_CLI: Builtin = Builtin {
    id: "wtm.native-cli",
    name: "Native CLI",
    description: "Claude Code and Codex in their original terminal interfaces.",
    slots: &[],
};
const APPEARANCE: Builtin = Builtin {
    id: "wtm.appearance",
    name: "Appearance settings",
    description: "Palette and display preferences.",
    slots: &[Slot::SettingsAppearance],
};
const GENERAL: Builtin = Builtin {
    id: "wtm.general",
    name: "General settings",
    description: "Message and worktree preferences.",
    slots: &[Slot::SettingsGeneral],
};
const NOTIFICATIONS: Builtin = Builtin {
    id: "wtm.notifications",
    name: "Notification settings",
    description: "Notification preferences. Hiding this view does not change saved notification behavior.",
    slots: &[Slot::SettingsNotifications],
};
const ADVANCED: Builtin = Builtin {
    id: "wtm.advanced",
    name: "Advanced settings",
    description: "PATH, dictation configuration and diagnostics.",
    slots: &[Slot::SettingsAdvanced],
};
const RECIPES: Builtin = Builtin {
    id: "wtm.recipes",
    name: "Recipe settings",
    description: "Recipe editing. Existing recipes stay available to sessions and keep running when WTM Chat is off.",
    slots: &[Slot::SettingsRecipes],
};
const ACTIONS: Builtin = Builtin {
    id: "wtm.actions",
    name: "Action settings",
    description: "Recipe action preferences. Existing action preferences are preserved.",
    slots: &[Slot::SettingsActions],
};

pub const BUILTINS: &[Builtin] = &[
    WORKBENCH,
    TITLEBAR,
    SIDEBAR,
    WORKTREE_BAR,
    HOME,
    SESSIONS,
    CODE,
    DATABASE,
    NEW_WORKTREE,
    CHAT,
    NATIVE_CLI,
    APPEARANCE,
    GENERAL,
    NOTIFICATIONS,
    ADVANCED,
    RECIPES,
    ACTIONS,
];

impl Slot {
    /// The built-in a region falls back to. A match rather than a search of [`BUILTINS`], so a
    /// region added without an owner fails to compile instead of panicking the first time it is
    /// drawn, or becoming a region that recovery cannot restore.
    pub fn builtin(self) -> &'static Builtin {
        match self {
            Self::Workbench => &WORKBENCH,
            Self::Titlebar => &TITLEBAR,
            Self::Sidebar => &SIDEBAR,
            Self::WorktreeBar => &WORKTREE_BAR,
            Self::Home => &HOME,
            Self::Sessions => &SESSIONS,
            Self::Code => &CODE,
            Self::Database => &DATABASE,
            Self::NewWorktree => &NEW_WORKTREE,
            Self::SettingsAppearance => &APPEARANCE,
            Self::SettingsGeneral => &GENERAL,
            Self::SettingsNotifications => &NOTIFICATIONS,
            Self::SettingsAdvanced => &ADVANCED,
            Self::SettingsRecipes => &RECIPES,
            Self::SettingsActions => &ACTIONS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Binding {
    Builtin,
    Hidden,
    Plugin { plugin: String, view: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_region_a_builtin_claims_falls_back_to_that_same_builtin() {
        for builtin in BUILTINS {
            for slot in builtin.slots {
                assert_eq!(slot.builtin().id, builtin.id, "{slot:?}");
            }
        }
    }
}

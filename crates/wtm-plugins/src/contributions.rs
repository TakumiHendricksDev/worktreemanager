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

pub const BUILTINS: &[Builtin] = &[
    Builtin {
        id: "wtm.workbench",
        name: "Workbench",
        description: "The complete workbench layout. Plugins can replace it while Settings and recovery stay available.",
        slots: &[Slot::Workbench],
    },
    Builtin {
        id: "wtm.titlebar",
        name: "Navigation",
        description: "Project navigation, Home shortcut and window toolbar.",
        slots: &[Slot::Titlebar],
    },
    Builtin {
        id: "wtm.sidebar",
        name: "Sidebar",
        description: "Worktree navigation and Home's session tree.",
        slots: &[Slot::Sidebar],
    },
    Builtin {
        id: "wtm.worktree-bar",
        name: "Worktree toolbar",
        description: "Worktree actions and view navigation.",
        slots: &[Slot::WorktreeBar],
    },
    Builtin {
        id: "wtm.home",
        name: "Home",
        description: "Your session tree, parent agent, and approvals inbox.",
        slots: &[Slot::Home],
    },
    Builtin {
        id: "wtm.sessions",
        name: "Sessions",
        description: "The session workbench, pane arrangement and session navigation.",
        slots: &[Slot::Sessions],
    },
    Builtin {
        id: "wtm.code",
        name: "Code",
        description: "The built-in file browser, editor and code review.",
        slots: &[Slot::Code],
    },
    Builtin {
        id: "wtm.database",
        name: "Database",
        description: "The built-in database browser and query interface.",
        slots: &[Slot::Database],
    },
    Builtin {
        id: "wtm.new-worktree",
        name: "New worktree",
        description: "The form for creating a worktree.",
        slots: &[Slot::NewWorktree],
    },
    Builtin {
        id: "wtm.chat",
        name: "WTM Chat",
        description: "Structured conversations, tool results, inline approvals and agent delegation.",
        slots: &[],
    },
    Builtin {
        id: "wtm.native-cli",
        name: "Native CLI",
        description: "Claude Code and Codex in their original terminal interfaces.",
        slots: &[],
    },
    Builtin {
        id: "wtm.appearance",
        name: "Appearance settings",
        description: "Palette and display preferences.",
        slots: &[Slot::SettingsAppearance],
    },
    Builtin {
        id: "wtm.general",
        name: "General settings",
        description: "Message and worktree preferences.",
        slots: &[Slot::SettingsGeneral],
    },
    Builtin {
        id: "wtm.notifications",
        name: "Notification settings",
        description: "Notification preferences. Hiding this view does not change saved notification behavior.",
        slots: &[Slot::SettingsNotifications],
    },
    Builtin {
        id: "wtm.advanced",
        name: "Advanced settings",
        description: "PATH, dictation configuration and diagnostics.",
        slots: &[Slot::SettingsAdvanced],
    },
    Builtin {
        id: "wtm.recipes",
        name: "Recipe settings",
        description: "Recipe editing. Existing configured recipes remain available to sessions.",
        slots: &[Slot::SettingsRecipes],
    },
    Builtin {
        id: "wtm.actions",
        name: "Action settings",
        description: "Recipe action preferences. Existing action preferences are preserved.",
        slots: &[Slot::SettingsActions],
    },
];

impl Slot {
    pub fn builtin(self) -> &'static Builtin {
        // Exhaustive ownership is also tested so a new region cannot silently become protected.
        BUILTINS
            .iter()
            .find(|b| b.slots.contains(&self))
            .expect("every slot has a builtin")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Binding {
    Builtin,
    Hidden,
    Plugin { plugin: String, view: String },
}

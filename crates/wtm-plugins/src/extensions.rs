//! Contributions target versioned host contracts, never private component selectors.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extensions {
    pub access: Vec<String>,
    pub runtime: Vec<String>,
    pub events: Vec<String>,
    pub styles: Vec<Style>,
    pub actions: Vec<Action>,
    pub content: Vec<Content>,
    pub settings: Vec<Setting>,
    pub hooks: Vec<Hook>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Style {
    pub id: String,
    pub title: String,
    pub target: String,
    pub properties: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub target: String,
    pub operation: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub replaces: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Content {
    pub id: String,
    pub title: String,
    pub target: String,
    #[serde(default)]
    pub position: Position,
    pub body: Vec<Widget>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    #[default]
    Before,
    After,
    Replace,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Widget {
    Text { text: String },
    Markdown { text: String },
    Action { action: String },
    Setting { setting: String },
    Group { title: String, children: Vec<Self> },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub section: String,
    pub kind: SettingKind,
    pub default: Value,
    #[serde(default)]
    pub options: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    Text,
    Boolean,
    Number,
    Select,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub id: String,
    pub title: String,
    pub operation: String,
    pub phase: Phase,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Before,
    After,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub disabled: BTreeSet<String>,
    /// An empty set means all workspaces. It is an application scope, not an OS sandbox.
    pub worktrees: BTreeSet<String>,
    pub values: BTreeMap<String, BTreeMap<String, Value>>,
}
impl Extensions {
    pub fn ids(&self) -> Vec<(&str, &str)> {
        self.styles
            .iter()
            .map(|v| (v.id.as_str(), v.title.as_str()))
            .chain(
                self.actions
                    .iter()
                    .map(|v| (v.id.as_str(), v.title.as_str())),
            )
            .chain(
                self.content
                    .iter()
                    .map(|v| (v.id.as_str(), v.title.as_str())),
            )
            .chain(
                self.settings
                    .iter()
                    .map(|v| (v.id.as_str(), v.title.as_str())),
            )
            .chain(self.hooks.iter().map(|v| (v.id.as_str(), v.title.as_str())))
            .collect()
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.ids().len() > 512
            || self.runtime.len() > 64
            || self.access.len() > 256
            || self.events.len() > 64
        {
            return Err("Contribution manifest exceeds its item limit".into());
        }
        let catalog = crate::catalog::catalog();
        let public = |op: &str| catalog.public(op).is_some();
        let target = |name: &str, kind: &str| catalog.surface(name, kind);
        if !self.events.is_empty() && self.runtime.is_empty() {
            return Err("Event subscriptions require a runtime".into());
        }
        if self.events.iter().any(|name| !catalog.event(name)) {
            return Err("Unknown event subscription".into());
        }
        if self.access.iter().any(|op| !public(op)) {
            return Err("Host access names an unknown or protected operation".into());
        }
        if !self.runtime.is_empty() {
            if self.runtime[0].is_empty() || self.runtime.iter().any(|a| a.contains('\0')) {
                return Err("Runtime must be an argv array without NUL bytes".into());
            }
            if self.runtime[0].contains('/')
                && !std::path::Path::new(&self.runtime[0]).is_absolute()
            {
                super::manifest::relative(&self.runtime[0])?;
                if !self.runtime[0].starts_with("./") {
                    return Err("Package runtime paths must start with ./".into());
                }
            }
        }
        for style in &self.styles {
            if !target(&style.target, "style") || style.properties.is_empty() {
                return Err(format!("Unsupported styling part: {}", style.target));
            }
            for (property, value) in &style.properties {
                validate_style(property, value)?;
            }
        }
        for action in &self.actions {
            if action
                .replaces
                .as_ref()
                .is_some_and(|operation| !public(operation) || !self.access.contains(operation))
            {
                return Err("Replacement requires declared access to a public operation".into());
            }
            if !target(&action.target, "actions")
                || (!self.access.contains(&action.operation)
                    && !action.operation.starts_with("plugin."))
            {
                return Err(format!(
                    "Action {} requires a public target and declared operation access",
                    action.id
                ));
            }
            if action.operation.starts_with("plugin.") && self.runtime.is_empty() {
                return Err("Plugin actions require a runtime".into());
            }
        }
        for content in &self.content {
            if !target(&content.target, "content") {
                return Err(format!("Unsupported content boundary: {}", content.target));
            }
            // No surface takes replacement content and the frontend never renders it; a package
            // that wants to own a whole region declares a view for it instead.
            if content.position == Position::Replace {
                return Err(format!(
                    "Content {} cannot replace a surface; add before or after it, or declare a view",
                    content.id
                ));
            }
            self.validate_widgets(&content.body, 0)?;
        }
        for setting in &self.settings {
            if !target(&setting.section, "settings") {
                return Err(format!("Unsupported settings section: {}", setting.section));
            }
            setting.validate(&setting.default)?;
        }
        for hook in &self.hooks {
            if !self.access.contains(&hook.operation) || self.runtime.is_empty() {
                return Err("Hooks require a runtime and declared public operation access".into());
            }
        }
        Ok(())
    }
    fn validate_widgets(&self, widgets: &[Widget], depth: usize) -> Result<(), String> {
        if depth > 8 || widgets.len() > 64 {
            return Err("Content exceeds its nesting or item limit".into());
        }
        for widget in widgets {
            match widget {
                Widget::Group { children, .. } => self.validate_widgets(children, depth + 1)?,
                Widget::Action { action } if !self.actions.iter().any(|a| &a.id == action) => {
                    return Err("Content refers to an undeclared action".into());
                }
                Widget::Setting { setting } if !self.settings.iter().any(|s| &s.id == setting) => {
                    return Err("Content refers to an undeclared setting".into());
                }
                _ => {}
            }
        }
        Ok(())
    }
}
impl Setting {
    pub fn validate(&self, value: &Value) -> Result<(), String> {
        let valid = match self.kind {
            SettingKind::Text => value.as_str().is_some_and(|s| s.len() <= 32_768),
            SettingKind::Boolean => value.is_boolean(),
            SettingKind::Number => value.is_number(),
            SettingKind::Select => value
                .as_str()
                .is_some_and(|s| self.options.iter().any(|v| v == s)),
        };
        if valid {
            Ok(())
        } else {
            Err(format!("Invalid value for setting {}", self.id))
        }
    }
}
/// Paint, typography and inner spacing only. URLs, selectors, overlays and executable CSS never
/// reach the host, and neither does anything that sets a box's own size, visibility or place.
///
/// Hidden surfaces are hidden by a toggled class (`.c-surface.is-hidden`), and an inline
/// declaration beats a class: an allowed `display` would have unhidden whatever the host had
/// hidden. Width, height and the grid's track sizes can collapse or swallow a neighbouring
/// region just as well, and a negative margin can slide one box over another, which is an overlay
/// by another name. Padding and gap only space out a box's own contents.
pub fn validate_style(property: &str, value: &str) -> Result<(), String> {
    const PROPERTIES: &[&str] = &[
        "color",
        "background-color",
        "border-color",
        "border-width",
        "border-style",
        "border-radius",
        "font-family",
        "font-size",
        "font-weight",
        "font-style",
        "line-height",
        "letter-spacing",
        "text-decoration",
        "text-decoration-color",
        "text-decoration-thickness",
        "text-underline-offset",
        "text-transform",
        "text-align",
        "padding",
        "padding-inline",
        "padding-block",
        "gap",
        "row-gap",
        "column-gap",
        "flex-wrap",
        "align-items",
        "justify-content",
        "white-space",
        "overflow-wrap",
    ];
    // Function names, not substrings: a font called "Curlz MT" is not a URL. CSS function names
    // are ASCII case-insensitive and admit no space before the parenthesis, and backslash escapes
    // and comments, the two ways to split a name, are refused outright.
    let lower = value.to_ascii_lowercase();
    if !PROPERTIES.contains(&property)
        || value.len() > 512
        || value.is_empty()
        || value.contains([';', '{', '}', '<', '>', '\\', '@', '!'])
        || lower.contains("url(")
        || lower.contains("expression(")
        || value.contains("/*")
    {
        return Err(format!("Unsupported style declaration: {property}"));
    }
    Ok(())
}

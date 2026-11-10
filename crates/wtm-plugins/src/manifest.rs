//! Explicit contribution names keep private frontend components out of the public API.
use crate::contributions::Slot;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path};

const PALETTE: &str = "Palettes require a unique id, title, hue 0–360, chroma 0–2 and four \
    #rrggbb brand colors";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub api: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub commands: Vec<Entrypoint>,
    #[serde(default)]
    pub panels: Vec<Panel>,
    #[serde(default)]
    pub session_backends: Vec<Entrypoint>,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub palettes: Vec<Palette>,
    #[serde(default)]
    pub views: Vec<View>,
    #[serde(default)]
    pub terminals: Vec<Entrypoint>,
    #[serde(default)]
    pub extensions: crate::extensions::Extensions,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entrypoint {
    pub id: String,
    pub title: String,
    pub command: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    pub id: String,
    pub title: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    pub id: String,
    pub title: String,
    pub file: String,
    pub slots: Vec<Slot>,
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".-_".contains(&c))
        && !id.starts_with('.')
        && !id.contains("..")
}
impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 1_048_576 {
            return Err("Manifest exceeds one MiB".into());
        }
        let value: Self = toml::from_str(text).map_err(|e| format!("Invalid manifest: {e}"))?;
        if value.schema != 1 || ![1, 2].contains(&value.api) {
            return Err(
                "This package requires an unsupported manifest or plugin API version".into(),
            );
        }
        if !valid_id(&value.id) || value.id.starts_with("wtm.") {
            return Err("Use a namespaced plugin id outside the reserved wtm.* namespace".into());
        }
        if !value.id.contains('.') || value.name.trim().is_empty() || value.name.len() > 100 {
            return Err(
                "A namespaced id and nonempty name of at most 100 characters are required".into(),
            );
        }
        if value.version.split('.').count() != 3
            || !value
                .version
                .split('.')
                .all(|v| !v.is_empty() && v.parse::<u32>().is_ok())
        {
            return Err("Version must be major.minor.patch".into());
        }
        if !value.platforms.is_empty() && !value.platforms.iter().any(|s| s == std::env::consts::OS)
        {
            return Err("This package does not support this platform".into());
        }
        value.extensions.validate()?;
        if value.api == 1 && value.extensions != crate::extensions::Extensions::default() {
            return Err("In-feature contributions require Plugin API 2".into());
        }
        let mut ids = BTreeSet::new();
        for (id, title) in value
            .commands
            .iter()
            .chain(&value.session_backends)
            .chain(&value.terminals)
            .map(|c| (&c.id, &c.title))
            .chain(value.panels.iter().map(|p| (&p.id, &p.title)))
            .chain(value.views.iter().map(|v| (&v.id, &v.title)))
        {
            if !valid_id(id) || title.trim().is_empty() || !ids.insert(id.as_str()) {
                return Err(
                    "Contribution ids must be valid and unique, with nonempty titles".into(),
                );
            }
        }
        for (id, title) in value.extensions.ids() {
            if !valid_id(id) || title.trim().is_empty() || !ids.insert(id) {
                return Err("Extension ids must be unique, valid and titled".into());
            }
        }
        for c in value
            .commands
            .iter()
            .chain(&value.session_backends)
            .chain(&value.terminals)
        {
            if c.command.is_empty()
                || c.command[0].is_empty()
                || c.command.iter().any(|s| s.contains('\0'))
            {
                return Err("Commands must be nonempty argv arrays without NUL bytes".into());
            }
        }
        for c in value
            .commands
            .iter()
            .chain(&value.session_backends)
            .chain(&value.terminals)
        {
            if c.command[0].contains('/') && !std::path::Path::new(&c.command[0]).is_absolute() {
                if !c.command[0].starts_with("./") {
                    return Err("Package executables must use ./relative paths".into());
                }
                relative(&c.command[0])?;
            }
        }
        for p in &value.panels {
            relative(&p.file)?;
        }
        for view in &value.views {
            relative(&view.file)?;
            if view.slots.is_empty()
                || view.slots.iter().collect::<BTreeSet<_>>().len() != view.slots.len()
            {
                return Err("Views must name at least one slot without duplicates".into());
            }
        }
        for palette in &value.palettes {
            if !valid_id(&palette.id)
                || !ids.insert(palette.id.as_str())
                || palette.title.trim().is_empty()
                || !(0.0..=360.0).contains(&palette.hue)
                || !(0.0..=2.0).contains(&palette.chroma)
                || palette.brand.len() != 4
                || !palette.brand.iter().all(|c| {
                    c.len() == 7
                        && c.starts_with('#')
                        && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
                })
            {
                return Err(PALETTE.into());
            }
        }
        Ok(value)
    }
    /// Whether this package can act as native code, and so gets the same trust warning.
    ///
    /// Declared host access counts even without a program: the public operations still read
    /// files, terminal output and transcripts, and change workspaces, with the user's authority.
    /// The review UI applies this same rule; keep the two in step.
    pub fn executable(&self) -> bool {
        !self.extensions.runtime.is_empty()
            || !self.extensions.access.is_empty()
            || !self.commands.is_empty()
            || !self.session_backends.is_empty()
            || !self.terminals.is_empty()
    }
}

/// A path inside the package, and never inside a `.git` directory.
///
/// `.git` is refused at any depth because preparation removes or skips the source's repository
/// metadata: a reviewed manifest that named a file there would point at something the snapshot
/// either lacks or that Git, not the author, wrote.
pub fn relative(value: &str) -> Result<(), String> {
    let inside = Path::new(value).components().all(|c| match c {
        Component::Normal(name) => name != ".git",
        Component::CurDir => true,
        _ => false,
    });
    if value.is_empty() || !inside {
        return Err("Package paths must stay inside the package, outside .git".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub id: String,
    pub title: String,
    pub hue: f64,
    pub chroma: f64,
    pub brand: Vec<String>,
}

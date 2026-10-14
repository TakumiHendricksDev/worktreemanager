//! Rust adaptation of Open Design's CSS token evidence collector (Apache-2.0).
//! The pinned original and license live in vendor/open-design. Keeping the bounded collector
//! in-process avoids shipping Node or running repository scripts just to inspect design tokens.

use crate::prototype_context::DesignSource;
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenEvidence {
    pub kind: &'static str,
    pub name: String,
    pub value: String,
    pub source: String,
    pub line: usize,
}

pub fn collect(sources: &[DesignSource]) -> Vec<TokenEvidence> {
    let Ok(properties) = regex::Regex::new(r"(--[a-zA-Z0-9-_]+)\s*:\s*([^;{}\n]+);") else {
        return Vec::new();
    };
    let Ok(literals) =
        regex::Regex::new(r"(?i)#[0-9a-f]{3,8}\b|rgba?\([^;\n]+?\)|hsla?\([^;\n]+?\)")
    else {
        return Vec::new();
    };
    let Ok(metrics) = regex::Regex::new(
        r"(font-family|padding|margin|gap|inset|top|left|right|bottom|border-radius|box-shadow)\s*:\s*([^;\n]+)",
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for source in sources {
        for cap in properties.captures_iter(&source.text) {
            let name = cap[1].to_ascii_lowercase();
            let value = cap[2].trim();
            if value.is_empty() || value.len() > 120 {
                continue;
            }
            let hints = |words: &[&str]| words.iter().any(|word| name.contains(word));
            let kind = if hints(&["font", "typeface"]) {
                "typography"
            } else if hints(&["radius"]) {
                "radius"
            } else if hints(&["shadow", "elev"]) {
                "shadow"
            } else if hints(&["space", "spacing", "gap", "gutter", "padding", "margin"]) {
                "spacing"
            } else if hints(&[
                "color",
                "fg",
                "bg",
                "accent",
                "primary",
                "secondary",
                "surface",
                "border",
                "muted",
            ]) || value.starts_with(['#'])
                || ["rgb", "hsl", "oklch", "color-mix", "var("]
                    .iter()
                    .any(|s| value.starts_with(s))
            {
                "color"
            } else {
                continue;
            };
            out.push(TokenEvidence {
                kind,
                name: cap[1].to_owned(),
                value: value.to_owned(),
                source: source.path.clone(),
                line: 1 + source.text[..cap.get(0).map_or(0, |m| m.start())]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count(),
            });
        }
        for cap in metrics.captures_iter(&source.text) {
            let kind = match &cap[1] {
                "font-family" => "typography",
                "border-radius" => "radius",
                "box-shadow" => "shadow",
                _ => "spacing",
            };
            out.push(TokenEvidence {
                kind,
                name: cap[1].to_owned(),
                value: cap[2].trim().to_owned(),
                source: source.path.clone(),
                line: 1 + source.text[..cap.get(0).map_or(0, |m| m.start())]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count(),
            });
        }
        for value in literals.find_iter(&source.text) {
            out.push(TokenEvidence {
                kind: "color",
                name: String::new(),
                value: value.as_str().to_owned(),
                source: source.path.clone(),
                line: 1 + source.text[..value.start()]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count(),
            });
        }
    }
    out.truncate(400);
    out
}

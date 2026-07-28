//! What the user typed.
//!
//! Values are kept as a small tagged enum rather than plain strings so the
//! template layer can distinguish "the box is empty" from "the box contains the
//! word false", and so a `bool` field can drive a `when` expression without
//! stringly-typed comparisons against `"true"`.
//!
//! [`FormValues`] holds both the raw input and the normalized value, because the
//! form shows them side by side: when a config normalizes `1234` into
//! `ACME-1234`, the user needs to see that happen *before* pressing Create.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One field's value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum FieldValue {
    Text(String),
    Number(f64),
    Bool(bool),
    List(Vec<String>),
    /// The field was left empty. Distinct from `Text("")` so `required` and
    /// `default_if_empty` behave predictably.
    Empty,
}

impl FieldValue {
    /// The value as a string, for template substitution. [`Self::Empty`] renders
    /// as the empty string — templates should use a `default_if_empty` filter
    /// rather than testing for a sentinel.
    #[must_use]
    pub fn as_template_string(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Number(n) => {
                // Render 8 as "8", not "8.0" — these end up in branch names.
                if n.fract() == 0.0 && n.is_finite() {
                    format!("{n:.0}")
                } else {
                    n.to_string()
                }
            }
            Self::Bool(b) => b.to_string(),
            Self::List(items) => items.join(","),
            Self::Empty => String::new(),
        }
    }

    /// Truthiness for `when` / `required_when` expressions.
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Text(s) => !s.is_empty(),
            Self::Number(n) => *n != 0.0,
            Self::Bool(b) => *b,
            Self::List(items) => !items.is_empty(),
            Self::Empty => false,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Text(s) => s.is_empty(),
            Self::List(items) => items.is_empty(),
            Self::Number(_) | Self::Bool(_) => false,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }
}

impl From<&str> for FieldValue {
    fn from(s: &str) -> Self {
        if s.is_empty() {
            Self::Empty
        } else {
            Self::Text(s.to_owned())
        }
    }
}

impl From<String> for FieldValue {
    fn from(s: String) -> Self {
        if s.is_empty() {
            Self::Empty
        } else {
            Self::Text(s)
        }
    }
}

impl From<bool> for FieldValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

/// A submitted form: raw input keyed by field, plus the normalized values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FormValues {
    /// Exactly what the user typed.
    pub raw: BTreeMap<String, FieldValue>,
    /// After each field's `normalize` template ran. Empty until validation.
    #[serde(default)]
    pub normalized: BTreeMap<String, FieldValue>,
}

impl FormValues {
    #[must_use]
    pub fn new(raw: BTreeMap<String, FieldValue>) -> Self {
        Self {
            raw,
            normalized: BTreeMap::new(),
        }
    }

    /// The effective value for `key`: normalized if normalization has run,
    /// otherwise raw.
    #[must_use]
    pub fn effective(&self, key: &str) -> &FieldValue {
        self.normalized
            .get(key)
            .or_else(|| self.raw.get(key))
            .unwrap_or(&FieldValue::Empty)
    }

    #[must_use]
    pub fn effective_str(&self, key: &str) -> String {
        self.effective(key).as_template_string()
    }

    /// True when the field was normalized into something different from the input
    /// — the signal the UI uses to show "`1234` → `ACME-1234`".
    #[must_use]
    pub fn was_rewritten(&self, key: &str) -> bool {
        match (self.raw.get(key), self.normalized.get(key)) {
            (Some(raw), Some(norm)) => raw != norm,
            _ => false,
        }
    }

    pub fn set_normalized(&mut self, key: impl Into<String>, value: FieldValue) {
        self.normalized.insert(key.into(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_becomes_empty_not_text() {
        // Otherwise `required` would pass on a blank box.
        assert_eq!(FieldValue::from(""), FieldValue::Empty);
        assert_eq!(FieldValue::from("x"), FieldValue::Text("x".to_owned()));
    }

    #[test]
    fn whole_numbers_render_without_a_decimal_point() {
        // These end up inside branch names; "8.0" would be wrong.
        assert_eq!(FieldValue::Number(8.0).as_template_string(), "8");
        assert_eq!(FieldValue::Number(8.5).as_template_string(), "8.5");
    }

    #[test]
    fn truthiness_distinguishes_empty_from_false() {
        assert!(!FieldValue::Empty.is_truthy());
        assert!(!FieldValue::Bool(false).is_truthy());
        assert!(FieldValue::Bool(true).is_truthy());
        assert!(!FieldValue::Text(String::new()).is_truthy());
        assert!(FieldValue::Text("a".to_owned()).is_truthy());
    }

    #[test]
    fn effective_prefers_normalized_and_falls_back_to_raw() {
        let mut v = FormValues::new(
            [("issue".to_owned(), FieldValue::from("1234"))]
                .into_iter()
                .collect(),
        );
        assert_eq!(v.effective_str("issue"), "1234");
        v.set_normalized("issue", FieldValue::from("ACME-1234"));
        assert_eq!(v.effective_str("issue"), "ACME-1234");
        assert!(v.was_rewritten("issue"));
    }

    #[test]
    fn missing_key_is_empty_not_a_panic() {
        let v = FormValues::default();
        assert_eq!(v.effective_str("nope"), "");
        assert!(!v.was_rewritten("nope"));
    }
}

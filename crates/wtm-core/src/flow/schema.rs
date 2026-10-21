//! Checking a value against a JSON Schema: the handful of keywords the run tools' schemas use.
//!
//! # Why a checker of its own, rather than a JSON Schema crate
//!
//! The schemas a run checks are this crate's own, written once in [`super::submission`], and they
//! use eleven keywords. A general validator would bring format checkers, URI resolution and remote
//! `$ref` loading into `wtm-core`'s dependency tree for a dozen keywords' worth of rules, and adding
//! a dependency is an open-an-issue-first change besides. A keyword this checker does not know is
//! refused by [`supported`], which a test runs over every schema the app ships, so a schema can never
//! lean on a keyword that is silently skipped.
//!
//! # Why every problem, not the first
//!
//! The answer goes back to a model as the reason its `flow_submit` was refused. One problem per
//! round trip turns a submission with three mistakes into three retries of a whole payload, and
//! each retry is spent from the same usage window as the review. So the check walks the whole value
//! and names each problem by its JSON Pointer.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;

/// One way a value fails its schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaProblem {
    /// A JSON Pointer to the offending value: `/findings/2/severity`. Empty for the whole value.
    pub at: String,
    pub message: String,
}

impl SchemaProblem {
    #[must_use]
    pub fn new(at: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            at: at.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SchemaProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.at.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.at, self.message)
        }
    }
}

/// The keywords [`check`] understands. `description` is read by models, not by the check.
const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "items",
    "minLength",
    "minimum",
    "minItems",
    "maxItems",
    "description",
];

/// Whether `schema` uses only keywords [`check`] enforces, at every depth.
///
/// # Errors
///
/// The first keyword it does not know, and where.
pub fn supported(schema: &Value) -> Result<(), String> {
    walk_schema(schema, "")
}

fn walk_schema(schema: &Value, at: &str) -> Result<(), String> {
    let Some(object) = schema.as_object() else {
        return Err(format!("{at}: a schema must be an object"));
    };
    for (key, value) in object {
        if !KEYWORDS.contains(&key.as_str()) {
            return Err(format!(
                "{at}: `{key}` is not a keyword this checker enforces"
            ));
        }
        match key.as_str() {
            "properties" => {
                for (name, inner) in value.as_object().into_iter().flatten() {
                    walk_schema(inner, &format!("{at}/properties/{name}"))?;
                }
            }
            "items" => walk_schema(value, &format!("{at}/items"))?,
            "additionalProperties" if value != &Value::Bool(false) => {
                return Err(format!(
                    "{at}: only `additionalProperties: false` is enforced"
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Every way `value` fails `schema`.
#[must_use]
pub fn check(schema: &Value, value: &Value) -> Vec<SchemaProblem> {
    let mut problems = Vec::new();
    visit(schema, value, &mut String::new(), &mut problems);
    problems
}

fn visit(schema: &Value, value: &Value, at: &mut String, problems: &mut Vec<SchemaProblem>) {
    if let Some(expected) = schema.get("type")
        && !type_matches(expected, value)
    {
        problems.push(SchemaProblem::new(
            at.clone(),
            format!(
                "expected {}, found {}",
                type_words(expected),
                kind_of(value)
            ),
        ));
        // Nothing below a wrong type can be checked meaningfully: every keyword after this one
        // describes a value of the expected type.
        return;
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        let names: Vec<String> = allowed.iter().map(Value::to_string).collect();
        problems.push(SchemaProblem::new(
            at.clone(),
            format!("must be one of {}, not {value}", names.join(", ")),
        ));
    }

    match value {
        Value::String(text) => {
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
                && (text.trim().chars().count() as u64) < min
            {
                problems.push(SchemaProblem::new(at.clone(), "must not be empty"));
            }
        }
        Value::Number(number) => {
            if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
                && number.as_f64().is_some_and(|n| n < min)
            {
                problems.push(SchemaProblem::new(
                    at.clone(),
                    format!("must be at least {min}"),
                ));
            }
        }
        Value::Array(items) => {
            let count = items.len() as u64;
            if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
                && count < min
            {
                problems.push(SchemaProblem::new(
                    at.clone(),
                    format!(
                        "needs at least {min} item{}",
                        if min == 1 { "" } else { "s" }
                    ),
                ));
            }
            if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
                && count > max
            {
                problems.push(SchemaProblem::new(
                    at.clone(),
                    format!("takes at most {max} items, not {count}"),
                ));
            }
            if let Some(item_schema) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    let len = at.len();
                    let _ = write!(at, "/{index}");
                    visit(item_schema, item, at, problems);
                    at.truncate(len);
                }
            }
        }
        Value::Object(fields) => {
            let properties = schema.get("properties").and_then(Value::as_object);
            for name in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !fields.contains_key(name) {
                    problems.push(SchemaProblem::new(
                        format!("{at}/{name}"),
                        "is required and missing",
                    ));
                }
            }
            let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
            for (name, field) in fields {
                let len = at.len();
                at.push('/');
                at.push_str(&escape(name));
                match properties.and_then(|props| props.get(name)) {
                    Some(inner) => visit(inner, field, at, problems),
                    None if closed => {
                        let known: Vec<&str> = properties
                            .map(|props| props.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        problems.push(SchemaProblem::new(
                            at.clone(),
                            format!("is not a field here; the fields are {}", known.join(", ")),
                        ));
                    }
                    None => {}
                }
                at.truncate(len);
            }
        }
        _ => {}
    }
}

/// A JSON Pointer reference token: `~` and `/` escaped, per RFC 6901.
fn escape(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

fn type_matches(expected: &Value, value: &Value) -> bool {
    match expected {
        Value::String(name) => is_type(name, value),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| is_type(name, value)),
        _ => true,
    }
}

fn is_type(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        // A whole number, which is how a model writes a line number. `1.0` is a float in JSON.
        "integer" => value.is_i64() || value.is_u64(),
        _ => false,
    }
}

fn type_words(expected: &Value) -> String {
    match expected {
        Value::String(name) => article(name),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .map(article)
            .collect::<Vec<_>>()
            .join(" or "),
        other => other.to_string(),
    }
}

fn article(name: &str) -> String {
    match name {
        "object" | "array" | "integer" => format!("an {name}"),
        "null" => "null".to_owned(),
        _ => format!("a {name}"),
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "an integer",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "minLength": 1 },
                "line": { "type": "integer", "minimum": 1 },
                "severity": { "type": "string", "enum": ["minor", "blocking"] },
                "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 2 },
            },
            "required": ["title", "severity"],
            "additionalProperties": false,
        })
    }

    fn pointers(problems: &[SchemaProblem]) -> Vec<&str> {
        problems.iter().map(|p| p.at.as_str()).collect()
    }

    #[test]
    fn a_value_that_fits_its_schema_has_no_problems() {
        let value = json!({ "title": "x", "severity": "minor", "line": 3, "tags": ["a"] });
        assert_eq!(check(&schema(), &value), []);
    }

    #[test]
    fn every_problem_is_reported_with_where_it_is_rather_than_only_the_first() {
        let value = json!({
            "title": "  ",
            "line": 0,
            "tags": ["a", 2, "c"],
            "approved": true,
        });
        let problems = check(&schema(), &value);
        // Sorted, because a map's order depends on whether something in the build turned on
        // `serde_json`'s `preserve_order`, and the order is not what this proves.
        let mut found = pointers(&problems);
        found.sort_unstable();
        assert_eq!(
            found,
            [
                "/approved",
                "/line",
                "/severity",
                "/tags",
                "/tags/1",
                "/title"
            ],
            "{problems:?}"
        );
        let stray = problems.iter().find(|p| p.at == "/approved").unwrap();
        assert!(stray.message.contains("the fields are"));
    }

    #[test]
    fn a_wrong_type_is_reported_once_rather_than_as_every_keyword_below_it() {
        let problems = check(&schema(), &json!(["not", "an", "object"]));
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].message, "expected an object, found an array");
    }

    #[test]
    fn a_float_is_not_a_line_number() {
        let value = json!({ "title": "x", "severity": "minor", "line": 3.5 });
        assert_eq!(pointers(&check(&schema(), &value)), ["/line"]);
    }

    #[test]
    fn a_field_name_with_a_slash_is_escaped_in_its_pointer() {
        let problems = check(
            &schema(),
            &json!({ "title": "x", "severity": "minor", "a/b": 1 }),
        );
        assert_eq!(pointers(&problems), ["/a~1b"]);
    }

    #[test]
    fn a_schema_using_a_keyword_the_checker_would_skip_is_refused() {
        assert!(supported(&schema()).is_ok());
        let pattern = json!({ "type": "object", "properties": { "x": { "pattern": "^a" } } });
        assert!(supported(&pattern).is_err_and(|why| why.contains("pattern")));
        let open = json!({ "additionalProperties": { "type": "string" } });
        assert!(supported(&open).is_err());
    }
}

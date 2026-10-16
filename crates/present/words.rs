//! Words for typed values in user-visible text.
//!
//! A reason, a coverage, a scope, or an authority is a typed value with a
//! stable wire spelling (`not-found`, `partial`). Interpolating it with `{:?}`
//! prints the Rust identifier or struct syntax instead (`NotFound`,
//! `Partial { completed: 12, total: 13 }`), which is neither what the wire
//! says nor something a reader can use. [`wire_words`] reads the same
//! spelling the wire carries.

use serde::Serialize;
use serde_json::Value;

/// A typed value in the words its wire spelling uses.
///
/// A fieldless variant is its wire name (`not-found`). A variant that carries
/// fields reads as its name followed by them (`partial (completed 12, total
/// 13)`); a list reads comma-separated.
#[must_use]
pub(crate) fn wire_words<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_value(value).map_or_else(|_| "unavailable".to_owned(), |value| words(&value))
}

fn words(value: &Value) -> String {
    match value {
        Value::Null => "none".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(words).collect::<Vec<_>>().join(", "),
        Value::Object(fields) => {
            // `{"state": "failed", "detail": {…}}`: a tagged variant with
            // content reads as the tag, then its content.
            if fields.len() == 2
                && let Some(detail) = fields.get("detail")
                && let Some(tag) = fields.iter().find_map(|(key, value)| {
                    (key != "detail").then_some(value).and_then(Value::as_str)
                })
            {
                return with_detail(tag, detail);
            }
            // `{"coverage": "complete"}`: a tagged variant without content
            // reads as its tag; `{"variant": content}`: an externally tagged
            // variant reads as its name, then its content.
            if let [(name, content)] = fields.iter().collect::<Vec<_>>().as_slice() {
                return match content {
                    Value::String(tag) => tag.clone(),
                    Value::Number(_) | Value::Bool(_) => format!("{name} {}", words(content)),
                    other => with_detail(name, other),
                };
            }
            fields
                .iter()
                .map(|(key, value)| format!("{key} {}", words(value)))
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

fn with_detail(name: &str, detail: &Value) -> String {
    match detail {
        Value::Null => name.to_owned(),
        Value::Object(fields) if fields.is_empty() => name.to_owned(),
        other => format!("{name} ({})", words(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::wire_words;
    use serde::Serialize;

    #[derive(Serialize)]
    #[serde(rename_all = "kebab-case")]
    enum Reason {
        NotFound,
    }

    #[derive(Serialize)]
    #[serde(tag = "coverage", content = "detail", rename_all = "kebab-case")]
    enum Coverage {
        Complete,
        Partial { completed: u32, total: u32 },
    }

    #[derive(Serialize)]
    #[serde(rename_all = "kebab-case")]
    enum External {
        Range { low: u8 },
    }

    #[test]
    fn typed_values_read_as_their_wire_words_never_as_rust_syntax() {
        assert_eq!(wire_words(&Reason::NotFound), "not-found");
        assert_eq!(wire_words(&Coverage::Complete), "complete");
        assert_eq!(
            wire_words(&Coverage::Partial {
                completed: 12,
                total: 13
            }),
            "partial (completed 12, total 13)"
        );
        assert_eq!(wire_words(&External::Range { low: 1 }), "range (low 1)");
        assert_eq!(
            wire_words(&[Reason::NotFound, Reason::NotFound][..]),
            "not-found, not-found"
        );
    }
}

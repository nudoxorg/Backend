//! A tiny JSON writer for harness reports (facet carries no serde).

use std::fmt::{self, Write as _};

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A number (non-finite values print as `null`).
    Num(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object, in insertion order.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// An object from `(key, value)` pairs.
    #[must_use]
    pub fn obj<const N: usize>(pairs: [(&str, Json); N]) -> Self {
        Self::Obj(
            pairs
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    /// A string value.
    #[must_use]
    pub fn str(value: impl Into<String>) -> Self {
        Self::Str(value.into())
    }

    /// A number value.
    #[must_use]
    pub fn num(value: impl Into<f64>) -> Self {
        Self::Num(value.into())
    }

    /// `Some(x)` as a number, `None` as null.
    #[must_use]
    pub fn opt(value: Option<impl Into<f64>>) -> Self {
        value.map_or(Self::Null, |value| Self::Num(value.into()))
    }
}

fn number(out: &mut fmt::Formatter<'_>, value: f64) -> fmt::Result {
    if !value.is_finite() {
        return out.write_str("null");
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return write!(out, "{value:.0}");
    }
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    out.write_str(if text.is_empty() || text == "-" || text == "-0" {
        "0"
    } else {
        text
    })
}

fn string(out: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    out.write_char('"')?;
    for ch in value.chars() {
        match ch {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            ch if (ch as u32) < 0x20 => write!(out, "\\u{:04x}", ch as u32)?,
            ch => out.write_char(ch)?,
        }
    }
    out.write_char('"')
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(value) => write!(f, "{value}"),
            Self::Num(value) => number(f, *value),
            Self::Str(value) => string(f, value),
            Self::Arr(items) => {
                f.write_char('[')?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_char(',')?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_char(']')
            }
            Self::Obj(pairs) => {
                f.write_char('{')?;
                for (index, (key, value)) in pairs.iter().enumerate() {
                    if index > 0 {
                        f.write_char(',')?;
                    }
                    string(f, key)?;
                    write!(f, ":{value}")?;
                }
                f.write_char('}')
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Json;

    #[test]
    fn prints_compact_valid_json() {
        let value = Json::obj([
            ("a", Json::num(1.5)),
            (
                "b",
                Json::Arr(vec![Json::Null, Json::Bool(true), Json::num(2.0)]),
            ),
            ("c", Json::str("q\"\\\n")),
            ("d", Json::num(f64::NAN)),
        ]);
        assert_eq!(
            value.to_string(),
            r#"{"a":1.5,"b":[null,true,2],"c":"q\"\\\n","d":null}"#
        );
    }
}

//! `Const`, compile-time constant declaration kind.
use crate::{kinds::Type, visitor::Visitor};

/// One lexical atom in a constant expression.
///
/// This is deliberately a small, language-neutral representation.  Keeping
/// the atoms (rather than treating the whole initializer as a value-shaped
/// source string) lets consumers distinguish literals, names, and operators
/// without committing the IR to one producer's parser.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum ConstToken {
    Identifier(String),
    Number(String),
    String(String),
    Character(String),
    Punctuation(String),
}

/// A typed source-level constant expression.
///
/// `source` is retained as an optional spelling for display and round-trip
/// compatibility.  Semantic consumers must use `tokens`: the expression
/// value is represented as a token sequence, not as an opaque source string.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct ConstExpr {
    pub ty: Type,
    /// Tokenized expression value.  Missing on old serialized IR.
    #[serde(default)]
    pub tokens: Box<[ConstToken]>,
    /// Original spelling, for display only and compatibility with old IR.
    #[serde(default)]
    pub source: String,
}

#[bon::bon]
impl ConstExpr {
    #[builder]
    pub fn new(ty: Type, source: String) -> Self {
        let tokens = tokenize(&source);
        ConstExpr { ty, tokens, source }
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub fn tokens(&self) -> &[ConstToken] {
        &self.tokens
    }
}

/// Tokenize an initializer without assigning language-specific meaning to it.
///
/// The tokenizer is intentionally conservative: quoted literals are kept as
/// one atom, identifiers and numeric runs are classified, and every other
/// character is retained as punctuation.  Thus even an expression a producer
/// cannot evaluate remains structurally distinguishable after lowering.
fn tokenize(source: &str) -> Box<[ConstToken]> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
            let start = i;
            i += 1;
            while i < chars.len() {
                let escaped = i > start && chars[i - 1] == '\\';
                let end = chars[i] == quote && !escaped;
                i += 1;
                if end {
                    break;
                }
            }
            let atom: String = chars[start..i].iter().collect();
            tokens.push(if quote == '"' {
                ConstToken::String(atom)
            } else {
                ConstToken::Character(atom)
            });
            continue;
        }
        if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            tokens.push(ConstToken::Identifier(chars[start..i].iter().collect()));
            continue;
        }
        if chars[i].is_ascii_digit()
            || (chars[i] == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
        {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || matches!(chars[i], '.' | '_')
                    || (matches!(chars[i], '+' | '-')
                        && i > start
                        && matches!(chars[i - 1], 'e' | 'E')))
            {
                i += 1;
            }
            tokens.push(ConstToken::Number(chars[start..i].iter().collect()));
            continue;
        }
        tokens.push(ConstToken::Punctuation(chars[i].to_string()));
        i += 1;
    }
    tokens.into_boxed_slice()
}

/// A compile-time constant declaration.
///
/// The const's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Const {
    /// The declared type of the constant.
    pub ty: Type,

    /// The constant's typed value, if available.
    pub value: Option<ConstExpr>,
}

#[bon::bon]
impl Const {
    #[builder]
    pub fn new(ty: Type, value: Option<ConstExpr>) -> Self {
        Const { ty, value }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Builder + serde round-trip for `Const.value`.
    #[test]
    fn const_value_roundtrip() {
        let c = Const::builder()
            .ty(Type::U64)
            .value(
                ConstExpr::builder()
                    .ty(Type::U64)
                    .source("18446744073709551615".to_owned())
                    .build(),
            )
            .build();

        assert_eq!(
            c.value.as_ref().map(|value| value.source.as_str()),
            Some("18446744073709551615")
        );

        let json = serde_json::to_string(&c).expect("serialize failed");
        let rt: Const = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(c, rt);
    }

    /// `value` defaults to `None` when not set via the builder.
    #[test]
    fn const_value_default_none() {
        let c = Const::builder().ty(Type::I32).build();
        assert_eq!(c.value, None);
    }

    #[test]
    fn expressions_keep_structure_beyond_spelling() {
        let literal = ConstExpr::builder()
            .ty(Type::I32)
            .source("1 + 2".to_owned())
            .build();
        let name = ConstExpr::builder()
            .ty(Type::I32)
            .source("one + two".to_owned())
            .build();

        assert_ne!(literal.tokens(), name.tokens());
        assert_eq!(
            literal.tokens(),
            &[
                ConstToken::Number("1".to_owned()),
                ConstToken::Punctuation("+".to_owned()),
                ConstToken::Number("2".to_owned()),
            ]
        );
    }
}

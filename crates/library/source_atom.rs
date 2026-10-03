//! Exact bounded UTF-8 text retained from source facts and semantic atoms.

use crate::{MAX_PRODUCT_TEXT_BYTES, ProductAdmissionError};
use serde::{
    Deserialize, Serialize,
    de::{self, Visitor},
};
use std::fmt;

/// Exact, bounded UTF-8 spelling from a source or semantic atom.
///
/// This type deliberately does not normalize text: empty and whitespace-only
/// values and embedded NUL are valid atom contents. Its boxed string keeps
/// retained capacity proportional to the admitted byte length. A transport
/// must enforce its outer frame bound before serde parses or allocates JSON
/// strings; this visitor bounds the retained typed value.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SourceAtomText(Box<str>);

impl SourceAtomText {
    /// Copies exact UTF-8 text after checking its byte bound.
    pub fn new(value: &str) -> Result<Self, ProductAdmissionError> {
        if value.len() > MAX_PRODUCT_TEXT_BYTES {
            return Err(ProductAdmissionError::TextBound);
        }
        Ok(Self(value.into()))
    }

    /// Admits an owned exact string without retaining excess string capacity.
    pub fn try_from_string(value: String) -> Result<Self, ProductAdmissionError> {
        if value.len() > MAX_PRODUCT_TEXT_BYTES {
            return Err(ProductAdmissionError::TextBound);
        }
        Ok(Self(value.into_boxed_str()))
    }

    /// Returns the exact admitted spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SourceAtomText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SourceAtomTextVisitor;

        impl<'de> Visitor<'de> for SourceAtomTextVisitor {
            type Value = SourceAtomText;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("bounded exact UTF-8 source atom text")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                SourceAtomText::new(value).map_err(E::custom)
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_str(value)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                SourceAtomText::try_from_string(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(SourceAtomTextVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::SourceAtomText;
    use crate::MAX_PRODUCT_TEXT_BYTES;

    #[test]
    fn exact_atom_text_round_trips_empty_whitespace_nul_and_unicode() {
        for value in ["", " ", "\0", "λ雪", " padded "] {
            let atom = SourceAtomText::new(value).expect("bounded exact atom");
            assert_eq!(atom.as_str(), value);
            let encoded = serde_json::to_vec(&atom).expect("atom JSON encodes");
            let decoded: SourceAtomText =
                serde_json::from_slice(&encoded).expect("atom JSON decodes");
            assert_eq!(decoded.as_str(), value);
        }
    }

    #[test]
    fn exact_atom_text_rejects_oversized_and_non_string_wire_values() {
        let oversized = "x".repeat(MAX_PRODUCT_TEXT_BYTES + 1);
        assert!(SourceAtomText::new(&oversized).is_err());
        let encoded = serde_json::to_string(&oversized).expect("oversized test string encodes");
        assert!(serde_json::from_str::<SourceAtomText>(&encoded).is_err());
        assert!(serde_json::from_str::<SourceAtomText>("null").is_err());
    }
}

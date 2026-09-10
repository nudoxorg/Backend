//! Defines parse behavior for `interface-search`, whose purpose is to define one honest multi-lane search vocabulary and its ranking over every retrieval backend.
//! This module owns the parse invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Deciding what a person meant by the text they typed, before any lane spends work on it.

use interface_identity::{Address, ContentKey, KeyExactness};

use crate::QueryText;

/// What one query turned out to be.
///
/// The exact lane needs this decision and so does every surface that wants to say *why* a result
/// matched. Making it a value rather than a chain of `if let`s inside a lane means the CLI, the
/// MCP server, and the GUI all classify the same text the same way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryShape {
    /// A complete readable address, with or without a key.
    Address(Address),
    /// A bare content key typed or pasted on its own.
    Key {
        /// The parsed key.
        key: ContentKey,
        /// Whether the spelling pinned one instance or only its family.
        exactness: KeyExactness,
    },
    /// A declaration name, or a prefix of one.
    Name(Box<str>),
}

/// Classifies one query without consulting any index.
///
/// A bare key is recognised first because its spelling is unambiguous; an address next, because it
/// carries an ecosystem tag and a version that a name never does; everything else is a name.
#[must_use]
pub fn classify(text: &QueryText) -> QueryShape {
    let raw = text.as_str();
    if let Ok((key, exactness)) = ContentKey::parse(raw) {
        return QueryShape::Key { key, exactness };
    }
    if raw.contains(':')
        && raw.contains('@')
        && let Ok(address) = Address::parse(raw)
    {
        return QueryShape::Address(address);
    }
    QueryShape::Name(raw.into())
}

/// The bare name a query asks about, whatever shape it took.
///
/// An address contributes its leaf segment, a key contributes nothing, and a name contributes
/// itself. This is what the lexical lane indexes against.
#[must_use]
pub fn query_term(shape: &QueryShape) -> Option<Box<str>> {
    match shape {
        QueryShape::Address(address) => address
            .path
            .leaf()
            .map(|segment| segment.name.as_str().into()),
        QueryShape::Key { .. } => None,
        QueryShape::Name(name) => Some(name.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_is_recognised_from_its_own_spelling() -> Result<(), crate::QueryTextError> {
        let name = classify(&QueryText::new("deserialize_map")?);
        assert_eq!(query_term(&name).as_deref(), Some("deserialize_map"));
        let address = classify(&QueryText::new("cargo:serde@1.0.196::de::from_str")?);
        assert!(matches!(address, QueryShape::Address(_)));
        assert_eq!(query_term(&address).as_deref(), Some("from_str"));
        let key = classify(&QueryText::new(&"ab".repeat(16))?);
        assert!(matches!(
            key,
            QueryShape::Key {
                exactness: KeyExactness::FamilyOnly,
                ..
            }
        ));
        assert_eq!(query_term(&key), None);
        Ok(())
    }
}

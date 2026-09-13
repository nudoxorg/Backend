/// A schema supplies a domain-separated canonical value encoding.
pub trait Schema: 'static {
    /// Domain byte assigned to the schema family.
    const DOMAIN: u8;
    /// Stable type tag within the domain.
    const TYPE: u16;
    /// Canonical value encoding version.
    const VERSION: u8 = CANONICAL_VERSION;
    /// Value represented by this schema.
    type Value: ?Sized;
    /// Appends the canonical bytes for one value.
    fn encode(value: &Self::Value, out: &mut Vec<u8>);
}

/// A relation supplies a domain-separated key/value schema.
pub trait Relation: 'static {
    /// Domain byte assigned to the relation family.
    const DOMAIN: u8;
    /// Stable relation type tag within the domain.
    const TYPE: u16;
    /// Canonical relation encoding version.
    const VERSION: u8 = CANONICAL_VERSION;
    /// Ordered logical key type.
    type Key: Ord + Clone + fmt::Debug + Eq;
    /// Complete logical value type.
    type Value: Clone + fmt::Debug + Eq;
    /// Appends the canonical bytes for one key.
    fn encode_key(key: &Self::Key, out: &mut Vec<u8>);
    /// Appends the canonical bytes for one value.
    fn encode_value(value: &Self::Value, out: &mut Vec<u8>);
}

/// Failure while decoding a relation key or value for canonical admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationDecodeError {
    /// The relation has no decoder for this canonical representation.
    Unsupported,
    /// The bytes are not one complete value in the relation's grammar.
    Malformed,
}

impl fmt::Display for RelationDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid relation value encoding: {self:?}")
    }
}

impl std::error::Error for RelationDecodeError {}

/// A relation with a bounded inverse for canonical node admission.
///
/// [`Relation`] deliberately requires only an encoder so producers can build
/// canonical roots without carrying a parser.  Storage and replication
/// boundaries must use this stronger capability before treating arbitrary
/// bytes as a typed relation root.  Decoders must consume exactly one value;
/// admission re-encodes the decoded value and rejects any noncanonical byte
/// spelling.
pub trait CanonicalRelation: Relation {
    /// Decodes one complete canonical key body.
    ///
    /// # Errors
    ///
    /// Returns [`RelationDecodeError::Unsupported`] when this relation does
    /// not expose a decoder, or [`RelationDecodeError::Malformed`] when the
    /// body cannot be decoded as one key.
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError>;
    /// Decodes one complete canonical value body.
    ///
    /// # Errors
    ///
    /// Returns [`RelationDecodeError::Unsupported`] when this relation does
    /// not expose a decoder, or [`RelationDecodeError::Malformed`] when the
    /// body cannot be decoded as one value.
    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError>;
}
use core::fmt;

use super::CANONICAL_VERSION;

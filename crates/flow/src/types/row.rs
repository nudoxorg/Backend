//! Canonical row identities, weighted deltas, and relation-root adapters.

use super::time::Time;
use crate::FlowError;
use backend_version::{
    CanonicalRelation, DeltaId, ObjectKey, ObjectVersion, Relation, RelationDecodeError, Schema,
    StateRoot,
};
use std::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem::size_of_val,
    sync::Arc,
};

/// Canonical payload encoding required when a value contributes to a batch,
/// run, or arrangement root identity.
pub trait CanonicalValue {
    /// Appends the canonical representation of this value.
    fn encode_canonical(&self, out: &mut Vec<u8>);

    /// Returns the canonical payload bytes required for work admission.
    /// Implementations with heap backed payloads should include that storage;
    /// the default covers fixed size values.
    fn canonical_len(&self) -> usize {
        size_of_val(self)
    }

    /// Returns bytes that must be reserved in addition to the inline value
    /// slot already covered by its enclosing row. Heap backed values override
    /// [`Self::canonical_len`] and therefore reserve their owned payload here.
    fn owned_bytes(&self) -> usize {
        self.canonical_len().saturating_sub(size_of_val(self))
    }

    /// Appends an injective, prefix-free representation whose byte order
    /// follows `Ord` exactly.
    ///
    /// This has no default: a content encoding is not necessarily an order
    /// encoding (signed integers are the simplest counterexample). Requiring
    /// every value type to state both contracts prevents authenticated tree
    /// order from silently diverging from Rust's `Ord` implementation.
    fn encode_ordered(&self, out: &mut Vec<u8>);
}
/// Schema for the stable identity of a relation recipe/source binding.
pub struct RelationSchema;
impl Schema for RelationSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 1;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema for an executable recipe identity.
pub struct RecipeSchema;
impl Schema for RecipeSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 2;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema for a stable logical object key used in row handles.
pub struct ObjectSchema;
impl Schema for ObjectSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 3;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// An opaque digest reference carried inside a row coordinate.
///
/// These references are authenticated by the containing canonical row/node
/// bytes. They are deliberately separate from `ObjectKey`, whose public
/// admission requires a canonical preimage.
pub struct DigestReference<S: Schema> {
    bytes: [u8; 32],
    marker: PhantomData<fn() -> S>,
}

impl<S: Schema> fmt::Debug for DigestReference<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DigestReference")
            .field(&self.bytes)
            .finish()
    }
}

impl<S: Schema> Copy for DigestReference<S> {}

impl<S: Schema> Clone for DigestReference<S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: Schema> PartialEq for DigestReference<S> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<S: Schema> Eq for DigestReference<S> {}

impl<S: Schema> PartialOrd for DigestReference<S> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<S: Schema> Ord for DigestReference<S> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

impl<S: Schema> Hash for DigestReference<S> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}

impl<S: Schema> DigestReference<S> {
    /// Derives the reference from a canonical schema value.
    #[must_use]
    pub fn from_value(value: &S::Value) -> Self {
        let key = ObjectKey::<S>::from_value(value);
        Self::from_bytes(*key.as_bytes())
    }

    /// Retains a digest already authenticated by its enclosing wire object.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            bytes,
            marker: PhantomData,
        }
    }

    /// Returns the fixed-width digest reference bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

/// Schema-marked relation digest reference.
pub type RelationIdentity = DigestReference<RelationSchema>;
/// Schema-marked immutable recipe identity.
pub type RecipeIdentity = ObjectVersion<RecipeSchema>;
/// Schema-marked object digest reference.
pub type ObjectIdentity = DigestReference<ObjectSchema>;

/// Schema for an exact immutable input manifest identity.
pub struct InputSchema;
impl Schema for InputSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 5;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema for an exact validated dependency read-set identity.
pub struct ReadSchema;
impl Schema for ReadSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 6;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema for an authority policy/receipt identity.
pub struct AuthoritySchema;
impl Schema for AuthoritySchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 7;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema for an output equivalence contract identity.
pub struct EquivalenceSchema;
impl Schema for EquivalenceSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 8;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Exact immutable input manifest identity used by demand and factor plans.
pub type InputIdentity = ObjectVersion<InputSchema>;
/// Exact validated read-set identity used by demand and factor plans.
pub type ReadIdentity = ObjectVersion<ReadSchema>;
/// Exact authority policy and receipt identity used by demand and factor plans.
pub type AuthorityIdentity = ObjectVersion<AuthoritySchema>;
/// Exact output equivalence contract identity used by demand and factor plans.
pub type EquivalenceIdentity = ObjectVersion<EquivalenceSchema>;

/// A stable relation row key. Its identity-bearing fields are typed by
/// `backend-version`; `key` is an ordered relation coordinate, not an ID.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowKey {
    /// Relation schema identity.
    pub relation: RelationIdentity,
    /// Stable object key within that relation.
    pub object: ObjectIdentity,
    /// Ordered row coordinate.
    pub key: u64,
}

/// Checked signed multiplicity used by weighted relation updates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Weight(i64);

impl Weight {
    /// Constructs a checked non-zero weight.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] for a zero multiplicity.
    pub const fn new(value: i64) -> Result<Self, FlowError> {
        if value == 0 {
            Err(FlowError::ZeroDiff)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the signed integer value.
    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }

    /// Checked addition of two weights. `None` means the two updates cancel.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Overflow`] when the sum is outside `i64`.
    pub fn checked_add(self, other: Self) -> Result<Option<Self>, FlowError> {
        let value = self.0.checked_add(other.0).ok_or(FlowError::Overflow)?;
        Ok((value != 0).then_some(Self(value)))
    }

    /// Checked multiplication of two weights.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Overflow`] when the product is outside `i64`.
    pub fn checked_mul(self, other: Self) -> Result<Self, FlowError> {
        let value = self.0.checked_mul(other.0).ok_or(FlowError::Overflow)?;
        debug_assert_ne!(value, 0);
        Ok(Self(value))
    }

    /// Checked negation of a weight.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Overflow`] when negating `i64::MIN`.
    pub fn checked_neg(self) -> Result<Self, FlowError> {
        Ok(Self(self.0.checked_neg().ok_or(FlowError::Overflow)?))
    }

    pub(crate) fn from_nonzero(value: i64) -> Result<Self, FlowError> {
        Self::new(value)
    }

    pub(crate) const fn one() -> Self {
        Self(1)
    }

    pub(crate) const fn minus_one() -> Self {
        Self(-1)
    }

    /// Re-enters a support value that was already checked before insertion in
    /// an arrangement's private visible map.
    pub(crate) const fn from_visible_support(value: i64) -> Self {
        Self(value)
    }
}

/// A weighted relation update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delta<V> {
    /// Stable row key.
    pub key: RowKey,
    /// Relation payload.
    pub value: V,
    /// Logical update time.
    pub time: Time,
    /// Signed multiplicity.
    pub diff: Weight,
}

impl<V> Delta<V> {
    /// Constructs an update after checking that its multiplicity is non-zero.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] when `diff` is zero.
    pub fn checked(key: RowKey, value: V, time: Time, diff: i64) -> Result<Self, FlowError> {
        Weight::new(diff).map(|diff| Self {
            key,
            value,
            time,
            diff,
        })
    }

    /// Returns whether this update has no effect.
    #[must_use]
    pub const fn is_zero(&self) -> bool {
        self.diff.value() == 0
    }
}

/// Typed relation used for arrangement state roots.
#[derive(Debug)]
pub struct ArrangementRelation<V>(pub(crate) PhantomData<fn() -> V>);

/// Arrangement key whose comparison is the exact bytes persisted in the
/// authenticated relation.  Keeping the encoded form beside the typed parts
/// prevents a downstream `Ord` implementation from disagreeing with the
/// canonical wire order.
#[derive(Clone, Debug)]
pub struct ArrangementKey<V> {
    row: RowKey,
    value: V,
    encoded: Arc<[u8]>,
}

impl<V: CanonicalValue> ArrangementKey<V> {
    /// Builds a key and freezes its canonical ordering bytes.
    #[must_use]
    pub fn new(row: RowKey, value: V) -> Self {
        let mut encoded = Vec::new();
        encode_arrangement_key(
            row.relation.as_bytes(),
            row.object.as_bytes(),
            row.key,
            &value,
            &mut encoded,
        );
        Self {
            row,
            value,
            encoded: Arc::from(encoded.into_boxed_slice()),
        }
    }

    /// Returns the typed row identity.
    #[must_use]
    pub const fn row(&self) -> RowKey {
        self.row
    }

    /// Borrows the typed payload.
    #[must_use]
    pub const fn value(&self) -> &V {
        &self.value
    }

    /// Returns the authenticated ordering bytes.
    #[must_use]
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// Splits the key back into its typed row identity and payload.
    #[must_use]
    pub fn into_parts(self) -> (RowKey, V) {
        (self.row, self.value)
    }
}

impl<V> PartialEq for ArrangementKey<V> {
    fn eq(&self, other: &Self) -> bool {
        self.encoded == other.encoded
    }
}
impl<V> Eq for ArrangementKey<V> {}
impl<V> PartialOrd for ArrangementKey<V> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<V> Ord for ArrangementKey<V> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.encoded.cmp(&other.encoded)
    }
}
fn encode_arrangement_key<V: CanonicalValue>(
    relation: &[u8; 32],
    object: &[u8; 32],
    key: u64,
    value: &V,
    out: &mut Vec<u8>,
) {
    out.extend_from_slice(relation);
    out.extend_from_slice(object);
    out.extend_from_slice(&key.to_be_bytes());
    value.encode_ordered(out);
}

impl<V: Clone + Ord + fmt::Debug + Eq + CanonicalValue + 'static> Relation
    for ArrangementRelation<V>
{
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 0x100;
    type Key = ArrangementKey<V>;
    type Value = i64;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        // The relation key is also the wire key used by lazy reopening. Keep
        // it injective and lexicographically ordered so canonical node
        // admission sees exactly the same order as the typed tree builder.
        encode_arrangement_key(
            key.row.relation.as_bytes(),
            key.row.object.as_bytes(),
            key.row.key,
            &key.value,
            out,
        );
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

impl<V> CanonicalRelation for ArrangementRelation<V>
where
    V: Clone + Ord + fmt::Debug + Eq + CanonicalValue + crate::CheckpointValue + 'static,
{
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        if bytes.len() < 72 {
            return Err(RelationDecodeError::Malformed);
        }
        let original = bytes;
        let relation: [u8; 32] = bytes[..32]
            .try_into()
            .map_err(|_| RelationDecodeError::Malformed)?;
        let object: [u8; 32] = bytes[32..64]
            .try_into()
            .map_err(|_| RelationDecodeError::Malformed)?;
        let key = u64::from_be_bytes(
            bytes[64..72]
                .try_into()
                .map_err(|_| RelationDecodeError::Malformed)?,
        );
        let mut value_bytes = &bytes[72..];
        let value = V::decode_checkpoint_ordered(&mut value_bytes)
            .map_err(|_| RelationDecodeError::Malformed)?;
        if !value_bytes.is_empty() {
            return Err(RelationDecodeError::Malformed);
        }
        let key = ArrangementKey::new(
            RowKey {
                relation: RelationIdentity::from_bytes(relation),
                object: ObjectIdentity::from_bytes(object),
                key,
            },
            value,
        );
        if key.encoded() != original {
            return Err(RelationDecodeError::Malformed);
        }
        Ok(key)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        bytes
            .try_into()
            .map(i64::from_be_bytes)
            .map_err(|_| RelationDecodeError::Malformed)
    }
}

/// Compatibility name for the single arrangement relation schema.
///
/// Logical state and canonical wire admission deliberately share one marker;
/// two Rust types must never claim the same `(DOMAIN, TYPE)` identity.
pub type ArrangementCanonicalRelation<V = i64> = ArrangementRelation<V>;

/// Schema-marked root of one arrangement's visible weighted relation.
pub type ArrangementRoot<V> = StateRoot<ArrangementRelation<V>>;
/// Backend-version transition identity for one arrangement relation.
pub type ArrangementDeltaId<V> = DeltaId<ArrangementRelation<V>>;

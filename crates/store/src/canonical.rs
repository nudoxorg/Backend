use super::{DEFAULT_CUT_POLICY, Hash, Relation, StoreError};
use backend_version::{CanonicalRelation, RelationDecodeError};
use std::mem::size_of;

pub(crate) fn put_u32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}
/// Raw relation adapter used by the byte-oriented storage kernel. Production
/// callers should define a domain-specific `backend_version::Relation`.
#[derive(Debug, Eq, PartialEq)]
/// Byte-oriented relation used by the store's public map facade.
pub struct RawRelation;
impl Relation for RawRelation {
    const DOMAIN: u8 = 0x73;
    const TYPE: u16 = 1;
    type Key = Vec<u8>;
    type Value = StoredValue;
    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        let Ok(length) = u32::try_from(key.len()) else {
            return;
        };
        put_u32(out, length);
        out.extend_from_slice(key);
    }
    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        value.encode_canonical(out);
    }
}

impl CanonicalRelation for RawRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        let length = bytes
            .get(..4)
            .and_then(|prefix| prefix.try_into().ok())
            .map(u32::from_le_bytes)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(RelationDecodeError::Malformed)?;
        let end = 4usize
            .checked_add(length)
            .ok_or(RelationDecodeError::Malformed)?;
        if end != bytes.len() {
            return Err(RelationDecodeError::Malformed);
        }
        Ok(bytes[4..end].to_vec())
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        let availability = *bytes.first().ok_or(RelationDecodeError::Malformed)?;
        let mut at = 1usize;
        let value_length = usize::try_from(read_u32(bytes, &mut at)?)
            .map_err(|_| RelationDecodeError::Malformed)?;
        let value_end = at
            .checked_add(value_length)
            .ok_or(RelationDecodeError::Malformed)?;
        let value = bytes
            .get(at..value_end)
            .ok_or(RelationDecodeError::Malformed)?
            .to_vec();
        at = value_end;

        let reference_count = usize::try_from(read_u32(bytes, &mut at)?)
            .map_err(|_| RelationDecodeError::Malformed)?;
        let reference_bytes = reference_count
            .checked_mul(size_of::<Hash>())
            .ok_or(RelationDecodeError::Malformed)?;
        let references_end = at
            .checked_add(reference_bytes)
            .ok_or(RelationDecodeError::Malformed)?;
        if references_end != bytes.len() {
            return Err(RelationDecodeError::Malformed);
        }
        let mut references = Vec::with_capacity(reference_count);
        for chunk in bytes[at..references_end].chunks_exact(size_of::<Hash>()) {
            references.push(
                chunk
                    .try_into()
                    .map_err(|_| RelationDecodeError::Malformed)?,
            );
        }
        Ok(StoredValue {
            value,
            availability,
            references,
        })
    }
}
/// Complete immutable value version associated with one map key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredValue {
    /// Canonical value bytes.
    pub value: Vec<u8>,
    /// Availability state carried by the value version.
    pub availability: u8,
    /// Content-addressed references captured by the value.
    pub references: Vec<Hash>,
}

/// Compatibility name for the byte-oriented value facade.
pub type RawValue = StoredValue;

pub(crate) fn raw_value(value: &StoredValue) -> StoredValue {
    value.clone()
}

fn read_u32(bytes: &[u8], at: &mut usize) -> Result<u32, RelationDecodeError> {
    let end = at
        .checked_add(size_of::<u32>())
        .ok_or(RelationDecodeError::Malformed)?;
    let value = bytes.get(*at..end).ok_or(RelationDecodeError::Malformed)?;
    *at = end;
    value
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| RelationDecodeError::Malformed)
}

impl StoredValue {
    /// Constructs a complete value version.
    pub fn new(value: impl Into<Vec<u8>>, availability: u8, references: Vec<Hash>) -> Self {
        Self {
            value: value.into(),
            availability,
            references,
        }
    }
    pub(crate) fn encode(&self, out: &mut Vec<u8>) -> Result<(), StoreError> {
        out.push(self.availability);
        put_u32(out, checked_wire_len(self.value.len())?);
        out.extend_from_slice(&self.value);
        put_u32(out, checked_wire_len(self.references.len())?);
        for r in &self.references {
            out.extend_from_slice(r);
        }
        Ok(())
    }

    fn encode_canonical(&self, out: &mut Vec<u8>) {
        let Ok(value_len) = u32::try_from(self.value.len()) else {
            return;
        };
        let Ok(reference_len) = u32::try_from(self.references.len()) else {
            return;
        };
        out.push(self.availability);
        put_u32(out, value_len);
        out.extend_from_slice(&self.value);
        put_u32(out, reference_len);
        for reference in &self.references {
            out.extend_from_slice(reference);
        }
    }
}

pub(crate) fn checked_wire_len(length: usize) -> Result<u32, StoreError> {
    u32::try_from(length).map_err(|_| StoreError::Bounds)
}

pub(crate) fn checked_key_len(length: usize) -> Result<u32, StoreError> {
    u32::try_from(length).map_err(|_| StoreError::OversizedKey)
}

pub(crate) fn value_wire_len(value: &StoredValue) -> Result<usize, StoreError> {
    checked_wire_len(value.references.len())?;
    let reference_bytes = value
        .references
        .len()
        .checked_mul(size_of::<Hash>())
        .ok_or(StoreError::Bounds)?;
    1usize
        .checked_add(4)
        .and_then(|length| length.checked_add(value.value.len()))
        .and_then(|length| length.checked_add(4))
        .and_then(|length| length.checked_add(reference_bytes))
        .ok_or(StoreError::Bounds)
}

pub(crate) fn record_wire_len(key: &[u8], value: &StoredValue) -> Result<usize, StoreError> {
    checked_key_len(key.len())?;
    let value_len = value_wire_len(value)?;
    4usize
        .checked_add(key.len())
        .and_then(|length| length.checked_add(value_len))
        .ok_or(StoreError::Bounds)
}

pub(crate) fn validate_value(value: &StoredValue) -> Result<(), StoreError> {
    checked_wire_len(value.value.len())?;
    checked_wire_len(value.references.len())?;
    Ok(())
}

pub(crate) fn validate_entry(key: &[u8], value: &StoredValue) -> Result<(), StoreError> {
    checked_key_len(key.len())?;
    let key_field_len = key.len().checked_add(4).ok_or(StoreError::OversizedKey)?;
    if key_field_len > DEFAULT_CUT_POLICY.max_encoded_bytes() {
        return Err(StoreError::OversizedKey);
    }
    validate_value(value)?;
    if record_wire_len(key, value)? > DEFAULT_CUT_POLICY.max_encoded_bytes() {
        return Err(StoreError::Bounds);
    }
    Ok(())
}

pub(crate) fn validate_entries(entries: &[(Vec<u8>, StoredValue)]) -> Result<(), StoreError> {
    for (key, value) in entries {
        validate_entry(key, value)?;
    }
    Ok(())
}

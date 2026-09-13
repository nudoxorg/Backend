//! Wire claims and checked identity admission.

use super::schema::{IntentId, IntentSchema};
use backend_version::{
    DeltaId as VersionDeltaId, ID_BYTES, IdAdmissionError, IdContext, IdDecodeError, ObjectKey,
    ObjectVersion, Relation, Schema, StateRoot, WireId,
};
use core::fmt;

/// Identity admission/decoding failure at a product transport boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// The wire text was not a fixed-width hexadecimal identity.
    InvalidHex,
    /// The typed schema context did not match the claim.
    Admission(IdAdmissionError),
    /// The claim had the wrong fixed-width representation.
    Decode(IdDecodeError),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHex => f.write_str("identity is not 64 hexadecimal characters"),
            Self::Admission(error) => error.fmt(f),
            Self::Decode(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for IdentityError {}

impl From<IdAdmissionError> for IdentityError {
    fn from(error: IdAdmissionError) -> Self {
        Self::Admission(error)
    }
}

impl From<IdDecodeError> for IdentityError {
    fn from(error: IdDecodeError) -> Self {
        Self::Decode(error)
    }
}

/// Encodes an accepted identity for a wire payload.
#[must_use]
pub fn encode_id(bytes: &[u8; ID_BYTES]) -> String {
    let mut out = String::with_capacity(ID_BYTES * 2);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Decodes one fixed-width hexadecimal wire identity.
///
/// # Errors
///
/// Returns [`IdentityError::InvalidHex`] when the text is not exactly one
/// fixed-width hexadecimal digest.
pub fn decode_id(text: &str) -> Result<[u8; ID_BYTES], IdentityError> {
    if text.len() != ID_BYTES * 2 {
        return Err(IdentityError::InvalidHex);
    }
    let bytes = text.as_bytes();
    let mut out = [0u8; ID_BYTES];
    for (index, slot) in out.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2]).ok_or(IdentityError::InvalidHex)?;
        let low = hex_nibble(bytes[index * 2 + 1]).ok_or(IdentityError::InvalidHex)?;
        *slot = (high << 4) | low;
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parses an object-key claim while retaining its untrusted wire context.
///
/// This function deliberately does not return a typed key.  Use
/// [`admit_key_value`] after the caller has the canonical logical value.
///
/// # Errors
///
/// Returns an identity decoding or schema-context error for malformed wire
/// text.
pub fn wire_key<T: Schema>(text: &str) -> Result<WireId<T>, IdentityError> {
    let bytes = decode_id(text)?;
    WireId::<T>::decode(&bytes, IdContext::object_key::<T>()).map_err(IdentityError::from)
}

/// Admits an object-key claim by recomputing its digest from the canonical
/// logical value.
///
/// # Errors
///
/// Returns an identity decoding or admission error when the claim does not
/// match the supplied logical value and schema.
pub fn admit_key_value<T: Schema>(
    text: &str,
    value: &T::Value,
) -> Result<ObjectKey<T>, IdentityError> {
    ObjectKey::admit_value(wire_key::<T>(text)?.into_untrusted(), value)
        .map_err(IdentityError::from)
}

/// Parses an object-version claim while retaining its untrusted wire context.
///
/// # Errors
///
/// Returns an identity decoding or schema-context error for malformed wire
/// text.
pub fn wire_version<T: Schema>(text: &str) -> Result<WireId<T>, IdentityError> {
    let bytes = decode_id(text)?;
    WireId::<T>::decode(&bytes, IdContext::schema::<T>()).map_err(IdentityError::from)
}

/// Admits an object-version claim by recomputing its digest from the complete
/// canonical logical value.
///
/// # Errors
///
/// Returns an identity decoding or admission error when the claim does not
/// match the supplied logical value and schema.
pub fn admit_version_value<T: Schema>(
    text: &str,
    value: &T::Value,
) -> Result<ObjectVersion<T>, IdentityError> {
    ObjectVersion::admit_value(wire_version::<T>(text)?.into_untrusted(), value)
        .map_err(IdentityError::from)
}

/// Parses a durable intent claim while retaining its untrusted context.
///
/// # Errors
///
/// Returns an identity decoding or schema-context error for malformed wire
/// text.
pub fn wire_intent(text: &str) -> Result<WireId<IntentSchema>, IdentityError> {
    wire_version::<IntentSchema>(text)
}

/// Admits a durable intent claim from its operation token and payload.
///
/// # Errors
///
/// Returns an identity decoding or admission error when the claim does not
/// match the operation token and payload.
pub fn admit_intent_value(
    text: &str,
    token: &str,
    payload: &[u8],
) -> Result<IntentId, IdentityError> {
    let value = super::values::intent_preimage(token, payload);
    ObjectVersion::admit_value(wire_intent(text)?.into_untrusted(), &value)
        .map_err(IdentityError::from)
}

/// Parses a relation-root claim while retaining its untrusted context.
///
/// # Errors
///
/// Returns an identity decoding or schema-context error for malformed wire
/// text.
pub fn wire_root<R: Relation>(text: &str) -> Result<WireId<R>, IdentityError> {
    let bytes = decode_id(text)?;
    WireId::<R>::decode(&bytes, IdContext::relation::<R>()).map_err(IdentityError::from)
}

/// Admits a relation-root claim from the exact canonical node bytes.
///
/// # Errors
///
/// Returns an identity decoding or admission error when the claim does not
/// match the canonical relation bytes.
pub fn admit_root_bytes<R: Relation>(
    text: &str,
    canonical_bytes: &[u8],
) -> Result<StateRoot<R>, IdentityError> {
    StateRoot::admit_canonical_bytes(wire_root::<R>(text)?.into_untrusted(), canonical_bytes)
        .map_err(IdentityError::from)
}

/// Parses a relation-delta claim while retaining its untrusted context.
///
/// # Errors
///
/// Returns an identity decoding or schema-context error for malformed wire
/// text.
pub fn wire_delta<R: Relation>(text: &str) -> Result<WireId<R>, IdentityError> {
    let bytes = decode_id(text)?;
    WireId::<R>::decode(&bytes, IdContext::delta::<R>()).map_err(IdentityError::from)
}

/// Admits a relation-delta claim from the exact checked transition body.
///
/// # Errors
///
/// Returns an identity decoding or admission error when the claim does not
/// match the checked base, target, and canonical change bytes.
pub fn admit_delta_transition<R: Relation>(
    text: &str,
    base: StateRoot<R>,
    target: StateRoot<R>,
    canonical_changes: &[u8],
) -> Result<VersionDeltaId<R>, IdentityError> {
    VersionDeltaId::admit_transition(
        wire_delta::<R>(text)?.into_untrusted(),
        base,
        target,
        canonical_changes,
    )
    .map_err(IdentityError::from)
}

/// Admits a wire key by exact comparison with an already accepted producer
/// value. This is useful when the surrounding request owns the expected
/// identity but does not carry its canonical preimage.
///
/// # Errors
///
/// Returns an identity decoding or digest/context error when the claim does
/// not equal the accepted expected identity.
pub fn admit_key_against<T: Schema>(
    text: &str,
    expected: ObjectKey<T>,
) -> Result<ObjectKey<T>, IdentityError> {
    admit_claim_against(
        wire_key::<T>(text)?,
        IdContext::object_key::<T>(),
        expected.as_bytes(),
    )
    .map(|()| expected)
}

/// Admits a wire object version by exact comparison with an accepted value.
///
/// # Errors
///
/// Returns an identity decoding or digest/context error when the claim does
/// not equal the accepted expected identity.
pub fn admit_version_against<T: Schema>(
    text: &str,
    expected: ObjectVersion<T>,
) -> Result<ObjectVersion<T>, IdentityError> {
    admit_claim_against(
        wire_version::<T>(text)?,
        IdContext::schema::<T>(),
        expected.as_bytes(),
    )
    .map(|()| expected)
}

/// Admits a wire relation root by exact comparison with an accepted root.
///
/// # Errors
///
/// Returns an identity decoding or digest/context error when the claim does
/// not equal the accepted expected root.
pub fn admit_root_against<R: Relation>(
    text: &str,
    expected: StateRoot<R>,
) -> Result<StateRoot<R>, IdentityError> {
    admit_claim_against(
        wire_root::<R>(text)?,
        IdContext::relation::<R>(),
        expected.as_bytes(),
    )
    .map(|()| expected)
}

/// Admits a wire relation delta by exact comparison with an accepted delta.
///
/// # Errors
///
/// Returns an identity decoding or digest/context error when the claim does
/// not equal the accepted expected transition.
pub fn admit_delta_against<R: Relation>(
    text: &str,
    expected: VersionDeltaId<R>,
) -> Result<VersionDeltaId<R>, IdentityError> {
    admit_claim_against(
        wire_delta::<R>(text)?,
        IdContext::delta::<R>(),
        expected.as_bytes(),
    )
    .map(|()| expected)
}

fn admit_claim_against<K>(
    claim: WireId<K>,
    expected_context: IdContext,
    expected_bytes: &[u8; ID_BYTES],
) -> Result<(), IdentityError> {
    let claim = claim.into_untrusted();
    if claim.context() != expected_context {
        return Err(IdentityError::Admission(
            IdAdmissionError::ContextMismatch {
                expected: expected_context,
                actual: claim.context(),
            },
        ));
    }
    if claim.as_bytes() != expected_bytes {
        return Err(IdentityError::Admission(IdAdmissionError::DigestMismatch));
    }
    Ok(())
}

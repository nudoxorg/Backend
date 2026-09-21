pub(crate) fn digest(
    class: u8,
    domain: u8,
    ty: u16,
    version: u8,
    parts: &[&[u8]],
) -> [u8; ID_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(HASH_DOMAIN);
    hasher.update(&[class, domain]);
    hasher.update(&ty.to_be_bytes());
    hasher.update(&[version]);
    for part in parts {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    *hasher.finalize().as_bytes()
}

/// Encodes one schema value using its caller-supplied canonical encoder.
pub(crate) fn encoded<F, V: ?Sized>(encode: F, value: &V) -> Vec<u8>
where
    F: Fn(&V, &mut Vec<u8>),
{
    let mut out = Vec::new();
    encode(value, &mut out);
    out
}

/// Appends one length-delimited canonical field.
pub(crate) fn append_field<F: FnOnce(&mut Vec<u8>)>(out: &mut Vec<u8>, write: F) {
    let start = out.len();
    out.extend_from_slice(&[0; 8]);
    write(out);
    let len = (out.len() - start - 8) as u64;
    out[start..start + 8].copy_from_slice(&len.to_be_bytes());
}

pub(crate) fn state_root_from_digest<R: Relation>(bytes: [u8; ID_BYTES]) -> StateRoot<R> {
    StateRoot {
        bytes,
        _marker: PhantomData,
    }
}

pub(crate) fn delta_id_from_digest<R: Relation>(bytes: [u8; ID_BYTES]) -> DeltaId<R> {
    DeltaId {
        bytes,
        _marker: PhantomData,
    }
}

pub(crate) fn canonical_version_delta_id<R: Relation>(
    base: StateRoot<R>,
    target: StateRoot<R>,
    changes: &[u8],
) -> DeltaId<R> {
    delta_id_from_digest::<R>(digest(
        CLASS_DELTA,
        R::DOMAIN,
        R::TYPE,
        R::VERSION,
        &[base.as_bytes(), target.as_bytes(), changes],
    ))
}

pub(crate) fn workspace_root_from_digest(bytes: [u8; ID_BYTES]) -> WorkspaceRoot {
    WorkspaceRoot { bytes }
}

pub(crate) fn commit_id_from_digest(bytes: [u8; ID_BYTES]) -> CommitId {
    CommitId { bytes }
}
use core::marker::PhantomData;

use super::{
    context::IdContext,
    identity::{CommitId, DeltaId, ObjectVersion, StateRoot, WorkspaceRoot},
    schema::{Relation, Schema},
    wire::{IdAdmissionError, UntrustedId},
    CLASS_DELTA, CLASS_OBJECT_VERSION, CLASS_STATE_ROOT, HASH_DOMAIN, ID_BYTES,
};
use crate::workspace::SchemaIdentity;

/// Failure while checking a runtime schema identity against canonical bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeIdentityError {
    /// The input or claimed digest was not exactly one fixed-width identity.
    WrongLength,
    /// The canonical byte length cannot be represented in the identity frame.
    Oversized,
    /// The claimed digest does not cover the supplied schema and bytes.
    DigestMismatch,
    /// The streamed payload did not match its declared length.
    LengthMismatch {
        /// Declared canonical payload length.
        expected: usize,
        /// Bytes supplied so far, including the rejected excess chunk.
        actual: usize,
    },
    /// The hasher was finalized for a schema different from the typed result.
    SchemaMismatch {
        /// Schema declared when the hasher was created.
        actual: SchemaIdentity,
        /// Schema requested by the typed result.
        expected: SchemaIdentity,
    },
}

impl core::fmt::Display for RuntimeIdentityError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "runtime canonical identity admission failed: {self:?}")
    }
}

impl std::error::Error for RuntimeIdentityError {}

/// Incremental object-version identity encoder owned by the version ABI.
///
/// The domain, identity class, schema framing, and payload length prefix are
/// intentionally private. Callers can provide only canonical payload chunks;
/// the digest is emitted after exactly the declared number of bytes arrives.
pub struct ObjectVersionHasher {
    hasher: blake3::Hasher,
    schema: SchemaIdentity,
    expected: usize,
    written: usize,
    failed: bool,
}

impl ObjectVersionHasher {
    /// Starts one object-version digest for a runtime schema and payload size.
    ///
    /// # Errors
    /// Returns [`RuntimeIdentityError::Oversized`] if `payload_len` cannot be
    /// represented by the canonical identity framing.
    pub fn new(
        schema: crate::workspace::SchemaIdentity,
        payload_len: usize,
    ) -> Result<Self, RuntimeIdentityError> {
        let length = u64::try_from(payload_len).map_err(|_| RuntimeIdentityError::Oversized)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(HASH_DOMAIN);
        hasher.update(&[CLASS_OBJECT_VERSION, schema.domain()]);
        hasher.update(&schema.ty().to_be_bytes());
        hasher.update(&[schema.version()]);
        hasher.update(&length.to_be_bytes());
        Ok(Self {
            hasher,
            schema,
            expected: payload_len,
            written: 0,
            failed: false,
        })
    }

    /// Appends one canonical payload chunk.
    ///
    /// # Errors
    /// Returns [`RuntimeIdentityError::LengthMismatch`] when this chunk would
    /// exceed the declared payload length or a prior chunk already failed.
    pub fn update(&mut self, chunk: &[u8]) -> Result<(), RuntimeIdentityError> {
        if self.failed {
            return Err(RuntimeIdentityError::LengthMismatch {
                expected: self.expected,
                actual: self.written,
            });
        }
        let actual = self
            .written
            .checked_add(chunk.len())
            .ok_or(RuntimeIdentityError::Oversized)?;
        if actual > self.expected {
            self.failed = true;
            self.written = actual;
            return Err(RuntimeIdentityError::LengthMismatch {
                expected: self.expected,
                actual,
            });
        }
        self.hasher.update(chunk);
        self.written = actual;
        Ok(())
    }

    /// Finishes the digest after the declared payload length is complete.
    ///
    /// # Errors
    /// Returns [`RuntimeIdentityError::LengthMismatch`] for an over- or
    /// under-filled stream.
    pub fn finish(self) -> Result<[u8; ID_BYTES], RuntimeIdentityError> {
        if self.failed || self.written != self.expected {
            return Err(RuntimeIdentityError::LengthMismatch {
                expected: self.expected,
                actual: self.written,
            });
        }
        Ok(*self.hasher.finalize().as_bytes())
    }

    /// Finishes a streamed digest as an opaque typed object version.
    ///
    /// The schema is checked against the type parameter before the opaque
    /// identity is constructed, so callers cannot turn an arbitrary digest
    /// into a typed identity or accidentally use the wrong schema framing.
    pub fn finish_version<T: Schema>(self) -> Result<ObjectVersion<T>, RuntimeIdentityError> {
        let expected = SchemaIdentity::new(T::DOMAIN, T::TYPE, T::VERSION);
        if self.schema != expected {
            return Err(RuntimeIdentityError::SchemaMismatch {
                actual: self.schema,
                expected,
            });
        }
        Ok(ObjectVersion::from_digest(self.finish()?))
    }
}

fn runtime_digest(
    class: u8,
    schema: crate::workspace::SchemaIdentity,
    bytes: &[u8],
) -> Result<[u8; ID_BYTES], RuntimeIdentityError> {
    let _ = u64::try_from(bytes.len()).map_err(|_| RuntimeIdentityError::Oversized)?;
    Ok(digest(
        class,
        schema.domain(),
        schema.ty(),
        schema.version(),
        &[bytes],
    ))
}

/// Computes the version identity for canonical bytes under a runtime schema.
///
/// # Errors
/// Returns [`RuntimeIdentityError::Oversized`] if the byte length cannot be
/// represented in the canonical identity frame.
pub fn object_version_digest(
    schema: crate::workspace::SchemaIdentity,
    bytes: &[u8],
) -> Result<[u8; ID_BYTES], RuntimeIdentityError> {
    let mut hasher = ObjectVersionHasher::new(schema, bytes.len())?;
    hasher.update(bytes)?;
    hasher.finish()
}

/// Computes the state-root identity for canonical bytes under a runtime schema.
///
/// # Errors
/// Returns [`RuntimeIdentityError::Oversized`] if the byte length cannot be
/// represented in the canonical identity frame.
pub fn state_root_digest(
    schema: crate::workspace::SchemaIdentity,
    bytes: &[u8],
) -> Result<[u8; ID_BYTES], RuntimeIdentityError> {
    let _ = u64::try_from(bytes.len()).map_err(|_| RuntimeIdentityError::Oversized)?;
    runtime_digest(CLASS_STATE_ROOT, schema, bytes)
}

/// Admits a claimed runtime object-version identity against canonical bytes.
///
/// # Errors
/// Returns [`RuntimeIdentityError`] for a wrong-length claim, oversized input,
/// or a digest mismatch.
pub fn admit_object_version_bytes(
    schema: crate::workspace::SchemaIdentity,
    bytes: &[u8],
    claimed: &[u8],
) -> Result<[u8; ID_BYTES], RuntimeIdentityError> {
    let claimed: [u8; ID_BYTES] = claimed
        .try_into()
        .map_err(|_| RuntimeIdentityError::WrongLength)?;
    let expected = object_version_digest(schema, bytes)?;
    if expected != claimed {
        return Err(RuntimeIdentityError::DigestMismatch);
    }
    Ok(claimed)
}

/// Admits a claimed state-root identity against canonical bytes.
///
/// # Errors
/// Returns [`RuntimeIdentityError`] for a wrong-length claim, oversized input,
/// or a digest mismatch.
pub fn admit_state_root_bytes(
    schema: crate::workspace::SchemaIdentity,
    bytes: &[u8],
    claimed: &[u8],
) -> Result<[u8; ID_BYTES], RuntimeIdentityError> {
    let claimed: [u8; ID_BYTES] = claimed
        .try_into()
        .map_err(|_| RuntimeIdentityError::WrongLength)?;
    let expected = state_root_digest(schema, bytes)?;
    if expected != claimed {
        return Err(RuntimeIdentityError::DigestMismatch);
    }
    Ok(claimed)
}

pub(crate) fn admit_context_only<K>(
    claim: UntrustedId<K>,
    expected: IdContext,
) -> Result<(), IdAdmissionError> {
    if claim.context != expected {
        return Err(IdAdmissionError::ContextMismatch {
            expected,
            actual: claim.context,
        });
    }
    Ok(())
}

pub(crate) fn admit_exact<K>(
    claim: UntrustedId<K>,
    expected: IdContext,
    digest: [u8; ID_BYTES],
) -> Result<[u8; ID_BYTES], IdAdmissionError> {
    admit_context_only(claim, expected)?;
    if claim.bytes != digest {
        return Err(IdAdmissionError::DigestMismatch);
    }
    Ok(claim.bytes)
}

//! Backend-version identity markers and explicit wire-claim admission.

use backend_version::{
    IdContext, IdDecodeError, ObjectKey as VersionObjectKey, ObjectVersion as VersionObjectVersion,
    Relation, Schema, StateRoot as VersionStateRoot, UntrustedId,
    WorkspaceRoot as VersionWorkspaceRoot,
};
use std::{
    fmt,
    hash::{Hash, Hasher},
};

use crate::ReplicationError;

/// Schema for logical immutable object keys and complete object versions.
pub struct ImmutableObjectSchema;
impl Schema for ImmutableObjectSchema {
    const DOMAIN: u8 = 0x72;
    const TYPE: u16 = 1;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Relation marker used for transport-level state-root summaries.
pub struct ReplicationRelation;
impl Relation for ReplicationRelation {
    const DOMAIN: u8 = 0x72;
    const TYPE: u16 = 7;
    type Key = [u8; 32];
    type Value = [u8; 32];
    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// A logical object identity bound to the replication object schema.
pub type ObjectKey = VersionObjectKey<ImmutableObjectSchema>;
/// A complete immutable object value bound to the replication object schema.
pub type ObjectVersion = VersionObjectVersion<ImmutableObjectSchema>;
/// A relation state root bound to the replication relation schema.
pub type StateRoot = VersionStateRoot<ReplicationRelation>;
/// The workspace root type owned by the version crate.
pub type WorkspaceRoot = VersionWorkspaceRoot;

/// A typed identity that can be compared with a wire claim without erasing its
/// backend-version marker.
pub trait TypedIdentity {
    /// Returns the canonical fixed-width identity bytes.
    fn identity_bytes(&self) -> &[u8; 32];
    /// Returns the identity class and schema context encoded by this type.
    fn identity_context() -> IdContext
    where
        Self: Sized;
}
impl<T: Schema> TypedIdentity for VersionObjectKey<T> {
    fn identity_bytes(&self) -> &[u8; 32] {
        self.as_bytes()
    }
    fn identity_context() -> IdContext {
        IdContext::object_key::<T>()
    }
}
impl<T: Schema> TypedIdentity for VersionObjectVersion<T> {
    fn identity_bytes(&self) -> &[u8; 32] {
        self.as_bytes()
    }
    fn identity_context() -> IdContext {
        IdContext::schema::<T>()
    }
}
impl<R: Relation> TypedIdentity for VersionStateRoot<R> {
    fn identity_bytes(&self) -> &[u8; 32] {
        self.as_bytes()
    }
    fn identity_context() -> IdContext {
        IdContext::relation::<R>()
    }
}

/// A fixed-width untrusted identity claim for a known backend-version class.
///
/// This wrapper is used for object transfer identities. Execution-specific
/// identities use [`WireIdentity`] so replication never creates a second
/// recipe, read-manifest, work-key, or receipt schema.
pub struct WireId<K>(UntrustedId<K>);
impl<K> Clone for WireId<K> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<K> Copy for WireId<K> {}
impl<K> fmt::Debug for WireId<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireId")
            .field("context", &self.0.context())
            .finish()
    }
}
impl<K> PartialEq for WireId<K> {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bytes() == other.0.as_bytes() && self.0.context() == other.0.context()
    }
}
impl<K> Eq for WireId<K> {}
impl<K> WireId<K> {
    /// Decodes one fixed-width claim while retaining the context supplied by
    /// the wire. The caller must run [`Self::admit_context`] or compare it to
    /// caller-owned typed material before using the claim.
    ///
    /// # Errors
    ///
    /// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
    pub(crate) fn from_wire(bytes: &[u8], context: IdContext) -> Result<Self, IdDecodeError> {
        UntrustedId::from_wire(bytes, context).map(Self::from_inner)
    }

    fn from_inner(inner: UntrustedId<K>) -> Self {
        Self(inner)
    }
    pub(crate) fn into_inner(self) -> UntrustedId<K> {
        self.into_untrusted()
    }
    /// Returns the claimed bytes without granting typed identity.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Returns the untrusted backend-version context carried by this claim.
    #[must_use]
    pub const fn context(&self) -> IdContext {
        self.0.context()
    }

    /// Admits only the identity class and schema context, retaining the
    /// digest as an explicitly untrusted backend-version claim.
    ///
    /// A context check cannot prove a digest. Callers that have the canonical
    /// preimage should pass the returned [`UntrustedId`] to the corresponding
    /// backend-version `admit_value`/`admit_canonical_bytes` method. Callers
    /// that already own the expected typed identity must compare its bytes
    /// before treating the claim as that identity.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityContext`] when the wire context
    /// does not equal the caller's expected context.
    pub fn admit_context(self, expected: IdContext) -> Result<UntrustedId<K>, ReplicationError> {
        if self.context() != expected {
            return Err(ReplicationError::IdentityContext);
        }
        Ok(self.into_inner())
    }

    /// Returns the backend-version untrusted claim without changing its trust
    /// level. The caller must use the expected schema's admission operation or
    /// compare it with caller-owned typed material before use.
    #[must_use]
    pub const fn into_untrusted(self) -> UntrustedId<K> {
        self.0
    }
}

/// A fixed-width claim for an object key received from the wire.
pub type WireObjectKey = WireId<ImmutableObjectSchema>;
/// A fixed-width claim for an object version received from the wire.
pub type WireObjectVersion = WireId<ImmutableObjectSchema>;
/// A fixed-width claim for a state root received from the wire.
pub type WireStateRoot = WireId<ReplicationRelation>;

/// A wire claim for an object key under the schema selected by the caller.
///
/// The marker is deliberately generic.  Keeping it on the claim prevents a
/// transfer of one schema's bytes from being reinterpreted as another schema
/// merely because the bytes happen to be equal.
pub type SchemaWireObjectKey<T> = WireId<T>;
/// A wire claim for an object version under the schema selected by the caller.
pub type SchemaWireObjectVersion<T> = WireId<T>;

/// A fixed-width execution identity claim with caller-supplied schema context.
///
/// The replication crate intentionally does not define or admit a recipe,
/// read-manifest, work-key, authority, output, or receipt schema. An execution
/// owner converts its own typed `backend-version` ID to this claim and later
/// calls [`WireIdentity::admit_against`] with the exact expected typed value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireIdentity {
    bytes: [u8; 32],
    context: IdContext,
}
impl Hash for WireIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
        self.context.class().hash(state);
        self.context.domain().hash(state);
        self.context.ty().hash(state);
        self.context.version().hash(state);
    }
}
impl Ord for WireIdentity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.bytes
            .cmp(&other.bytes)
            .then_with(|| self.context.class().cmp(&other.context.class()))
            .then_with(|| self.context.domain().cmp(&other.context.domain()))
            .then_with(|| self.context.ty().cmp(&other.context.ty()))
            .then_with(|| self.context.version().cmp(&other.context.version()))
    }
}
impl PartialOrd for WireIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl WireIdentity {
    /// Decodes one fixed-width claim and retains its untrusted context.
    ///
    /// # Errors
    ///
    /// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
    pub(crate) fn from_wire(bytes: &[u8], context: IdContext) -> Result<Self, IdDecodeError> {
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| IdDecodeError::WrongLength)?;
        Ok(Self { bytes, context })
    }

    /// Copies a typed identity into an untrusted wire claim.
    ///
    /// The resulting value is still untrusted; it is only a wire encoding of
    /// an identity the caller already admitted.
    #[must_use]
    pub fn from_typed<T: TypedIdentity>(value: &T) -> Self {
        Self {
            bytes: *value.identity_bytes(),
            context: T::identity_context(),
        }
    }

    /// Returns the claimed bytes without granting a typed identity.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.bytes
    }

    /// Returns the untrusted backend-version context carried by this claim.
    #[must_use]
    pub const fn context(self) -> IdContext {
        self.context
    }

    /// Converts this claim into `backend_version::UntrustedId<K>` for the
    /// caller's own typed `ObjectKey`, `ObjectVersion`, or relation admission.
    ///
    /// # Errors
    ///
    /// Returns [`IdDecodeError::WrongLength`] if the fixed-width representation
    /// changes in a future backend-version ABI.
    pub fn into_untrusted<K>(self) -> Result<UntrustedId<K>, IdDecodeError> {
        UntrustedId::from_wire(&self.bytes, self.context)
    }

    /// Admits this claim only by exact comparison with a caller-owned typed ID.
    ///
    /// This returns the caller's already-typed value after checking both its
    /// bytes and context. It never constructs an execution identity in this
    /// crate.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityContext`] for a context mismatch or
    /// [`ReplicationError::IdentityMismatch`] for a different identity.
    pub fn admit_against<T: TypedIdentity>(
        self,
        expected: T,
        expected_context: IdContext,
    ) -> Result<T, ReplicationError> {
        // Keep the context tied to the type marker as well as to the caller's
        // explicit expectation. Otherwise a caller could accidentally pass a
        // context for a different class/schema and relabel matching bytes as
        // the typed value.
        if expected_context != T::identity_context() || self.context != expected_context {
            return Err(ReplicationError::IdentityContext);
        }
        if self.bytes != *expected.identity_bytes() {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(expected)
    }

    /// Admits this claim against a typed identity while deriving the expected
    /// context from that identity's marker.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityContext`] when the wire claim's
    /// class or schema differs, or [`ReplicationError::IdentityMismatch`]
    /// when its digest differs.
    pub fn admit_typed<T: TypedIdentity>(self, expected: T) -> Result<T, ReplicationError> {
        self.admit_against(expected, T::identity_context())
    }

    /// Checks this claim against the canonical digest of an already encoded
    /// schema value.
    ///
    /// The version layer's object identity is the domain-separated digest of
    /// the complete encoded value.  A replication result carries the encoded
    /// value rather than a typed `Schema::Value`, so this small boundary
    /// helper performs the same digest calculation without first copying the
    /// value into another allocation.  The caller must still compare the
    /// context to the expected typed schema separately; this method only
    /// proves that the claim's bytes commit to `encoded_value` under the
    /// object-version identity grammar.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityContext`] when the claim is not an
    /// object-version claim, or [`ReplicationError::IdentityMismatch`] when
    /// its digest does not reproduce the supplied canonical bytes.
    pub fn admit_canonical_value(self, encoded_value: &[u8]) -> Result<(), ReplicationError> {
        if self.context.class() != 0x56 {
            return Err(ReplicationError::IdentityContext);
        }
        let mut hasher = backend_version::ObjectVersionHasher::new(
            backend_version::SchemaIdentity::new(
                self.context.domain(),
                self.context.ty(),
                self.context.version(),
            ),
            encoded_value.len(),
        )
        .map_err(|_| ReplicationError::Overflow)?;
        hasher
            .update(encoded_value)
            .map_err(|_| ReplicationError::IdentityMismatch)?;
        if hasher
            .finish()
            .map_err(|_| ReplicationError::IdentityMismatch)?
            != self.bytes
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(())
    }
}

/// A caller-owned expected identity encoded for exact wire comparison.
///
/// Construct this with [`ExpectedIdentity::from_typed`] at an execution
/// boundary. Its fields remain private so a raw byte array cannot masquerade
/// as expected execution material.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExpectedIdentity(WireIdentity);
impl ExpectedIdentity {
    /// Encodes a caller-owned typed identity as expected wire material.
    #[must_use]
    pub fn from_typed<T: TypedIdentity>(value: &T) -> Self {
        Self(WireIdentity::from_typed(value))
    }
    pub(crate) fn matches(self, claim: WireIdentity) -> Result<(), ReplicationError> {
        if claim.context() != self.0.context() {
            return Err(ReplicationError::IdentityContext);
        }
        if claim.as_bytes() != self.0.as_bytes() {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(())
    }
}

/// Returns an untrusted object-key claim for a wire message.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn object_key_claim(bytes: &[u8]) -> Result<WireObjectKey, IdDecodeError> {
    WireObjectKey::from_wire(bytes, IdContext::object_key::<ImmutableObjectSchema>())
}

/// Decodes an object-key claim for the schema selected by the typed API.
///
/// The schema context is derived from `T`; callers cannot relabel bytes from
/// one schema as another by supplying an independent context.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn schema_object_key_claim<T: Schema>(
    bytes: &[u8],
) -> Result<SchemaWireObjectKey<T>, IdDecodeError> {
    SchemaWireObjectKey::from_wire(bytes, IdContext::object_key::<T>())
}

/// Decodes a schema-bound object-key identity used by APIs that retain the
/// common execution-wire wrapper. The marker type, rather than the caller,
/// selects the context.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn schema_object_key_identity_claim<T: Schema>(
    bytes: &[u8],
) -> Result<WireIdentity, IdDecodeError> {
    WireIdentity::from_wire(bytes, IdContext::object_key::<T>())
}
/// Returns an untrusted object-version claim for a wire message.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn object_version_claim(bytes: &[u8]) -> Result<WireObjectVersion, IdDecodeError> {
    WireObjectVersion::from_wire(bytes, IdContext::schema::<ImmutableObjectSchema>())
}

/// Decodes an object-version claim for the schema selected by the typed API.
///
/// The schema context is derived from `T`; callers cannot relabel bytes from
/// one schema as another by supplying an independent context.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn schema_object_version_claim<T: Schema>(
    bytes: &[u8],
) -> Result<SchemaWireObjectVersion<T>, IdDecodeError> {
    SchemaWireObjectVersion::from_wire(bytes, IdContext::schema::<T>())
}

/// Decodes a schema-bound object-version identity used by APIs that retain
/// the common execution-wire wrapper. The marker type, rather than the
/// caller, selects the context.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn schema_object_version_identity_claim<T: Schema>(
    bytes: &[u8],
) -> Result<WireIdentity, IdDecodeError> {
    WireIdentity::from_wire(bytes, IdContext::schema::<T>())
}

/// Decodes a replication relation-root identity with its fixed relation
/// marker. Relation roots cannot be relabeled through this API.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn relation_identity_claim(bytes: &[u8]) -> Result<WireIdentity, IdDecodeError> {
    WireIdentity::from_wire(bytes, IdContext::relation::<ReplicationRelation>())
}
/// Returns an untrusted execution identity claim for a wire message.
///
/// The expected typed identity supplies the class and schema context. The
/// caller cannot relabel the claim by passing a separately constructed
/// context; the bytes remain untrusted until the owner compares them with the
/// same expected identity.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn execution_identity_claim<T: TypedIdentity>(
    bytes: &[u8],
    _expected: &T,
) -> Result<WireIdentity, IdDecodeError> {
    WireIdentity::from_wire(bytes, T::identity_context())
}

/// Returns an explicitly untrusted execution claim when only a wire context
/// is available. This is intended for a decoder; an owner must call
/// [`WireIdentity::admit_against`] before using the claim as execution data.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn untrusted_execution_identity_claim(
    bytes: &[u8],
    context: IdContext,
) -> Result<WireIdentity, IdDecodeError> {
    WireIdentity::from_wire(bytes, context)
}
/// Returns an untrusted state-root claim for a wire message.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn state_root_claim(bytes: &[u8]) -> Result<WireStateRoot, IdDecodeError> {
    WireStateRoot::from_wire(bytes, IdContext::relation::<ReplicationRelation>())
}
/// Returns an untrusted workspace-root claim for a wire message.
///
/// `backend-version` currently exposes no `WorkspaceRoot::admit` constructor,
/// so workspace claims are admitted by matching them to the exact requested
/// typed root in [`WorkspaceRootClaim::admit`]. The bytes remain untrusted
/// until that match succeeds.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] when the claim is not 32 bytes.
pub fn workspace_root_claim(bytes: &[u8]) -> Result<WorkspaceRootClaim, IdDecodeError> {
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| IdDecodeError::WrongLength)?;
    Ok(WorkspaceRootClaim::from_bytes(bytes))
}

/// Converts an accepted object key into a claim suitable for a wire frame.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] if the fixed-width identity cannot
/// be represented by the wire claim.
pub fn claim_object_key(key: ObjectKey) -> Result<WireObjectKey, IdDecodeError> {
    WireObjectKey::from_wire(
        key.as_bytes(),
        IdContext::object_key::<ImmutableObjectSchema>(),
    )
}

/// Converts a caller-owned schema-marked object key into an untrusted claim.
///
/// This is the generic counterpart to [`claim_object_key`].  It is used by
/// object transfer so the true backend-version schema remains part of the
/// transfer type and wire context.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] if the backend-version identity does
/// not have the fixed wire width.
pub fn claim_schema_object_key<T: Schema>(
    key: VersionObjectKey<T>,
) -> Result<SchemaWireObjectKey<T>, IdDecodeError> {
    SchemaWireObjectKey::from_wire(key.as_bytes(), IdContext::object_key::<T>())
}
/// Converts an accepted object version into a claim suitable for a wire frame.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] if the fixed-width identity cannot
/// be represented by the wire claim.
pub fn claim_object_version(version: ObjectVersion) -> Result<WireObjectVersion, IdDecodeError> {
    WireObjectVersion::from_wire(
        version.as_bytes(),
        IdContext::schema::<ImmutableObjectSchema>(),
    )
}

/// Converts a caller-owned schema-marked object version into an untrusted
/// claim while retaining its exact schema context.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] if the backend-version identity does
/// not have the fixed wire width.
pub fn claim_schema_object_version<T: Schema>(
    version: VersionObjectVersion<T>,
) -> Result<SchemaWireObjectVersion<T>, IdDecodeError> {
    SchemaWireObjectVersion::from_wire(version.as_bytes(), IdContext::schema::<T>())
}
/// Converts an accepted typed identity into a wire claim.
#[must_use]
pub fn claim_typed_identity<T: TypedIdentity>(value: &T) -> WireIdentity {
    WireIdentity::from_typed(value)
}
/// Converts an accepted relation root into a wire claim.
///
/// # Errors
///
/// Returns [`IdDecodeError::WrongLength`] if the fixed-width identity cannot
/// be represented by the wire claim.
pub fn claim_state_root(root: StateRoot) -> Result<WireStateRoot, IdDecodeError> {
    WireStateRoot::from_wire(
        root.as_bytes(),
        IdContext::relation::<ReplicationRelation>(),
    )
}

/// A workspace-root claim admitted by matching a requested typed root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceRootClaim {
    bytes: [u8; 32],
}
impl WorkspaceRootClaim {
    /// Creates an untrusted root claim from fixed-width wire bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }
    /// Returns the claimed bytes without treating them as a typed root.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.bytes
    }
    /// Admits the claim only when it equals the exact requested root.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityMismatch`] when the claim differs
    /// from the requested root.
    pub fn admit(self, expected: WorkspaceRoot) -> Result<WorkspaceRoot, ReplicationError> {
        if self.bytes == *expected.as_bytes() {
            Ok(expected)
        } else {
            Err(ReplicationError::IdentityMismatch)
        }
    }
}

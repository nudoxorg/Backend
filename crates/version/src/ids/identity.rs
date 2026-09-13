use core::{
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
};

use super::{
    CLASS_OBJECT_KEY, CLASS_OBJECT_VERSION, CLASS_STATE_ROOT, ID_BYTES,
    context::IdContext,
    hash::{
        admit_context_only, admit_exact, canonical_version_delta_id, digest, encoded,
        state_root_from_digest,
    },
    schema::{Relation, Schema},
    wire::{IdAdmissionError, UntrustedId},
};

/// Opaque stable key identity for one schema.
pub struct ObjectKey<T: Schema> {
    pub(crate) bytes: [u8; ID_BYTES],
    pub(crate) _marker: PhantomData<fn() -> T>,
}

/// Opaque complete value identity for one schema.
///
/// A typed identity can only be derived from its canonical preimage or
/// admitted through a checked wire boundary. Arbitrary digest bytes cannot be
/// converted into this type:
///
/// ```compile_fail
/// use backend_version::{ObjectVersion, Schema};
/// struct S;
/// impl Schema for S {
///     const DOMAIN: u8 = 1;
///     const TYPE: u16 = 1;
///     type Value = u64;
///     fn encode(value: &u64, out: &mut Vec<u8>) { out.extend_from_slice(&value.to_be_bytes()); }
/// }
/// let _forged = ObjectVersion::<S> {
///     bytes: [0; 32],
///     _marker: core::marker::PhantomData,
/// };
/// ```
pub struct ObjectVersion<T: Schema> {
    pub(crate) bytes: [u8; ID_BYTES],
    pub(crate) _marker: PhantomData<fn() -> T>,
}

/// Opaque canonical visible relation root.
pub struct StateRoot<R: Relation> {
    pub(crate) bytes: [u8; ID_BYTES],
    pub(crate) _marker: PhantomData<fn() -> R>,
}

/// Opaque canonical root of roots for one workspace state.
///
/// Workspace roots are emitted by checked manifests; a raw digest is not a
/// public constructor.
///
/// ```compile_fail
/// use backend_version::WorkspaceRoot;
/// let _forged = WorkspaceRoot { bytes: [0; 32] };
/// ```
pub struct WorkspaceRoot {
    pub(crate) bytes: [u8; ID_BYTES],
}

/// Opaque history identity for one workspace commit.
pub struct CommitId {
    pub(crate) bytes: [u8; ID_BYTES],
}

/// Opaque exact transition identity for one relation.
pub struct DeltaId<R: Relation> {
    pub(crate) bytes: [u8; ID_BYTES],
    pub(crate) _marker: PhantomData<fn() -> R>,
}

macro_rules! id_impl {
    ($name:ident $(<$($gen:ident : $bound:path),+>)? ) => {
        impl$(<$($gen: $bound),+>)? $name$(<$($gen),+>)? {
            /// Returns the raw hash bytes.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; ID_BYTES] {
                &self.bytes
            }

            /// Copies the raw hash bytes.
            #[must_use]
            pub const fn to_bytes(self) -> [u8; ID_BYTES] {
                self.bytes
            }
        }

        impl$(<$($gen: $bound),+>)? fmt::Debug for $name$(<$($gen),+>)? {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}:", stringify!($name))?;
                for byte in self.bytes {
                    write!(f, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl$(<$($gen: $bound),+>)? Copy for $name$(<$($gen),+>)? {}

        #[allow(
            clippy::expl_impl_clone_on_copy,
            reason = "fixed-width identity wrappers clone by copying their digest"
        )]
        impl$(<$($gen: $bound),+>)? Clone for $name$(<$($gen),+>)? {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl$(<$($gen: $bound),+>)? PartialEq for $name$(<$($gen),+>)? {
            fn eq(&self, other: &Self) -> bool {
                self.bytes == other.bytes
            }
        }

        impl$(<$($gen: $bound),+>)? Eq for $name$(<$($gen),+>)? {}

        impl$(<$($gen: $bound),+>)? PartialOrd for $name$(<$($gen),+>)? {
            fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl$(<$($gen: $bound),+>)? Ord for $name$(<$($gen),+>)? {
            fn cmp(&self, other: &Self) -> core::cmp::Ordering {
                self.bytes.cmp(&other.bytes)
            }
        }

        impl$(<$($gen: $bound),+>)? Hash for $name$(<$($gen),+>)? {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.bytes.hash(state);
            }
        }
    };
}

id_impl!(ObjectKey<T: Schema>);
id_impl!(ObjectVersion<T: Schema>);
id_impl!(StateRoot<R: Relation>);
id_impl!(DeltaId<R: Relation>);
id_impl!(WorkspaceRoot);
id_impl!(CommitId);

impl<T: Schema> ObjectKey<T> {
    /// Admits a fixed-width logical-key claim asserted by an already
    /// authenticated producer authority.
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] when the claim is not a
    /// key in schema `T`.
    pub fn admit_from_producer(
        claim: UntrustedId<T>,
        _producer: &crate::AuthorizedCompleteCoverage,
    ) -> Result<Self, IdAdmissionError> {
        admit_context_only(claim, IdContext::object_key::<T>())?;
        Ok(Self {
            bytes: claim.bytes,
            _marker: PhantomData,
        })
    }

    /// Derives a stable key from the schema's canonical logical key bytes.
    #[must_use]
    pub fn from_value(value: &T::Value) -> Self {
        Self {
            bytes: digest(
                CLASS_OBJECT_KEY,
                T::DOMAIN,
                T::TYPE,
                T::VERSION,
                &[&encoded(T::encode, value)],
            ),
            _marker: PhantomData,
        }
    }

    /// Rejects context-only admission of a wire claim.
    ///
    /// This compatibility method intentionally never creates a typed key:
    /// matching class/schema metadata is not a digest proof.  Use
    /// [`Self::admit_value`] with the canonical value instead.
    ///
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] for a wrong context or
    /// [`IdAdmissionError::UnverifiedDigest`] for a matching but unverified
    /// context.
    pub fn admit(claim: UntrustedId<T>) -> Result<Self, IdAdmissionError> {
        admit_context_only(claim, IdContext::object_key::<T>())?;
        Err(IdAdmissionError::UnverifiedDigest)
    }

    /// Admits a wire key after recomputing its digest from the canonical value.
    ///
    /// Context metadata is checked first, then the supplied value is encoded
    /// with `T` and hashed under the object-key domain.  A matching context
    /// with an arbitrary digest is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] for a wrong schema/class
    /// or [`IdAdmissionError::DigestMismatch`] for a forged digest.
    pub fn admit_value(claim: UntrustedId<T>, value: &T::Value) -> Result<Self, IdAdmissionError> {
        let expected = Self::from_value(value);
        admit_exact(claim, IdContext::object_key::<T>(), expected.bytes).map(|bytes| Self {
            bytes,
            _marker: PhantomData,
        })
    }
}

impl<T: Schema> ObjectVersion<T> {
    /// Hashes one complete canonical value.
    #[must_use]
    pub fn from_value(value: &T::Value) -> Self {
        Self {
            bytes: digest(
                CLASS_OBJECT_VERSION,
                T::DOMAIN,
                T::TYPE,
                T::VERSION,
                &[&encoded(T::encode, value)],
            ),
            _marker: PhantomData,
        }
    }

    /// Rejects context-only admission of a wire claim.
    ///
    /// This compatibility method intentionally never creates a typed value
    /// version: matching metadata is not a digest proof.  Use
    /// [`Self::admit_value`] with the canonical value instead.
    ///
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] for a wrong context or
    /// [`IdAdmissionError::UnverifiedDigest`] for a matching but unverified
    /// context.
    pub fn admit(claim: UntrustedId<T>) -> Result<Self, IdAdmissionError> {
        admit_context_only(claim, IdContext::schema::<T>())?;
        Err(IdAdmissionError::UnverifiedDigest)
    }

    /// Admits a wire value version after recomputing its canonical digest.
    ///
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] for a wrong schema/class
    /// or [`IdAdmissionError::DigestMismatch`] for a forged digest.
    pub fn admit_value(claim: UntrustedId<T>, value: &T::Value) -> Result<Self, IdAdmissionError> {
        let expected = Self::from_value(value);
        admit_exact(claim, IdContext::schema::<T>(), expected.bytes).map(|bytes| Self {
            bytes,
            _marker: PhantomData,
        })
    }
}

impl<R: Relation> StateRoot<R> {
    /// Admits a fixed-width state-root claim asserted by an already
    /// authenticated producer authority.
    ///
    /// The producer observation is deliberately required even though its
    /// fields are not inspected here: possession proves that the caller has
    /// crossed an authority verifier rather than merely decoded a digest.
    /// This is the bounded counterpart to [`Self::admit_canonical_bytes`] for
    /// protocols where the complete source relation is intentionally not
    /// transferred alongside a derived snapshot.
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] when the wire claim is
    /// not a state root for `R`.
    pub fn admit_from_producer(
        claim: UntrustedId<R>,
        _producer: &crate::AuthorizedCompleteCoverage,
    ) -> Result<Self, IdAdmissionError> {
        admit_context_only(claim, IdContext::relation::<R>())?;
        Ok(state_root_from_digest(claim.bytes))
    }

    /// Rejects context-only admission of a wire claim.
    ///
    /// Matching relation metadata is not a state-root proof.  Use
    /// [`Self::admit_canonical_bytes`] after validating the canonical node.
    ///
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] for a wrong context or
    /// [`IdAdmissionError::UnverifiedDigest`] for a matching but unverified
    /// context.
    pub fn admit(claim: UntrustedId<R>) -> Result<Self, IdAdmissionError> {
        admit_context_only(claim, IdContext::relation::<R>())?;
        Err(IdAdmissionError::UnverifiedDigest)
    }

    /// Admits a wire root after recomputing the commitment from canonical node
    /// bytes.  The caller must validate the node grammar before invoking this
    /// method; this method then binds the claim to those exact bytes.
    ///
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] for a wrong relation or
    /// [`IdAdmissionError::DigestMismatch`] for a forged root.
    pub fn admit_canonical_bytes(
        claim: UntrustedId<R>,
        canonical_bytes: &[u8],
    ) -> Result<Self, IdAdmissionError> {
        let expected = state_root_from_digest::<R>(digest(
            CLASS_STATE_ROOT,
            R::DOMAIN,
            R::TYPE,
            R::VERSION,
            &[canonical_bytes],
        ));
        admit_exact(claim, IdContext::relation::<R>(), expected.bytes).map(|bytes| Self {
            bytes,
            _marker: PhantomData,
        })
    }
}

impl<R: Relation> DeltaId<R> {
    /// Rejects context-only admission of a wire claim.
    ///
    /// Matching relation metadata is not a transition proof.  Use
    /// [`Self::admit_transition`] with the exact canonical transition body.
    ///
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] for a wrong context or
    /// [`IdAdmissionError::UnverifiedDigest`] for a matching but unverified
    /// context.
    pub fn admit(claim: UntrustedId<R>) -> Result<Self, IdAdmissionError> {
        admit_context_only(claim, IdContext::delta::<R>())?;
        Err(IdAdmissionError::UnverifiedDigest)
    }

    /// Admits a transition identity after recomputing it from exact canonical
    /// base/target roots and the canonical encoded change sequence.
    ///
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::ContextMismatch`] for a wrong relation or
    /// [`IdAdmissionError::DigestMismatch`] for a forged transition ID.
    pub fn admit_transition(
        claim: UntrustedId<R>,
        base: StateRoot<R>,
        target: StateRoot<R>,
        canonical_changes: &[u8],
    ) -> Result<Self, IdAdmissionError> {
        let expected = canonical_version_delta_id(base, target, canonical_changes);
        admit_exact(claim, IdContext::delta::<R>(), expected.bytes).map(|bytes| Self {
            bytes,
            _marker: PhantomData,
        })
    }
}

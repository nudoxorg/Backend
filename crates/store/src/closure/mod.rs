//! Checked immutable objects and complete closure manifests.
//!
//! A closure is the durable boundary between a logical root and the immutable
//! objects that make that root meaningful.  Objects enter a closure through a
//! typed [`backend_version::Schema`] value, so a caller cannot pair arbitrary
//! bytes with a claimed object version.  The manifest then commits to every
//! object, its schema, key, version, and canonical byte length.

use super::{Hash, RawRelation, StoreError, digest};
use backend_version::{ID_BYTES, SchemaIdentity, admit_object_version_bytes};

mod admission;
mod manifest_tree;
pub(crate) use manifest_tree::ManifestRelation;

const CLOSURE_MAGIC: &[u8] = b"LUNA_CLOSURE_V1\0";
const WORKSPACE_CLOSURE_DOMAIN: &[u8] = b"store.workspace-closure.v1\0";
const OBJECT_DOMAIN: &[u8] = b"store.object.v1\0";
const MIN_OBJECT_BYTES: usize = 1 + 2 + 1 + ID_BYTES + ID_BYTES + 8;

pub use admission::RelationAdmissionRegistry;

/// Opaque identity of one immutable typed object.
///
/// A raw digest cannot be promoted to this type at a wire boundary. Use
/// [`UntrustedObjectId`] and admit it against a checked immutable object.
///
/// ```compile_fail
/// use backend_store::ObjectId;
/// let _raw = ObjectId::from_bytes([0; 32]);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectId(Hash);

impl ObjectId {
    /// Returns the fixed-width object identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &Hash {
        &self.0
    }

    /// Wraps a fixed-width identity received from a checked wire or journal
    /// record. Filesystem reads still verify the corresponding object before
    /// exposing its typed contents.
    pub(crate) const fn from_bytes(bytes: Hash) -> Self {
        Self(bytes)
    }
}

/// An object identity obtained from an untrusted wire or filesystem field.
///
/// This value is intentionally distinct from [`ObjectId`].  It becomes an
/// admitted identity only after the complete immutable object envelope has
/// been checked against its schema, key, version, length, and bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UntrustedObjectId(Hash);

impl UntrustedObjectId {
    /// Wraps fixed-width bytes received from an untrusted boundary.
    #[must_use]
    pub const fn from_bytes(bytes: Hash) -> Self {
        Self(bytes)
    }

    /// Returns the untrusted fixed-width bytes for a filesystem lookup.
    #[must_use]
    pub const fn as_bytes(&self) -> &Hash {
        &self.0
    }

    /// Admits this identity against a fully checked immutable object.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the object commitment differs
    /// from the received identity.
    pub fn admit(self, object: &TypedObject) -> Result<ObjectId, StoreError> {
        let admitted = object.id();
        if admitted.0 == self.0 {
            Ok(admitted)
        } else {
            Err(StoreError::Corrupt)
        }
    }
}

/// One checked edge between immutable objects in a closure.
///
/// Relation branch nodes are represented by their physical object IDs after
/// the manifest resolves each child state-root version. Raw relation leaf
/// values may also carry object IDs; those edges are returned even when the
/// target object lives in another shared closure.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectEdge {
    from: ObjectId,
    to: ObjectId,
}

impl ObjectEdge {
    pub(super) const fn new(from: ObjectId, to: ObjectId) -> Self {
        Self { from, to }
    }

    /// Returns the object that contains the reference.
    #[must_use]
    pub const fn from(self) -> ObjectId {
        self.from
    }

    /// Returns the referenced object.
    #[must_use]
    pub const fn to(self) -> ObjectId {
        self.to
    }
}

/// Opaque identity of one complete immutable closure manifest.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClosureId(Hash);

impl ClosureId {
    /// Returns the fixed-width closure identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &Hash {
        &self.0
    }

    pub(crate) const fn from_bytes(bytes: Hash) -> Self {
        Self(bytes)
    }
}

mod manifest;
mod object;
mod workspace;

pub use manifest::ClosureManifest;
pub use manifest::{ManifestChange, ManifestWork, PreparedManifestDelta};
pub use object::TypedObject;
pub use workspace::{CheckedRelationNode, WorkspaceBinding, WorkspaceClosure};

/// Verifies one runtime schema identity against the canonical backend-version
/// object-version digest.  This is the dynamic counterpart of
/// `ObjectVersion::<T>::from_value`: storage has the schema context and the
/// already canonical encoded bytes, but does not have a Rust `Schema` type to
/// instantiate at runtime.
///
/// Admission is delegated to backend-version's runtime object-version ABI so
/// storage does not maintain a second hash/preimage implementation.
#[must_use]
pub fn verify_backend_object_version(
    schema: SchemaIdentity,
    canonical_bytes: &[u8],
    claimed: &Hash,
) -> bool {
    admit_object_version_bytes(schema, canonical_bytes, claimed).is_ok()
}

/// Admits one runtime schema/version claim after reproducing its canonical
/// backend-version digest.
///
/// This result-returning form is the safe storage boundary for callers that
/// need to retain admission failure information rather than treating a
/// boolean verification result as a trusted object.
///
/// # Errors
///
/// Returns [`StoreError::Corrupt`] when the length is not representable or the
/// claimed digest does not match the canonical bytes.
pub fn admit_backend_object_version(
    schema: SchemaIdentity,
    canonical_bytes: &[u8],
    claimed: &Hash,
) -> Result<(), StoreError> {
    admit_object_version_bytes(schema, canonical_bytes, claimed)
        .map(|_| ())
        .map_err(|_| StoreError::Corrupt)
}

fn relation_key(schema: SchemaIdentity) -> Hash {
    let mut bytes = Vec::with_capacity(4);
    bytes.push(schema.domain());
    bytes.extend_from_slice(&schema.ty().to_be_bytes());
    bytes.push(schema.version());
    digest(b"store.relation-key.v1\0", &bytes)
}

impl WorkspaceBinding {
    pub(crate) fn from_parts(root: Hash, closure: ClosureId) -> Self {
        let mut bytes = Vec::with_capacity(ID_BYTES * 2);
        bytes.extend_from_slice(&root);
        bytes.extend_from_slice(closure.as_bytes());
        Self {
            root,
            closure,
            proof: digest(WORKSPACE_CLOSURE_DOMAIN, &bytes),
        }
    }

    /// Returns the typed workspace root bytes in this checked binding.
    #[must_use]
    pub const fn root(&self) -> &Hash {
        &self.root
    }

    /// Returns the complete closure identity.
    #[must_use]
    pub const fn closure(&self) -> ClosureId {
        self.closure
    }

    /// Returns the binding proof digest.
    #[must_use]
    pub const fn proof(&self) -> &Hash {
        &self.proof
    }

    pub(crate) fn verify(self) -> Result<(), StoreError> {
        if Self::from_parts(self.root, self.closure).proof != self.proof {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
}

fn object_commitment(object: &TypedObject) -> Hash {
    let mut bytes = Vec::with_capacity(1 + 2 + 1 + ID_BYTES * 2 + 8 + object.bytes.len());
    bytes.push(object.schema.domain());
    bytes.extend_from_slice(&object.schema.ty().to_le_bytes());
    bytes.push(object.schema.version());
    bytes.extend_from_slice(&object.key);
    bytes.extend_from_slice(&object.version);
    let length = u64::try_from(object.bytes.len()).unwrap_or(u64::MAX);
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&object.bytes);
    digest(OBJECT_DOMAIN, &bytes)
}

fn put_u32(output: &mut Vec<u8>, value: usize) -> Result<(), StoreError> {
    output.extend_from_slice(
        &u32::try_from(value)
            .map_err(|_| StoreError::Bounds)?
            .to_le_bytes(),
    );
    Ok(())
}

fn put_u64(output: &mut Vec<u8>, value: usize) -> Result<(), StoreError> {
    output.extend_from_slice(
        &u64::try_from(value)
            .map_err(|_| StoreError::Bounds)?
            .to_le_bytes(),
    );
    Ok(())
}

fn read_u32(bytes: &[u8], at: &mut usize) -> Result<u32, StoreError> {
    let end = at.checked_add(4).ok_or(StoreError::Bounds)?;
    let value = bytes.get(*at..end).ok_or(StoreError::Corrupt)?;
    *at = end;
    value
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| StoreError::Corrupt)
}

fn read_u64(bytes: &[u8], at: &mut usize) -> Result<u64, StoreError> {
    let end = at.checked_add(8).ok_or(StoreError::Bounds)?;
    let value = bytes.get(*at..end).ok_or(StoreError::Corrupt)?;
    *at = end;
    value
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| StoreError::Corrupt)
}

fn read_hash(bytes: &[u8], at: &mut usize) -> Result<Hash, StoreError> {
    let end = at.checked_add(32).ok_or(StoreError::Bounds)?;
    let value = bytes.get(*at..end).ok_or(StoreError::Corrupt)?;
    *at = end;
    value.try_into().map_err(|_| StoreError::Corrupt)
}

#[cfg(test)]
mod tests;

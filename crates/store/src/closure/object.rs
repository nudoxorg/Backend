//! Typed immutable object admission and backend-version identity checks.

use super::{
    Hash, ObjectId, RelationAdmissionRegistry, StoreError, admit_backend_object_version,
    object_commitment, relation_key,
};
use backend_version::{
    CanonicalRelation, IdContext, ObjectKey, ObjectVersion, Schema, SchemaIdentity, UntrustedId,
    admit_canonical_root_claim,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Admission {
    TypedObject,
    CheckedRelation,
    Wire,
}

/// A typed immutable object admitted into a [`crate::ClosureManifest`].
#[derive(Clone, Debug)]
pub struct TypedObject {
    pub(super) schema: SchemaIdentity,
    pub(super) key: Hash,
    pub(super) version: Hash,
    pub(super) bytes: Arc<[u8]>,
    admission: Admission,
}

impl PartialEq for TypedObject {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.key == other.key
            && self.version == other.version
            && self.bytes == other.bytes
    }
}

impl Eq for TypedObject {}

impl TypedObject {
    /// Encodes and admits one object from its canonical typed value.
    ///
    /// The key and version are derived from the same schema and value
    /// encoding.  There is no public constructor that accepts an arbitrary
    /// version alongside arbitrary bytes.
    #[must_use]
    pub fn from_value<T: Schema>(key: &ObjectKey<T>, value: &T::Value) -> Self {
        let mut bytes = Vec::new();
        T::encode(value, &mut bytes);
        Self {
            schema: SchemaIdentity::new(T::DOMAIN, T::TYPE, T::VERSION),
            key: key.to_bytes(),
            version: ObjectVersion::<T>::from_value(value).to_bytes(),
            bytes: Arc::from(bytes.into_boxed_slice()),
            admission: Admission::TypedObject,
        }
    }

    /// Materializes one checked relation state as a typed immutable object.
    ///
    /// The relation root is the object's complete version, while the bytes
    /// are the canonical root node emitted by `backend-version`.  The version
    /// layer owns canonical materialization; this adapter consumes its checked
    /// descriptor without iterating or rebuilding the relation tree.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the checked descriptor cannot be
    /// represented as a store object.
    pub fn from_relation_state<R: CanonicalRelation>(
        state: &backend_version::RelationState<R>,
    ) -> Result<Self, StoreError> {
        let materialized = state.materialize();
        Self::from_checked_state_object_ref(materialized)
    }

    /// Materializes a relation object from version's lifetime-bound checked
    /// state-object view without rebuilding or cloning the canonical node.
    ///
    /// The returned store object makes the one required immutable byte copy;
    /// the version tree remains borrowed only for the duration of this call.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the checked node commitment does
    /// not equal its checked state root.
    pub fn from_checked_state_object_ref<R: CanonicalRelation>(
        object: backend_version::CheckedStateObjectRef<'_, R>,
    ) -> Result<Self, StoreError> {
        let schema = SchemaIdentity::of_relation::<R>();
        Ok(Self {
            schema,
            key: relation_key(schema),
            version: object.root().to_bytes(),
            bytes: Arc::from(object.canonical_bytes().to_vec().into_boxed_slice()),
            admission: Admission::CheckedRelation,
        })
    }

    /// Materializes a relation object from version's checked state-object
    /// capability.
    ///
    /// [`backend_version::CheckedStateObject`] can only be produced by a
    /// checked relation state or by `StateRoot::admit_canonical_node`, so the
    /// store never accepts an unpaired root claim and byte string.  The
    /// canonical node is copied once into the immutable object representation.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the checked node commitment does
    /// not equal its checked state root.
    pub fn from_checked_state_object<R: CanonicalRelation>(
        object: &backend_version::CheckedStateObject<R>,
    ) -> Result<Self, StoreError> {
        Self::from_state_root(object.root(), object.canonical_node())
    }

    /// Materializes a checked state root from its opaque canonical node.
    ///
    /// `CanonicalNode` can only be produced by the backend-version canonical
    /// builders.  Comparing its commitment with the supplied typed root keeps
    /// this convenience safe for owners that already hold the node bytes, and
    /// avoids requiring access to the version crate's private tree storage.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the node commitment differs from
    /// `root`.
    pub fn from_state_root<R: CanonicalRelation>(
        root: backend_version::StateRoot<R>,
        node: &backend_version::CanonicalNode<R>,
    ) -> Result<Self, StoreError> {
        let claim = UntrustedId::<R>::from_wire(root.as_bytes(), IdContext::relation::<R>())
            .map_err(|_| StoreError::Corrupt)?;
        let admitted = admit_canonical_root_claim::<R>(claim, node.as_bytes())
            .map_err(|_| StoreError::Corrupt)?;
        let (admitted_root, admitted_node) = admitted.into_parts();
        if admitted_root != root || admitted_node.commitment() != root {
            return Err(StoreError::Corrupt);
        }
        let schema = SchemaIdentity::of_relation::<R>();
        Ok(Self {
            schema,
            key: relation_key(schema),
            version: root.to_bytes(),
            bytes: Arc::from(admitted_node.as_bytes().to_vec().into_boxed_slice()),
            admission: Admission::CheckedRelation,
        })
    }

    /// Encodes one object from a typed value and checks an independently
    /// supplied typed version against the canonical bytes.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the supplied version does not
    /// equal the version recomputed from `value`.
    pub fn from_version<T: Schema>(
        key: &ObjectKey<T>,
        version: ObjectVersion<T>,
        value: &T::Value,
    ) -> Result<Self, StoreError> {
        let actual = ObjectVersion::<T>::from_value(value);
        if actual != version {
            return Err(StoreError::Corrupt);
        }
        Ok(Self::from_value(key, value))
    }

    /// Returns the immutable object identity used by the physical object
    /// store.  The identity includes schema, key, version, and bytes.
    #[must_use]
    pub fn id(&self) -> ObjectId {
        ObjectId(object_commitment(self))
    }

    /// Returns the typed schema identity.
    #[must_use]
    pub const fn schema(&self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the stable logical object key.
    #[must_use]
    pub const fn key(&self) -> &Hash {
        &self.key
    }

    /// Returns the complete object version.
    #[must_use]
    pub const fn version(&self) -> &Hash {
        &self.version
    }

    /// Returns the canonical encoded object bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn from_wire_parts(
        schema: SchemaIdentity,
        key: Hash,
        version: Hash,
        bytes: Box<[u8]>,
    ) -> Self {
        Self {
            schema,
            key,
            version,
            bytes: Arc::from(bytes),
            admission: Admission::Wire,
        }
    }

    pub(crate) fn verify_version(&self) -> Result<(), StoreError> {
        match self.admission {
            Admission::TypedObject | Admission::CheckedRelation => Ok(()),
            Admission::Wire => {
                admit_backend_object_version(self.schema, &self.bytes, &self.version)
            }
        }
    }

    pub(crate) fn verify_wire_version(
        &self,
        registry: &RelationAdmissionRegistry,
    ) -> Result<(), StoreError> {
        if registry.contains_schema(self.schema) {
            return registry.admit_relation(self.schema, &self.version, &self.bytes);
        }
        admit_backend_object_version(self.schema, &self.bytes, &self.version)
    }

    pub(crate) fn verify_for_kind(
        &self,
        kind: backend_version::ClosureKind,
        registry: &RelationAdmissionRegistry,
    ) -> Result<(), StoreError> {
        match kind {
            backend_version::ClosureKind::Relation => {
                registry.admit_relation(self.schema, &self.version, &self.bytes)
            }
            backend_version::ClosureKind::Basis
            | backend_version::ClosureKind::Authority
            | backend_version::ClosureKind::Transaction => {
                admit_backend_object_version(self.schema, &self.bytes, &self.version)
            }
        }
    }
}

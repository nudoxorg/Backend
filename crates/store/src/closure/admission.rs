//! Runtime schema admission for closure objects.

use super::manifest_tree::ManifestRelation;
use super::{Hash, RawRelation, StoreError};
use backend_version::{
    CanonicalRelation, IdContext, SchemaIdentity, UntrustedId, admit_canonical_root_claim,
};
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub(crate) struct RelationNodeReferences {
    pub(crate) children: Vec<Hash>,
    pub(crate) value_references: Vec<Hash>,
}

trait RelationVerifier: Send + Sync {
    fn references(
        &self,
        version: &Hash,
        bytes: &[u8],
    ) -> Result<RelationNodeReferences, StoreError>;
}

struct TypedRelationVerifier<R: CanonicalRelation>(PhantomData<fn() -> R>);

impl<R: CanonicalRelation> RelationVerifier for TypedRelationVerifier<R> {
    fn references(
        &self,
        version: &Hash,
        bytes: &[u8],
    ) -> Result<RelationNodeReferences, StoreError> {
        let claim = UntrustedId::<R>::from_wire(version, IdContext::relation::<R>())
            .map_err(|_| StoreError::Corrupt)?;
        let admitted =
            admit_canonical_root_claim::<R>(claim, bytes).map_err(|_| StoreError::Corrupt)?;
        Ok(RelationNodeReferences {
            children: admitted
                .child_summaries()
                .map_err(|_| StoreError::Corrupt)?
                .into_iter()
                .map(|child| child.commitment.to_bytes())
                .collect(),
            value_references: Vec::new(),
        })
    }
}

struct RawRelationVerifier;

impl RelationVerifier for RawRelationVerifier {
    fn references(
        &self,
        version: &Hash,
        bytes: &[u8],
    ) -> Result<RelationNodeReferences, StoreError> {
        let claim =
            UntrustedId::<RawRelation>::from_wire(version, IdContext::relation::<RawRelation>())
                .map_err(|_| StoreError::Corrupt)?;
        let admitted = admit_canonical_root_claim::<RawRelation>(claim, bytes)
            .map_err(|_| StoreError::Corrupt)?;
        let value_references = if admitted.node().level() == 0 {
            admitted
                .leaf_entries()
                .map_err(|_| StoreError::Corrupt)?
                .into_iter()
                .flat_map(|(_, value)| value.references)
                .collect()
        } else {
            Vec::new()
        };
        Ok(RelationNodeReferences {
            children: admitted
                .child_summaries()
                .map_err(|_| StoreError::Corrupt)?
                .into_iter()
                .map(|child| child.commitment.to_bytes())
                .collect(),
            value_references,
        })
    }
}

/// Runtime relation admission registry used when a typed root crosses a wire
/// or filesystem boundary.
///
/// The default registry contains the store's [`RawRelation`]. Product owners
/// that persist another [`backend_version::CanonicalRelation`] must register
/// it before opening the store. Unknown relation schemas fail closed during
/// object and closure admission while ordinary typed object schemas remain
/// valid through their object-version digest.
#[derive(Clone)]
pub struct RelationAdmissionRegistry {
    verifiers: Arc<BTreeMap<SchemaIdentity, Arc<dyn RelationVerifier>>>,
}

impl fmt::Debug for RelationAdmissionRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelationAdmissionRegistry")
            .field("schema_count", &self.verifiers.len())
            .finish()
    }
}

impl Default for RelationAdmissionRegistry {
    fn default() -> Self {
        let mut verifiers = BTreeMap::new();
        verifiers.insert(
            SchemaIdentity::of_relation::<RawRelation>(),
            Arc::new(RawRelationVerifier) as Arc<dyn RelationVerifier>,
        );
        verifiers.insert(
            SchemaIdentity::of_relation::<ManifestRelation>(),
            Arc::new(TypedRelationVerifier::<ManifestRelation>(PhantomData))
                as Arc<dyn RelationVerifier>,
        );
        Self {
            verifiers: Arc::new(verifiers),
        }
    }
}

impl RelationAdmissionRegistry {
    /// Creates a registry containing the built-in byte-oriented relation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a registry with one additional checked relation decoder.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::MalformedDelta`] when the relation schema is
    /// already registered.
    pub fn with_relation<R: CanonicalRelation>(self) -> Result<Self, StoreError> {
        let mut verifiers = (*self.verifiers).clone();
        let schema = SchemaIdentity::of_relation::<R>();
        if verifiers.contains_key(&schema) {
            return Err(StoreError::MalformedDelta);
        }
        verifiers.insert(
            schema,
            Arc::new(TypedRelationVerifier::<R>(PhantomData)) as Arc<dyn RelationVerifier>,
        );
        Ok(Self {
            verifiers: Arc::new(verifiers),
        })
    }

    pub(crate) fn admit_relation(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.node_references(schema, version, bytes).map(|_| ())
    }

    pub(crate) fn node_references(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
        bytes: &[u8],
    ) -> Result<RelationNodeReferences, StoreError> {
        self.verifiers
            .get(&schema)
            .ok_or(StoreError::Corrupt)?
            .references(version, bytes)
    }

    /// Returns whether a checked relation decoder is registered for `schema`.
    ///
    /// Store and engine workspace owners use this narrow query while
    /// partitioning ordinary closure objects from lazily authenticated
    /// relation descendants. It exposes registration state only; it does not
    /// admit bytes or construct a relation capability.
    #[must_use]
    pub fn contains_schema(&self, schema: SchemaIdentity) -> bool {
        self.verifiers.contains_key(&schema)
    }
}

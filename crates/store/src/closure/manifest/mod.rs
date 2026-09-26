//! Canonical immutable closure manifests and reference graph checks.

use super::manifest_tree::{ManifestEntry, ManifestRelation, ManifestTree};
use super::{
    CLOSURE_MAGIC, ClosureId, Hash, MIN_OBJECT_BYTES, ObjectEdge, ObjectId,
    RelationAdmissionRegistry, StoreError, TypedObject, put_u32, put_u64, read_hash, read_u32,
    read_u64,
};
use backend_version::{CanonicalRelation, SchemaIdentity, TreeNodeHandle};
use std::collections::BTreeMap;
use std::sync::Arc;

mod delta;
mod index;
mod read;
mod storage;
mod traversal;
mod wire;

/// One exact object-index update for a closure manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestChange {
    key: ObjectId,
    before: Option<ManifestEntry>,
    after: Option<TypedObject>,
}

impl ManifestChange {
    /// Returns the exact delete-plus-insert sequence for an immutable object
    /// replacement. Object identities include their bytes, so a changed value
    /// cannot be replaced at the old key. The returned changes are ordered by
    /// their physical object identities and can be passed directly to
    /// [`ClosureManifest::prepare_delta`].
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Corrupt` when either object descriptor is too large
    /// for the bounded manifest representation.
    pub fn replacement(before: &TypedObject, after: &TypedObject) -> Result<Vec<Self>, StoreError> {
        if before.id() == after.id() {
            return Ok(Vec::new());
        }
        let mut changes = vec![Self::delete(before)?, Self::insert(after)?];
        changes.sort_by_key(Self::key);
        Ok(changes)
    }

    /// Inserts one object that is absent from the exact base manifest.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Bounds` when the object descriptor cannot be
    /// represented in the bounded manifest index.
    pub fn insert(after: &TypedObject) -> Result<Self, StoreError> {
        Ok(Self {
            key: after.id(),
            before: None,
            after: Some(after.clone()),
        })
    }

    /// Deletes one object that must match the exact base manifest.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Bounds` when the object descriptor cannot be
    /// represented in the bounded manifest index.
    pub fn delete(before: &TypedObject) -> Result<Self, StoreError> {
        Ok(Self {
            key: before.id(),
            before: Some(ManifestEntry::from_object(before)?),
            after: None,
        })
    }

    /// Returns the immutable object key changed by this update.
    #[must_use]
    pub const fn key(&self) -> ObjectId {
        self.key
    }
}

/// Exact work performed by a persistent closure-index update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ManifestWork {
    /// Canonical closure-index nodes inspected by preparation.
    pub visited_nodes: usize,
    /// Canonical closure-index nodes rebuilt by preparation.
    pub copied_nodes: usize,
    /// Rows retained from unchanged subtrees.
    pub reused_rows: usize,
}

impl ManifestWork {
    fn from_tree(work: backend_version::TreeWork) -> Self {
        Self {
            visited_nodes: work.visited_nodes,
            copied_nodes: work.copied_nodes,
            reused_rows: work.reused_nodes,
        }
    }
}

/// A prepared exact-base closure-index update.
#[derive(Clone, Debug)]
pub struct PreparedManifestDelta {
    base: ClosureId,
    target: ClosureManifest,
    work: ManifestWork,
}

impl PreparedManifestDelta {
    /// Returns the exact base closure identity required for commit.
    #[must_use]
    pub const fn base(&self) -> ClosureId {
        self.base
    }

    /// Returns the path-copy work observed while preparing this update.
    #[must_use]
    pub const fn work(&self) -> ManifestWork {
        self.work
    }

    /// Publishes the already checked target closure.
    #[must_use]
    pub fn commit(self) -> ClosureManifest {
        self.target
    }
}

use index::{ManifestLookup, ManifestState};

/// A complete immutable closure represented by a persistent canonical object
/// index. The byte array returned by objects is a bounded export cache for
/// compatibility; canonical identity and updates use the persistent relation
/// root directly.
#[derive(Clone, Debug)]
pub struct ClosureManifest {
    state: Arc<ManifestState>,
    id: ClosureId,
}

impl PartialEq for ClosureManifest {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ClosureManifest {}

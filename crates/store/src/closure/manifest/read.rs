//! Reads, admission, and encoding for a closure manifest.

use super::*;

impl ClosureManifest {
    /// Constructs a closure manifest from typed objects for unit tests.
    #[cfg(test)]
    pub(in crate::closure) fn from_test_parts(objects: &[TypedObject], id: ClosureId) -> Self {
        let records = objects
            .iter()
            .cloned()
            .map(|object| (object.id(), Arc::new(object)))
            .collect::<BTreeMap<_, _>>();
        let tree = ManifestTree::from_objects(&records).unwrap_or_else(|_| {
            let empty = BTreeMap::new();
            ManifestTree::from_objects(&empty).unwrap_or_else(|_| unreachable!())
        });
        Self {
            state: Arc::new(ManifestState::new(
                tree,
                ManifestLookup::root(&records),
                Arc::from(
                    records
                        .values()
                        .cloned()
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                ),
            )),
            id,
        }
    }

    /// Constructs a canonical closure from already typed objects.
    ///
    /// Objects must be strictly ordered by (schema, key, version). The
    /// constructor rejects duplicates and does not silently reorder caller
    /// data. The persistent index itself is anchored by immutable object
    /// identity, so equivalent input generations have one root.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::MalformedDelta` when objects are unordered or
    /// duplicated, or `StoreError::Corrupt` when an object's claimed version
    /// does not reproduce its canonical bytes.
    pub fn new(objects: Vec<TypedObject>) -> Result<Self, StoreError> {
        let manifest = Self::new_index(objects)?;
        for object in manifest.objects() {
            object.verify_version()?;
        }
        Ok(manifest)
    }

    /// Constructs a canonical closure and admits relation roots through an
    /// explicit decoder registry.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Corrupt` when a relation root is unknown to the
    /// registry or fails its canonical grammar check.
    pub fn new_with_registry(
        objects: Vec<TypedObject>,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        let manifest = Self::new_index(objects)?;
        manifest.admit_with_registry(registry)?;
        Ok(manifest)
    }

    /// Builds a closure manifest from typed objects without version verification.
    pub(super) fn new_index(objects: Vec<TypedObject>) -> Result<Self, StoreError> {
        if objects.windows(2).any(|window| {
            (window[0].schema(), window[0].key(), window[0].version())
                >= (window[1].schema(), window[1].key(), window[1].version())
        }) {
            return Err(StoreError::MalformedDelta);
        }
        let mut records = BTreeMap::new();
        for object in objects {
            let id = object.id();
            if records.insert(id, Arc::new(object)).is_some() {
                return Err(StoreError::MalformedDelta);
            }
        }
        let tree = ManifestTree::from_objects(&records)?;
        let id = ClosureId::from_bytes(tree.root_id());
        Ok(Self {
            state: Arc::new(ManifestState::new(
                tree,
                ManifestLookup::root(&records),
                Arc::from(
                    records
                        .values()
                        .cloned()
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                ),
            )),
            id,
        })
    }

    pub(crate) fn from_index(objects: Vec<TypedObject>) -> Result<Self, StoreError> {
        Self::new_index(objects)
    }

    pub(crate) fn admit_with_registry(
        &self,
        registry: &RelationAdmissionRegistry,
    ) -> Result<(), StoreError> {
        traversal::verify_relation_children(self.objects(), registry)?;
        Ok(())
    }

    pub(crate) fn admit_objects_with_registry(
        &self,
        registry: &RelationAdmissionRegistry,
    ) -> Result<(), StoreError> {
        for object in self.objects() {
            object.verify_wire_version(registry)?;
        }
        Ok(())
    }

    /// Returns the closure identity, which is the canonical persistent index
    /// root commitment.
    #[must_use]
    pub const fn id(&self) -> ClosureId {
        self.id
    }

    /// Returns every immutable object in canonical compatibility order.
    ///
    /// This materializes a bounded export cache. Durable and incremental
    /// callers should use `prepare_delta` so a one-object update stays
    /// path-copy bounded.
    #[must_use]
    pub fn objects(&self) -> &[TypedObject] {
        self.state.objects()
    }

    /// Returns the authenticated number of retained object descriptors.
    ///
    /// This reads the persistent manifest root summary and does not build the
    /// compatibility export returned by [`Self::objects`].  Workspace
    /// placement and retention accounting can therefore inspect closure size
    /// without traversing every retained object.
    #[must_use]
    pub fn object_count(&self) -> u64 {
        self.state.tree.root_handle().summary().row_count
    }

    /// Tests exact immutable-object membership through the persistent
    /// manifest index without materializing the compatibility export.
    ///
    /// The export returned by [`Self::objects`] is intentionally lazy and
    /// cached for compatibility callers. Recovery and publication paths only
    /// need a membership witness, so they must use this bounded tree probe to
    /// avoid turning a one-object check into an O(n) allocation and scan.
    #[must_use]
    pub fn contains_object_id(&self, object: ObjectId) -> bool {
        self.state.tree.contains(object.as_bytes())
    }

    pub(crate) fn root_handle(&self) -> TreeNodeHandle<ManifestRelation> {
        self.state.tree.root_handle()
    }

    pub(crate) fn tree_source_root(&self) -> Option<Hash> {
        self.state.tree.source_root()
    }

    pub(crate) fn tree_frontier(&self) -> Option<&[TreeNodeHandle<ManifestRelation>]> {
        self.state.tree.frontier()
    }

    pub(crate) fn changed_objects(&self) -> impl Iterator<Item = Arc<TypedObject>> + '_ {
        self.state.changed.iter().cloned()
    }

    pub(crate) fn find_reference(
        &self,
        schema: SchemaIdentity,
        version: Hash,
    ) -> Option<Arc<TypedObject>> {
        self.state.lookup.find((schema, version))
    }

    /// Prepares an exact-base persistent object-index update.
    ///
    /// Every before descriptor must equal the row currently held at its object
    /// identity. Only changed tree paths are copied; unchanged index nodes and
    /// immutable object byte storage are retained by Arc.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::WrongBase` for a base identity mismatch,
    /// `StoreError::BeforeMismatch` for an exact row mismatch, and
    /// `StoreError::MalformedDelta` for unordered or duplicate keys.
    pub fn prepare_delta(
        &self,
        changes: &[ManifestChange],
    ) -> Result<PreparedManifestDelta, StoreError> {
        delta::prepare_delta(self, changes)
    }

    /// Returns checked physical object edges encoded by relation nodes and raw
    /// relation values.
    ///
    /// Branch child commitments are resolved against this manifest, so a
    /// missing descendant is an admission error. Raw relation value references
    /// are retained as external edges when their target is in a shared closure.
    /// The result is sorted and deduplicated for streaming GC callers.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Corrupt` when relation admission fails or a branch
    /// points at an object absent from this manifest.
    pub fn object_edges(
        &self,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Vec<ObjectEdge>, StoreError> {
        traversal::object_edges(self, registry)
    }

    /// Encodes a bounded flat compatibility export. Canonical closure
    /// identity comes from the persistent index root; durable publication can
    /// use the root descriptor and node CAS without invoking this export.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Bounds` when a count, object length, or complete
    /// encoded manifest exceeds the checked wire limit.
    pub fn encode(&self, max_bytes: usize) -> Result<Vec<u8>, StoreError> {
        wire::encode(self, max_bytes)
    }

    /// Admits a serialized flat compatibility export after checking its
    /// identity, ordering, and every checked length field.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Corrupt` for invalid framing, ordering, or an
    /// identity mismatch, and `StoreError::Bounds` for oversized input.
    pub fn decode(bytes: &[u8], max_bytes: usize) -> Result<Self, StoreError> {
        wire::decode(bytes, max_bytes)
    }

    /// Admits a serialized closure using an explicit relation decoder registry.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Corrupt` for an unknown or malformed relation root,
    /// and `StoreError::Bounds` for oversized input.
    pub fn decode_with_registry(
        bytes: &[u8],
        max_bytes: usize,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        wire::decode_with_registry(bytes, max_bytes, registry)
    }

    pub(crate) fn decode_root_with_registry(
        bytes: &[u8],
        max_bytes: usize,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        wire::decode_root_with_registry(bytes, max_bytes, registry)
    }

    pub(crate) fn root_only_from_node<R: CanonicalRelation>(
        node: &TreeNodeHandle<R>,
    ) -> Result<Self, StoreError> {
        storage::root_only_from_node(node)
    }

    /// Builds a complete relation-node closure from a checked relation state.
    ///
    /// The relation node walker borrows the existing persistent tree and never
    /// rebuilds it from logical rows.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Corrupt` when a checked relation node cannot be
    /// represented as a closure object.
    pub fn for_relation_state<R: CanonicalRelation>(
        state: &backend_version::RelationState<R>,
    ) -> Result<Self, StoreError> {
        let objects = traversal::relation_node_objects(state)?;
        Self::new(objects)
    }

    /// Builds a complete relation-node closure with an explicit decoder registry.
    ///
    /// Every branch commitment is represented by one admitted relation object,
    /// so reopen can validate transitive reachability.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Corrupt` when a checked node cannot be admitted or a
    /// relation decoder rejects its canonical bytes.
    pub fn for_relation_state_with_registry<R: CanonicalRelation>(
        state: &backend_version::RelationState<R>,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        let objects = traversal::relation_node_objects(state)?;
        Self::new_with_registry(objects, registry)
    }
}

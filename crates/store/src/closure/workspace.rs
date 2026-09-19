//! Checked workspace closure binding and reference admission.

use super::{ClosureId, ClosureManifest, Hash, RelationAdmissionRegistry, StoreError};
use backend_version::{
    CanonicalNode, CanonicalRelation, SchemaIdentity, StateRoot, TreeNodeHandle, WorkspaceRoot,
};

/// Borrowed access to a node whose canonical bytes already carry version
/// admission evidence.
///
/// Both retained trees and lazy path-copy updates implement this seam, so the
/// workspace closure can publish one relation frontier without converting it
/// into a second tree representation.
pub trait CheckedRelationNode<R: CanonicalRelation> {
    /// Returns the admitted canonical node.
    fn admitted_node(&self) -> &CanonicalNode<R>;
}

impl<R: CanonicalRelation> CheckedRelationNode<R> for TreeNodeHandle<R> {
    fn admitted_node(&self) -> &CanonicalNode<R> {
        self.canonical()
    }
}

impl<R: CanonicalRelation> CheckedRelationNode<R> for CanonicalNode<R> {
    fn admitted_node(&self) -> &CanonicalNode<R> {
        self
    }
}

impl<R, N> CheckedRelationNode<R> for &N
where
    R: CanonicalRelation,
    N: CheckedRelationNode<R>,
{
    fn admitted_node(&self) -> &CanonicalNode<R> {
        (*self).admitted_node()
    }
}

/// A closure manifest bound to a checked typed workspace root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceClosure {
    root: WorkspaceRoot,
    manifest: ClosureManifest,
    binding: WorkspaceBinding,
    root_only: bool,
    /// Relation nodes in the current path-copy frontier that still need
    /// physical publication. Only the selected root is retained in the
    /// closure manifest; child nodes move directly into the relation CAS.
    selected_roots: Vec<super::TypedObject>,
}

/// Exact workspace-root and closure binding persisted with a publication.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceBinding {
    pub(super) root: Hash,
    pub(super) closure: ClosureId,
    pub(super) proof: Hash,
}

impl WorkspaceClosure {
    /// Extends a checked closure with a path-copy relation frontier and extra
    /// immutable payload objects.
    ///
    /// `target` must be the checked workspace manifest that names
    /// `target_root`. The frontier must contain that root. Each supplied node
    /// is converted directly from its checked canonical handle; the base
    /// manifest's persistent index is then path-copied, removing the replaced
    /// root from a root-only base so generations do not grow monotonically.
    /// Unchanged object bytes remain shared by the persistent index. Relation
    /// references are re-admitted before the returned closure is constructed.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the target root is not retained or
    /// a relation reference cannot be admitted, and `StoreError::Bounds` when
    /// an object descriptor cannot be represented.
    pub fn extend_checked_nodes<R, I, J, N>(
        base: &Self,
        target: &backend_version::CheckedWorkspaceManifest,
        target_root: StateRoot<R>,
        changed_nodes: I,
        extra_objects: J,
    ) -> Result<Self, StoreError>
    where
        R: CanonicalRelation,
        I: IntoIterator<Item = N>,
        N: CheckedRelationNode<R>,
        J: IntoIterator<Item = super::TypedObject>,
    {
        Self::extend_checked_nodes_with_registry(
            base,
            target,
            target_root,
            changed_nodes,
            extra_objects,
            &RelationAdmissionRegistry::default(),
        )
    }

    /// Extends a checked closure with a path-copy frontier using explicit
    /// relation decoders.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the target root is not retained or
    /// a relation reference cannot be admitted, and `StoreError::Bounds` when
    /// an object descriptor cannot be represented.
    pub fn extend_checked_nodes_with_registry<R, I, J, N>(
        base: &Self,
        target: &backend_version::CheckedWorkspaceManifest,
        target_root: StateRoot<R>,
        changed_nodes: I,
        extra_objects: J,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError>
    where
        R: CanonicalRelation,
        I: IntoIterator<Item = N>,
        N: CheckedRelationNode<R>,
        J: IntoIterator<Item = super::TypedObject>,
    {
        target.validate().map_err(|_| StoreError::Corrupt)?;
        let schema = SchemaIdentity::of_relation::<R>();
        let target_version = target_root.to_bytes();
        if !target
            .relations()
            .iter()
            .any(|binding| binding.schema() == schema && binding.root() == target_version)
        {
            return Err(StoreError::Corrupt);
        }
        let mut target_retained = false;
        let mut changes = Vec::new();
        let mut checked_objects = Vec::new();
        let mut publication_nodes = Vec::new();
        let mut retained_ids = Vec::new();
        for node in changed_nodes {
            let node = node.admitted_node();
            let root = node.commitment();
            let is_target = root == target_root;
            if is_target {
                target_retained = true;
            }
            let object = super::TypedObject::from_state_root(root, node)?;
            checked_objects.push(object.clone());
            publication_nodes.push(object.clone());
            if is_target {
                retained_ids.push(object.id());
                if !base.manifest.contains_object_id(object.id()) {
                    changes.push(super::ManifestChange::insert(&object)?);
                }
            }
        }
        for object in extra_objects {
            checked_objects.push(object.clone());
            retained_ids.push(object.id());
            if base.manifest.contains_object_id(object.id()) {
                continue;
            }
            changes.push(super::ManifestChange::insert(&object)?);
        }
        if !target_retained {
            return Err(StoreError::Corrupt);
        }
        for previous_root in &base.selected_roots {
            if !retained_ids.contains(&previous_root.id())
                && base.manifest.contains_object_id(previous_root.id())
            {
                changes.push(super::ManifestChange::delete(previous_root)?);
            }
        }
        changes.sort_by_key(super::ManifestChange::key);
        changes.dedup_by_key(|change| change.key());
        let manifest = if changes.is_empty() {
            base.manifest.clone()
        } else {
            base.manifest.prepare_delta(&changes)?.commit()
        };
        verify_extension_objects(&checked_objects, registry)?;
        let refs = target.closure_refs().map_err(|_| StoreError::Corrupt)?;
        ensure_refs(&refs, &manifest, registry)?;
        publication_nodes.sort_by_key(super::TypedObject::id);
        publication_nodes.dedup_by_key(|object| object.id());
        Ok(Self::new_root_only_with_selected(
            target.root(),
            manifest,
            publication_nodes,
        ))
    }

    /// Binds a complete closure to an already checked workspace root.
    #[must_use]
    pub(crate) fn new(root: WorkspaceRoot, manifest: ClosureManifest) -> Self {
        let binding = WorkspaceBinding::from_parts(root.to_bytes(), manifest.id());
        Self {
            root,
            manifest,
            binding,
            root_only: false,
            selected_roots: Vec::new(),
        }
    }

    fn new_root_only_with_selected(
        root: WorkspaceRoot,
        manifest: ClosureManifest,
        selected_roots: Vec<super::TypedObject>,
    ) -> Self {
        let binding = WorkspaceBinding::from_parts(root.to_bytes(), manifest.id());
        Self {
            root,
            manifest,
            binding,
            root_only: true,
            selected_roots,
        }
    }

    /// Admits a closure against a checked backend workspace manifest.
    ///
    /// Every object reference emitted by the manifest must be present in the
    /// supplied immutable object manifest with the same schema and version.
    /// This keeps a raw workspace-root claim from becoming a durable closure.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the workspace manifest is
    /// unverified or any referenced typed object is absent.
    pub fn from_checked_manifest(
        manifest: &backend_version::WorkspaceManifest,
        objects: ClosureManifest,
    ) -> Result<Self, StoreError> {
        Self::from_checked_manifest_with_registry(
            manifest,
            objects,
            &RelationAdmissionRegistry::default(),
        )
    }

    /// Admits a closure against a checked workspace manifest and explicit
    /// relation decoder registry.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the workspace manifest is
    /// unverified or a referenced typed object is absent or not canonical.
    pub fn from_checked_manifest_with_registry(
        manifest: &backend_version::WorkspaceManifest,
        objects: ClosureManifest,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        manifest.validate().map_err(|_| StoreError::Corrupt)?;
        objects.admit_with_registry(registry)?;
        let refs = manifest.closure_refs().map_err(|_| StoreError::Corrupt)?;
        ensure_refs(&refs, &objects, registry)?;
        Ok(Self::new(manifest.root(), objects))
    }

    /// Admits a checked workspace manifest whose relation roots are backed by
    /// the store's schema/version reference index rather than by a flat
    /// transitive object list.
    ///
    /// Root-only closures are the durable handoff for lazy relation trees:
    /// each selected root is authenticated here, while child nodes are
    /// fetched through the relation reference index by
    /// [`backend_version::LazyTree`].  The selected root and authority
    /// objects still remain in the closure index, so the workspace binding is
    /// complete and exact-base publication remains unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the manifest, root object, or
    /// selected typed references fail admission.
    pub fn from_checked_manifest_root_only_with_registry(
        manifest: &backend_version::WorkspaceManifest,
        objects: ClosureManifest,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        manifest.validate().map_err(|_| StoreError::Corrupt)?;
        objects.admit_objects_with_registry(registry)?;
        let refs = manifest.closure_refs().map_err(|_| StoreError::Corrupt)?;
        ensure_refs(&refs, &objects, registry)?;
        let selected_roots = refs
            .iter()
            .filter(|reference| reference.kind() == backend_version::ClosureKind::Relation)
            .filter_map(|reference| objects.find_reference(reference.schema(), reference.version()))
            .map(|object| (*object).clone())
            .collect();
        Ok(Self::new_root_only_with_selected(
            manifest.root(),
            objects,
            selected_roots,
        ))
    }

    /// Rebinds this closure to a checked manifest and a path-copied object
    /// index while preserving an admitted root-only publication frontier.
    ///
    /// This is the intermediate form used before a new transition's payload
    /// objects exist. It validates the target manifest now; the later
    /// transition rebind adds and validates commit provenance without ever
    /// lowering the physical relation evidence to an untyped object list.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] if this closure does not name the exact
    /// target or the supplied object index cannot satisfy its references.
    pub fn rebind_checked_manifest_with_registry(
        &self,
        target: &backend_version::WorkspaceManifest,
        objects: ClosureManifest,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        if self.root != target.root() {
            return Err(StoreError::Corrupt);
        }
        if !self.root_only {
            return Self::from_checked_manifest_with_registry(target, objects, registry);
        }
        let mut rebound =
            Self::from_checked_manifest_root_only_with_registry(target, objects, registry)?;
        if !self.selected_roots.iter().any(|object| {
            target.relations().iter().any(|relation| {
                relation.schema() == object.schema() && relation.root() == *object.version()
            })
        }) {
            return Err(StoreError::Corrupt);
        }
        rebound.selected_roots.clone_from(&self.selected_roots);
        Ok(rebound)
    }

    /// Admits a closure against a checked workspace transition and optional
    /// checked commit.  Transition and commit roots are decoded only to
    /// compare them with the checked target manifest; their private checked
    /// capabilities remain the authority for admission.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a capability does not target the
    /// supplied manifest or a referenced immutable object is absent.
    pub fn from_checked_transition(
        target: &backend_version::WorkspaceManifest,
        transition: &backend_version::CheckedWorkspaceTransition,
        commit: Option<&backend_version::CheckedCommit>,
        objects: ClosureManifest,
    ) -> Result<Self, StoreError> {
        Self::from_checked_transition_with_registry(
            target,
            transition,
            commit,
            objects,
            &RelationAdmissionRegistry::default(),
        )
    }

    /// Admits a checked workspace transition with an explicit relation
    /// decoder registry.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a capability does not target the
    /// supplied manifest or a referenced immutable object is absent or not
    /// canonical.
    pub fn from_checked_transition_with_registry(
        target: &backend_version::WorkspaceManifest,
        transition: &backend_version::CheckedWorkspaceTransition,
        commit: Option<&backend_version::CheckedCommit>,
        objects: ClosureManifest,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        target.validate().map_err(|_| StoreError::Corrupt)?;
        objects.admit_with_registry(registry)?;
        if !transition.binds_target(target) {
            return Err(StoreError::Corrupt);
        }
        let mut refs = target.closure_refs().map_err(|_| StoreError::Corrupt)?;
        refs.extend(transition.closure_refs().map_err(|_| StoreError::Corrupt)?);
        if let Some(commit) = commit {
            if !commit.binds_target(target) {
                return Err(StoreError::Corrupt);
            }
            refs.extend(commit.closure_refs());
        }
        ensure_refs(&refs, &objects, registry)?;
        Ok(Self::new(target.root(), objects))
    }

    /// Admits a checked transition while retaining only its selected root and
    /// changed frontier. Untouched relation children remain in the durable
    /// CAS and are resolved lazily through the relation reference index.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the transition, commit, or any
    /// retained object reference fails checked admission.
    pub fn from_checked_transition_root_only_with_registry(
        target: &backend_version::WorkspaceManifest,
        transition: &backend_version::CheckedWorkspaceTransition,
        commit: Option<&backend_version::CheckedCommit>,
        objects: ClosureManifest,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        target.validate().map_err(|_| StoreError::Corrupt)?;
        objects.admit_objects_with_registry(registry)?;
        if !transition.binds_target(target) {
            return Err(StoreError::Corrupt);
        }
        let mut refs = target.closure_refs().map_err(|_| StoreError::Corrupt)?;
        refs.extend(transition.closure_refs().map_err(|_| StoreError::Corrupt)?);
        if let Some(commit) = commit {
            if !commit.binds_target(target) {
                return Err(StoreError::Corrupt);
            }
            refs.extend(commit.closure_refs());
        }
        ensure_refs(&refs, &objects, registry)?;
        let selected_roots = refs
            .iter()
            .filter(|reference| reference.kind() == backend_version::ClosureKind::Relation)
            .filter_map(|reference| objects.find_reference(reference.schema(), reference.version()))
            .map(|object| (*object).clone())
            .collect();
        Ok(Self::new_root_only_with_selected(
            target.root(),
            objects,
            selected_roots,
        ))
    }

    /// Rebinds this closure to a checked transition while preserving an
    /// already admitted path-copy publication frontier when one exists.
    ///
    /// Adding journal, catalog, or request payloads changes the closure-index
    /// root but must not erase newly split relation children that are waiting
    /// for the same atomic durable publication. This method keeps those
    /// physical nodes as typed objects outside the logical closure index.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] if this closure does not name the exact
    /// target or the supplied transition/object index is invalid.
    pub fn rebind_checked_transition_with_registry(
        &self,
        target: &backend_version::WorkspaceManifest,
        transition: &backend_version::CheckedWorkspaceTransition,
        commit: Option<&backend_version::CheckedCommit>,
        objects: ClosureManifest,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        if self.root != target.root() {
            return Err(StoreError::Corrupt);
        }
        if !self.root_only {
            return Self::from_checked_transition_with_registry(
                target, transition, commit, objects, registry,
            );
        }
        let mut rebound = Self::from_checked_transition_root_only_with_registry(
            target, transition, commit, objects, registry,
        )?;
        if !self.selected_roots.iter().any(|object| {
            target.relations().iter().any(|relation| {
                relation.schema() == object.schema() && relation.root() == *object.version()
            })
        }) {
            return Err(StoreError::Corrupt);
        }
        rebound.selected_roots.clone_from(&self.selected_roots);
        Ok(rebound)
    }

    /// Returns the checked workspace root.
    #[must_use]
    pub const fn root(&self) -> WorkspaceRoot {
        self.root
    }

    /// Returns the complete object closure.
    #[must_use]
    pub const fn manifest(&self) -> &ClosureManifest {
        &self.manifest
    }

    /// Returns the exact persisted root/closure binding.
    #[must_use]
    pub const fn binding(&self) -> WorkspaceBinding {
        self.binding
    }

    /// Returns whether this closure retains only selected roots and delegates
    /// child-node reachability to the durable relation CAS.
    #[must_use]
    pub const fn is_root_only(&self) -> bool {
        self.root_only
    }

    /// Returns the exact selected relation-root frontier that must exist in
    /// the durable node CAS before this workspace can become visible.
    pub(crate) fn selected_roots(&self) -> &[super::TypedObject] {
        &self.selected_roots
    }
}

fn verify_extension_objects(
    objects: &[super::TypedObject],
    registry: &RelationAdmissionRegistry,
) -> Result<(), StoreError> {
    for object in objects {
        object.verify_wire_version(registry)?;
        if !registry.contains_schema(object.schema()) {
            continue;
        }
        // Decoding the node proves its authenticated child commitments.  A
        // root-only closure deliberately does not retain untouched children;
        // those are resolved from the durable relation CAS during traversal.
        let _ = registry.node_references(object.schema(), object.version(), object.bytes())?;
    }
    Ok(())
}

fn ensure_refs(
    references: &[backend_version::ClosureRef],
    objects: &ClosureManifest,
    registry: &RelationAdmissionRegistry,
) -> Result<(), StoreError> {
    for reference in references {
        let object = objects
            .find_reference(reference.schema(), reference.version())
            .ok_or(StoreError::Corrupt)?;
        object.verify_for_kind(reference.kind(), registry)?;
    }
    Ok(())
}

use alloc::{collections::BTreeMap, vec::Vec};

use crate::ir::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, DeclarationIdentity,
    ExternalTargetIdentity, SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError,
    SemanticReader, TypeId,
};

use super::wire::{atom_key, identity_bytes, put_bytes, typed_row_key};
use super::{
    ATOM_LIST_TAG, ATOM_TAG, EXTERNAL_TARGET_TAG, FREE_PREDICATES_TAG, OBJECT_MEMBERS_TAG,
    ROOT_TAG, TEMPLATE_PARTS_TAG, TUPLE_ELEMENTS_TAG, TYPE_LIST_TAG, TYPE_PARAMETER_BOUNDS_TAG,
    TYPE_PARAMETERS_TAG, TYPE_TAG,
};

const MAX_CACHED_NODE_PAYLOAD_BYTES: usize = 128 * 1024;
const MAX_CACHED_NODE_PAYLOAD_ROWS: usize = 512;
const MAX_CACHED_NODE_PAYLOAD_ROW_BYTES: usize = 4096;

// Local projection vocabulary. The legacy planner's model is private to the
// NXFI transaction, so this generic reader planner keeps the same role grammar
// while resolving every terminal through `SemanticReader`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedPlanNode {
    Type(crate::ir::TypeId),
    TypeList(crate::ir::TypeListId),
    TupleElements(crate::ir::TupleElementListId),
    ObjectMembers(crate::ir::ObjectMemberListId),
    TemplateParts(crate::ir::TemplatePartListId),
    AtomList(crate::ir::AtomListId),
    TypeParameters(crate::ir::TypeParameterListId),
    TypeParameterBounds(crate::ir::TypeParameterBoundListId),
    FreePredicates(crate::ir::FreePredicateListId),
}
impl TypedPlanNode {
    const fn raw(self) -> u32 {
        match self {
            Self::Type(v) => v.raw,
            Self::TypeList(v) => v.raw,
            Self::TupleElements(v) => v.raw,
            Self::ObjectMembers(v) => v.raw,
            Self::TemplateParts(v) => v.raw,
            Self::AtomList(v) => v.raw,
            Self::TypeParameters(v) => v.raw,
            Self::TypeParameterBounds(v) => v.raw,
            Self::FreePredicates(v) => v.raw,
        }
    }
    const fn domain_code(self) -> u8 {
        match self {
            Self::Type(_) => 0,
            Self::TypeList(_) => 1,
            Self::TupleElements(_) => 2,
            Self::ObjectMembers(_) => 3,
            Self::TemplateParts(_) => 4,
            Self::AtomList(_) => 5,
            Self::TypeParameters(_) => 6,
            Self::TypeParameterBounds(_) => 7,
            Self::FreePredicates(_) => 8,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedEdgeRole {
    TypeTag,
    TypeField(u8),
    ListElement(u32),
    TupleLabel(u32),
    TupleType(u32),
    TupleKind(u32),
    ObjectKind(u32),
    ObjectKey(u32),
    ObjectType(u32),
    ObjectOptional(u32),
    ObjectReadonly(u32),
    ObjectParameter(u32),
    ObjectValue(u32),
    TemplatePart(u32),
    ParameterName(u32),
    ParameterBound(u32),
    ParameterDefault(u32),
    ParameterVariance(u32),
    ParameterKind(u32),
    ParameterRequirement(u32),
    ParameterConstructor(u32),
    ParameterAllowsRefLike(u32),
    BoundKind(u32),
    BoundValue(u32),
    PredicateSubject(u32),
    PredicateBound(u32),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedPlanTarget {
    Node(TypedPlanNode),
    Atom(crate::ir::AtomId),
    Entity(crate::ir::EntityId),
    External(crate::ir::ExternalId),
    Scalar(u64),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TypedPlanEdge {
    role: TypedEdgeRole,
    target: TypedPlanTarget,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedPlanFault {
    AnonymousCycle,
    InvalidProjection,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedPlanError {
    Typed(TypedPlanFault),
}
impl From<TypedPlanFault> for TypedPlanError {
    fn from(value: TypedPlanFault) -> Self {
        Self::Typed(value)
    }
}

/// Semantic scope represented by this family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypesClosureSemantics {
    /// Only typed/list/atom rows reachable from mandatory reader roots are
    /// persisted. This deliberately differs from exact legacy pool census.
    NormalizedReachable,
}

/// Reader-local handle retained while SPIR sorts stable keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypesRowHandle {
    /// One declaration's optional semantic-type association.
    EntityRoot(DeclarationIdentity),
    /// One structurally keyed typed or typed-list node.
    Node { domain: u8, raw: u32 },
    /// One exact byte atom reached from a typed or extension row.
    Atom(crate::ir::AtomId),
    /// One exact external endpoint in this image.
    External(crate::ir::ExternalId),
}

/// Reusable, iterative typed graph fingerprints and reachability plan.
///
/// Persisted keys and references never contain image-local pool ordinals.
pub struct TypedRecordPlan {
    nodes: Vec<TypedPlanNode>,
    edges: Vec<TypedPlanEdge>,
    edge_offsets: Vec<usize>,
    node_positions: Vec<((u8, u32), usize)>,
    node_keys: Vec<[u8; 32]>,
    cached_node_payloads: Vec<CachedNodePayload>,
    node_payload_slab: Vec<u8>,
    key_projection_payload_bytes: u64,
    node_payload_reencode_rows: usize,
    node_payload_reencode_bytes: u64,
    roots: BTreeMap<DeclarationIdentity, Option<TypeId>>,
    atom_ids: Vec<([u8; 32], crate::ir::AtomId)>,
    atom_keys_by_raw: Vec<(u32, [u8; 32])>,
    external_ids: Vec<([u8; 32], crate::ir::ExternalId)>,
    external_keys_by_raw: Vec<(u32, [u8; 32])>,
}

#[derive(Clone, Copy)]
struct CachedNodePayload {
    slot: usize,
    start: u32,
    len: u32,
}

impl TypedRecordPlan {
    /// Declares that this plan uses V2 normalized reachable semantics.
    #[must_use]
    pub const fn semantics(&self) -> TypesClosureSemantics {
        TypesClosureSemantics::NormalizedReachable
    }

    /// Number of reachable structural type/list nodes in this plan.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of distinct exact atom rows in this plan.
    #[must_use]
    pub fn atom_count(&self) -> usize {
        self.atom_ids.len()
    }

    /// Number of distinct external-target rows in this plan.
    #[must_use]
    pub fn external_target_count(&self) -> usize {
        self.external_ids.len()
    }

    /// Number of payload bytes projected once to materialize structural keys.
    #[must_use]
    pub const fn key_projection_payload_bytes(&self) -> u64 {
        self.key_projection_payload_bytes
    }

    /// Number of node rows that canonical streaming will project a second time.
    #[must_use]
    pub const fn node_payload_reencode_rows(&self) -> usize {
        self.node_payload_reencode_rows
    }

    /// Payload bytes projected again while streaming uncached node rows.
    #[must_use]
    pub const fn node_payload_reencode_bytes(&self) -> u64 {
        self.node_payload_reencode_bytes
    }

    /// Number of bounded cached structural payloads reused by row emission.
    #[must_use]
    pub fn cached_node_payload_count(&self) -> usize {
        self.cached_node_payloads.len()
    }

    /// Number of payload bytes retained in the bounded emission cache.
    #[must_use]
    pub fn cached_node_payload_bytes(&self) -> usize {
        self.node_payload_slab.len()
    }

    fn node_key(&self, node: TypedPlanNode) -> Result<[u8; 32], SemanticPlaneRecordError> {
        let slot = self.node_slot(node)?;
        self.node_keys
            .get(slot)
            .copied()
            .ok_or(SemanticPlaneRecordError::ReaderReference)
    }
    fn node_slot(&self, node: TypedPlanNode) -> Result<usize, SemanticPlaneRecordError> {
        self.node_positions
            .binary_search_by_key(&node_key(node), |(key, _)| *key)
            .ok()
            .and_then(|index| self.node_positions.get(index).map(|(_, slot)| *slot))
            .ok_or(SemanticPlaneRecordError::ReaderReference)
    }
    fn cached_node_payload(&self, slot: usize) -> Option<&[u8]> {
        let entry = self
            .cached_node_payloads
            .binary_search_by_key(&slot, |entry| entry.slot)
            .ok()
            .and_then(|index| self.cached_node_payloads.get(index))?;
        let start = usize::try_from(entry.start).ok()?;
        let len = usize::try_from(entry.len).ok()?;
        self.node_payload_slab.get(start..start.checked_add(len)?)
    }
    fn atom_key(&self, atom: crate::ir::AtomId) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.atom_keys_by_raw
            .binary_search_by_key(&atom.raw, |(raw, _)| *raw)
            .ok()
            .and_then(|index| self.atom_keys_by_raw.get(index).map(|(_, key)| *key))
            .ok_or(SemanticPlaneRecordError::ReaderReference)
    }
    fn external_key(
        &self,
        external: crate::ir::ExternalId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.external_keys_by_raw
            .binary_search_by_key(&external.raw, |(raw, _)| *raw)
            .ok()
            .and_then(|index| self.external_keys_by_raw.get(index).map(|(_, key)| *key))
            .ok_or(SemanticPlaneRecordError::ReaderReference)
    }

    /// Stable key for a reachable semantic type row.
    pub fn type_key(&self, id: crate::ir::TypeId) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_key(TypedPlanNode::Type(id))
    }

    /// Stable key for a reachable ordered type list.
    pub fn type_list_key(
        &self,
        id: crate::ir::TypeListId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_key(TypedPlanNode::TypeList(id))
    }

    /// Stable key for a reachable tuple or callable parameter/result list.
    pub fn tuple_elements_key(
        &self,
        id: crate::ir::TupleElementListId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_key(TypedPlanNode::TupleElements(id))
    }

    /// Stable key for a reachable ordered object-member list.
    pub fn object_members_key(
        &self,
        id: crate::ir::ObjectMemberListId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_key(TypedPlanNode::ObjectMembers(id))
    }

    /// Stable key for a reachable ordered template-parts list.
    pub fn template_parts_key(
        &self,
        id: crate::ir::TemplatePartListId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_key(TypedPlanNode::TemplateParts(id))
    }

    /// Stable key for a reachable arbitrary-byte atom list.
    pub fn atom_list_key(
        &self,
        id: crate::ir::AtomListId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_key(TypedPlanNode::AtomList(id))
    }

    /// Stable key for a reachable generic parameter list.
    pub fn type_parameters_key(
        &self,
        id: crate::ir::TypeParameterListId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_key(TypedPlanNode::TypeParameters(id))
    }

    /// Stable key for a reachable generic parameter bound list.
    pub fn type_parameter_bounds_key(
        &self,
        id: crate::ir::TypeParameterBoundListId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_key(TypedPlanNode::TypeParameterBounds(id))
    }

    /// Stable key for a reachable Rust free-predicate list.
    pub fn free_predicates_key(
        &self,
        id: crate::ir::FreePredicateListId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_key(TypedPlanNode::FreePredicates(id))
    }

    /// Stable key for one external target row used by another family.
    pub fn external_target_key(
        &self,
        id: crate::ir::ExternalId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.external_key(id)
    }

    /// Optional structural type root for one exact declaration identity.
    pub fn entity_type_root(
        &self,
        identity: DeclarationIdentity,
    ) -> Result<Option<[u8; 32]>, SemanticPlaneRecordError> {
        match self.roots.get(&identity).copied() {
            Some(Some(id)) => self.type_key(id).map(Some),
            Some(None) => Ok(None),
            None => Err(SemanticPlaneRecordError::ReaderReference),
        }
    }
}

/// Canonical Types-plane row family for SPIR V1 framing.
#[derive(Clone, Copy, Debug, Default)]
pub struct TypesRows;

impl CanonicalPlaneRowEncoder for TypesRows {
    type Handle = TypesRowHandle;
    type Plan = TypedRecordPlan;

    fn kind(&self) -> SemanticPlaneKind {
        SemanticPlaneKind::Ir(SemanticIrPlane::Types)
    }
    fn build_plan<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
    ) -> Result<Self::Plan, SemanticPlaneRecordError> {
        TypedRecordPlan::build(reader)
    }
    fn collect_keys<Reader: SemanticReader + ?Sized>(
        &self,
        _reader: &Reader,
        plan: &Self::Plan,
        sink: &mut CanonicalSemanticPlaneKeySink<Self::Handle>,
    ) -> Result<(), SemanticPlaneRecordError> {
        for identity in plan.roots.keys().copied() {
            sink.push(
                root_key(self.kind(), identity),
                TypesRowHandle::EntityRoot(identity),
            )?;
        }
        for node in plan.nodes.iter().copied() {
            let (domain, raw) = node_key(node);
            sink.push(plan.node_key(node)?, TypesRowHandle::Node { domain, raw })?;
        }
        for (key, atom) in &plan.atom_ids {
            sink.push(*key, TypesRowHandle::Atom(*atom))?;
        }
        for (key, external) in &plan.external_ids {
            sink.push(*key, TypesRowHandle::External(*external))?;
        }
        Ok(())
    }
    fn encode_row<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        plan: &Self::Plan,
        handle: Self::Handle,
        payload: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        match handle {
            TypesRowHandle::EntityRoot(identity) => {
                encode_entity_type_root(plan, identity, payload)?;
                Ok(ROOT_TAG)
            }
            TypesRowHandle::Node { domain, raw } => {
                let node = node_from_key(domain, raw)?;
                let slot = plan.node_slot(node)?;
                if let Some(cached) = plan.cached_node_payload(slot) {
                    payload.extend_from_slice(cached);
                } else {
                    encode_node_payload(reader, plan, node, payload)?;
                }
                Ok(node_tag(node))
            }
            TypesRowHandle::Atom(atom) => {
                put_bytes(
                    payload,
                    reader
                        .atom(atom)
                        .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                )?;
                Ok(ATOM_TAG)
            }
            TypesRowHandle::External(external) => {
                encode_external_target(reader, external, payload)?;
                Ok(EXTERNAL_TARGET_TAG)
            }
        }
    }
}

impl TypedRecordPlan {
    /// Builds one checked reachable closure without recursive process-stack descent.
    pub fn build<Reader: SemanticReader + ?Sized>(
        reader: &Reader,
    ) -> Result<Self, SemanticPlaneRecordError> {
        let mut roots = BTreeMap::new();
        let mut root_nodes = Vec::new();
        for entity in reader.canonical_entities() {
            let identity = entity.version.identity();
            if roots.insert(identity, entity.semantic_type).is_some() {
                return Err(SemanticPlaneRecordError::StableKeyCollision);
            }
            if let Some(ty) = entity.semantic_type {
                root_nodes.push(TypedPlanNode::Type(ty));
            }
        }
        // Extension rows belong to LanguageExtensions; this scan seeds all their typed references.
        for (owner, facts) in reader.typescript_extensions() {
            require_extension_owner(reader, &roots, owner)?;
            root_nodes.push(TypedPlanNode::TypeParameters(facts.type_parameters));
            if let Some(ty) = facts.declared {
                root_nodes.push(TypedPlanNode::Type(ty));
            }
            if let Some(ty) = facts.observed {
                root_nodes.push(TypedPlanNode::Type(ty));
            }
        }
        for (owner, facts) in reader.csharp_extensions() {
            require_extension_owner(reader, &roots, owner)?;
            root_nodes.push(TypedPlanNode::TypeParameters(facts.constraints));
            root_nodes.push(TypedPlanNode::AtomList(facts.attributes));
        }
        for (owner, facts) in reader.go_extensions() {
            require_extension_owner(reader, &roots, owner)?;
            root_nodes.push(TypedPlanNode::TypeList(facts.signature.parameters));
            root_nodes.push(TypedPlanNode::TypeList(facts.signature.results));
            root_nodes.push(TypedPlanNode::TypeParameters(facts.type_parameters));
            root_nodes.push(TypedPlanNode::AtomList(facts.build_constraints));
            root_nodes.push(TypedPlanNode::AtomList(facts.constant_value));
        }
        for (owner, facts) in reader.rust_extensions() {
            require_extension_owner(reader, &roots, owner)?;
            root_nodes.push(TypedPlanNode::AtomList(facts.lifetimes));
            root_nodes.push(TypedPlanNode::TypeParameters(facts.where_clauses));
            root_nodes.push(TypedPlanNode::AtomList(facts.macros));
            root_nodes.push(TypedPlanNode::AtomList(facts.const_defaults));
            root_nodes.push(TypedPlanNode::FreePredicates(facts.free_predicates));
        }
        for (owner, facts) in reader.python_extensions() {
            require_extension_owner(reader, &roots, owner)?;
            root_nodes.push(TypedPlanNode::AtomList(facts.decorators));
        }
        for (owner, facts) in reader.java_extensions() {
            require_extension_owner(reader, &roots, owner)?;
            root_nodes.push(TypedPlanNode::TypeList(facts.throws));
            root_nodes.push(TypedPlanNode::AtomList(facts.annotations));
        }
        for (owner, facts) in reader.clang_extensions() {
            require_extension_owner(reader, &roots, owner)?;
            root_nodes.push(TypedPlanNode::TypeParameters(facts.templates));
            root_nodes.push(TypedPlanNode::AtomList(facts.includes));
        }
        // External endpoints have a public complete census and are also roots for other families.
        let external_roots = reader
            .canonical_externals()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let (nodes, edges, edge_offsets, node_positions, mut atoms, mut externals) =
            discover_typed_graph(reader, root_nodes)?;
        externals.extend(external_roots);
        for external in externals.iter().copied() {
            collect_external_atoms(reader, external, &mut atoms)?;
        }
        let mut plan = Self {
            nodes,
            edges,
            edge_offsets,
            node_positions,
            node_keys: Vec::new(),
            cached_node_payloads: Vec::new(),
            node_payload_slab: Vec::new(),
            key_projection_payload_bytes: 0,
            node_payload_reencode_rows: 0,
            node_payload_reencode_bytes: 0,
            roots,
            atom_ids: Vec::new(),
            atom_keys_by_raw: Vec::new(),
            external_ids: Vec::new(),
            external_keys_by_raw: Vec::new(),
        };
        plan.materialize_atoms(reader, atoms)?;
        plan.materialize_externals(reader, externals)?;
        plan.materialize_node_keys(reader)?;
        Ok(plan)
    }

    fn materialize_atoms(
        &mut self,
        reader: &(impl SemanticReader + ?Sized),
        mut atoms: Vec<crate::ir::AtomId>,
    ) -> Result<(), SemanticPlaneRecordError> {
        atoms.sort_unstable_by_key(|atom| atom.raw);
        atoms.dedup_by_key(|atom| atom.raw);
        let mut keyed_atoms = Vec::new();
        keyed_atoms
            .try_reserve_exact(atoms.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.atom_keys_by_raw
            .try_reserve_exact(atoms.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        for atom in atoms {
            let bytes = reader
                .atom(atom)
                .ok_or(SemanticPlaneRecordError::ReaderReference)?;
            let key = atom_key(bytes)?;
            keyed_atoms.push((key, atom));
            self.atom_keys_by_raw.push((atom.raw, key));
        }
        keyed_atoms.sort_unstable_by_key(|(key, _)| *key);
        let mut cursor = 0;
        while cursor < keyed_atoms.len() {
            let (key, representative) = keyed_atoms[cursor];
            let representative_bytes = reader
                .atom(representative)
                .ok_or(SemanticPlaneRecordError::ReaderReference)?;
            let mut end = cursor + 1;
            while end < keyed_atoms.len() && keyed_atoms[end].0 == key {
                if reader
                    .atom(keyed_atoms[end].1)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?
                    != representative_bytes
                {
                    return Err(SemanticPlaneRecordError::StableKeyCollision);
                }
                end += 1;
            }
            self.atom_ids.push((key, representative));
            cursor = end;
        }
        Ok(())
    }

    fn materialize_externals(
        &mut self,
        reader: &(impl SemanticReader + ?Sized),
        mut externals: Vec<crate::ir::ExternalId>,
    ) -> Result<(), SemanticPlaneRecordError> {
        externals.sort_unstable_by_key(|external| external.raw);
        externals.dedup_by_key(|external| external.raw);
        let mut keyed_externals = Vec::new();
        keyed_externals
            .try_reserve_exact(externals.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.external_keys_by_raw
            .try_reserve_exact(externals.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        for external in externals {
            let identity = ExternalTargetIdentity::capture(reader, external)
                .map_err(|_| SemanticPlaneRecordError::ReaderReference)?;
            let key = *identity.as_bytes();
            keyed_externals.push((key, external));
            self.external_keys_by_raw.push((external.raw, key));
        }
        keyed_externals.sort_unstable_by_key(|(key, _)| *key);
        let mut cursor = 0;
        while cursor < keyed_externals.len() {
            let (key, representative) = keyed_externals[cursor];
            let mut representative_payload = Vec::new();
            encode_external_target(reader, representative, &mut representative_payload)?;
            let mut end = cursor + 1;
            while end < keyed_externals.len() && keyed_externals[end].0 == key {
                let mut duplicate_payload = Vec::new();
                encode_external_target(reader, keyed_externals[end].1, &mut duplicate_payload)?;
                if duplicate_payload != representative_payload {
                    return Err(SemanticPlaneRecordError::StableKeyCollision);
                }
                end += 1;
            }
            self.external_ids.push((key, representative));
            cursor = end;
        }
        Ok(())
    }

    fn materialize_node_keys(
        &mut self,
        reader: &(impl SemanticReader + ?Sized),
    ) -> Result<(), SemanticPlaneRecordError> {
        let mut states = Vec::new();
        states
            .try_reserve_exact(self.nodes.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        states.resize(self.nodes.len(), 0_u8);
        let mut stack: Vec<(usize, usize)> = Vec::new();
        self.node_keys
            .try_reserve_exact(self.nodes.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.node_keys.resize(self.nodes.len(), [0; 32]);
        let mut key_order = Vec::new();
        key_order
            .try_reserve_exact(self.nodes.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.cached_node_payloads
            .try_reserve_exact(self.nodes.len().min(MAX_CACHED_NODE_PAYLOAD_ROWS))
            .map_err(SemanticPlaneRecordError::Allocation)?;
        let mut payload = Vec::new();
        for start in 0..self.nodes.len() {
            if states[start] != 0 {
                continue;
            }
            states[start] = 1;
            stack.push((start, self.edge_offsets[start]));
            while let Some((slot, cursor)) = stack.last().copied() {
                let end = self.edge_offsets[slot + 1];
                if cursor < self.edge_offsets[slot] || cursor > end {
                    return Err(SemanticPlaneRecordError::RowGrammar);
                }
                if cursor == end {
                    let node = self.nodes[slot];
                    payload.clear();
                    encode_node_payload(reader, self, node, &mut payload)?;
                    let key = typed_row_key(node_tag(node), &payload);
                    self.key_projection_payload_bytes = self
                        .key_projection_payload_bytes
                        .checked_add(
                            u64::try_from(payload.len())
                                .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
                        )
                        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
                    let cached = payload.len() <= MAX_CACHED_NODE_PAYLOAD_ROW_BYTES
                        && self.cached_node_payloads.len() < MAX_CACHED_NODE_PAYLOAD_ROWS
                        && self
                            .node_payload_slab
                            .len()
                            .checked_add(payload.len())
                            .is_some_and(|total| total <= MAX_CACHED_NODE_PAYLOAD_BYTES);
                    if cached {
                        self.node_payload_slab
                            .try_reserve_exact(payload.len())
                            .map_err(SemanticPlaneRecordError::Allocation)?;
                        self.cached_node_payloads
                            .try_reserve(1)
                            .map_err(SemanticPlaneRecordError::Allocation)?;
                        let start = u32::try_from(self.node_payload_slab.len())
                            .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
                        let len = u32::try_from(payload.len())
                            .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
                        self.node_payload_slab.extend_from_slice(&payload);
                        self.cached_node_payloads
                            .push(CachedNodePayload { slot, start, len });
                    } else {
                        self.node_payload_reencode_rows = self
                            .node_payload_reencode_rows
                            .checked_add(1)
                            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
                        self.node_payload_reencode_bytes = self
                            .node_payload_reencode_bytes
                            .checked_add(
                                u64::try_from(payload.len())
                                    .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
                            )
                            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
                    }
                    self.node_keys[slot] = key;
                    key_order.push((key, slot));
                    states[slot] = 2;
                    stack.pop();
                    continue;
                }
                let next = cursor
                    .checked_add(1)
                    .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
                if let Some(active) = stack.last_mut() {
                    active.1 = next;
                }
                let edge = *self
                    .edges
                    .get(cursor)
                    .ok_or(SemanticPlaneRecordError::RowGrammar)?;
                let TypedPlanTarget::Node(target) = edge.target else {
                    continue;
                };
                let target_slot = self.node_slot(target)?;
                match states[target_slot] {
                    0 => {
                        states[target_slot] = 1;
                        stack.push((target_slot, self.edge_offsets[target_slot]));
                    }
                    1 => return Err(SemanticPlaneRecordError::TypedDependencyCycle),
                    2 => {}
                    _ => return Err(SemanticPlaneRecordError::TypedDependencyCycle),
                }
            }
        }
        key_order.sort_unstable_by_key(|(key, _)| *key);
        if key_order.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(SemanticPlaneRecordError::StableKeyCollision);
        }
        self.cached_node_payloads
            .sort_unstable_by_key(|entry| entry.slot);
        Ok(())
    }
}

fn require_extension_owner<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    roots: &BTreeMap<DeclarationIdentity, Option<TypeId>>,
    owner: crate::ir::EntityId,
) -> Result<(), SemanticPlaneRecordError> {
    let entity = reader
        .entity(owner)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    if !roots.contains_key(&entity.version.identity()) {
        return Err(SemanticPlaneRecordError::ReaderReference);
    }
    Ok(())
}

fn discover_typed_graph<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    roots: Vec<TypedPlanNode>,
) -> Result<
    (
        Vec<TypedPlanNode>,
        Vec<TypedPlanEdge>,
        Vec<usize>,
        Vec<((u8, u32), usize)>,
        Vec<crate::ir::AtomId>,
        Vec<crate::ir::ExternalId>,
    ),
    SemanticPlaneRecordError,
> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut offsets = Vec::new();
    let mut positions = BTreeMap::new();
    let mut pending = roots;
    let mut atoms = Vec::new();
    let mut externals = Vec::new();
    while let Some(node) = pending.pop() {
        let key = node_key(node);
        if positions.contains_key(&key) {
            continue;
        }
        let slot = nodes.len();
        positions.insert(key, slot);
        nodes.push(node);
        offsets.push(edges.len());
        for_each_reader_edge(reader, node, &mut |edge| {
            match edge.target {
                TypedPlanTarget::Node(child) => pending.push(child),
                TypedPlanTarget::Atom(atom) => atoms.push(atom),
                TypedPlanTarget::External(external) => externals.push(external),
                TypedPlanTarget::Entity(entity) => {
                    if reader.entity(entity).is_none() {
                        return Err(TypedPlanError::Typed(TypedPlanFault::InvalidProjection));
                    }
                }
                TypedPlanTarget::Scalar(_) => {}
            }
            edges.push(edge);
            Ok(())
        })
        .map_err(map_typed_plan_error)?;
    }
    offsets.push(edges.len());
    let mut sorted_positions = Vec::new();
    sorted_positions
        .try_reserve_exact(positions.len())
        .map_err(SemanticPlaneRecordError::Allocation)?;
    sorted_positions.extend(positions);
    Ok((nodes, edges, offsets, sorted_positions, atoms, externals))
}

fn map_typed_plan_error(error: TypedPlanError) -> SemanticPlaneRecordError {
    match error {
        TypedPlanError::Typed(TypedPlanFault::AnonymousCycle) => {
            SemanticPlaneRecordError::TypedDependencyCycle
        }
        _ => SemanticPlaneRecordError::ReaderReference,
    }
}
fn node_key(node: TypedPlanNode) -> (u8, u32) {
    (node.domain_code(), node.raw())
}
fn node_from_key(domain: u8, raw: u32) -> Result<TypedPlanNode, SemanticPlaneRecordError> {
    Ok(match domain {
        0 => TypedPlanNode::Type(crate::ir::TypeId::new(raw)),
        1 => TypedPlanNode::TypeList(crate::ir::TypeListId::new(raw)),
        2 => TypedPlanNode::TupleElements(crate::ir::TupleElementListId::new(raw)),
        3 => TypedPlanNode::ObjectMembers(crate::ir::ObjectMemberListId::new(raw)),
        4 => TypedPlanNode::TemplateParts(crate::ir::TemplatePartListId::new(raw)),
        5 => TypedPlanNode::AtomList(crate::ir::AtomListId::new(raw)),
        6 => TypedPlanNode::TypeParameters(crate::ir::TypeParameterListId::new(raw)),
        7 => TypedPlanNode::TypeParameterBounds(crate::ir::TypeParameterBoundListId::new(raw)),
        8 => TypedPlanNode::FreePredicates(crate::ir::FreePredicateListId::new(raw)),
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    })
}
fn node_tag(node: TypedPlanNode) -> u8 {
    match node {
        TypedPlanNode::Type(_) => TYPE_TAG,
        TypedPlanNode::TypeList(_) => TYPE_LIST_TAG,
        TypedPlanNode::TupleElements(_) => TUPLE_ELEMENTS_TAG,
        TypedPlanNode::ObjectMembers(_) => OBJECT_MEMBERS_TAG,
        TypedPlanNode::TemplateParts(_) => TEMPLATE_PARTS_TAG,
        TypedPlanNode::AtomList(_) => ATOM_LIST_TAG,
        TypedPlanNode::TypeParameters(_) => TYPE_PARAMETERS_TAG,
        TypedPlanNode::TypeParameterBounds(_) => TYPE_PARAMETER_BOUNDS_TAG,
        TypedPlanNode::FreePredicates(_) => FREE_PREDICATES_TAG,
    }
}
fn root_key(kind: SemanticPlaneKind, identity: DeclarationIdentity) -> [u8; 32] {
    super::declaration_plane_key(kind, identity)
}
fn encode_entity_type_root(
    plan: &TypedRecordPlan,
    identity: DeclarationIdentity,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let ty = plan
        .roots
        .get(&identity)
        .copied()
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    out.extend_from_slice(&identity_bytes(identity));
    if let Some(ty) = ty {
        out.push(1);
        out.extend_from_slice(&plan.node_key(TypedPlanNode::Type(ty))?);
    } else {
        out.push(0);
        out.extend_from_slice(&[0; 32]);
    }
    Ok(())
}
fn encode_node_payload<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    plan: &TypedRecordPlan,
    node: TypedPlanNode,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let slot = plan.node_slot(node)?;
    let start = *plan
        .edge_offsets
        .get(slot)
        .ok_or(SemanticPlaneRecordError::RowGrammar)?;
    let end = *plan
        .edge_offsets
        .get(slot + 1)
        .ok_or(SemanticPlaneRecordError::RowGrammar)?;
    let edges = plan
        .edges
        .get(start..end)
        .ok_or(SemanticPlaneRecordError::RowGrammar)?;
    out.extend_from_slice(
        &u32::try_from(edges.len())
            .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
            .to_be_bytes(),
    );
    for edge in edges {
        let (role, index) = wire_role(edge.role);
        out.push(role);
        out.extend_from_slice(&index.to_be_bytes());
        match edge.target {
            TypedPlanTarget::Node(target) => {
                out.push(0);
                out.extend_from_slice(&plan.node_key(target)?);
            }
            TypedPlanTarget::Atom(atom) => {
                out.push(1);
                out.extend_from_slice(&plan.atom_key(atom)?);
            }
            TypedPlanTarget::Entity(entity) => {
                let value = reader
                    .entity(entity)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?;
                out.push(2);
                out.extend_from_slice(&identity_bytes(value.version.identity()));
            }
            TypedPlanTarget::External(external) => {
                out.push(3);
                out.extend_from_slice(&plan.external_key(external)?);
            }
            TypedPlanTarget::Scalar(value) => {
                out.push(4);
                out.extend_from_slice(&[0; 24]);
                out.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
    Ok(())
}
fn encode_external_target<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    external: crate::ir::ExternalId,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let target = reader
        .external(external)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    match target {
        crate::ir::ExternalTarget::Stable { target } => {
            out.push(0);
            out.extend_from_slice(target.fragment.as_ref());
            out.extend_from_slice(&identity_bytes(target.declaration));
        }
        crate::ir::ExternalTarget::Foreign(target) => {
            out.push(1);
            out.extend_from_slice(target.identity.foreign.as_bytes());
            match target.identity.variant {
                crate::ir::VariantAvailability::Known(value) => {
                    out.push(1);
                    out.extend_from_slice(value.as_bytes());
                }
                crate::ir::VariantAvailability::Unavailable => out.push(0),
            }
            match target.origin {
                crate::ir::ForeignTargetOrigin::Package { ecosystem, package } => {
                    out.push(0);
                    encode_reader_atom(reader, ecosystem, out)?;
                    encode_reader_atom(reader, package, out)?;
                }
                crate::ir::ForeignTargetOrigin::Namespace {
                    ecosystem,
                    namespace,
                } => {
                    out.push(1);
                    encode_reader_atom(reader, ecosystem, out)?;
                    encode_reader_atom(reader, namespace, out)?;
                }
                crate::ir::ForeignTargetOrigin::Universe { ecosystem } => {
                    out.push(2);
                    encode_reader_atom(reader, ecosystem, out)?;
                }
                crate::ir::ForeignTargetOrigin::Unspecified { ecosystem } => {
                    out.push(3);
                    encode_reader_atom(reader, ecosystem, out)?;
                }
            }
            encode_reader_atom(reader, target.path, out)?;
            encode_reader_atom(reader, target.display, out)?;
            match target.kind {
                Some(kind) => {
                    out.push(1);
                    out.extend_from_slice(&u16::from(kind).to_be_bytes());
                }
                None => out.push(0),
            }
        }
        crate::ir::ExternalTarget::FragmentEntity { target, display } => {
            out.push(2);
            out.extend_from_slice(target.fragment.as_ref());
            out.extend_from_slice(&target.ordinal.to_be_bytes());
            encode_reader_atom(reader, display, out)?;
        }
    }
    Ok(())
}

fn collect_external_atoms<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    external: crate::ir::ExternalId,
    atoms: &mut Vec<crate::ir::AtomId>,
) -> Result<(), SemanticPlaneRecordError> {
    let target = reader
        .external(external)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    match target {
        crate::ir::ExternalTarget::Stable { .. } => {}
        crate::ir::ExternalTarget::Foreign(target) => {
            match target.origin {
                crate::ir::ForeignTargetOrigin::Package { ecosystem, package } => {
                    atoms.push(ecosystem);
                    atoms.push(package);
                }
                crate::ir::ForeignTargetOrigin::Namespace {
                    ecosystem,
                    namespace,
                } => {
                    atoms.push(ecosystem);
                    atoms.push(namespace);
                }
                crate::ir::ForeignTargetOrigin::Universe { ecosystem }
                | crate::ir::ForeignTargetOrigin::Unspecified { ecosystem } => {
                    atoms.push(ecosystem);
                }
            }
            atoms.push(target.path);
            atoms.push(target.display);
        }
        crate::ir::ExternalTarget::FragmentEntity { display, .. } => atoms.push(display),
    }
    Ok(())
}
fn encode_reader_atom<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    atom: crate::ir::AtomId,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    put_bytes(
        out,
        reader
            .atom(atom)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?,
    )
}
fn wire_role(role: TypedEdgeRole) -> (u8, u32) {
    match role {
        TypedEdgeRole::TypeTag => (0, 0),
        TypedEdgeRole::TypeField(i) => (1, u32::from(i)),
        TypedEdgeRole::ListElement(i) => (2, i),
        TypedEdgeRole::TupleLabel(i) => (3, i),
        TypedEdgeRole::TupleType(i) => (4, i),
        TypedEdgeRole::TupleKind(i) => (5, i),
        TypedEdgeRole::ObjectKind(i) => (6, i),
        TypedEdgeRole::ObjectKey(i) => (7, i),
        TypedEdgeRole::ObjectType(i) => (8, i),
        TypedEdgeRole::ObjectOptional(i) => (9, i),
        TypedEdgeRole::ObjectReadonly(i) => (10, i),
        TypedEdgeRole::ObjectParameter(i) => (11, i),
        TypedEdgeRole::ObjectValue(i) => (12, i),
        TypedEdgeRole::TemplatePart(i) => (13, i),
        TypedEdgeRole::ParameterName(i) => (14, i),
        TypedEdgeRole::ParameterBound(i) => (15, i),
        TypedEdgeRole::ParameterDefault(i) => (16, i),
        TypedEdgeRole::ParameterVariance(i) => (17, i),
        TypedEdgeRole::ParameterKind(i) => (18, i),
        TypedEdgeRole::ParameterRequirement(i) => (19, i),
        TypedEdgeRole::ParameterConstructor(i) => (20, i),
        TypedEdgeRole::ParameterAllowsRefLike(i) => (21, i),
        TypedEdgeRole::BoundKind(i) => (22, i),
        TypedEdgeRole::BoundValue(i) => (23, i),
        TypedEdgeRole::PredicateSubject(i) => (24, i),
        TypedEdgeRole::PredicateBound(i) => (25, i),
    }
}

fn for_each_reader_edge<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    node: TypedPlanNode,
    sink: &mut impl FnMut(TypedPlanEdge) -> Result<(), TypedPlanError>,
) -> Result<(), TypedPlanError> {
    macro_rules! emit {
        ($role:expr, $target:expr $(,)?) => {
            sink(TypedPlanEdge {
                role: $role,
                target: $target,
            })?
        };
    }
    macro_rules! type_tag {
        ($tag:expr) => {
            emit!(TypedEdgeRole::TypeTag, TypedPlanTarget::Scalar($tag))
        };
    }
    macro_rules! type_child {
        ($field:expr, $id:expr) => {
            emit!(
                TypedEdgeRole::TypeField($field),
                TypedPlanTarget::Node(TypedPlanNode::Type($id)),
            )
        };
    }
    macro_rules! type_list {
        ($field:expr, $id:expr) => {
            emit!(
                TypedEdgeRole::TypeField($field),
                TypedPlanTarget::Node(TypedPlanNode::TypeList($id)),
            )
        };
    }
    macro_rules! scalar {
        ($field:expr, $value:expr) => {
            emit!(
                TypedEdgeRole::TypeField($field),
                TypedPlanTarget::Scalar($value)
            )
        };
    }

    match node {
        TypedPlanNode::Type(id) => {
            let expression = reader.ty(id).ok_or(missing_node(node)?)?;
            match expression {
                crate::ir::TypeExpr::Concrete(value) => match value {
                    crate::ir::ConcreteType::Builtin(value) => {
                        type_tag!(1);
                        scalar!(0, builtin_type(value));
                    }
                    crate::ir::ConcreteType::Literal(value) => {
                        type_tag!(2);
                        match value {
                            crate::ir::LiteralType::String(atom) => {
                                scalar!(0, 0);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(atom));
                            }
                            crate::ir::LiteralType::Number(atom) => {
                                scalar!(0, 1);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(atom));
                            }
                            crate::ir::LiteralType::BigInt(atom) => {
                                scalar!(0, 2);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(atom));
                            }
                            crate::ir::LiteralType::Boolean(value) => {
                                scalar!(0, 3);
                                scalar!(1, u64::from(value));
                            }
                            crate::ir::LiteralType::Null => scalar!(0, 4),
                            crate::ir::LiteralType::Undefined => scalar!(0, 5),
                        }
                    }
                    crate::ir::ConcreteType::Nominal(entity) => {
                        type_tag!(3);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::Entity(entity));
                    }
                    crate::ir::ConcreteType::External(external) => {
                        type_tag!(4);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::External(external)
                        );
                    }
                    crate::ir::ConcreteType::Parameter(atom) => {
                        type_tag!(5);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::Atom(atom));
                    }
                    crate::ir::ConcreteType::Applied {
                        constructor,
                        arguments,
                    } => {
                        type_tag!(6);
                        type_child!(0, constructor);
                        type_list!(1, arguments);
                    }
                    crate::ir::ConcreteType::Tuple(elements) => {
                        type_tag!(7);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::TupleElements(elements)),
                        );
                    }
                    crate::ir::ConcreteType::Object(members) => {
                        type_tag!(8);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::ObjectMembers(members)),
                        );
                    }
                    crate::ir::ConcreteType::Function {
                        parameters,
                        results,
                        abi,
                        variadic,
                        unsafe_,
                    } => {
                        type_tag!(9);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::TupleElements(parameters)),
                        );
                        emit!(
                            TypedEdgeRole::TypeField(1),
                            TypedPlanTarget::Node(TypedPlanNode::TupleElements(results)),
                        );
                        scalar!(2, u64::from(abi.is_some()));
                        if let Some(abi) = abi {
                            emit!(TypedEdgeRole::TypeField(3), TypedPlanTarget::Atom(abi));
                        }
                        scalar!(4, variadic_form(variadic));
                        scalar!(5, u64::from(unsafe_));
                    }
                    crate::ir::ConcreteType::Reference {
                        target,
                        mutability,
                        lifetime,
                    } => {
                        type_tag!(10);
                        type_child!(0, target);
                        scalar!(1, mutability_code(mutability));
                        scalar!(2, u64::from(lifetime.is_some()));
                        if let Some(lifetime) = lifetime {
                            emit!(TypedEdgeRole::TypeField(3), TypedPlanTarget::Atom(lifetime));
                        }
                    }
                    crate::ir::ConcreteType::CxxReference { target, category } => {
                        type_tag!(11);
                        type_child!(0, target);
                        scalar!(1, cxx_reference_category(category));
                    }
                    crate::ir::ConcreteType::CPointer { target } => {
                        type_tag!(12);
                        type_child!(0, target);
                    }
                    crate::ir::ConcreteType::CxxMemberPointer { owner, member } => {
                        type_tag!(13);
                        type_child!(0, owner);
                        type_child!(1, member);
                    }
                    crate::ir::ConcreteType::CQualified { target, qualifiers } => {
                        type_tag!(14);
                        type_child!(0, target);
                        scalar!(1, u64::from(u8::from(qualifiers)));
                    }
                    crate::ir::ConcreteType::CBlockPointer { target } => {
                        type_tag!(15);
                        type_child!(0, target);
                    }
                    crate::ir::ConcreteType::NativeCharacter { role, width } => {
                        type_tag!(16);
                        scalar!(0, native_character_role(role));
                        scalar!(1, u64::from(width.get()));
                    }
                    crate::ir::ConcreteType::Pointer { target, mutability } => {
                        type_tag!(17);
                        type_child!(0, target);
                        scalar!(1, mutability_code(mutability));
                    }
                    crate::ir::ConcreteType::Slice(target) => {
                        type_tag!(18);
                        type_child!(0, target);
                    }
                    crate::ir::ConcreteType::Array { element, shape } => {
                        type_tag!(19);
                        type_child!(0, element);
                        match shape {
                            crate::ir::ArrayShape::Sequence => scalar!(1, 0),
                            crate::ir::ArrayShape::Rectangular { rank } => {
                                scalar!(1, 1);
                                scalar!(2, u64::from(rank.get()));
                            }
                            crate::ir::ArrayShape::FixedValue { length } => {
                                scalar!(1, 2);
                                scalar!(2, length);
                            }
                            crate::ir::ArrayShape::ConstExpression(atom) => {
                                scalar!(1, 3);
                                emit!(TypedEdgeRole::TypeField(2), TypedPlanTarget::Atom(atom));
                            }
                            crate::ir::ArrayShape::Incomplete => scalar!(1, 4),
                        }
                    }
                    crate::ir::ConcreteType::Optional(target) => {
                        type_tag!(20);
                        type_child!(0, target);
                    }
                    crate::ir::ConcreteType::Union(items) => {
                        type_tag!(21);
                        type_list!(0, items);
                    }
                    crate::ir::ConcreteType::Intersection(items) => {
                        type_tag!(22);
                        type_list!(0, items);
                    }
                    crate::ir::ConcreteType::ImplTrait(items) => {
                        type_tag!(23);
                        type_list!(0, items);
                    }
                    crate::ir::ConcreteType::DynTrait(items) => {
                        type_tag!(24);
                        type_list!(0, items);
                    }
                    crate::ir::ConcreteType::Wildcard(bound) => {
                        type_tag!(25);
                        match bound {
                            crate::ir::WildcardBound::Unbounded => scalar!(0, 0),
                            crate::ir::WildcardBound::Extends(target) => {
                                scalar!(0, 1);
                                type_child!(1, target);
                            }
                            crate::ir::WildcardBound::Super(target) => {
                                scalar!(0, 2);
                                type_child!(1, target);
                            }
                        }
                    }
                    crate::ir::ConcreteType::Annotated { kind, target } => {
                        type_tag!(26);
                        scalar!(0, annotation_kind(kind));
                        type_child!(1, target);
                    }
                    crate::ir::ConcreteType::Inferred(spelling) => {
                        type_tag!(27);
                        scalar!(0, u64::from(spelling.is_some()));
                        if let Some(spelling) = spelling {
                            emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(spelling));
                        }
                    }
                    crate::ir::ConcreteType::QualifiedPath {
                        self_type,
                        trait_type,
                        segments,
                        spelling,
                    } => {
                        type_tag!(28);
                        type_child!(0, self_type);
                        scalar!(1, u64::from(trait_type.is_some()));
                        if let Some(trait_type) = trait_type {
                            type_child!(2, trait_type);
                        }
                        match segments {
                            crate::ir::QualifiedSegments::Captured(segments) => {
                                scalar!(3, 1);
                                emit!(
                                    TypedEdgeRole::TypeField(4),
                                    TypedPlanTarget::Node(TypedPlanNode::AtomList(segments)),
                                );
                            }
                            crate::ir::QualifiedSegments::Unavailable => scalar!(3, 0),
                        }
                        emit!(TypedEdgeRole::TypeField(5), TypedPlanTarget::Atom(spelling));
                    }
                    crate::ir::ConcreteType::Map { key, value } => {
                        type_tag!(29);
                        type_child!(0, key);
                        type_child!(1, value);
                    }
                    crate::ir::ConcreteType::Channel { direction, element } => {
                        type_tag!(30);
                        scalar!(0, channel_direction(direction));
                        type_child!(1, element);
                    }
                },
                crate::ir::TypeExpr::Computed(value) => match value {
                    crate::ir::ComputedType::KeyOf(target) => {
                        type_tag!(40);
                        type_child!(0, target);
                    }
                    crate::ir::ComputedType::TypeOf(query) => {
                        type_tag!(41);
                        match query {
                            crate::ir::TypeQuery::Entity(entity) => {
                                scalar!(0, 0);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Entity(entity));
                            }
                            crate::ir::TypeQuery::Path(path) => {
                                scalar!(0, 1);
                                emit!(
                                    TypedEdgeRole::TypeField(1),
                                    TypedPlanTarget::Node(TypedPlanNode::AtomList(path)),
                                );
                            }
                            crate::ir::TypeQuery::External(external) => {
                                scalar!(0, 2);
                                emit!(
                                    TypedEdgeRole::TypeField(1),
                                    TypedPlanTarget::External(external),
                                );
                            }
                        }
                    }
                    crate::ir::ComputedType::IndexedAccess { object, index } => {
                        type_tag!(42);
                        type_child!(0, object);
                        type_child!(1, index);
                    }
                    crate::ir::ComputedType::Conditional {
                        check,
                        extends,
                        then_type,
                        else_type,
                        distributive,
                    } => {
                        type_tag!(43);
                        type_child!(0, check);
                        type_child!(1, extends);
                        type_child!(2, then_type);
                        type_child!(3, else_type);
                        scalar!(4, u64::from(distributive));
                    }
                    crate::ir::ComputedType::Mapped {
                        parameter,
                        constraint,
                        name_as,
                        value,
                        readonly,
                        optional,
                    } => {
                        type_tag!(44);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Atom(parameter)
                        );
                        type_child!(1, constraint);
                        scalar!(2, u64::from(name_as.is_some()));
                        if let Some(name_as) = name_as {
                            type_child!(3, name_as);
                        }
                        type_child!(4, value);
                        scalar!(5, mapped_modifier(readonly));
                        scalar!(6, mapped_modifier(optional));
                    }
                    crate::ir::ComputedType::Infer {
                        parameter,
                        constraint,
                    } => {
                        type_tag!(45);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Atom(parameter)
                        );
                        scalar!(1, u64::from(constraint.is_some()));
                        if let Some(constraint) = constraint {
                            type_child!(2, constraint);
                        }
                    }
                    crate::ir::ComputedType::TemplateLiteral(parts) => {
                        type_tag!(46);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::TemplateParts(parts)),
                        );
                    }
                    crate::ir::ComputedType::Import {
                        specifier,
                        qualifier,
                        arguments,
                    } => {
                        type_tag!(47);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Atom(specifier)
                        );
                        emit!(
                            TypedEdgeRole::TypeField(1),
                            TypedPlanTarget::Node(TypedPlanNode::AtomList(qualifier)),
                        );
                        type_list!(2, arguments);
                    }
                    crate::ir::ComputedType::Awaited(target) => {
                        type_tag!(48);
                        type_child!(0, target);
                    }
                    crate::ir::ComputedType::This => type_tag!(49),
                },
                crate::ir::TypeExpr::Unknown(value) => {
                    type_tag!(60);
                    scalar!(0, unknown_reason(value.reason));
                    scalar!(1, u64::from(value.spelling.is_some()));
                    if let Some(spelling) = value.spelling {
                        emit!(TypedEdgeRole::TypeField(2), TypedPlanTarget::Atom(spelling));
                    }
                }
            }
        }
        TypedPlanNode::TypeList(id) => {
            let values = reader.types(id).ok_or(missing_node(node)?)?;
            for (index, value) in values.enumerate() {
                let index = list_index(node, index)?;
                emit!(
                    TypedEdgeRole::ListElement(index),
                    TypedPlanTarget::Node(TypedPlanNode::Type(value)),
                );
            }
        }
        TypedPlanNode::AtomList(id) => {
            let values = reader.atom_list(id).ok_or(missing_node(node)?)?;
            for (index, value) in values.enumerate() {
                emit!(
                    TypedEdgeRole::ListElement(list_index(node, index)?),
                    TypedPlanTarget::Atom(value),
                );
            }
        }
        TypedPlanNode::TupleElements(id) => {
            let values = reader.tuple_elements(id).ok_or(missing_node(node)?)?;
            for (index, value) in values.enumerate() {
                let index = list_index(node, index)?;
                emit!(
                    TypedEdgeRole::TupleLabel(index),
                    TypedPlanTarget::Scalar(u64::from(value.label.is_some())),
                );
                if let Some(label) = value.label {
                    emit!(
                        TypedEdgeRole::TupleLabel(index),
                        TypedPlanTarget::Atom(label)
                    );
                }
                emit!(
                    TypedEdgeRole::TupleType(index),
                    TypedPlanTarget::Node(TypedPlanNode::Type(value.ty)),
                );
                emit!(
                    TypedEdgeRole::TupleKind(index),
                    TypedPlanTarget::Scalar(tuple_element_kind(value.kind)),
                );
            }
        }
        TypedPlanNode::ObjectMembers(id) => {
            let values = reader.object_members(id).ok_or(missing_node(node)?)?;
            for (index, value) in values.enumerate() {
                let index = list_index(node, index)?;
                match value {
                    crate::ir::ObjectMember::Property {
                        key,
                        ty,
                        optional,
                        readonly,
                    } => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(0));
                        for_each_property_key(key, index, sink)?;
                        emit!(
                            TypedEdgeRole::ObjectType(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(ty)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectOptional(index),
                            TypedPlanTarget::Scalar(u64::from(optional)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectReadonly(index),
                            TypedPlanTarget::Scalar(u64::from(readonly)),
                        );
                    }
                    crate::ir::ObjectMember::Method {
                        key,
                        signature,
                        optional,
                    } => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(1));
                        for_each_property_key(key, index, sink)?;
                        emit!(
                            TypedEdgeRole::ObjectType(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(signature)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectOptional(index),
                            TypedPlanTarget::Scalar(u64::from(optional)),
                        );
                    }
                    crate::ir::ObjectMember::Index {
                        parameter,
                        key,
                        value,
                        readonly,
                    } => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(2));
                        emit!(
                            TypedEdgeRole::ObjectKey(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(key)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectReadonly(index),
                            TypedPlanTarget::Scalar(u64::from(readonly)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectParameter(index),
                            TypedPlanTarget::Atom(parameter)
                        );
                        emit!(
                            TypedEdgeRole::ObjectValue(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(value)),
                        );
                    }
                    crate::ir::ObjectMember::Call(signature) => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(3));
                        emit!(
                            TypedEdgeRole::ObjectType(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(signature)),
                        );
                    }
                    crate::ir::ObjectMember::Construct(signature) => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(4));
                        emit!(
                            TypedEdgeRole::ObjectType(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(signature)),
                        );
                    }
                }
            }
        }
        TypedPlanNode::TemplateParts(id) => {
            let values = reader.template_parts(id).ok_or(missing_node(node)?)?;
            for (index, value) in values.enumerate() {
                let index = list_index(node, index)?;
                match value {
                    crate::ir::TemplatePart::Bytes(atom) => {
                        emit!(
                            TypedEdgeRole::TemplatePart(index),
                            TypedPlanTarget::Scalar(0)
                        );
                        emit!(
                            TypedEdgeRole::TemplatePart(index),
                            TypedPlanTarget::Atom(atom)
                        );
                    }
                    crate::ir::TemplatePart::Placeholder(ty) => {
                        emit!(
                            TypedEdgeRole::TemplatePart(index),
                            TypedPlanTarget::Scalar(1)
                        );
                        emit!(
                            TypedEdgeRole::TemplatePart(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(ty)),
                        );
                    }
                }
            }
        }
        TypedPlanNode::TypeParameters(id) => {
            let values = reader.type_parameters(id).ok_or(missing_node(node)?)?;
            for (index, value) in values.enumerate() {
                let index = list_index(node, index)?;
                emit!(
                    TypedEdgeRole::ParameterName(index),
                    TypedPlanTarget::Atom(value.name)
                );
                emit!(
                    TypedEdgeRole::ParameterBound(index),
                    TypedPlanTarget::Node(TypedPlanNode::TypeParameterBounds(value.bounds)),
                );
                emit!(
                    TypedEdgeRole::ParameterDefault(index),
                    TypedPlanTarget::Scalar(u64::from(value.default.is_some())),
                );
                if let Some(default) = value.default {
                    emit!(
                        TypedEdgeRole::ParameterDefault(index),
                        TypedPlanTarget::Node(TypedPlanNode::Type(default)),
                    );
                }
                emit!(
                    TypedEdgeRole::ParameterVariance(index),
                    TypedPlanTarget::Scalar(variance(value.variance)),
                );
                match value.kind {
                    crate::ir::TypeParameterKind::Type { inference } => {
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Scalar(0),
                        );
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Scalar(type_parameter_inference(inference)),
                        );
                    }
                    crate::ir::TypeParameterKind::ConstValue { value_type } => {
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Scalar(1),
                        );
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(value_type)),
                        );
                    }
                    crate::ir::TypeParameterKind::Lifetime => emit!(
                        TypedEdgeRole::ParameterKind(index),
                        TypedPlanTarget::Scalar(2),
                    ),
                }
                emit!(
                    TypedEdgeRole::ParameterRequirement(index),
                    TypedPlanTarget::Scalar(primary_requirement(value.requirements.primary)),
                );
                emit!(
                    TypedEdgeRole::ParameterConstructor(index),
                    TypedPlanTarget::Scalar(u64::from(value.requirements.constructor)),
                );
                emit!(
                    TypedEdgeRole::ParameterAllowsRefLike(index),
                    TypedPlanTarget::Scalar(u64::from(value.requirements.allows_ref_like)),
                );
            }
        }
        TypedPlanNode::TypeParameterBounds(id) => {
            let values = reader
                .type_parameter_bounds(id)
                .ok_or(missing_node(node)?)?;
            for (index, value) in values.enumerate() {
                let index = list_index(node, index)?;
                match value {
                    crate::ir::TypeParameterBound::Type(ty) => {
                        emit!(TypedEdgeRole::BoundKind(index), TypedPlanTarget::Scalar(0));
                        emit!(
                            TypedEdgeRole::BoundValue(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(ty)),
                        );
                    }
                    crate::ir::TypeParameterBound::Lifetime(atom) => {
                        emit!(TypedEdgeRole::BoundKind(index), TypedPlanTarget::Scalar(1));
                        emit!(
                            TypedEdgeRole::BoundValue(index),
                            TypedPlanTarget::Atom(atom)
                        );
                    }
                }
            }
        }
        TypedPlanNode::FreePredicates(id) => {
            let values = reader.free_predicates(id).ok_or(missing_node(node)?)?;
            for (index, value) in values.enumerate() {
                let index = list_index(node, index)?;
                emit!(
                    TypedEdgeRole::PredicateSubject(index),
                    TypedPlanTarget::Node(TypedPlanNode::Type(value.subject)),
                );
                emit!(
                    TypedEdgeRole::PredicateBound(index),
                    TypedPlanTarget::Node(TypedPlanNode::TypeParameterBounds(value.bounds)),
                );
            }
        }
    }
    Ok(())
}

fn for_each_property_key(
    key: crate::ir::PropertyKey,
    index: u32,
    sink: &mut impl FnMut(TypedPlanEdge) -> Result<(), TypedPlanError>,
) -> Result<(), TypedPlanError> {
    let (tag, target) = match key {
        crate::ir::PropertyKey::Named(atom) => (0, TypedPlanTarget::Atom(atom)),
        crate::ir::PropertyKey::Private(atom) => (1, TypedPlanTarget::Atom(atom)),
        crate::ir::PropertyKey::Numeric(atom) => (2, TypedPlanTarget::Atom(atom)),
        crate::ir::PropertyKey::Computed(ty) => (3, TypedPlanTarget::Node(TypedPlanNode::Type(ty))),
    };
    sink(TypedPlanEdge {
        role: TypedEdgeRole::ObjectKey(index),
        target: TypedPlanTarget::Scalar(tag),
    })?;
    sink(TypedPlanEdge {
        role: TypedEdgeRole::ObjectKey(index),
        target,
    })
}

fn primary_requirement(value: crate::ir::TypeParameterPrimaryRequirement) -> u64 {
    match value {
        crate::ir::TypeParameterPrimaryRequirement::None => 0,
        crate::ir::TypeParameterPrimaryRequirement::Reference { nullable: false } => 1,
        crate::ir::TypeParameterPrimaryRequirement::Reference { nullable: true } => 2,
        crate::ir::TypeParameterPrimaryRequirement::Value => 3,
        crate::ir::TypeParameterPrimaryRequirement::Unmanaged => 4,
        crate::ir::TypeParameterPrimaryRequirement::NotNull => 5,
        crate::ir::TypeParameterPrimaryRequirement::Default => 6,
    }
}

fn tuple_element_kind(value: crate::ir::TupleElementKind) -> u64 {
    match value {
        crate::ir::TupleElementKind::Required => 0,
        crate::ir::TupleElementKind::Optional => 1,
        crate::ir::TupleElementKind::Rest => 2,
    }
}

fn variance(value: crate::ir::Variance) -> u64 {
    match value {
        crate::ir::Variance::Invariant => 0,
        crate::ir::Variance::Covariant => 1,
        crate::ir::Variance::Contravariant => 2,
        crate::ir::Variance::Bivariant => 3,
    }
}

fn type_parameter_inference(value: crate::ir::TypeParameterInference) -> u64 {
    match value {
        crate::ir::TypeParameterInference::Ordinary => 0,
        crate::ir::TypeParameterInference::Const => 1,
    }
}

fn list_index(_node: TypedPlanNode, index: usize) -> Result<u32, TypedPlanError> {
    u32::try_from(index).map_err(|_| TypedPlanFault::InvalidProjection.into())
}

fn missing_node(_node: TypedPlanNode) -> Result<TypedPlanFault, TypedPlanError> {
    Ok(TypedPlanFault::InvalidProjection)
}

fn builtin_type(value: crate::ir::BuiltinType) -> u64 {
    match value {
        crate::ir::BuiltinType::Unit => 0,
        crate::ir::BuiltinType::Never => 1,
        crate::ir::BuiltinType::Bool => 2,
        crate::ir::BuiltinType::LegacyChar => 3,
        crate::ir::BuiltinType::I8 => 4,
        crate::ir::BuiltinType::I16 => 5,
        crate::ir::BuiltinType::I32 => 6,
        crate::ir::BuiltinType::I64 => 7,
        crate::ir::BuiltinType::I128 => 8,
        crate::ir::BuiltinType::U8 => 9,
        crate::ir::BuiltinType::U16 => 10,
        crate::ir::BuiltinType::U32 => 11,
        crate::ir::BuiltinType::U64 => 12,
        crate::ir::BuiltinType::U128 => 13,
        crate::ir::BuiltinType::F16 => 14,
        crate::ir::BuiltinType::F32 => 15,
        crate::ir::BuiltinType::F64 => 16,
        crate::ir::BuiltinType::String => 17,
        crate::ir::BuiltinType::Bytes => 18,
        crate::ir::BuiltinType::Object => 19,
        crate::ir::BuiltinType::Any => 20,
        crate::ir::BuiltinType::Unknown => 21,
        crate::ir::BuiltinType::Void => 22,
        crate::ir::BuiltinType::Number => 23,
        crate::ir::BuiltinType::BigInt => 24,
        crate::ir::BuiltinType::Symbol => 25,
        crate::ir::BuiltinType::UniqueSymbol => 26,
        crate::ir::BuiltinType::Null => 27,
        crate::ir::BuiltinType::Undefined => 28,
        crate::ir::BuiltinType::None_ => 30,
        crate::ir::BuiltinType::List => 31,
        crate::ir::BuiltinType::Dict => 32,
        crate::ir::BuiltinType::Set => 33,
        crate::ir::BuiltinType::FrozenSet => 34,
        crate::ir::BuiltinType::Complex => 36,
        crate::ir::BuiltinType::Decimal => 37,
        crate::ir::BuiltinType::ArbitraryInteger => 38,
        crate::ir::BuiltinType::NativeSignedInteger => 39,
        crate::ir::BuiltinType::NativeUnsignedInteger => 40,
        crate::ir::BuiltinType::PointerAddressInteger => 41,
    }
}

fn variadic_form(value: crate::ir::VariadicForm) -> u64 {
    match value {
        crate::ir::FunctionVariadicForm::None => 0,
        crate::ir::FunctionVariadicForm::TypedLast => 1,
        crate::ir::FunctionVariadicForm::CUnbounded => 2,
    }
}

fn mutability_code(value: crate::ir::Mutability) -> u64 {
    match value {
        crate::ir::Mutability::Immutable => 0,
        crate::ir::Mutability::Mutable => 1,
    }
}

fn cxx_reference_category(value: crate::ir::CxxReferenceCategory) -> u64 {
    match value {
        crate::ir::CxxReferenceCategory::Lvalue => 0,
        crate::ir::CxxReferenceCategory::Rvalue => 1,
    }
}

fn native_character_role(value: crate::ir::NativeCharacterRole) -> u64 {
    match value {
        crate::ir::NativeCharacterRole::UnicodeScalar => 0,
        crate::ir::NativeCharacterRole::Utf16CodeUnit => 1,
        crate::ir::NativeCharacterRole::Utf32CodeUnit => 2,
        crate::ir::NativeCharacterRole::CPlainSigned => 3,
        crate::ir::NativeCharacterRole::CPlainUnsigned => 4,
        crate::ir::NativeCharacterRole::CSigned => 5,
        crate::ir::NativeCharacterRole::CUnsigned => 6,
        crate::ir::NativeCharacterRole::CWideSigned => 7,
        crate::ir::NativeCharacterRole::CWideUnsigned => 8,
        crate::ir::NativeCharacterRole::CWideSignednessUnavailable => 9,
    }
}

fn annotation_kind(value: crate::ir::AnnotationKind) -> u64 {
    match value {
        crate::ir::AnnotationKind::Readonly => 0,
        crate::ir::AnnotationKind::NullableValue => 1,
        crate::ir::AnnotationKind::NullableReference => 2,
        crate::ir::AnnotationKind::NonNullableReference => 3,
    }
}

fn channel_direction(value: crate::ir::ChannelDirection) -> u64 {
    match value {
        crate::ir::ChannelDirection::Both => 0,
        crate::ir::ChannelDirection::Send => 1,
        crate::ir::ChannelDirection::Receive => 2,
    }
}

fn mapped_modifier(value: crate::ir::MappedModifier) -> u64 {
    match value {
        crate::ir::MappedModifier::Preserve => 0,
        crate::ir::MappedModifier::Add => 1,
        crate::ir::MappedModifier::Remove => 2,
    }
}

fn unknown_reason(value: crate::ir::UnknownReason) -> u64 {
    match value {
        crate::ir::UnknownReason::Unannotated => 0,
        crate::ir::UnknownReason::DynamicallyTyped => 1,
        crate::ir::UnknownReason::UnresolvedLocalName => 2,
        crate::ir::UnknownReason::UnresolvedExternal => 3,
        crate::ir::UnknownReason::TruncatedAtDepthLimit => 4,
        crate::ir::UnknownReason::OracleGap => 5,
        crate::ir::UnknownReason::NoIrRepresentation => 6,
        crate::ir::UnknownReason::Error => 7,
    }
}

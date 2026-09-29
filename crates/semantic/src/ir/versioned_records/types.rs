//! Normalized reachable typed closure for the SPIR Types plane.
//!
//! The closure seeds entity type associations, typed/list references owned by
//! language-extension facts, and the complete external-target catalog. It
//! follows every child row iteratively. Unreferenced type/list/atom rows are
//! intentionally omitted: this is V2 normalized reachable semantics, not a
//! bytewise or census-equivalent reconstruction of legacy NXFI pools.
//! `TextId` is documentation-owned and has no typed/extension reference here;
//! documentation rows retain their UTF-8 values in the Documentation family.

use crate::ir::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, DeclarationIdentity,
    ExternalTargetIdentity, SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError,
    SemanticReader, TypeId,
};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};

const ROOT_TAG: u8 = 1;
const TYPE_TAG: u8 = 2;
const TYPE_LIST_TAG: u8 = 3;
const TUPLE_ELEMENTS_TAG: u8 = 4;
const OBJECT_MEMBERS_TAG: u8 = 5;
const TEMPLATE_PARTS_TAG: u8 = 6;
const ATOM_LIST_TAG: u8 = 7;
const TYPE_PARAMETERS_TAG: u8 = 8;
const TYPE_PARAMETER_BOUNDS_TAG: u8 = 9;
const FREE_PREDICATES_TAG: u8 = 10;
const ATOM_TAG: u8 = 11;
const EXTERNAL_TARGET_TAG: u8 = 12;
const TYPES_PLANE_CODE: u8 = 2;
const TYPES_ROW_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.types-row-key.v1\0";
const TYPES_FAMILY_ROOT_DOMAIN: &[u8] = b"backend.semantic.ir.types-family-root.v2\0";

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
    node_positions: BTreeMap<(u8, u32), usize>,
    node_keys: BTreeMap<(u8, u32), [u8; 32]>,
    roots: BTreeMap<DeclarationIdentity, Option<TypeId>>,
    atom_ids: BTreeMap<[u8; 32], crate::ir::AtomId>,
    atom_keys_by_raw: BTreeMap<u32, [u8; 32]>,
    external_ids: BTreeMap<[u8; 32], crate::ir::ExternalId>,
    external_keys_by_raw: BTreeMap<u32, [u8; 32]>,
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

    fn node_key(&self, node: TypedPlanNode) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.node_keys
            .get(&node_key(node))
            .copied()
            .ok_or(SemanticPlaneRecordError::ReaderReference)
    }
    fn atom_key(&self, atom: crate::ir::AtomId) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.atom_keys_by_raw
            .get(&atom.raw)
            .copied()
            .ok_or(SemanticPlaneRecordError::ReaderReference)
    }
    fn external_key(
        &self,
        external: crate::ir::ExternalId,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        self.external_keys_by_raw
            .get(&external.raw)
            .copied()
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
                encode_node_payload(reader, plan, node, payload)?;
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
            node_keys: BTreeMap::new(),
            roots,
            atom_ids: BTreeMap::new(),
            atom_keys_by_raw: BTreeMap::new(),
            external_ids: BTreeMap::new(),
            external_keys_by_raw: BTreeMap::new(),
        };
        plan.materialize_atoms(reader, atoms)?;
        plan.materialize_externals(reader, externals)?;
        plan.materialize_node_keys(reader)?;
        Ok(plan)
    }

    fn materialize_atoms(
        &mut self,
        reader: &(impl SemanticReader + ?Sized),
        atoms: Vec<crate::ir::AtomId>,
    ) -> Result<(), SemanticPlaneRecordError> {
        let mut seen_raw = BTreeSet::new();
        for atom in atoms {
            if !seen_raw.insert(atom.raw) {
                continue;
            }
            let bytes = reader
                .atom(atom)
                .ok_or(SemanticPlaneRecordError::ReaderReference)?;
            let key = atom_key(bytes)?;
            if let Some(previous) = self.atom_ids.get(&key).copied() {
                if reader
                    .atom(previous)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?
                    != bytes
                {
                    return Err(SemanticPlaneRecordError::StableKeyCollision);
                }
            } else {
                self.atom_ids.insert(key, atom);
            }
            self.atom_keys_by_raw.insert(atom.raw, key);
        }
        Ok(())
    }

    fn materialize_externals(
        &mut self,
        reader: &(impl SemanticReader + ?Sized),
        externals: Vec<crate::ir::ExternalId>,
    ) -> Result<(), SemanticPlaneRecordError> {
        let mut seen_raw = BTreeSet::new();
        for external in externals {
            if !seen_raw.insert(external.raw) {
                continue;
            }
            let identity = ExternalTargetIdentity::capture(reader, external)
                .map_err(|_| SemanticPlaneRecordError::ReaderReference)?;
            let key = *identity.as_bytes();
            if let Some(previous) = self.external_ids.get(&key).copied() {
                let mut old_payload = Vec::new();
                let mut new_payload = Vec::new();
                encode_external_target(reader, previous, &mut old_payload)?;
                encode_external_target(reader, external, &mut new_payload)?;
                if old_payload != new_payload {
                    return Err(SemanticPlaneRecordError::StableKeyCollision);
                }
            } else {
                self.external_ids.insert(key, external);
            }
            self.external_keys_by_raw.insert(external.raw, key);
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
        let mut unique_keys = BTreeSet::new();
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
                    if !unique_keys.insert(key) {
                        return Err(SemanticPlaneRecordError::StableKeyCollision);
                    }
                    self.node_keys.insert(node_key(node), key);
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
                let target_slot = self
                    .node_positions
                    .get(&node_key(target))
                    .copied()
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?;
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
        BTreeMap<(u8, u32), usize>,
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
    Ok((nodes, edges, offsets, positions, atoms, externals))
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
fn typed_row_key(tag: u8, payload: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(TYPES_ROW_KEY_DOMAIN);
    hasher.update(&[TYPES_PLANE_CODE, tag]);
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}
fn atom_key(bytes: &[u8]) -> Result<[u8; 32], SemanticPlaneRecordError> {
    let length = u32::try_from(bytes.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(TYPES_ROW_KEY_DOMAIN);
    hasher.update(&[TYPES_PLANE_CODE, ATOM_TAG]);
    hasher.update(&length.to_be_bytes());
    hasher.update(bytes);
    Ok(*hasher.finalize().as_bytes())
}
fn identity_bytes(identity: DeclarationIdentity) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(identity.family.as_bytes());
    bytes[16..].copy_from_slice(identity.variant.as_bytes());
    bytes
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
    let slot = plan
        .node_positions
        .get(&node_key(node))
        .copied()
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
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
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), SemanticPlaneRecordError> {
    out.extend_from_slice(
        &u32::try_from(bytes.len())
            .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
            .to_be_bytes(),
    );
    out.extend_from_slice(bytes);
    Ok(())
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

/// Closed row domain advertised by a checked Types-family reference catalog.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TypesRowDomainV2 {
    /// Entity-to-optional-type association row.
    EntityRoot,
    /// Any of the nine structural type/list domains.
    TypedNode,
    /// Structural semantic type.
    Type,
    /// Ordered type sequence.
    TypeList,
    /// Ordered tuple or callable parameter/result sequence.
    TupleElements,
    /// Ordered object-member sequence.
    ObjectMembers,
    /// Ordered template-literal pieces.
    TemplateParts,
    /// Ordered arbitrary-byte atom sequence.
    AtomList,
    /// Ordered type-parameter sequence.
    TypeParameters,
    /// Ordered type-parameter bounds.
    TypeParameterBounds,
    /// Ordered Rust free predicates.
    FreePredicates,
    /// Exact arbitrary-byte atom.
    Atom,
    /// Exact external target identity and payload.
    ExternalTarget,
}

/// A typed row reference extracted from a verified row.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TypesReferenceV2 {
    /// Required target row domain.
    pub domain: TypesRowDomainV2,
    /// Stable target key.
    pub key: [u8; 32],
}

/// Locally checked Types family, safe to pass to the all-family V2 proof.
///
/// The catalog contains all stable row keys and cross-family declaration
/// references. Every intra-Types reference has already been resolved and every
/// row payload has passed the independent strict decoder.
pub struct CheckedTypesFamilyV2 {
    row_keys: alloc::boxed::Box<[[u8; 32]]>,
    domains: BTreeMap<[u8; 32], TypesRowDomainV2>,
    root_identities: alloc::boxed::Box<[[u8; 32]]>,
    declaration_references: alloc::boxed::Box<[[u8; 32]]>,
    external_target_keys: alloc::boxed::Box<[[u8; 32]]>,
    local_root: [u8; 32],
    row_count: u64,
}

impl CheckedTypesFamilyV2 {
    /// Strictly admits the complete merged record sequence for one Types plane.
    ///
    /// Input keys must be globally strictly ascending. All typed/list/atom/
    /// external references must resolve inside the supplied family; declaration
    /// identities remain for the aggregate proof to bind to Core.
    pub fn from_records<'bytes>(
        records: impl IntoIterator<Item = ([u8; 32], u8, &'bytes [u8])>,
    ) -> Result<Self, SemanticPlaneRecordError> {
        let mut row_keys = Vec::new();
        let mut domains = BTreeMap::new();
        let mut local_references = BTreeSet::new();
        let mut root_identities = BTreeSet::new();
        let mut declaration_references = BTreeSet::new();
        let mut previous = None;
        let mut family_hasher = blake3::Hasher::new();
        family_hasher.update(TYPES_FAMILY_ROOT_DOMAIN);
        let mut row_count = 0_u64;
        for (key, tag, payload) in records {
            if previous.is_some_and(|prior| prior >= key) {
                return Err(SemanticPlaneRecordError::RecordOrder);
            }
            let parsed = parse_types_row(key, tag, payload)?;
            let payload_length =
                u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
            family_hasher.update(&key);
            family_hasher.update(&[tag]);
            family_hasher.update(&payload_length.to_be_bytes());
            family_hasher.update(payload);
            row_count = row_count
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if domains.insert(key, parsed.domain).is_some() {
                return Err(SemanticPlaneRecordError::StableKeyCollision);
            }
            if let Some(identity) = parsed.root_identity {
                if !root_identities.insert(identity) {
                    return Err(SemanticPlaneRecordError::StableKeyCollision);
                }
            }
            for reference in parsed.references {
                local_references.insert(reference);
            }
            declaration_references.extend(parsed.declaration_references);
            row_keys
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            row_keys.push(key);
            previous = Some(key);
        }
        for reference in &local_references {
            let observed = domains.get(&reference.key);
            let resolved = match reference.domain {
                TypesRowDomainV2::TypedNode => {
                    observed.is_some_and(|domain| domain.is_typed_node())
                }
                domain => observed == Some(&domain),
            };
            if !resolved {
                return Err(SemanticPlaneRecordError::ReaderReference);
            }
        }
        let external_target_keys = domains
            .iter()
            .filter_map(|(key, domain)| {
                (*domain == TypesRowDomainV2::ExternalTarget).then_some(*key)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        family_hasher.update(&row_count.to_be_bytes());
        let local_root = *family_hasher.finalize().as_bytes();
        Ok(Self {
            row_keys: row_keys.into_boxed_slice(),
            domains,
            root_identities: root_identities
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            declaration_references: declaration_references
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            external_target_keys,
            local_root,
            row_count,
        })
    }

    /// Declares the normalized reachability semantics accepted by this catalog.
    #[must_use]
    pub const fn semantics(&self) -> TypesClosureSemantics {
        TypesClosureSemantics::NormalizedReachable
    }

    /// Sorted stable keys in the admitted Types family.
    #[must_use]
    pub fn row_keys(&self) -> &[[u8; 32]] {
        &self.row_keys
    }

    /// Declaration identities that have an entity→optional-type root row.
    #[must_use]
    pub fn root_identities(&self) -> &[[u8; 32]] {
        &self.root_identities
    }

    /// Declaration identities referenced by type nodes and checked against Core by V2.
    #[must_use]
    pub fn declaration_references(&self) -> &[[u8; 32]] {
        &self.declaration_references
    }

    /// Cross-family declaration identities that the aggregate must resolve.
    #[must_use]
    pub fn unresolved_references(&self) -> &[[u8; 32]] {
        &self.declaration_references
    }

    /// Exact external-target stable keys owned by this family.
    #[must_use]
    pub fn external_target_keys(&self) -> &[[u8; 32]] {
        &self.external_target_keys
    }

    /// Local canonical commitment over sorted keys, tags, and exact payloads.
    #[must_use]
    pub const fn local_root(&self) -> &[u8; 32] {
        &self.local_root
    }

    /// Number of checked rows, including roots and explicit empty lists.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Resolves one typed/list/atom/external root emitted by another family.
    pub fn require_reference(
        &self,
        reference: TypesReferenceV2,
    ) -> Result<(), SemanticPlaneRecordError> {
        let matches = match reference.domain {
            TypesRowDomainV2::TypedNode => self
                .domains
                .get(&reference.key)
                .is_some_and(|domain| domain.is_typed_node()),
            domain => self.domains.get(&reference.key) == Some(&domain),
        };
        if matches {
            Ok(())
        } else {
            Err(SemanticPlaneRecordError::ReaderReference)
        }
    }
}

/// Validates the Types row sequence from already independently admitted SPIR segments.
pub fn validate_types_family_v2<'bytes>(
    segments: impl IntoIterator<Item = super::CanonicalSemanticPlaneSegmentView<'bytes>>,
) -> Result<CheckedTypesFamilyV2, SemanticPlaneRecordError> {
    let mut records = Vec::new();
    let mut previous = None;
    for segment in segments {
        if segment.kind() != SemanticPlaneKind::Ir(SemanticIrPlane::Types) {
            return Err(SemanticPlaneRecordError::PlaneKind);
        }
        for record in segment.records() {
            if previous.is_some_and(|prior| prior >= record.key()) {
                return Err(SemanticPlaneRecordError::RecordOrder);
            }
            previous = Some(record.key());
            records.push((record.key(), record.tag(), record.payload()));
        }
    }
    CheckedTypesFamilyV2::from_records(records)
}

/// Strict row dispatcher used by the common SPIR decoder.
pub(super) fn validate_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    if kind != SemanticPlaneKind::Ir(SemanticIrPlane::Types) {
        return Err(SemanticPlaneRecordError::PlaneKind);
    }
    let _ = parse_types_row(key, tag, payload)?;
    Ok(())
}

struct ParsedTypesRow {
    domain: TypesRowDomainV2,
    root_identity: Option<[u8; 32]>,
    references: Vec<TypesReferenceV2>,
    declaration_references: Vec<[u8; 32]>,
}

fn parse_types_row(
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<ParsedTypesRow, SemanticPlaneRecordError> {
    match tag {
        ROOT_TAG => {
            if payload.len() != 65 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            let identity: [u8; 32] = payload[..32]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::RowGrammar)?;
            let present = payload[32];
            let target: [u8; 32] = payload[33..65]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::RowGrammar)?;
            if present > 1 || (present == 0 && target != [0; 32]) {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            if root_key_from_bytes(identity) != key {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            let mut references = Vec::new();
            if present == 1 {
                references.push(TypesReferenceV2 {
                    domain: TypesRowDomainV2::Type,
                    key: target,
                });
            }
            Ok(ParsedTypesRow {
                domain: TypesRowDomainV2::EntityRoot,
                root_identity: Some(identity),
                references,
                declaration_references: Vec::new(),
            })
        }
        TYPE_TAG..=FREE_PREDICATES_TAG => parse_typed_node(key, tag, payload),
        ATOM_TAG => {
            let mut cursor = RowCursor::new(payload);
            let bytes = cursor.bytes32()?;
            cursor.finish()?;
            if atom_key(bytes)? != key {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            Ok(ParsedTypesRow {
                domain: TypesRowDomainV2::Atom,
                root_identity: None,
                references: Vec::new(),
                declaration_references: Vec::new(),
            })
        }
        EXTERNAL_TARGET_TAG => {
            let (identity, references) = external_identity_from_payload(payload)?;
            if identity != key {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            Ok(ParsedTypesRow {
                domain: TypesRowDomainV2::ExternalTarget,
                root_identity: None,
                references,
                declaration_references: Vec::new(),
            })
        }
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn parse_typed_node(
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<ParsedTypesRow, SemanticPlaneRecordError> {
    let mut cursor = RowCursor::new(payload);
    let count =
        usize::try_from(cursor.u32()?).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    let expected = count
        .checked_mul(38)
        .and_then(|length| length.checked_add(4))
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    if payload.len() != expected {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut edges = Vec::new();
    edges
        .try_reserve_exact(count)
        .map_err(SemanticPlaneRecordError::Allocation)?;
    let mut previous_role = None;
    for _ in 0..count {
        let role = cursor.u8()?;
        let index = cursor.u32()?;
        let target_tag = cursor.u8()?;
        let target: [u8; 32] = cursor
            .take(32)?
            .try_into()
            .map_err(|_| SemanticPlaneRecordError::RowGrammar)?;
        if role > 25 || target_tag > 4 {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        if let Some((prior_index, prior_role)) = previous_role {
            if (index, role) < (prior_index, prior_role) {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
        }
        previous_role = Some((index, role));
        edges.push(TypedWireEdge {
            role,
            index,
            target_tag,
            target,
        });
    }
    cursor.finish()?;

    let mut references = Vec::new();
    let mut declaration_references = Vec::new();
    let mut edges = TypedWireCursor::new(&edges);
    if tag == TYPE_TAG {
        let type_kind = edges.scalar(0, 0, |value| matches!(value, 1..=30 | 40..=49 | 60))?;
        validate_type_fields(
            type_kind,
            &mut edges,
            &mut references,
            &mut declaration_references,
        )?;
    } else {
        validate_list_node(
            tag,
            &mut edges,
            &mut references,
            &mut declaration_references,
        )?;
    }
    edges.finish()?;
    if typed_row_key(tag, payload) != key {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    Ok(ParsedTypesRow {
        domain: domain_for_tag(tag)?,
        root_identity: None,
        references,
        declaration_references,
    })
}

#[derive(Clone, Copy)]
struct TypedWireEdge {
    role: u8,
    index: u32,
    target_tag: u8,
    target: [u8; 32],
}

struct TypedWireCursor<'edges> {
    edges: &'edges [TypedWireEdge],
    position: usize,
}

impl<'edges> TypedWireCursor<'edges> {
    const fn new(edges: &'edges [TypedWireEdge]) -> Self {
        Self { edges, position: 0 }
    }

    fn take(
        &mut self,
        role: u8,
        index: u32,
        target_tag: u8,
    ) -> Result<[u8; 32], SemanticPlaneRecordError> {
        let edge = self
            .edges
            .get(self.position)
            .ok_or(SemanticPlaneRecordError::RowGrammar)?;
        if edge.role != role || edge.index != index || edge.target_tag != target_tag {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        self.position += 1;
        Ok(edge.target)
    }

    fn scalar(
        &mut self,
        role: u8,
        index: u32,
        valid: impl FnOnce(u64) -> bool,
    ) -> Result<u64, SemanticPlaneRecordError> {
        let bytes = self.take(role, index, 4)?;
        if bytes[..24] != [0; 24] {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        let value = u64::from_be_bytes(
            bytes[24..]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::RowGrammar)?,
        );
        if valid(value) {
            Ok(value)
        } else {
            Err(SemanticPlaneRecordError::RowGrammar)
        }
    }

    fn node(
        &mut self,
        role: u8,
        index: u32,
        domain: TypesRowDomainV2,
        references: &mut Vec<TypesReferenceV2>,
    ) -> Result<(), SemanticPlaneRecordError> {
        let key = self.take(role, index, 0)?;
        references
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        references.push(TypesReferenceV2 { domain, key });
        Ok(())
    }

    fn atom(
        &mut self,
        role: u8,
        index: u32,
        references: &mut Vec<TypesReferenceV2>,
    ) -> Result<(), SemanticPlaneRecordError> {
        self.reference(role, index, 1, TypesRowDomainV2::Atom, references)
    }

    fn external(
        &mut self,
        role: u8,
        index: u32,
        references: &mut Vec<TypesReferenceV2>,
    ) -> Result<(), SemanticPlaneRecordError> {
        self.reference(role, index, 3, TypesRowDomainV2::ExternalTarget, references)
    }

    fn reference(
        &mut self,
        role: u8,
        index: u32,
        target_tag: u8,
        domain: TypesRowDomainV2,
        references: &mut Vec<TypesReferenceV2>,
    ) -> Result<(), SemanticPlaneRecordError> {
        let key = self.take(role, index, target_tag)?;
        references
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        references.push(TypesReferenceV2 { domain, key });
        Ok(())
    }

    fn entity(
        &mut self,
        role: u8,
        index: u32,
        declaration_references: &mut Vec<[u8; 32]>,
    ) -> Result<(), SemanticPlaneRecordError> {
        declaration_references
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        declaration_references.push(self.take(role, index, 2)?);
        Ok(())
    }

    fn peek_index(&self) -> Option<u32> {
        self.edges.get(self.position).map(|edge| edge.index)
    }

    const fn is_empty(&self) -> bool {
        self.position == self.edges.len()
    }

    fn finish(self) -> Result<(), SemanticPlaneRecordError> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(SemanticPlaneRecordError::RowGrammar)
        }
    }
}

fn validate_type_fields(
    kind: u64,
    edges: &mut TypedWireCursor<'_>,
    references: &mut Vec<TypesReferenceV2>,
    declaration_references: &mut Vec<[u8; 32]>,
) -> Result<(), SemanticPlaneRecordError> {
    macro_rules! scalar {
        ($field:expr, $valid:expr) => {
            edges.scalar(1, $field, $valid)?
        };
    }
    macro_rules! node {
        ($field:expr, $domain:expr) => {
            edges.node(1, $field, $domain, references)?
        };
    }
    macro_rules! atom {
        ($field:expr) => {
            edges.atom(1, $field, references)?
        };
    }
    macro_rules! external {
        ($field:expr) => {
            edges.external(1, $field, references)?
        };
    }
    macro_rules! entity {
        ($field:expr) => {
            edges.entity(1, $field, declaration_references)?
        };
    }
    const BOOL: fn(u64) -> bool = |value| value <= 1;
    const ANY: fn(u64) -> bool = |_| true;
    const MUTABILITY: fn(u64) -> bool = |value| value <= 1;
    const VARIADIC: fn(u64) -> bool = |value| value <= 2;
    const MODIFIER: fn(u64) -> bool = |value| value <= 2;

    use TypesRowDomainV2::{AtomList, ObjectMembers, TemplateParts, TupleElements, Type, TypeList};
    match kind {
        1 => {
            if !matches!(
                scalar!(0, ANY),
                0..=28 | 30..=34 | 36..=41
            ) {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
        }
        2 => match scalar!(0, |value| value <= 5) {
            0..=2 => atom!(1),
            3 => {
                scalar!(1, BOOL);
            }
            4 | 5 => {}
            _ => return Err(SemanticPlaneRecordError::RowGrammar),
        },
        3 => entity!(0),
        4 => external!(0),
        5 => atom!(0),
        6 => {
            node!(0, Type);
            node!(1, TypeList);
        }
        7 => node!(0, TupleElements),
        8 => node!(0, ObjectMembers),
        9 => {
            node!(0, TupleElements);
            node!(1, TupleElements);
            if scalar!(2, BOOL) == 1 {
                atom!(3);
            }
            scalar!(4, VARIADIC);
            scalar!(5, BOOL);
        }
        10 => {
            node!(0, Type);
            scalar!(1, MUTABILITY);
            if scalar!(2, BOOL) == 1 {
                atom!(3);
            }
        }
        11 => {
            node!(0, Type);
            scalar!(1, |value| value <= 1);
        }
        12 => node!(0, Type),
        13 => {
            node!(0, Type);
            node!(1, Type);
        }
        14 => {
            node!(0, Type);
            scalar!(1, |value| value <= u8::MAX as u64);
        }
        15 => node!(0, Type),
        16 => {
            scalar!(0, |value| value <= 9);
            scalar!(1, |value| (1..=u16::MAX as u64).contains(&value));
        }
        17 => {
            node!(0, Type);
            scalar!(1, MUTABILITY);
        }
        18 => node!(0, Type),
        19 => {
            node!(0, Type);
            match scalar!(1, |value| value <= 4) {
                0 | 4 => {}
                1 => {
                    scalar!(2, |value| (1..=u16::MAX as u64).contains(&value));
                }
                2 => {
                    scalar!(2, ANY);
                }
                3 => atom!(2),
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
        }
        20 => node!(0, Type),
        21..=24 => node!(0, TypeList),
        25 => match scalar!(0, |value| value <= 2) {
            0 => {}
            1 | 2 => node!(1, Type),
            _ => return Err(SemanticPlaneRecordError::RowGrammar),
        },
        26 => {
            scalar!(0, |value| value <= 3);
            node!(1, Type);
        }
        27 => {
            if scalar!(0, BOOL) == 1 {
                atom!(1);
            }
        }
        28 => {
            node!(0, Type);
            if scalar!(1, BOOL) == 1 {
                node!(2, Type);
            }
            if scalar!(3, BOOL) == 1 {
                node!(4, AtomList);
            }
            atom!(5);
        }
        29 => {
            node!(0, Type);
            node!(1, Type);
        }
        30 => {
            scalar!(0, |value| value <= 2);
            node!(1, Type);
        }
        40 => node!(0, Type),
        41 => match scalar!(0, |value| value <= 2) {
            0 => entity!(1),
            1 => node!(1, AtomList),
            2 => external!(1),
            _ => return Err(SemanticPlaneRecordError::RowGrammar),
        },
        42 => {
            node!(0, Type);
            node!(1, Type);
        }
        43 => {
            node!(0, Type);
            node!(1, Type);
            node!(2, Type);
            node!(3, Type);
            scalar!(4, BOOL);
        }
        44 => {
            atom!(0);
            node!(1, Type);
            if scalar!(2, BOOL) == 1 {
                node!(3, Type);
            }
            node!(4, Type);
            scalar!(5, MODIFIER);
            scalar!(6, MODIFIER);
        }
        45 => {
            atom!(0);
            if scalar!(1, BOOL) == 1 {
                node!(2, Type);
            }
        }
        46 => node!(0, TemplateParts),
        47 => {
            atom!(0);
            node!(1, AtomList);
            node!(2, TypeList);
        }
        48 => node!(0, Type),
        49 => {}
        60 => {
            scalar!(0, |value| value <= 7);
            if scalar!(1, BOOL) == 1 {
                atom!(2);
            }
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    }
    Ok(())
}

fn validate_list_node(
    tag: u8,
    edges: &mut TypedWireCursor<'_>,
    references: &mut Vec<TypesReferenceV2>,
    _declaration_references: &mut Vec<[u8; 32]>,
) -> Result<(), SemanticPlaneRecordError> {
    use TypesRowDomainV2::{Type, TypeParameterBounds};
    let mut expected_index = 0_u32;
    while !edges.is_empty() {
        if edges.peek_index() != Some(expected_index) {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        match tag {
            TYPE_LIST_TAG => edges.node(2, expected_index, Type, references)?,
            ATOM_LIST_TAG => edges.atom(2, expected_index, references)?,
            TUPLE_ELEMENTS_TAG => {
                if edges.scalar(3, expected_index, |value| value <= 1)? == 1 {
                    edges.atom(3, expected_index, references)?;
                }
                edges.node(4, expected_index, Type, references)?;
                edges.scalar(5, expected_index, |value| value <= 2)?;
            }
            OBJECT_MEMBERS_TAG => match edges.scalar(6, expected_index, |value| value <= 4)? {
                0 => {
                    validate_object_key(edges, expected_index, references)?;
                    edges.node(8, expected_index, Type, references)?;
                    edges.scalar(9, expected_index, |value| value <= 1)?;
                    edges.scalar(10, expected_index, |value| value <= 1)?;
                }
                1 => {
                    validate_object_key(edges, expected_index, references)?;
                    edges.node(8, expected_index, Type, references)?;
                    edges.scalar(9, expected_index, |value| value <= 1)?;
                }
                2 => {
                    edges.node(7, expected_index, Type, references)?;
                    edges.scalar(10, expected_index, |value| value <= 1)?;
                    edges.atom(11, expected_index, references)?;
                    edges.node(12, expected_index, Type, references)?;
                }
                3 | 4 => edges.node(8, expected_index, Type, references)?,
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            },
            TEMPLATE_PARTS_TAG => match edges.scalar(13, expected_index, |value| value <= 1)? {
                0 => edges.atom(13, expected_index, references)?,
                1 => edges.node(13, expected_index, Type, references)?,
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            },
            TYPE_PARAMETERS_TAG => {
                edges.atom(14, expected_index, references)?;
                edges.node(15, expected_index, TypeParameterBounds, references)?;
                if edges.scalar(16, expected_index, |value| value <= 1)? == 1 {
                    edges.node(16, expected_index, Type, references)?;
                }
                edges.scalar(17, expected_index, |value| value <= 3)?;
                match edges.scalar(18, expected_index, |value| value <= 2)? {
                    0 => {
                        edges.scalar(18, expected_index, |value| value <= 1)?;
                    }
                    1 => edges.node(18, expected_index, Type, references)?,
                    2 => {}
                    _ => return Err(SemanticPlaneRecordError::RowGrammar),
                }
                edges.scalar(19, expected_index, |value| value <= 6)?;
                edges.scalar(20, expected_index, |value| value <= 1)?;
                edges.scalar(21, expected_index, |value| value <= 1)?;
            }
            TYPE_PARAMETER_BOUNDS_TAG => {
                match edges.scalar(22, expected_index, |value| value <= 1)? {
                    0 => edges.node(23, expected_index, Type, references)?,
                    1 => edges.atom(23, expected_index, references)?,
                    _ => return Err(SemanticPlaneRecordError::RowGrammar),
                }
            }
            FREE_PREDICATES_TAG => {
                edges.node(24, expected_index, Type, references)?;
                edges.node(25, expected_index, TypeParameterBounds, references)?;
            }
            _ => return Err(SemanticPlaneRecordError::RowGrammar),
        }
        expected_index = expected_index
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    }
    Ok(())
}

fn validate_object_key(
    edges: &mut TypedWireCursor<'_>,
    index: u32,
    references: &mut Vec<TypesReferenceV2>,
) -> Result<(), SemanticPlaneRecordError> {
    match edges.scalar(7, index, |value| value <= 3)? {
        0..=2 => edges.atom(7, index, references),
        3 => edges.node(7, index, TypesRowDomainV2::Type, references),
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn domain_for_tag(tag: u8) -> Result<TypesRowDomainV2, SemanticPlaneRecordError> {
    Ok(match tag {
        TYPE_TAG => TypesRowDomainV2::Type,
        TYPE_LIST_TAG => TypesRowDomainV2::TypeList,
        TUPLE_ELEMENTS_TAG => TypesRowDomainV2::TupleElements,
        OBJECT_MEMBERS_TAG => TypesRowDomainV2::ObjectMembers,
        TEMPLATE_PARTS_TAG => TypesRowDomainV2::TemplateParts,
        ATOM_LIST_TAG => TypesRowDomainV2::AtomList,
        TYPE_PARAMETERS_TAG => TypesRowDomainV2::TypeParameters,
        TYPE_PARAMETER_BOUNDS_TAG => TypesRowDomainV2::TypeParameterBounds,
        FREE_PREDICATES_TAG => TypesRowDomainV2::FreePredicates,
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    })
}

impl TypesRowDomainV2 {
    const fn is_typed_node(self) -> bool {
        matches!(
            self,
            Self::Type
                | Self::TypeList
                | Self::TupleElements
                | Self::ObjectMembers
                | Self::TemplateParts
                | Self::AtomList
                | Self::TypeParameters
                | Self::TypeParameterBounds
                | Self::FreePredicates
        )
    }
}

fn root_key_from_bytes(identity: [u8; 32]) -> [u8; 32] {
    identity
}

struct RowCursor<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}
impl<'bytes> RowCursor<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, length: usize) -> Result<&'bytes [u8], SemanticPlaneRecordError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(SemanticPlaneRecordError::Truncated)?;
        self.offset = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, SemanticPlaneRecordError> {
        Ok(*self
            .take(1)?
            .first()
            .ok_or(SemanticPlaneRecordError::Truncated)?)
    }
    fn u16(&mut self) -> Result<u16, SemanticPlaneRecordError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?,
        ))
    }
    fn u32(&mut self) -> Result<u32, SemanticPlaneRecordError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?,
        ))
    }
    fn bytes32(&mut self) -> Result<&'bytes [u8], SemanticPlaneRecordError> {
        let length =
            usize::try_from(self.u32()?).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        self.take(length)
    }
    fn finish(self) -> Result<(), SemanticPlaneRecordError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(SemanticPlaneRecordError::RowTrailingBytes)
        }
    }
}

fn external_identity_from_payload(
    payload: &[u8],
) -> Result<([u8; 32], Vec<TypesReferenceV2>), SemanticPlaneRecordError> {
    let mut cursor = RowCursor::new(payload);
    let mut hasher = blake3::Hasher::new();
    let mut references = Vec::new();
    hasher.update(b"compiler-ir.external-target.v1\0");
    match cursor.u8()? {
        0 => {
            hasher.update(&[0]);
            hasher.update(cursor.take(32)?);
            hasher.update(cursor.take(32)?);
        }
        1 => {
            hasher.update(&[1]);
            hasher.update(cursor.take(16)?);
            match cursor.u8()? {
                0 => hasher.update(&[0]),
                1 => {
                    hasher.update(&[1]);
                    hasher.update(cursor.take(16)?);
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
            match cursor.u8()? {
                0 => {
                    hasher.update(&[0]);
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                }
                1 => {
                    hasher.update(&[1]);
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                }
                2 => {
                    hasher.update(&[2]);
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                }
                3 => {
                    hasher.update(&[3]);
                    hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
            hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
            hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
            match cursor.u8()? {
                0 => hasher.update(&[0]),
                1 => {
                    hasher.update(&[1]);
                    let kind = cursor.u16()?;
                    if crate::ir::ItemKind::try_from(kind).is_err() {
                        return Err(SemanticPlaneRecordError::RowGrammar);
                    }
                    hasher.update(&kind.to_be_bytes());
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
        }
        2 => {
            hasher.update(&[2]);
            hasher.update(cursor.take(32)?);
            hasher.update(cursor.take(4)?);
            hash_external_atom_cell(&mut cursor, &mut hasher, &mut references)?;
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    }
    cursor.finish()?;
    Ok((*hasher.finalize().as_bytes(), references))
}

fn hash_external_atom_cell(
    cursor: &mut RowCursor<'_>,
    hasher: &mut blake3::Hasher,
    references: &mut Vec<TypesReferenceV2>,
) -> Result<(), SemanticPlaneRecordError> {
    let bytes = cursor.bytes32()?;
    let length = u64::try_from(bytes.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    hasher.update(&length.to_be_bytes());
    hasher.update(bytes);
    references
        .try_reserve(1)
        .map_err(SemanticPlaneRecordError::Allocation)?;
    references.push(TypesReferenceV2 {
        domain: TypesRowDomainV2::Atom,
        key: atom_key(bytes)?,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_types_catalog_rejects_missing_extension_roots() {
        let catalog =
            CheckedTypesFamilyV2::from_records(core::iter::empty::<([u8; 32], u8, &[u8])>())
                .expect("an explicit empty Types family is valid");
        let missing_parameters = TypesReferenceV2 {
            domain: TypesRowDomainV2::TypeParameters,
            key: [0x31; 32],
        };
        let missing_free_predicates = TypesReferenceV2 {
            domain: TypesRowDomainV2::FreePredicates,
            key: [0x42; 32],
        };

        assert!(matches!(
            catalog.require_reference(missing_parameters),
            Err(SemanticPlaneRecordError::ReaderReference)
        ));
        assert!(matches!(
            catalog.require_reference(missing_free_predicates),
            Err(SemanticPlaneRecordError::ReaderReference)
        ));
    }

    #[test]
    fn extension_type_parameter_and_free_predicate_refs_keep_their_domains() {
        let payload = [0_u8, 0, 0, 0];
        let key = typed_row_key(TYPE_PARAMETERS_TAG, &payload);
        let catalog =
            CheckedTypesFamilyV2::from_records([(key, TYPE_PARAMETERS_TAG, &payload[..])])
                .expect("an explicit empty parameter list is a complete row");

        assert!(
            catalog
                .require_reference(TypesReferenceV2 {
                    domain: TypesRowDomainV2::TypeParameters,
                    key,
                })
                .is_ok()
        );
        assert!(matches!(
            catalog.require_reference(TypesReferenceV2 {
                domain: TypesRowDomainV2::FreePredicates,
                key,
            }),
            Err(SemanticPlaneRecordError::ReaderReference)
        ));
    }

    #[test]
    fn typed_child_references_require_the_exact_domain() {
        let empty_tuple = [0_u8, 0, 0, 0];
        let tuple_key = typed_row_key(TUPLE_ELEMENTS_TAG, &empty_tuple);
        let mut list_payload = Vec::new();
        list_payload.extend_from_slice(&1_u32.to_be_bytes());
        list_payload.push(2);
        list_payload.extend_from_slice(&0_u32.to_be_bytes());
        list_payload.push(0);
        list_payload.extend_from_slice(&tuple_key);
        let list_key = typed_row_key(TYPE_LIST_TAG, &list_payload);
        let mut records = vec![
            (tuple_key, TUPLE_ELEMENTS_TAG, &empty_tuple[..]),
            (list_key, TYPE_LIST_TAG, &list_payload[..]),
        ];
        records.sort_unstable_by_key(|(key, _, _)| *key);

        assert!(matches!(
            CheckedTypesFamilyV2::from_records(records),
            Err(SemanticPlaneRecordError::ReaderReference)
        ));
    }

    #[test]
    fn fragment_external_identity_commits_its_kind_and_atom_rows() {
        let display = b"remote declaration";
        let mut payload = Vec::new();
        payload.push(2);
        payload.extend_from_slice(&[0x31; 32]);
        payload.extend_from_slice(&7_u32.to_be_bytes());
        put_bytes(&mut payload, display).expect("short test atom fits u32");

        let mut expected = blake3::Hasher::new();
        expected.update(b"compiler-ir.external-target.v1\0");
        expected.update(&[2]);
        expected.update(&[0x31; 32]);
        expected.update(&7_u32.to_be_bytes());
        expected.update(&(display.len() as u64).to_be_bytes());
        expected.update(display);
        let expected_key = *expected.finalize().as_bytes();
        let (actual_key, references) =
            external_identity_from_payload(&payload).expect("valid fragment target");
        assert_eq!(actual_key, expected_key);
        let expected_references = [TypesReferenceV2 {
            domain: TypesRowDomainV2::Atom,
            key: atom_key(display).expect("short test atom fits u32"),
        }];
        assert_eq!(references.as_slice(), expected_references.as_slice());

        let mut atom_payload = Vec::new();
        put_bytes(&mut atom_payload, display).expect("short test atom fits u32");
        let atom_row_key = atom_key(display).expect("short test atom fits u32");
        let mut records = vec![
            (expected_key, EXTERNAL_TARGET_TAG, &payload[..]),
            (atom_row_key, ATOM_TAG, &atom_payload[..]),
        ];
        records.sort_unstable_by_key(|(key, _, _)| *key);
        let catalog = CheckedTypesFamilyV2::from_records(records)
            .expect("external endpoint atom must be present in the closure");
        assert_eq!(catalog.external_target_keys(), &[expected_key]);
    }

    #[test]
    fn stable_external_identity_commits_its_kind() {
        let fragment = [0x61; 32];
        let declaration = [0x72; 32];
        let mut payload = Vec::new();
        payload.push(0);
        payload.extend_from_slice(&fragment);
        payload.extend_from_slice(&declaration);

        let mut expected = blake3::Hasher::new();
        expected.update(b"compiler-ir.external-target.v1\0");
        expected.update(&[0]);
        expected.update(&fragment);
        expected.update(&declaration);
        assert_eq!(
            external_identity_from_payload(&payload)
                .expect("valid stable target")
                .0,
            *expected.finalize().as_bytes()
        );
    }

    #[test]
    fn foreign_external_identity_commits_origin_and_all_atom_rows() {
        let atoms: [&[u8]; 3] = [b"ecosystem", b"pkg/path", b"display"];
        let mut payload = Vec::new();
        payload.push(1);
        payload.extend_from_slice(&[0x51; 16]);
        payload.push(0); // variant unavailable
        payload.push(3); // unspecified origin
        for atom in atoms {
            put_bytes(&mut payload, atom).expect("short test atom fits u32");
        }
        payload.push(0); // item kind unavailable

        let mut expected = blake3::Hasher::new();
        expected.update(b"compiler-ir.external-target.v1\0");
        expected.update(&[1]);
        expected.update(&[0x51; 16]);
        expected.update(&[0]);
        expected.update(&[3]);
        for atom in atoms {
            expected.update(&(atom.len() as u64).to_be_bytes());
            expected.update(atom);
        }
        expected.update(&[0]);
        let expected_key = *expected.finalize().as_bytes();
        let (actual_key, references) =
            external_identity_from_payload(&payload).expect("valid foreign target");
        assert_eq!(actual_key, expected_key);
        let expected_references = atoms.map(|bytes| TypesReferenceV2 {
            domain: TypesRowDomainV2::Atom,
            key: atom_key(bytes).expect("short test atom fits u32"),
        });
        assert_eq!(references.as_slice(), expected_references.as_slice());
    }
}

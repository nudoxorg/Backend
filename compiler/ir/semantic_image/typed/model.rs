//! Typed dependency graph model and measured scratch geometry.

use alloc::vec::Vec;
use core::fmt;

use crate::{
    ArenaRange, AtomId, AtomListId, EntityId, ExternalId, Ir,
    ObjectMemberListId, TemplatePartListId, TupleElementListId, TypeId,
    TypeListId, TypeParameterBoundListId, TypeParameterListId,
};

/// A closed graph vertex; each list domain remains distinct even though each
/// one ultimately contains type-bearing semantic values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypedPlanNode {
    Type(TypeId),
    TypeList(TypeListId),
    TupleElements(TupleElementListId),
    ObjectMembers(ObjectMemberListId),
    TemplateParts(TemplatePartListId),
    AtomList(AtomListId),
    TypeParameters(TypeParameterListId),
    TypeParameterBounds(TypeParameterBoundListId),
}

impl TypedPlanNode {
    pub(crate) const fn raw(self) -> u32 {
        match self {
            Self::Type(value) => value.raw,
            Self::TypeList(value) => value.raw,
            Self::TupleElements(value) => value.raw,
            Self::ObjectMembers(value) => value.raw,
            Self::TemplateParts(value) => value.raw,
            Self::AtomList(value) => value.raw,
            Self::TypeParameters(value) => value.raw,
            Self::TypeParameterBounds(value) => value.raw,
        }
    }

    pub(crate) const fn domain(self) -> TypedPlanDomain {
        match self {
            Self::Type(_) => TypedPlanDomain::Type,
            Self::TypeList(_) => TypedPlanDomain::TypeList,
            Self::TupleElements(_) => TypedPlanDomain::TupleElements,
            Self::ObjectMembers(_) => TypedPlanDomain::ObjectMembers,
            Self::TemplateParts(_) => TypedPlanDomain::TemplateParts,
            Self::AtomList(_) => TypedPlanDomain::AtomList,
            Self::TypeParameters(_) => TypedPlanDomain::TypeParameters,
            Self::TypeParameterBounds(_) => TypedPlanDomain::TypeParameterBounds,
        }
    }

    pub(crate) const fn from_domain_raw(domain: TypedPlanDomain, raw: u32) -> Self {
        match domain {
            TypedPlanDomain::Type => Self::Type(TypeId::new(raw)),
            TypedPlanDomain::TypeList => Self::TypeList(TypeListId::new(raw)),
            TypedPlanDomain::TupleElements => Self::TupleElements(TupleElementListId::new(raw)),
            TypedPlanDomain::ObjectMembers => Self::ObjectMembers(ObjectMemberListId::new(raw)),
            TypedPlanDomain::TemplateParts => Self::TemplateParts(TemplatePartListId::new(raw)),
            TypedPlanDomain::AtomList => Self::AtomList(AtomListId::new(raw)),
            TypedPlanDomain::TypeParameters => Self::TypeParameters(TypeParameterListId::new(raw)),
            TypedPlanDomain::TypeParameterBounds => {
                Self::TypeParameterBounds(TypeParameterBoundListId::new(raw))
            }
        }
    }
}

impl TypedPlanDomain {
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Type => 0,
            Self::TypeList => 1,
            Self::TupleElements => 2,
            Self::ObjectMembers => 3,
            Self::TemplateParts => 4,
            Self::AtomList => 5,
            Self::TypeParameters => 6,
            Self::TypeParameterBounds => 7,
        }
    }

    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Type => 0,
            Self::TypeList => 1,
            Self::TupleElements => 2,
            Self::ObjectMembers => 3,
            Self::TemplateParts => 4,
            Self::AtomList => 5,
            Self::TypeParameters => 6,
            Self::TypeParameterBounds => 7,
        }
    }
}

/// One stable typed-pool domain. The discriminant begins every local key,
/// so structurally similar rows from different pools cannot alias.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TypedPlanDomain {
    Type,
    TypeList,
    TupleElements,
    ObjectMembers,
    TemplateParts,
    AtomList,
    TypeParameters,
    TypeParameterBounds,
}

/// Exact typed-pool graph rejection. No cycle is converted into a synthetic
/// semantic type, and no raw coordinate becomes a canonical tie-breaker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypedPlanFault {
    GeometryOverflow {
        nodes: usize,
    },
    GeometrySumOverflow {
        accumulated: u32,
        additional: u32,
    },
    EdgeGeometryOverflow {
        edges: usize,
    },
    MissingNode {
        node: TypedPlanNode,
        count: u32,
    },
    EdgeOutOfBounds {
        from: TypedPlanNode,
        to: TypedPlanNode,
        count: u32,
    },
    AnonymousCycle {
        from: TypedPlanNode,
        to: TypedPlanNode,
    },
    DuplicateCanonicalKey {
        /// Equal raw rows violate finalized owned IR's acyclic
        /// `Interner`/`ListInterner` uniqueness invariant: those interners
        /// hash-cons exact child coordinates before this planner runs. Normal
        /// admitted IR never reaches this arm; it is retained so a malformed
        /// full-image input has no ordinal tie-breaker to manufacture a second
        /// canonical coordinate.
        first: TypedPlanNode,
        second: TypedPlanNode,
    },
    KeyLengthOverflow {
        node: TypedPlanNode,
    },
    ListIndexOverflow {
        node: TypedPlanNode,
        index: usize,
    },
    TerminalOutOfBounds {
        from: TypedPlanNode,
        target: TypedPlanTerminal,
        count: u32,
    },
}

impl fmt::Display for TypedPlanFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "typed semantic-image plan rejected: {self:?}")
    }
}
impl core::error::Error for TypedPlanFault {}

/// Semantic position retained on every dependency edge. The eventual key
/// writer frames this role before its target, so a reordered pair/triple or a
/// label/default/constraint change cannot collide with a different shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedEdgeRole {
    /// The closed `TypeExpr`/constructor discriminator.
    TypeTag,
    /// A constructor-owned scalar or child field. The enclosing type tag and
    /// field position jointly name pair/triple/quad positions without a raw
    /// packed-record interpretation.
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
}

/// A full semantic dependency target, never a raw untyped `u32`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedPlanTarget {
    Node(TypedPlanNode),
    Atom(AtomId),
    Entity(EntityId),
    External(ExternalId),
    Scalar(u64),
}

/// Terminal coordinate domain retained by a rejected edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypedPlanTerminal {
    Atom(AtomId),
    Entity(EntityId),
    External(ExternalId),
}

/// One source-ordered, role-bearing typed dependency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TypedPlanEdge {
    pub(crate) role: TypedEdgeRole,
    pub(crate) target: TypedPlanTarget,
}

/// A fixed domain-separated structural fingerprint. It is never itself a
/// canonical tie-breaker: equal values are resolved by an explicit iterative
/// comparator over the framed edge graph before ordering or rejection.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct TypedFingerprint(pub(crate) [u8; 32]);

impl TypedFingerprint {
    pub(crate) const ZERO: Self = Self([0; 32]);

    #[cfg(test)]
    pub(crate) const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Dense global-state bases for all typed graph domains. Every node maps to
/// one state/key slot in O(1); no per-edge search is permitted.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedPlanBases {
    types: u32,
    type_lists: u32,
    tuples: u32,
    objects: u32,
    templates: u32,
    atoms: u32,
    parameters: u32,
    bounds: u32,
    total: u32,
}

impl TypedPlanBases {
    pub(crate) fn new(counts: [u32; 8]) -> Result<Self, TypedPlanFault> {
        let mut bases = [0_u32; 8];
        let mut total = 0_u32;
        for (index, count) in counts.into_iter().enumerate() {
            bases[index] = total;
            total = total.checked_add(count).ok_or(TypedPlanFault::GeometrySumOverflow {
                accumulated: total,
                additional: count,
            })?;
        }
        Ok(Self {
            types: bases[0],
            type_lists: bases[1],
            tuples: bases[2],
            objects: bases[3],
            templates: bases[4],
            atoms: bases[5],
            parameters: bases[6],
            bounds: bases[7],
            total,
        })
    }

    pub(crate) const fn total(self) -> u32 { self.total }

    pub(crate) fn slot(self, node: TypedPlanNode, counts: [u32; 8]) -> Result<u32, TypedPlanFault> {
        let (base, count) = match node {
            TypedPlanNode::Type(_) => (self.types, counts[0]),
            TypedPlanNode::TypeList(_) => (self.type_lists, counts[1]),
            TypedPlanNode::TupleElements(_) => (self.tuples, counts[2]),
            TypedPlanNode::ObjectMembers(_) => (self.objects, counts[3]),
            TypedPlanNode::TemplateParts(_) => (self.templates, counts[4]),
            TypedPlanNode::AtomList(_) => (self.atoms, counts[5]),
            TypedPlanNode::TypeParameters(_) => (self.parameters, counts[6]),
            TypedPlanNode::TypeParameterBounds(_) => (self.bounds, counts[7]),
        };
        let raw = node.raw();
        if raw >= count {
            return Err(TypedPlanFault::MissingNode { node, count });
        }
        Ok(base + raw)
    }
}

/// Exact domain counts used for every global-state and remap lookup.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedPlanCounts {
    values: [u32; 8],
    node_capacity: usize,
}

impl TypedPlanCounts {
    pub(crate) fn from_ir(ir: &Ir) -> Result<Self, TypedPlanFault> {
        let columns = ir.storage_columns();
        let source = [
            columns.types.headers.len(),
            columns.type_lists.ranges.len(),
            columns.tuple_elements.ranges.len(),
            columns.object_members.ranges.len(),
            columns.template_parts.ranges.len(),
            columns.atom_lists.ranges.len(),
            columns.type_parameters.ranges.len(),
            columns.type_parameter_bounds.ranges.len(),
        ];
        let mut node_capacity = 0_usize;
        for count in source {
            node_capacity = node_capacity
                .checked_add(count)
                .ok_or(TypedPlanFault::GeometryOverflow { nodes: node_capacity })?;
        }
        let mut values = [0_u32; 8];
        for (index, count) in source.into_iter().enumerate() {
            values[index] = count_u32(count)?;
        }
        Ok(Self { values, node_capacity })
    }

    pub(crate) const fn as_array(self) -> [u32; 8] {
        self.values
    }

    pub(crate) const fn at(self, domain: TypedPlanDomain) -> u32 {
        self.values[domain.index()]
    }

    pub(crate) const fn node_count(self) -> usize {
        self.node_capacity
    }
}

fn count_u32(count: usize) -> Result<u32, TypedPlanFault> {
    u32::try_from(count).map_err(|_| TypedPlanFault::GeometryOverflow { nodes: count })
}

/// A compact explicit-stack frame. `next` walks one shared flat edge lane;
/// there is never a heap allocation per child or recursive process stack.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedPlanFrame {
    pub(crate) node: TypedPlanNode,
    pub(crate) edge_start: u32,
    pub(crate) edge_end: u32,
    pub(crate) next: u32,
}

impl TypedPlanFrame {
    pub(crate) const fn new(node: TypedPlanNode, edge_start: u32, edge_end: u32) -> Self {
        Self {
            node,
            edge_start,
            edge_end,
            next: edge_start,
        }
    }
}

/// Scratch owned exactly by the current full-image planning transaction.
///
/// Finalized rows fill one flat edge lane, then explicit postorder produces
/// framed local keys and collision-checked canonical coordinates. This shape
/// establishes the O(nodes + edges) storage law before any wire directory can
/// consume the result.
pub(crate) struct TypedPlanScratch {
    pub(crate) nodes: Vec<TypedPlanNode>,
    pub(crate) edge_offsets: Vec<u32>,
    pub(crate) edges: Vec<TypedPlanEdge>,
    pub(crate) states: Vec<u8>,
    pub(crate) stack: Vec<TypedPlanFrame>,
    pub(crate) postorder: Vec<TypedPlanNode>,
    pub(crate) key_ranges: Vec<ArenaRange>,
    pub(crate) key_bytes: Vec<u8>,
    pub(crate) fingerprints: Vec<TypedFingerprint>,
    pub(crate) canonical_slots: Vec<u32>,
    pub(crate) canonical_nodes: Vec<TypedPlanNode>,
}

impl TypedPlanScratch {
    pub(crate) fn with_capacity(nodes: usize, edges: usize) -> Result<Self, TypedPlanFault> {
        let offsets = nodes.checked_add(1).ok_or(TypedPlanFault::GeometryOverflow { nodes })?;
        Ok(Self {
            nodes: Vec::with_capacity(nodes),
            edge_offsets: Vec::with_capacity(offsets),
            edges: Vec::with_capacity(edges),
            states: Vec::with_capacity(nodes),
            stack: Vec::new(),
            postorder: Vec::with_capacity(nodes),
            key_ranges: Vec::with_capacity(nodes),
            key_bytes: Vec::new(),
            fingerprints: Vec::with_capacity(nodes),
            canonical_slots: Vec::with_capacity(nodes),
            canonical_nodes: Vec::with_capacity(nodes),
        })
    }
}

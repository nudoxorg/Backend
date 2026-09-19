use super::*;

/// A labeled symbolic edge inside an SCC/component descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ComponentEdge {
    /// Source member identity.
    from: EntityId,
    /// Target member identity.
    to: EntityId,
    /// Edge kind.
    kind: EdgeKind,
}

impl ComponentEdge {
    /// Creates one labeled edge candidate for a component descriptor.
    #[must_use]
    pub const fn new(from: EntityId, to: EntityId, kind: EdgeKind) -> Self {
        Self { from, to, kind }
    }

    /// Returns the source member.
    #[must_use]
    pub const fn from(self) -> EntityId {
        self.from
    }

    /// Returns the target member.
    #[must_use]
    pub const fn to(self) -> EntityId {
        self.to
    }

    /// Returns the edge label.
    #[must_use]
    pub const fn kind(self) -> EdgeKind {
        self.kind
    }
}

/// An exact external facet/version input to a component computation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ComponentInput {
    /// Logical entity read by the component recipe.
    entity: EntityId,
    /// Facet read by the recipe.
    facet: FacetKind,
    /// Complete value version observed by the recipe.
    version: FacetVersion,
}

impl ComponentInput {
    /// Creates an exact input from the admitted facet value itself.
    #[must_use]
    pub fn new(entity: EntityId, value: FacetValue) -> Self {
        let facet = value.facet();
        let version = ObjectVersion::from_value(&value);
        drop(value);
        Self {
            entity,
            facet,
            version,
        }
    }

    /// Returns the logical entity read by the component recipe.
    #[must_use]
    pub const fn entity(self) -> EntityId {
        self.entity
    }

    /// Returns the facet family read by the component recipe.
    #[must_use]
    pub const fn facet(self) -> FacetKind {
        self.facet
    }

    /// Returns the exact admitted value version.
    #[must_use]
    pub const fn version(self) -> FacetVersion {
        self.version
    }
}

/// Canonical finite SCC descriptor. The vectors are encoded sorted, so the
/// identity is independent of discovery order. Component versions are derived
/// values, not stable entity keys.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ComponentDescriptor {
    /// Optional graph relation root that supplied the SCC.
    graph_root: Option<TypeRelationRoot>,
    /// Typed recipe version used for component computation.
    recipe: Option<RecipeVersion>,
    /// Stable member identities.
    members: Arc<[EntityId]>,
    /// Labeled internal symbolic edges.
    internal_edges: Arc<[ComponentEdge]>,
    /// Exact external facet/version inputs.
    external_inputs: Arc<[ComponentInput]>,
}

impl ComponentDescriptor {
    /// Creates a descriptor without optional graph/recipe basis bindings.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidComponent`] for an empty or duplicate
    /// member set, duplicate inputs/edges, or an edge whose endpoints are not
    /// members of the component.
    pub fn new(
        members: Vec<EntityId>,
        internal_edges: Vec<ComponentEdge>,
        external_inputs: Vec<ComponentInput>,
    ) -> Result<Self, SemanticError> {
        Self::admit(None, None, members, internal_edges, external_inputs)
    }

    /// Creates a descriptor with exact graph-root and recipe bindings.
    /// Both bindings are required together so an SCC cannot be detached from
    /// the recipe and graph state that define its identity.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidComponent`] when the descriptor data
    /// violates the component membership or basis binding laws.
    pub fn with_basis(
        graph_root: TypeRelationRoot,
        recipe: RecipeVersion,
        members: Vec<EntityId>,
        internal_edges: Vec<ComponentEdge>,
        external_inputs: Vec<ComponentInput>,
    ) -> Result<Self, SemanticError> {
        Self::admit(
            Some(graph_root),
            Some(recipe),
            members,
            internal_edges,
            external_inputs,
        )
    }

    fn admit(
        graph_root: Option<TypeRelationRoot>,
        recipe: Option<RecipeVersion>,
        mut members: Vec<EntityId>,
        mut internal_edges: Vec<ComponentEdge>,
        mut external_inputs: Vec<ComponentInput>,
    ) -> Result<Self, SemanticError> {
        if members.is_empty() || graph_root.is_some() != recipe.is_some() {
            return Err(SemanticError::InvalidComponent);
        }
        members.sort();
        if members.windows(2).any(|window| window[0] == window[1]) {
            return Err(SemanticError::InvalidComponent);
        }
        internal_edges.sort();
        if internal_edges
            .windows(2)
            .any(|window| window[0] == window[1])
            || internal_edges.iter().any(|edge| {
                members.binary_search(&edge.from()).is_err()
                    || members.binary_search(&edge.to()).is_err()
            })
        {
            return Err(SemanticError::InvalidComponent);
        }
        if members.len() > 1 {
            let mut forward = vec![Vec::new(); members.len()];
            let mut reverse = vec![Vec::new(); members.len()];
            for edge in &internal_edges {
                let from = members
                    .binary_search(&edge.from())
                    .map_err(|_| SemanticError::InvalidComponent)?;
                let to = members
                    .binary_search(&edge.to())
                    .map_err(|_| SemanticError::InvalidComponent)?;
                forward[from].push(to);
                reverse[to].push(from);
            }
            if !all_reachable(&forward) || !all_reachable(&reverse) {
                return Err(SemanticError::InvalidComponent);
            }
        }
        external_inputs.sort();
        if external_inputs.windows(2).any(|window| {
            (window[0].entity(), window[0].facet()) == (window[1].entity(), window[1].facet())
        }) {
            return Err(SemanticError::InvalidComponent);
        }
        Ok(Self {
            graph_root,
            recipe,
            members: Arc::from(members),
            internal_edges: Arc::from(internal_edges),
            external_inputs: Arc::from(external_inputs),
        })
    }

    /// Returns the graph relation root used for this SCC, when bound.
    #[must_use]
    pub const fn graph_root(&self) -> Option<TypeRelationRoot> {
        self.graph_root
    }

    /// Returns the recipe version used for this SCC, when bound.
    #[must_use]
    pub const fn recipe(&self) -> Option<RecipeVersion> {
        self.recipe
    }

    /// Returns sorted, unique member identities.
    #[must_use]
    pub fn members(&self) -> &[EntityId] {
        &self.members
    }

    /// Returns sorted, unique internal edges.
    #[must_use]
    pub fn internal_edges(&self) -> &[ComponentEdge] {
        &self.internal_edges
    }

    /// Returns sorted, unique external value inputs.
    #[must_use]
    pub fn external_inputs(&self) -> &[ComponentInput] {
        &self.external_inputs
    }

    /// Returns canonical bytes with order-insensitive descriptor collections.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_component(self)
    }

    /// Derives the component value version from its complete descriptor.
    #[must_use]
    pub fn version(&self) -> ComponentVersion {
        ObjectVersion::from_value(self)
    }
}

fn all_reachable(adjacency: &[Vec<usize>]) -> bool {
    if adjacency.is_empty() {
        return false;
    }
    let mut seen = vec![false; adjacency.len()];
    let mut stack = vec![0];
    let mut visited = 0;
    while let Some(index) = stack.pop() {
        if index >= adjacency.len() {
            return false;
        }
        if seen[index] {
            continue;
        }
        seen[index] = true;
        visited += 1;
        if visited == adjacency.len() {
            return true;
        }
        for &next in &adjacency[index] {
            if next >= adjacency.len() {
                return false;
            }
            if !seen[next] {
                stack.push(next);
            }
        }
    }
    false
}

/// Component relation value with coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentRecord {
    /// Complete descriptor when available.
    descriptor: Option<Arc<ComponentDescriptor>>,
    /// Component authority coverage.
    coverage: FacetCoverage,
    /// Authority/source/version basis.
    provenance: Provenance,
}

impl ComponentRecord {
    /// Admits a component row with a valid value/coverage state.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] when descriptor
    /// presence disagrees with coverage.
    pub fn new(
        descriptor: Option<ComponentDescriptor>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(descriptor.is_some(), coverage)?;
        Ok(Self {
            descriptor: descriptor.map(Arc::new),
            coverage,
            provenance,
        })
    }

    /// Returns the SCC descriptor, when live.
    #[must_use]
    pub fn descriptor(&self) -> Option<&ComponentDescriptor> {
        self.descriptor.as_deref()
    }

    /// Returns the checked coverage/state.
    #[must_use]
    pub const fn coverage(&self) -> FacetCoverage {
        self.coverage
    }

    /// Returns the bound provenance.
    #[must_use]
    pub const fn provenance(&self) -> Provenance {
        self.provenance
    }
}

/// Content identity of an SCC/component descriptor.
pub type ComponentVersion = ObjectVersion<ComponentSchema>;
/// Root of the typed graph relation used as an SCC basis.
pub type TypeRelationRoot = StateRoot<Types>;

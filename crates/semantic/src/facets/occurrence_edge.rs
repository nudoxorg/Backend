use super::*;

/// Link occurrence identity. An ordinal preserves equal repeated observations.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OccurrenceKey {
    /// Source evidence span.
    pub span: SourceSpan,
    /// Stable authority occurrence ordinal, including duplicates.
    pub ordinal: u32,
}

/// Link occurrence fact kept as a compact compatibility value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OccurrenceFact {
    /// Target declaration.
    pub target: EntityId,
    /// Source declaration.
    pub source: EntityId,
    /// Scoped observation identity.
    pub key: OccurrenceKey,
    /// Occurrence multiplicity.
    pub multiplicity: u32,
    /// Evidence support/confidence weight.
    pub support: u16,
}

/// Globally scoped occurrence address used to derive a stable logical key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OccurrenceAddress {
    /// Source declaration.
    pub source: EntityId,
    /// Target declaration.
    pub target: EntityId,
    /// Authority observation identity.
    pub key: OccurrenceKey,
}

impl OccurrenceFact {
    /// Returns the complete scoped identity of this observation.
    #[must_use]
    pub fn address(&self) -> OccurrenceAddress {
        OccurrenceAddress {
            source: self.source,
            target: self.target,
            key: self.key.clone(),
        }
    }

    /// Returns the stable logical occurrence key.
    #[must_use]
    pub fn id(&self) -> OccurrenceId {
        ObjectKey::from_value(&self.address())
    }
}

/// Occurrence relation value with per-observation coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OccurrenceRecord {
    /// Complete occurrence fact when live.
    value: Option<Arc<OccurrenceFact>>,
    /// Coverage and explicit deletion state.
    coverage: FacetCoverage,
    /// Authority/source/version basis.
    provenance: Provenance,
}

impl OccurrenceRecord {
    /// Admits an occurrence row with a valid value/coverage state.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] or
    /// [`SemanticError::InvalidSupport`] when the row is malformed.
    pub fn new(
        value: Option<OccurrenceFact>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(value.is_some(), coverage)?;
        if value
            .as_ref()
            .is_some_and(|occurrence| occurrence.multiplicity == 0 || occurrence.support == 0)
        {
            return Err(SemanticError::InvalidSupport);
        }
        Ok(Self {
            value: value.map(Arc::new),
            coverage,
            provenance,
        })
    }

    /// Returns the occurrence fact, when live.
    #[must_use]
    pub fn value(&self) -> Option<&OccurrenceFact> {
        self.value.as_deref()
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

/// Stable occurrence logical key.
pub type OccurrenceId = ObjectKey<OccurrenceSchema>;
/// Occurrence value version.
pub type OccurrenceVersion = ObjectVersion<OccurrenceValueSchema>;

/// Graph edge kind.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EdgeKind {
    /// A source/reference edge.
    Reference,
    /// A containment edge.
    Containment,
    /// A type dependency edge.
    TypeDependency,
    /// A call edge.
    Call,
}

pub(crate) fn edge_kind_tag(value: EdgeKind) -> u8 {
    match value {
        EdgeKind::Reference => 1,
        EdgeKind::Containment => 2,
        EdgeKind::TypeDependency => 3,
        EdgeKind::Call => 4,
    }
}

/// Stable graph edge key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EdgeKey {
    /// Source declaration.
    pub from: EntityId,
    /// Target declaration.
    pub to: EntityId,
    /// Edge relation kind.
    pub kind: EdgeKind,
}

/// Derived graph edge fact. Support is recomputed from surviving occurrences.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Edge {
    /// Source declaration.
    pub from: EntityId,
    /// Target declaration.
    pub to: EntityId,
    /// Edge relation kind.
    pub kind: EdgeKind,
    /// Sum of surviving occurrence multiplicities.
    pub multiplicity: u32,
    /// Sum of surviving occurrence support weights.
    pub support: u16,
}

impl Edge {
    /// Returns this edge's logical key.
    #[must_use]
    pub const fn key(&self) -> EdgeKey {
        EdgeKey {
            from: self.from,
            to: self.to,
            kind: self.kind,
        }
    }

    /// Returns the stable logical edge key.
    #[must_use]
    pub fn id(&self) -> EdgeId {
        ObjectKey::from_value(&self.key())
    }
}

/// Derived graph edge relation value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeRecord {
    /// Edge value when live.
    value: Option<Arc<Edge>>,
    /// Coverage and explicit deletion state.
    coverage: FacetCoverage,
    /// Authority/source/version basis for the derivation.
    provenance: Provenance,
}

impl EdgeRecord {
    /// Admits a derived edge row with a valid value/coverage state.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] or
    /// [`SemanticError::InvalidSupport`] when the row is malformed.
    pub fn new(
        value: Option<Edge>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(value.is_some(), coverage)?;
        if value
            .as_ref()
            .is_some_and(|edge| edge.multiplicity == 0 || edge.support == 0)
        {
            return Err(SemanticError::InvalidSupport);
        }
        Ok(Self {
            value: value.map(Arc::new),
            coverage,
            provenance,
        })
    }

    /// Returns the edge value, when live.
    #[must_use]
    pub fn value(&self) -> Option<&Edge> {
        self.value.as_deref()
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

/// Stable graph edge logical key.
pub type EdgeId = ObjectKey<EdgeSchema>;
/// Edge value version.
pub type EdgeVersion = ObjectVersion<EdgeValueSchema>;

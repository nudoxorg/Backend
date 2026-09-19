use super::*;

/// Semantic fact family. Each family has its own relation/schema marker.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FacetKind {
    /// Source bytes and file membership.
    Source,
    /// Declaration identity/header.
    Entity,
    /// Generic facet manifest row.
    Facet,
    /// Name facet.
    Name,
    /// Lexical token facet.
    Lexical,
    /// Visibility facet.
    Visibility,
    /// Documentation facet.
    Documentation,
    /// Signature facet.
    Signature,
    /// Attribute facet.
    Attributes,
    /// Generic parameter facet.
    Generics,
    /// Constraint facet.
    Constraints,
    /// Rich type facet.
    Type,
    /// Recursive component/SCC facet.
    Component,
    /// Ordered member facet.
    Member,
    /// Link occurrence evidence.
    Occurrence,
    /// Derived graph edge.
    Edge,
    /// Language extension facet.
    Extension,
    /// Embedding input facet.
    EmbeddingInput,
    /// Documentation link facet.
    DocLink,
    /// Source span evidence facet.
    SourceSpan,
    /// Authority/provenance facet.
    Authority,
    /// Configuration/environment facet.
    Configuration,
    /// Generated source/input facet.
    GeneratedSource,
}

impl FacetKind {
    /// All registered semantic families in canonical tag order.
    pub const ALL: [Self; 23] = [
        Self::Source,
        Self::Entity,
        Self::Facet,
        Self::Name,
        Self::Lexical,
        Self::Visibility,
        Self::Documentation,
        Self::Signature,
        Self::Attributes,
        Self::Generics,
        Self::Constraints,
        Self::Type,
        Self::Component,
        Self::Member,
        Self::Occurrence,
        Self::Edge,
        Self::Extension,
        Self::EmbeddingInput,
        Self::DocLink,
        Self::SourceSpan,
        Self::Authority,
        Self::Configuration,
        Self::GeneratedSource,
    ];

    /// Stable schema tag.
    #[must_use]
    pub const fn tag(self) -> u16 {
        facet_tag(self)
    }
}

/// Generic facet key for the facet manifest relation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FacetKey {
    /// Entity owning the facet.
    entity: EntityId,
    /// Facet family.
    facet: FacetKind,
}

impl FacetKey {
    /// Creates a logical key for one entity/facet family pair.
    #[must_use]
    pub const fn new(entity: EntityId, facet: FacetKind) -> Self {
        Self { entity, facet }
    }

    /// Returns the owning entity identity.
    #[must_use]
    pub const fn entity(self) -> EntityId {
        self.entity
    }

    /// Returns the facet family.
    #[must_use]
    pub const fn facet(self) -> FacetKind {
        self.facet
    }
}

/// Opaque canonical payload used by the facet manifest relation.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FacetValue {
    facet: FacetKind,
    bytes: Arc<[u8]>,
}

impl FacetValue {
    /// Creates a canonical payload for one semantic facet family.
    #[must_use]
    pub fn new(facet: FacetKind, bytes: Vec<u8>) -> Self {
        Self {
            facet,
            bytes: Arc::from(bytes),
        }
    }

    /// Reuses an already admitted immutable payload allocation.
    #[must_use]
    pub fn from_shared(facet: FacetKind, bytes: Arc<[u8]>) -> Self {
        Self { facet, bytes }
    }

    /// Returns the represented facet family.
    #[must_use]
    pub const fn facet(&self) -> FacetKind {
        self.facet
    }

    /// Returns canonical payload bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the immutable canonical payload allocation for zero-copy
    /// retention by versioned records and transport envelopes.
    #[must_use]
    pub fn shared_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }

    /// Consumes the value while preserving the payload allocation.
    #[must_use]
    pub fn into_shared_bytes(self) -> Arc<[u8]> {
        self.bytes
    }

    /// Consumes the value and returns its payload for compatibility callers.
    /// The borrowed [`bytes`](Self::bytes) and [`shared_bytes`](Self::shared_bytes)
    /// accessors are the zero-copy APIs; converting an unsized shared slice
    /// into a `Vec` necessarily copies.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.to_vec()
    }
}

/// Version of a facet payload, separate from its logical key.
pub type FacetVersion = ObjectVersion<FacetValueSchema>;

/// Generic facet manifest relation value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FacetRecord {
    key: FacetKey,
    /// Immutable admitted payload retained beside its content version.
    /// Keeping the object available lets local-first readers reuse bytes
    /// without reopening storage from a digest-only compatibility row.
    value: Option<Arc<FacetValue>>,
    value_version: Option<FacetVersion>,
    coverage: FacetCoverage,
    provenance: Provenance,
}

impl FacetRecord {
    /// Admits a live facet record from its typed value and complete witness.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidFacetBinding`] when the value's facet
    /// family differs from the logical key's family.
    pub fn present(
        key: FacetKey,
        value: FacetValue,
        coverage: AuthorizedCompleteCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        if value.facet() != key.facet() {
            return Err(SemanticError::InvalidFacetBinding);
        }
        let value_version = ObjectVersion::from_value(&value);
        Ok(Self {
            key,
            value: Some(Arc::new(value)),
            value_version: Some(value_version),
            coverage: FacetCoverage::complete(coverage),
            provenance,
        })
    }

    /// Constructs a complete captured-empty facet record.
    #[must_use]
    pub const fn captured_empty(
        key: FacetKey,
        coverage: AuthorizedCompleteCoverage,
        provenance: Provenance,
    ) -> Self {
        Self {
            key,
            value: None,
            value_version: None,
            coverage: FacetCoverage::captured_empty(coverage),
            provenance,
        }
    }

    /// Constructs a complete absence facet record.
    #[must_use]
    pub const fn absent(
        key: FacetKey,
        coverage: AuthorizedCompleteCoverage,
        provenance: Provenance,
    ) -> Self {
        Self {
            key,
            value: None,
            value_version: None,
            coverage: FacetCoverage::absent(coverage),
            provenance,
        }
    }

    /// Constructs a complete deletion facet record.
    #[must_use]
    pub const fn deleted(
        key: FacetKey,
        coverage: AuthorizedCompleteCoverage,
        provenance: Provenance,
    ) -> Self {
        Self {
            key,
            value: None,
            value_version: None,
            coverage: FacetCoverage::deleted(coverage),
            provenance,
        }
    }

    /// Returns the logical facet key.
    #[must_use]
    pub const fn key(&self) -> FacetKey {
        self.key
    }

    /// Returns the typed payload version, when live.
    #[must_use]
    pub const fn value_version(&self) -> Option<FacetVersion> {
        self.value_version
    }

    /// Returns the admitted immutable payload when this row is live.
    #[must_use]
    pub fn value(&self) -> Option<&FacetValue> {
        self.value.as_deref()
    }

    /// Returns the retained payload allocation for zero-copy consumers.
    #[must_use]
    pub fn shared_value(&self) -> Option<Arc<FacetValue>> {
        self.value.as_ref().map(Arc::clone)
    }

    /// Returns the checked coverage and explicit state.
    #[must_use]
    pub const fn coverage(&self) -> FacetCoverage {
        self.coverage
    }

    /// Returns the explicit value/deletion state.
    #[must_use]
    pub const fn deletion(&self) -> Deletion {
        self.coverage.deletion()
    }

    /// Returns the authority/source/version provenance.
    #[must_use]
    pub const fn provenance(&self) -> Provenance {
        self.provenance
    }
}

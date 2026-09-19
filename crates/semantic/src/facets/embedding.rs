use super::*;

/// Embedding model identity and immutable configuration.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingModel {
    /// Provider/model namespace.
    provider: String,
    /// Immutable provider revision.
    revision: String,
    /// Output dimension.
    dimensions: u32,
    /// Metric/normalization recipe bytes.
    metric: Vec<u8>,
}

impl EmbeddingModel {
    /// Admits an immutable embedding model configuration.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidValue`] for an empty identity or zero
    /// output dimension.
    pub fn new(
        provider: impl Into<String>,
        revision: impl Into<String>,
        dimensions: u32,
        metric: Vec<u8>,
    ) -> Result<Self, SemanticError> {
        let provider = provider.into();
        let revision = revision.into();
        if provider.is_empty() || revision.is_empty() || dimensions == 0 {
            return Err(SemanticError::InvalidValue);
        }
        Ok(Self {
            provider,
            revision,
            dimensions,
            metric,
        })
    }

    /// Returns the provider/model namespace.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Returns the immutable provider revision.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Returns the output dimension.
    #[must_use]
    pub const fn dimensions(&self) -> u32 {
        self.dimensions
    }

    /// Returns the metric/normalization bytes.
    #[must_use]
    pub fn metric(&self) -> &[u8] {
        &self.metric
    }
}

/// Embedding input compatibility key.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingInput {
    /// Entity being embedded.
    entity: EntityId,
    /// Exact complete entity value read as input.
    input_version: EntityVersion,
    /// Compact compatibility model identifier.
    model: u16,
}

impl EmbeddingInput {
    /// Creates an exact embedding input identity.
    #[must_use]
    pub const fn new(entity: EntityId, input_version: EntityVersion, model: u16) -> Self {
        Self {
            entity,
            input_version,
            model,
        }
    }

    /// Returns the embedded entity.
    #[must_use]
    pub const fn entity(&self) -> EntityId {
        self.entity
    }

    /// Returns the exact normalized input version.
    #[must_use]
    pub const fn input_version(&self) -> EntityVersion {
        self.input_version
    }

    /// Returns the compact model identifier.
    #[must_use]
    pub const fn model(&self) -> u16 {
        self.model
    }
}

/// Full versioned embedding input value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingInputValue {
    /// Entity being embedded.
    entity: EntityId,
    /// Exact normalized input value version.
    input_version: EntityVersion,
    /// Immutable model configuration version.
    model_version: ObjectVersion<EmbeddingModelSchema>,
    /// Context reads, including graph neighbors when configured.
    reads: ReadManifest,
}

impl EmbeddingInputValue {
    /// Creates a complete embedding input value from exact dependencies.
    #[must_use]
    pub const fn new(
        entity: EntityId,
        input_version: EntityVersion,
        model_version: ObjectVersion<EmbeddingModelSchema>,
        reads: ReadManifest,
    ) -> Self {
        Self {
            entity,
            input_version,
            model_version,
            reads,
        }
    }

    /// Returns the embedded entity.
    #[must_use]
    pub const fn entity(&self) -> EntityId {
        self.entity
    }

    /// Returns the exact normalized input value version.
    #[must_use]
    pub const fn input_version(&self) -> EntityVersion {
        self.input_version
    }

    /// Returns the immutable model configuration version.
    #[must_use]
    pub const fn model_version(&self) -> ObjectVersion<EmbeddingModelSchema> {
        self.model_version
    }

    /// Returns the exact dependency manifest.
    #[must_use]
    pub const fn reads(&self) -> &ReadManifest {
        &self.reads
    }
}

/// Embedding relation value with coverage/provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingInputRecord {
    /// Full embedding input when live.
    value: Option<Arc<EmbeddingInputValue>>,
    /// Coverage and explicit absence.
    coverage: FacetCoverage,
    /// Authority/source/version basis.
    provenance: Provenance,
}

impl EmbeddingInputRecord {
    /// Admits an embedding input row with a valid value/coverage state.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] when value presence
    /// disagrees with coverage.
    pub fn new(
        value: Option<EmbeddingInputValue>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(value.is_some(), coverage)?;
        Ok(Self {
            value: value.map(Arc::new),
            coverage,
            provenance,
        })
    }

    /// Returns the embedding input value, when live.
    #[must_use]
    pub fn value(&self) -> Option<&EmbeddingInputValue> {
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

/// Embedding model configuration version.
pub type EmbeddingModelVersion = ObjectVersion<EmbeddingModelSchema>;
/// Embedding input logical key.
pub type EmbeddingInputId = ObjectKey<EmbeddingInputSchema>;
/// Embedding input value version.
pub type EmbeddingInputVersion = ObjectVersion<EmbeddingInputValueSchema>;

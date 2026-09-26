//! Embedding vector-space recipes and their authority identities.

/// Identity of every compatibility-relevant embedding recipe field.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingRecipeId([u8; 32]);

impl EmbeddingRecipeId {
    /// Admits a recipe identity from a verified capability manifest.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the complete fixed-width identity.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Source projection supplied to an embedding producer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingSource {
    /// Raw UTF-8 source bytes.
    RawUtf8,
    /// One canonical compiler declaration.
    SemanticDeclaration,
    /// Canonical documentation text.
    Documentation,
    /// One row from the admitted compiler/structural semantic evidence corpus.
    SemanticEvidence,
}

/// Model pooling operation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingPooling {
    /// First/CLS token.
    Cls,
    /// Mean of admitted token vectors.
    Mean,
    /// Last token.
    LastToken,
}

/// Vector normalization rule.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingNormalization {
    /// Preserve model output.
    None,
    /// Normalize to unit L2 length.
    UnitL2,
}

/// Coordinate representation emitted by inference.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingEncoding {
    /// IEEE 754 binary32.
    Float32,
    /// IEEE 754 binary16.
    Float16,
    /// Signed quantized byte with recipe-owned scale semantics.
    Signed8,
}

/// Metric belonging to the embedding space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingMetric {
    /// Cosine distance/similarity.
    Cosine,
    /// Dot product.
    Dot,
    /// Euclidean distance.
    Euclidean,
}

/// Full vector-space recipe retained by capability health.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingCapabilityRecipe {
    /// Identity of every compatibility-relevant field below.
    identity: EmbeddingRecipeId,
    /// Verified model artifact identity.
    pub model: [u8; 32],
    /// Verified tokenizer artifact identity.
    pub tokenizer: [u8; 32],
    /// Typed input projection.
    pub source: EmbeddingSource,
    /// Maximum UTF-8 input bytes admitted per embedding invocation, when byte-bounded.
    pub maximum_input_bytes: Option<u32>,
    /// Maximum tokenizer output admitted per chunk, when token-bounded.
    pub maximum_tokens: Option<u32>,
    /// Tokens repeated between adjacent chunks.
    pub overlap_tokens: u16,
    /// Number of emitted coordinates.
    pub dimensions: u32,
    /// Search metric.
    pub metric: EmbeddingMetric,
    /// Model pooling rule.
    pub pooling: EmbeddingPooling,
    /// Coordinate normalization rule.
    pub normalization: EmbeddingNormalization,
    /// Coordinate representation.
    pub encoding: EmbeddingEncoding,
    /// Exact query-side treatment identity.
    pub query_treatment: [u8; 32],
    /// Exact document-side treatment identity.
    pub document_treatment: [u8; 32],
}

impl EmbeddingCapabilityRecipe {
    /// Constructs a recipe whose identity is derived from every compatibility
    /// relevant field.  Producers should use this constructor instead of
    /// accepting an independently supplied identity.
    #[allow(
        clippy::too_many_arguments,
        reason = "each vector-space compatibility fact is independently meaningful"
    )]
    #[must_use]
    pub fn new(
        model: [u8; 32],
        tokenizer: [u8; 32],
        source: EmbeddingSource,
        maximum_input_bytes: Option<u32>,
        maximum_tokens: Option<u32>,
        overlap_tokens: u16,
        dimensions: u32,
        metric: EmbeddingMetric,
        pooling: EmbeddingPooling,
        normalization: EmbeddingNormalization,
        encoding: EmbeddingEncoding,
        query_treatment: [u8; 32],
        document_treatment: [u8; 32],
    ) -> Self {
        let mut recipe = Self {
            identity: EmbeddingRecipeId::new([0; 32]),
            model,
            tokenizer,
            source,
            maximum_input_bytes,
            maximum_tokens,
            overlap_tokens,
            dimensions,
            metric,
            pooling,
            normalization,
            encoding,
            query_treatment,
            document_treatment,
        };
        recipe.identity = embedding_authority_recipe(&recipe);
        recipe
    }

    /// Recomputes the canonical identity from the recipe fields.
    #[must_use]
    pub fn recomputed_identity(&self) -> EmbeddingRecipeId {
        embedding_authority_recipe(self)
    }

    /// Returns the identity claimed by this recipe.
    #[must_use]
    pub const fn identity(&self) -> EmbeddingRecipeId {
        self.identity
    }

    /// Replaces the identity with an untrusted wire claim for admission
    /// verification.  Callers must compare [`Self::recomputed_identity`]
    /// before accepting the resulting recipe.
    #[must_use]
    pub(crate) fn with_claimed_identity(mut self, claimed_identity: EmbeddingRecipeId) -> Self {
        self.identity = claimed_identity;
        self
    }
}

/// Derives the canonical identity of an embedding authority claim.
///
/// The identity commits to the model and tokenizer artifacts, input source and
/// bounds, chunk overlap, coordinate shape and semantics, and both treatment
/// identities.  Optional bounds carry an explicit presence tag so a missing
/// bound cannot alias a zero bound.
#[must_use]
pub fn embedding_authority_recipe(value: &EmbeddingCapabilityRecipe) -> EmbeddingRecipeId {
    let mut canonical = blake3::Hasher::new();
    canonical.update(b"backend.embedding-authority-recipe.v1\0");
    canonical.update(&value.model);
    canonical.update(&value.tokenizer);
    canonical.update(&[embedding_source_code(value.source)]);
    encode_optional_u32(value.maximum_input_bytes, &mut canonical);
    encode_optional_u32(value.maximum_tokens, &mut canonical);
    canonical.update(&value.overlap_tokens.to_be_bytes());
    canonical.update(&value.dimensions.to_be_bytes());
    canonical.update(&[
        embedding_metric_code(value.metric),
        embedding_pooling_code(value.pooling),
        embedding_normalization_code(value.normalization),
        embedding_encoding_code(value.encoding),
    ]);
    canonical.update(&value.query_treatment);
    canonical.update(&value.document_treatment);
    EmbeddingRecipeId::new(*canonical.finalize().as_bytes())
}

fn encode_optional_u32(value: Option<u32>, output: &mut blake3::Hasher) {
    match value {
        Some(value) => {
            output.update(&[1]);
            output.update(&value.to_be_bytes());
        }
        None => {
            output.update(&[0]);
        }
    }
}

const fn embedding_source_code(value: EmbeddingSource) -> u8 {
    match value {
        EmbeddingSource::RawUtf8 => 0,
        EmbeddingSource::SemanticDeclaration => 1,
        EmbeddingSource::Documentation => 2,
        EmbeddingSource::SemanticEvidence => 3,
    }
}

const fn embedding_metric_code(value: EmbeddingMetric) -> u8 {
    match value {
        EmbeddingMetric::Cosine => 0,
        EmbeddingMetric::Dot => 1,
        EmbeddingMetric::Euclidean => 2,
    }
}

const fn embedding_pooling_code(value: EmbeddingPooling) -> u8 {
    match value {
        EmbeddingPooling::Cls => 0,
        EmbeddingPooling::Mean => 1,
        EmbeddingPooling::LastToken => 2,
    }
}

const fn embedding_normalization_code(value: EmbeddingNormalization) -> u8 {
    match value {
        EmbeddingNormalization::None => 0,
        EmbeddingNormalization::UnitL2 => 1,
    }
}

const fn embedding_encoding_code(value: EmbeddingEncoding) -> u8 {
    match value {
        EmbeddingEncoding::Float32 => 0,
        EmbeddingEncoding::Float16 => 1,
        EmbeddingEncoding::Signed8 => 2,
    }
}

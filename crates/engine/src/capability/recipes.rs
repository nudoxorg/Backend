use std::num::{NonZeroU16, NonZeroU32};

use backend_compile::ToolchainId;
use backend_semantic::vocabulary::{LanguageProfile, NativeTool};
use backend_version::ObjectVersion;

use super::{EmbeddingRecipeId, LanguageOracleTask, TokenizerId, TreatmentId};

/// Complete language-oracle execution recipe.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LanguageOracleRecipe {
    /// Exact source language profile.
    pub profile: LanguageProfile,
    /// Semantic job implemented by the oracle.
    pub task: LanguageOracleTask,
    /// Oracle wire protocol version.
    pub protocol: u16,
    /// Closed native tool family selected by the compiler owner.
    pub native_tool: NativeTool,
    /// Exact toolchain/helper closure identity used by compile sessions.
    pub toolchain: ToolchainId,
    /// Exact package-resolution authority configuration.
    pub package_authority: [u8; 32],
}

/// Input projection performed before tokenization.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceExtraction {
    /// Raw UTF-8 source.
    RawUtf8,
    /// One canonical semantic declaration.
    SemanticDeclaration,
    /// Documentation text projection.
    Documentation,
}

/// Token-bounded chunking policy.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Chunking {
    /// Maximum tokens in one model input.
    pub maximum_tokens: NonZeroU32,
    /// Number of tokens repeated between adjacent chunks.
    pub overlap_tokens: u16,
}

/// Model pooling operation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Pooling {
    /// First/CLS token.
    Cls,
    /// Mean of admitted token vectors.
    Mean,
    /// Last token.
    LastToken,
}

/// Vector normalization rule.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Normalization {
    /// Preserve model output.
    None,
    /// Normalize to unit L2 length.
    L2,
}

/// Query-side text treatment.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum QueryTreatment {
    /// No prefix or template.
    Plain,
    /// Model-specific query prefix/template.
    ModelPrefix(TreatmentId),
}

/// Document-side text treatment.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DocumentTreatment {
    /// No prefix or template.
    Plain,
    /// Model-specific document prefix/template.
    ModelPrefix(TreatmentId),
}

/// Coordinate representation emitted by inference.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NumericRepresentation {
    /// IEEE 754 binary32.
    F32,
    /// IEEE 754 binary16.
    F16,
    /// Signed quantized byte with recipe-owned scale semantics.
    I8,
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

/// Complete, self-identifying embedding-space recipe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingModelRecipe {
    identity: EmbeddingRecipeId,
    /// Exact tokenizer identity.
    pub tokenizer: TokenizerId,
    /// Source projection.
    pub extraction: SourceExtraction,
    /// Token chunking.
    pub chunking: Chunking,
    /// Pooling rule.
    pub pooling: Pooling,
    /// Normalization rule.
    pub normalization: Normalization,
    /// Query-side treatment.
    pub query: QueryTreatment,
    /// Document-side treatment.
    pub document: DocumentTreatment,
    /// Emitted coordinate count.
    pub dimensions: NonZeroU16,
    /// Coordinate representation.
    pub numeric: NumericRepresentation,
    /// Search metric.
    pub metric: EmbeddingMetric,
}

impl EmbeddingModelRecipe {
    /// Derives an identity from every fact that defines compatible coordinates.
    #[allow(
        clippy::too_many_arguments,
        reason = "each vector-space fact is independently meaningful"
    )]
    #[must_use]
    pub fn new(
        tokenizer: TokenizerId,
        extraction: SourceExtraction,
        chunking: Chunking,
        pooling: Pooling,
        normalization: Normalization,
        query: QueryTreatment,
        document: DocumentTreatment,
        dimensions: NonZeroU16,
        numeric: NumericRepresentation,
        metric: EmbeddingMetric,
    ) -> Self {
        let mut canonical = Vec::with_capacity(48);
        canonical.extend_from_slice(&tokenizer.as_bytes());
        canonical.extend_from_slice(&[
            extraction as u8,
            pooling as u8,
            normalization as u8,
            numeric as u8,
            metric as u8,
        ]);
        encode_query_treatment(query, &mut canonical);
        encode_document_treatment(document, &mut canonical);
        canonical.extend_from_slice(&chunking.maximum_tokens.get().to_be_bytes());
        canonical.extend_from_slice(&chunking.overlap_tokens.to_be_bytes());
        canonical.extend_from_slice(&dimensions.get().to_be_bytes());
        Self {
            identity: ObjectVersion::from_value(canonical.as_slice()),
            tokenizer,
            extraction,
            chunking,
            pooling,
            normalization,
            query,
            document,
            dimensions,
            numeric,
            metric,
        }
    }

    /// Returns the complete vector-space identity.
    #[must_use]
    pub const fn identity(&self) -> EmbeddingRecipeId {
        self.identity
    }
}

fn encode_query_treatment(value: QueryTreatment, output: &mut Vec<u8>) {
    match value {
        QueryTreatment::Plain => output.push(0),
        QueryTreatment::ModelPrefix(identity) => {
            output.push(1);
            output.extend_from_slice(&identity.as_bytes());
        }
    }
}

fn encode_document_treatment(value: DocumentTreatment, output: &mut Vec<u8>) {
    match value {
        DocumentTreatment::Plain => output.push(0),
        DocumentTreatment::ModelPrefix(identity) => {
            output.push(1);
            output.extend_from_slice(&identity.as_bytes());
        }
    }
}

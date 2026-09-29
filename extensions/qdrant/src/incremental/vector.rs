//! Exact vector facts and finite vector values.

use crate::{
    Binding, CandidateId, CandidateState, Error, ModelVersion, Recipe, TokenizerVersion,
    TreatmentVersion,
};
use backend_version::CoverageWitness;
use std::mem::size_of;
use std::num::NonZeroU32;
use std::sync::Arc;

/// Distance/similarity semantics for one immutable vector recipe.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Metric {
    /// Squared Euclidean distance; smaller is better.
    EuclideanSquared,
    /// Cosine distance (`1 - cosine`); smaller is better.
    CosineDistance,
    /// Negative dot product; smaller is better.
    NegativeDot,
}

/// Token-state pooling performed by the admitted embedding runtime.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingPooling {
    /// Use the model's designated classification token.
    ClassificationToken = 1,
    /// Mean-pool non-padding token states.
    Mean = 2,
    /// Use the final non-padding token state.
    LastToken = 3,
}

/// Post-inference normalization committed by an embedding recipe.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingNormalization {
    /// Preserve model output coordinates.
    None = 1,
    /// Normalize coordinates to unit L2 length.
    UnitL2 = 2,
}

/// Coordinate representation committed by an embedding recipe.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingEncoding {
    /// IEEE-754 single-precision coordinates.
    Float32 = 1,
    /// Symmetric signed eight-bit scalar quantization.
    SignedInt8 = 2,
}

/// Complete reproducibility contract for query and document embeddings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddingRecipe {
    /// Verified model artifact/runtime revision.
    pub model: ModelVersion,
    /// Verified tokenizer vocabulary/configuration revision.
    pub tokenizer: TokenizerVersion,
    /// Exact model output dimension.
    pub dimensions: NonZeroU32,
    /// Distance semantics used by collection and reranker.
    pub metric: Metric,
    /// Token-state pooling behavior.
    pub pooling: EmbeddingPooling,
    /// Post-inference normalization.
    pub normalization: EmbeddingNormalization,
    /// Stored coordinate representation.
    pub encoding: EmbeddingEncoding,
    /// Canonical query-side instructions or prefix.
    pub query_treatment: TreatmentVersion,
    /// Canonical document-side instructions or prefix.
    pub document_treatment: TreatmentVersion,
}

impl EmbeddingRecipe {
    /// Derives the existing extension recipe identity from every coordinate-producing fact.
    #[must_use]
    pub fn version(self) -> Recipe {
        let mut bytes = Vec::with_capacity(171);
        // V2 commits the binding-scoped UUID point layout and logical
        // candidate payload introduced by the HTTP provider. Keeping V1 here
        // would let a numeric-ID projection satisfy the same binding filter
        // even though the new reader cannot safely interpret its point IDs.
        bytes.extend_from_slice(b"backend.qdrant.embedding-recipe.v2\0");
        bytes.extend_from_slice(self.model.as_bytes());
        bytes.extend_from_slice(self.tokenizer.as_bytes());
        bytes.extend_from_slice(&self.dimensions.get().to_be_bytes());
        bytes.extend_from_slice(&[
            self.metric as u8,
            self.pooling as u8,
            self.normalization as u8,
            self.encoding as u8,
        ]);
        bytes.extend_from_slice(self.query_treatment.as_bytes());
        bytes.extend_from_slice(self.document_treatment.as_bytes());
        Recipe::from_value(&bytes)
    }

    fn validate(self, values: &[f32]) -> Result<(), Error> {
        let dimensions = usize::try_from(self.dimensions.get()).map_err(|_| Error::SizeLimit)?;
        if values.len() != dimensions || values.iter().any(|value| !value.is_finite()) {
            return Err(Error::DimensionMismatch);
        }
        if self.normalization == EmbeddingNormalization::UnitL2 {
            let squared_norm = values
                .iter()
                .map(|value| f64::from(*value).powi(2))
                .sum::<f64>();
            if (squared_norm - 1.0).abs() > 1.0e-4 {
                return Err(Error::MalformedInput);
            }
        }
        Ok(())
    }
}

/// An exact finite vector owned by the query/facts boundary.
#[derive(Clone, Debug)]
pub struct VectorPoint {
    id: CandidateId,
    values: Arc<[f32]>,
}

impl PartialEq for VectorPoint {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.values.len() == other.values.len()
            && self
                .values
                .iter()
                .zip(other.values.iter())
                .all(|(left, right)| left.to_bits() == right.to_bits())
    }
}

impl Eq for VectorPoint {}

impl VectorPoint {
    /// Creates a point after rejecting zero IDs, empty vectors, NaNs, and
    /// infinities.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedInput`] when the identity or coordinates are
    /// invalid.
    pub fn new(id: CandidateId, values: Vec<f32>) -> Result<Self, Error> {
        if !id.is_valid()
            || values.is_empty()
            || values.len() > u32::MAX as usize
            || values.iter().any(|value| !value.is_finite())
        {
            return Err(Error::MalformedInput);
        }
        Ok(Self {
            id,
            values: Arc::from(values),
        })
    }

    /// Creates a point by copying a borrowed query/vector slice once.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedInput`] when the identity or coordinates are
    /// invalid.
    pub fn from_slice(id: CandidateId, values: &[f32]) -> Result<Self, Error> {
        Self::new(id, values.to_vec())
    }

    fn from_shared(id: CandidateId, values: Arc<[f32]>) -> Result<Self, Error> {
        if !id.is_valid()
            || values.is_empty()
            || values.len() > u32::MAX as usize
            || values.iter().any(|value| !value.is_finite())
        {
            return Err(Error::MalformedInput);
        }
        Ok(Self { id, values })
    }

    /// Returns the stable logical candidate identity.
    #[must_use]
    pub const fn id(&self) -> CandidateId {
        self.id
    }

    /// Returns immutable coordinates without another allocation.
    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// Encodes coordinates into the canonical candidate payload grammar.
    #[must_use]
    pub fn to_payload(&self) -> Vec<u8> {
        encode_values(&self.values)
    }

    /// Decodes the canonical candidate payload grammar.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedInput`] for invalid lengths, identities, or
    /// non-finite coordinates.
    pub fn from_payload(id: CandidateId, payload: &[u8]) -> Result<Self, Error> {
        let admitted = AdmittedEncodedPoint::validate(payload, None)?;
        let mut values = Vec::with_capacity(admitted.coordinates.len());
        values.extend(admitted.coordinates());
        Self::new(id, values)
    }
}

/// A document-side embedding admitted under the recipe's document treatment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentVector {
    recipe: EmbeddingRecipe,
    point: VectorPoint,
}

impl DocumentVector {
    /// Admits finite, dimension-correct document coordinates.
    ///
    /// # Errors
    /// Returns a typed malformed-input or dimension error when coordinates violate the recipe.
    pub fn new(recipe: EmbeddingRecipe, id: CandidateId, values: Vec<f32>) -> Result<Self, Error> {
        recipe.validate(&values)?;
        Ok(Self {
            recipe,
            point: VectorPoint::new(id, values)?,
        })
    }

    /// Admits already-shared finite, dimension-correct document coordinates.
    ///
    /// This keeps unchanged embeddings shared between an owner cache and a
    /// replacement projection instead of copying every coordinate.
    ///
    /// # Errors
    /// Returns a typed malformed-input or dimension error when coordinates violate the recipe.
    pub fn from_shared(
        recipe: EmbeddingRecipe,
        id: CandidateId,
        values: Arc<[f32]>,
    ) -> Result<Self, Error> {
        recipe.validate(&values)?;
        Ok(Self {
            recipe,
            point: VectorPoint::from_shared(id, values)?,
        })
    }

    /// Complete coordinate-production recipe.
    #[must_use]
    pub const fn recipe(&self) -> EmbeddingRecipe {
        self.recipe
    }

    /// Canonical point representation for durable vector facts.
    #[must_use]
    pub const fn point(&self) -> &VectorPoint {
        &self.point
    }
}

/// Exact vector facts backed by the canonical candidate relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VectorFacts {
    state: CandidateState,
    model: ModelVersion,
    metric: Metric,
    dimensions: usize,
}

/// Borrowed coordinates whose canonical payload, dimension, and finite-value
/// invariants were established when their owning `VectorFacts` was created.
/// Its private field prevents callers from manufacturing this proof wrapper
/// around unchecked provider bytes.
pub(super) struct AdmittedEncodedPoint<'a> {
    coordinates: &'a [[u8; 4]],
}

impl AdmittedEncodedPoint<'_> {
    /// Decodes the already-validated big-endian coordinate bytes without
    /// repeating shape or finite-value checks in the per-candidate scorer.
    pub(super) fn coordinates(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.coordinates
            .iter()
            .map(|bytes| f32::from_bits(u32::from_be_bytes(*bytes)))
    }
}

impl<'a> AdmittedEncodedPoint<'a> {
    /// Proves the canonical payload shape, optional recipe dimension, and
    /// finite IEEE-754 values without materializing decoded coordinates.
    fn validate(payload: &'a [u8], expected_dimensions: Option<usize>) -> Result<Self, Error> {
        let dimension_bytes: [u8; 4] = payload
            .get(..4)
            .ok_or(Error::MalformedInput)?
            .try_into()
            .map_err(|_| Error::MalformedInput)?;
        let dimension =
            usize::try_from(u32::from_be_bytes(dimension_bytes)).map_err(|_| Error::SizeLimit)?;
        if dimension == 0 {
            return Err(Error::MalformedInput);
        }
        let expected_bytes = dimension
            .checked_mul(size_of::<u32>())
            .and_then(|bytes| bytes.checked_add(4))
            .ok_or(Error::SizeLimit)?;
        if expected_bytes != payload.len() {
            return Err(Error::MalformedInput);
        }
        let encoded_coordinates = payload.get(4..).ok_or(Error::MalformedInput)?;
        let (coordinates, remainder) = encoded_coordinates.as_chunks::<4>();
        if !remainder.is_empty() || coordinates.len() != dimension {
            return Err(Error::MalformedInput);
        }
        for bytes in coordinates {
            let value = f32::from_bits(u32::from_be_bytes(*bytes));
            if !value.is_finite() {
                return Err(Error::MalformedInput);
            }
        }
        if expected_dimensions.is_some_and(|expected| expected != dimension) {
            return Err(Error::DimensionMismatch);
        }
        Ok(Self { coordinates })
    }

    /// Borrows bytes from a payload already admitted by its owning facts.
    /// The immutable state and exact dimension proof make rechecking every
    /// coordinate unnecessary in the candidate scoring loop.
    fn from_validated(payload: &'a [u8], dimensions: usize) -> Option<Self> {
        let encoded_coordinates = payload.get(4..)?;
        let (coordinates, remainder) = encoded_coordinates.as_chunks::<4>();
        if remainder.is_empty() && coordinates.len() == dimensions {
            Some(Self { coordinates })
        } else {
            None
        }
    }
}

impl VectorFacts {
    /// Builds exact facts only when the durable relation is bound to this complete recipe.
    ///
    /// # Errors
    /// Returns a typed binding, coverage, coordinate, dimension, or size error.
    pub fn from_recipe(state: CandidateState, recipe: EmbeddingRecipe) -> Result<Self, Error> {
        if state.binding().recipe != recipe.version() {
            return Err(Error::StaleRoot);
        }
        let dimensions = usize::try_from(recipe.dimensions.get()).map_err(|_| Error::SizeLimit)?;
        Self::new(state, recipe.model, recipe.metric, dimensions)
    }

    /// Validates every relation payload once and binds the model/metric
    /// recipe to the exact relation root.
    ///
    /// # Errors
    ///
    /// Returns an error when dimensions are invalid or any canonical payload
    /// is malformed or has a different dimension.
    pub fn new(
        state: CandidateState,
        model: ModelVersion,
        metric: Metric,
        dimensions: usize,
    ) -> Result<Self, Error> {
        if dimensions == 0 {
            return Err(Error::MalformedInput);
        }
        if !matches!(
            state.coverage(),
            CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
        ) {
            return Err(Error::IncompleteCoverage);
        }
        for (_, payload) in state.iter() {
            AdmittedEncodedPoint::validate(payload, Some(dimensions))?;
        }
        Ok(Self {
            state,
            model,
            metric,
            dimensions,
        })
    }

    /// Returns the exact relation binding.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.state.binding()
    }

    /// Returns the complete relation witness.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.state.coverage()
    }

    /// Returns the immutable model/provider revision.
    #[must_use]
    pub const fn model(&self) -> ModelVersion {
        self.model
    }

    /// Returns the fixed vector metric.
    #[must_use]
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// Returns the fixed vector dimension.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Decodes one exact point from the canonical relation without retaining a
    /// second authoritative map.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected canonical payload is malformed.
    pub fn point(&self, id: CandidateId) -> Result<Option<VectorPoint>, Error> {
        self.state
            .payload(id)
            .map(|payload| VectorPoint::from_payload(id, payload))
            .transpose()
    }

    /// Returns one already-admitted point in its canonical big-endian payload
    /// form without allocating decoded coordinates.
    ///
    /// The constructor validates every payload and binds its dimension to
    /// these facts, so internal reranking can borrow the immutable bytes and
    /// decode one coordinate at a time.
    pub(super) fn encoded_point(&self, id: CandidateId) -> Option<AdmittedEncodedPoint<'_>> {
        let payload = self.state.payload(id)?;
        AdmittedEncodedPoint::from_validated(payload, self.dimensions)
    }

    /// Iterates exact point identities in canonical relation order.
    pub fn ids(&self) -> impl Iterator<Item = CandidateId> {
        self.state.iter().map(|(id, _)| id)
    }
}

/// Query vector tied to the model, metric, and dimensions selected by a base.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorQuery {
    model: ModelVersion,
    metric: Metric,
    values: Arc<[f32]>,
}

impl VectorQuery {
    /// Creates a finite query vector.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedInput`] for an empty or non-finite vector.
    pub fn new(model: ModelVersion, metric: Metric, values: Vec<f32>) -> Result<Self, Error> {
        if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
            return Err(Error::MalformedInput);
        }
        Ok(Self {
            model,
            metric,
            values: Arc::from(values),
        })
    }

    /// Returns the model revision.
    #[must_use]
    pub const fn model(&self) -> ModelVersion {
        self.model
    }

    /// Returns metric semantics.
    #[must_use]
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// Returns immutable query coordinates.
    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.values
    }
}

/// A query-side embedding admitted under the recipe's query treatment.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryVector {
    recipe: EmbeddingRecipe,
    query: VectorQuery,
}

impl QueryVector {
    /// Admits finite, dimension-correct query coordinates.
    ///
    /// # Errors
    /// Returns a typed malformed-input or dimension error when coordinates violate the recipe.
    pub fn new(recipe: EmbeddingRecipe, values: Vec<f32>) -> Result<Self, Error> {
        recipe.validate(&values)?;
        Ok(Self {
            recipe,
            query: VectorQuery::new(recipe.model, recipe.metric, values)?,
        })
    }

    /// Complete coordinate-production recipe.
    #[must_use]
    pub const fn recipe(&self) -> EmbeddingRecipe {
        self.recipe
    }

    /// Internal query representation accepted by the index.
    #[must_use]
    pub const fn query(&self) -> &VectorQuery {
        &self.query
    }
}

fn encode_values(values: &[f32]) -> Vec<u8> {
    let Ok(dimension) = u32::try_from(values.len()) else {
        return Vec::new();
    };
    let Some(capacity) = values
        .len()
        .checked_mul(size_of::<u32>())
        .and_then(|size| size.checked_add(4))
    else {
        return Vec::new();
    };
    let mut payload = Vec::with_capacity(capacity);
    payload.extend_from_slice(&dimension.to_be_bytes());
    for value in values {
        payload.extend_from_slice(&value.to_bits().to_be_bytes());
    }
    payload
}

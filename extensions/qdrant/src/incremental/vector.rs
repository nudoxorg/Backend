//! Exact vector facts and finite vector values.

use crate::{Binding, CandidateId, CandidateState, Error, ModelVersion};
use backend_version::CoverageWitness;
use std::mem::size_of;
use std::sync::Arc;

/// Distance/similarity semantics for one immutable vector recipe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Metric {
    /// Squared Euclidean distance; smaller is better.
    EuclideanSquared,
    /// Cosine distance (`1 - cosine`); smaller is better.
    CosineDistance,
    /// Negative dot product; smaller is better.
    NegativeDot,
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
        if payload.len() < 4 {
            return Err(Error::MalformedInput);
        }
        let mut dimension_bytes = [0; 4];
        dimension_bytes.copy_from_slice(&payload[..4]);
        let dimension =
            usize::try_from(u32::from_be_bytes(dimension_bytes)).map_err(|_| Error::SizeLimit)?;
        let expected = dimension
            .checked_mul(size_of::<u32>())
            .and_then(|size| size.checked_add(4))
            .ok_or(Error::SizeLimit)?;
        if dimension == 0 || expected != payload.len() {
            return Err(Error::MalformedInput);
        }
        let mut values = Vec::with_capacity(dimension);
        for bytes in payload[4..].chunks_exact(4) {
            let mut bits = [0; 4];
            bits.copy_from_slice(bytes);
            let value = f32::from_bits(u32::from_be_bytes(bits));
            if !value.is_finite() {
                return Err(Error::MalformedInput);
            }
            values.push(value);
        }
        Self::new(id, values)
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

impl VectorFacts {
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
        for (id, payload) in state.iter() {
            let point = VectorPoint::from_payload(id, payload)?;
            if point.values().len() != dimensions {
                return Err(Error::DimensionMismatch);
            }
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

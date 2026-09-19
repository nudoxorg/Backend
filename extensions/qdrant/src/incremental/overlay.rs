//! Bounded exact overlay transitions for vector materializations.

use super::vector::{Metric, VectorPoint};
use crate::delta::CandidateDelta;
use crate::{Binding, CandidateId, Error, ModelVersion};
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::sync::Arc;

/// Exact fresh points and tombstones over an immutable ANN base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactOverlay {
    binding: Binding,
    model: ModelVersion,
    metric: Metric,
    dimensions: usize,
    tombstones: Arc<[CandidateId]>,
    points: Arc<BTreeMap<CandidateId, Arc<VectorPoint>>>,
    bytes: usize,
}

impl ExactOverlay {
    pub(crate) fn from_delta(
        delta: &CandidateDelta,
        model: ModelVersion,
        metric: Metric,
        dimensions: usize,
    ) -> Result<Self, Error> {
        let (tombstones, points, bytes) = overlay_parts(delta, dimensions)?;
        Ok(Self {
            binding: delta.binding,
            model,
            metric,
            dimensions,
            tombstones: Arc::from(tombstones),
            points: Arc::new(points),
            bytes,
        })
    }

    pub(crate) fn merge_delta(&self, delta: &CandidateDelta) -> Result<Self, Error> {
        if delta.delta.base() != self.binding.root {
            return Err(Error::StaleRoot);
        }
        let mut tombstones = self.tombstones.iter().copied().collect::<BTreeSet<_>>();
        let mut points = self
            .points
            .iter()
            .map(|(id, point)| (*id, Arc::clone(point)))
            .collect::<BTreeMap<_, _>>();
        for change in delta.delta.changes() {
            let id = CandidateId(change.key);
            tombstones.insert(id);
            points.remove(&id);
            if let Some(payload) = &change.after {
                let point = VectorPoint::from_payload(id, payload)?;
                if point.values().len() != self.dimensions {
                    return Err(Error::DimensionMismatch);
                }
                points.insert(id, Arc::new(point));
            }
        }
        let bytes = overlay_bytes(&tombstones, &points)?;
        Ok(Self {
            binding: delta.binding,
            model: self.model,
            metric: self.metric,
            dimensions: self.dimensions,
            tombstones: Arc::from(tombstones.into_iter().collect::<Vec<_>>()),
            points: Arc::new(points),
            bytes,
        })
    }

    /// Returns the exact target binding.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns the immutable model revision.
    #[must_use]
    pub const fn model(&self) -> ModelVersion {
        self.model
    }

    /// Returns metric semantics.
    #[must_use]
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// Returns vector dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Returns exact hidden identities.
    #[must_use]
    pub fn tombstones(&self) -> &[CandidateId] {
        &self.tombstones
    }

    /// Returns one fresh exact point without copying coordinates.
    #[must_use]
    pub fn point(&self, id: CandidateId) -> Option<&VectorPoint> {
        self.points.get(&id).map(Arc::as_ref)
    }

    /// Returns all fresh points in canonical candidate order.
    #[must_use]
    pub fn points(&self) -> &BTreeMap<CandidateId, Arc<VectorPoint>> {
        &self.points
    }

    /// Returns retained overlay bytes.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

type OverlayParts = (
    Vec<CandidateId>,
    BTreeMap<CandidateId, Arc<VectorPoint>>,
    usize,
);

fn overlay_parts(delta: &CandidateDelta, dimensions: usize) -> Result<OverlayParts, Error> {
    let mut tombstones = BTreeSet::new();
    let mut points = BTreeMap::new();
    for change in delta.delta.changes() {
        let id = CandidateId(change.key);
        tombstones.insert(id);
        if let Some(payload) = &change.after {
            let point = VectorPoint::from_payload(id, payload)?;
            if point.values().len() != dimensions {
                return Err(Error::DimensionMismatch);
            }
            points.insert(id, Arc::new(point));
        }
    }
    let bytes = overlay_bytes(&tombstones, &points)?;
    Ok((tombstones.into_iter().collect(), points, bytes))
}

fn overlay_bytes(
    tombstones: &BTreeSet<CandidateId>,
    points: &BTreeMap<CandidateId, Arc<VectorPoint>>,
) -> Result<usize, Error> {
    let tombstone_bytes = tombstones
        .len()
        .checked_mul(size_of::<CandidateId>())
        .ok_or(Error::SizeLimit)?;
    points
        .iter()
        .try_fold(tombstone_bytes, |bytes, (_, point)| {
            bytes
                .checked_add(size_of::<CandidateId>())
                .and_then(|bytes| bytes.checked_add(point.to_payload().len()))
                .ok_or(Error::SizeLimit)
        })
}

//! Defines identity behavior for `backend-extension-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the identity invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Physical-coordinate derivation for the disposable Qdrant projection.

use server_index_graph_vector::Metric;

use super::{
    contract::{PhysicalPointId, QdrantDataKey},
    limits::PHYSICAL_ID_ZERO_REPLACEMENT,
};

const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
const FNV_PRIME: u64 = 1_099_511_628_211;

#[repr(u8)]
enum MetricIdentityTag {
    SquaredEuclidean = 1,
    NegativeDotProduct = 2,
}

impl From<Metric> for MetricIdentityTag {
    fn from(metric: Metric) -> Self {
        match metric {
            Metric::SquaredEuclidean => Self::SquaredEuclidean,
            Metric::NegativeDotProduct => Self::NegativeDotProduct,
        }
    }
}

impl From<MetricIdentityTag> for u8 {
    fn from(tag: MetricIdentityTag) -> Self {
        tag as Self
    }
}

impl PhysicalPointId {
    /// Derives a deterministic physical coordinate from complete semantic authority.
    #[must_use]
    pub fn for_key(key: QdrantDataKey) -> Self {
        let mut hasher = FnvHasher::new();
        hasher.write(key.authority.snapshot.as_ref());
        hasher.write(key.authority.model.as_ref());
        hasher.write(key.segment.as_ref());
        hasher.write(&key.authority.dimension.to_be_bytes());
        hasher.write(&[u8::from(MetricIdentityTag::from(key.authority.metric))]);
        hasher.write(&key.partition.raw.to_be_bytes());
        hasher.write(&key.entity.raw.to_be_bytes());
        let raw = hasher.finish();
        Self(if raw == 0 {
            PHYSICAL_ID_ZERO_REPLACEMENT
        } else {
            raw
        })
    }
}

struct FnvHasher(u64);

impl FnvHasher {
    const fn new() -> Self {
        Self(FNV_OFFSET)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

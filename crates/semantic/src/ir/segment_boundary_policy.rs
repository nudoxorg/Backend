//! Canonical, bounded boundary policy for stable-key semantic plane segments.
//!
//! The manifest persists the algorithm and all byte parameters. Verification
//! is incremental and retains only one family cursor, so cold admission can
//! prove canonical cuts without retaining segment payloads.

use crate::ir::versioned_records::{SPIR_HEADER_BYTES, SemanticPlaneRecordError};
use crate::ir::{CanonicalSemanticPlaneSegmentView, SemanticIrPlane};

const STABLE_KEY_BOUNDARY_DOMAIN: &str = "backend.semantic.ir.segment-boundary.family-key.v1";

/// Closed stable-key boundary algorithm committed by the typed-plane manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SemanticPlaneSegmentBoundaryAlgorithm {
    /// Family-key BLAKE3 anchors with a linearly increasing cut threshold.
    StableKeyHashRampV1 = 1,
}

impl TryFrom<u8> for SemanticPlaneSegmentBoundaryAlgorithm {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            value if value == Self::StableKeyHashRampV1 as u8 => Ok(Self::StableKeyHashRampV1),
            value => Err(value),
        }
    }
}

/// Canonical segment policy committed separately for every typed IR family.
/// All limits count the SPIR header and row framing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticPlaneSegmentBoundaryPolicy {
    minimum_bytes: u32,
    target_bytes: u32,
    maximum_bytes: u32,
}

impl SemanticPlaneSegmentBoundaryPolicy {
    /// Constructs the closed stable-key hash-ramp algorithm.
    pub fn stable_key_hash_ramp(
        minimum_bytes: u32,
        target_bytes: u32,
        maximum_bytes: u32,
    ) -> Result<Self, SemanticPlaneRecordError> {
        let minimum = usize::try_from(minimum_bytes)
            .map_err(|_| SemanticPlaneRecordError::BoundaryPolicyRange)?;
        let target = usize::try_from(target_bytes)
            .map_err(|_| SemanticPlaneRecordError::BoundaryPolicyRange)?;
        let maximum = usize::try_from(maximum_bytes)
            .map_err(|_| SemanticPlaneRecordError::BoundaryPolicyRange)?;
        if maximum == 0 || maximum > crate::ir::MAX_SEMANTIC_SEGMENT_BYTES {
            return Err(SemanticPlaneRecordError::InvalidByteCeiling {
                observed: maximum,
                maximum: crate::ir::MAX_SEMANTIC_SEGMENT_BYTES,
            });
        }
        if minimum < SPIR_HEADER_BYTES || minimum > target || target > maximum {
            return Err(SemanticPlaneRecordError::InvalidSegmentBoundaryRange {
                minimum,
                target,
                maximum,
            });
        }
        Ok(Self {
            minimum_bytes,
            target_bytes,
            maximum_bytes,
        })
    }

    /// Exact closed algorithm/version tag persisted by the manifest.
    #[must_use]
    pub const fn algorithm(self) -> SemanticPlaneSegmentBoundaryAlgorithm {
        SemanticPlaneSegmentBoundaryAlgorithm::StableKeyHashRampV1
    }

    /// Earliest encoded byte size at which key anchors can cut.
    #[must_use]
    pub const fn minimum_bytes(self) -> usize {
        self.minimum_bytes as usize
    }

    /// Compatibility getter for producer metrics and existing call sites.
    #[must_use]
    pub const fn minimum_segment_bytes(self) -> usize {
        self.minimum_bytes()
    }

    /// Encoded byte size where an anchor is guaranteed before the next row.
    #[must_use]
    pub const fn target_bytes(self) -> usize {
        self.target_bytes as usize
    }

    /// Compatibility getter for producer metrics and existing call sites.
    #[must_use]
    pub const fn target_segment_bytes(self) -> usize {
        self.target_bytes()
    }

    /// Hard maximum encoded bytes in one segment, including its header.
    #[must_use]
    pub const fn maximum_bytes(self) -> usize {
        self.maximum_bytes as usize
    }

    /// Whether this candidate requires evaluating the stable-key hash.
    #[must_use]
    pub(crate) const fn hashes_candidate(self, current_segment_bytes: usize) -> bool {
        self.target_bytes > self.minimum_bytes
            && current_segment_bytes >= self.minimum_bytes()
            && current_segment_bytes < self.target_bytes()
    }

    /// Canonical cut decision immediately before `next_key`.
    #[must_use]
    pub(crate) fn cuts_before(
        self,
        family: SemanticIrPlane,
        current_segment_bytes: usize,
        next_key: &[u8; 32],
    ) -> bool {
        if current_segment_bytes < self.minimum_bytes() {
            return false;
        }
        if current_segment_bytes >= self.target_bytes() {
            return true;
        }
        let span = self.target_bytes() - self.minimum_bytes() + 1;
        let distance = current_segment_bytes - self.minimum_bytes() + 1;
        let threshold = (distance as u128 * (1_u128 << 64)) / span as u128;
        u128::from(stable_key_family_hash(family, next_key)) < threshold
    }
}

/// Incremental complete-family verifier for one manifest-committed policy.
///
/// Call [`Self::push_segment`] once for each strict-decoded segment in manifest
/// order, then call [`Self::finish`] with the manifest's row and segment totals.
/// It retains no payload references or row index.
pub struct CanonicalSemanticPlaneBoundaryFamilyVerifier {
    family: SemanticIrPlane,
    policy: SemanticPlaneSegmentBoundaryPolicy,
    previous_key: Option<[u8; 32]>,
    current_segment_bytes: usize,
    row_count: u64,
    segment_count: u64,
    segment_open: bool,
    segment_encoded_len: usize,
    segment_row_count: u32,
    segment_rows_seen: u32,
}

impl CanonicalSemanticPlaneBoundaryFamilyVerifier {
    /// Begins checking the exact family-policy pair from a V2 descriptor.
    #[must_use]
    pub const fn begin_family(
        family: SemanticIrPlane,
        policy: SemanticPlaneSegmentBoundaryPolicy,
    ) -> Self {
        Self {
            family,
            policy,
            previous_key: None,
            current_segment_bytes: 0,
            row_count: 0,
            segment_count: 0,
            segment_open: false,
            segment_encoded_len: 0,
            segment_row_count: 0,
            segment_rows_seen: 0,
        }
    }

    /// Accepts one independently decoded segment without retaining its bytes.
    pub fn push_segment(
        &mut self,
        segment: CanonicalSemanticPlaneSegmentView<'_>,
    ) -> Result<(), SemanticPlaneRecordError> {
        self.begin_segment(segment.kind(), segment.row_count(), segment.encoded_len())?;
        for record in segment.records() {
            self.push_record(record.key(), record.encoded_len())?;
        }
        self.finish_segment()
    }

    /// Starts the boundary-policy stage for one segment before its row parser
    /// lends records to the aggregate visitor.
    pub(crate) fn begin_segment(
        &mut self,
        kind: crate::ir::SemanticPlaneKind,
        row_count: u32,
        encoded_len: usize,
    ) -> Result<(), SemanticPlaneRecordError> {
        if self.segment_open {
            return Err(SemanticPlaneRecordError::NonCanonicalSegmentBoundary);
        }
        if kind != crate::ir::SemanticPlaneKind::Ir(self.family) {
            return Err(SemanticPlaneRecordError::PlaneKind);
        }
        if row_count == 0 || encoded_len > self.policy.maximum_bytes() {
            return Err(SemanticPlaneRecordError::NonCanonicalSegmentBoundary);
        }
        self.segment_open = true;
        self.segment_encoded_len = encoded_len;
        self.segment_row_count = row_count;
        self.segment_rows_seen = 0;
        Ok(())
    }

    /// Adds one grammar-checked row to the canonical family boundary state.
    /// The key and encoded length are copied values so the row borrow does not
    /// escape into retained verifier state.
    pub(crate) fn push_record(
        &mut self,
        key: [u8; 32],
        encoded_len: usize,
    ) -> Result<(), SemanticPlaneRecordError> {
        if !self.segment_open || self.segment_rows_seen >= self.segment_row_count {
            return Err(SemanticPlaneRecordError::NonCanonicalSegmentBoundary);
        }
        if self.previous_key.is_some_and(|previous| previous >= key) {
            return Err(SemanticPlaneRecordError::RecordOrder);
        }
        let projected = self
            .current_segment_bytes
            .checked_add(encoded_len)
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        let expected_cut = self.row_count > 0
            && (projected > self.policy.maximum_bytes()
                || self
                    .policy
                    .cuts_before(self.family, self.current_segment_bytes, &key));
        let actual_cut = self.segment_rows_seen == 0 && self.row_count > 0;
        if expected_cut != actual_cut {
            return Err(SemanticPlaneRecordError::NonCanonicalSegmentBoundary);
        }
        if actual_cut || self.row_count == 0 {
            self.current_segment_bytes = crate::ir::versioned_records::SPIR_HEADER_BYTES;
        }
        self.current_segment_bytes = self
            .current_segment_bytes
            .checked_add(encoded_len)
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        if self.current_segment_bytes > self.policy.maximum_bytes() {
            return Err(SemanticPlaneRecordError::NonCanonicalSegmentBoundary);
        }
        self.previous_key = Some(key);
        self.row_count = self
            .row_count
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        self.segment_rows_seen = self
            .segment_rows_seen
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        Ok(())
    }

    /// Closes the current segment after its parser has exhausted the row
    /// stream, checking that the visitor observed the exact declared census.
    pub(crate) fn finish_segment(&mut self) -> Result<(), SemanticPlaneRecordError> {
        if !self.segment_open
            || self.segment_rows_seen != self.segment_row_count
            || self.current_segment_bytes != self.segment_encoded_len
        {
            return Err(SemanticPlaneRecordError::NonCanonicalSegmentBoundary);
        }
        self.segment_open = false;
        self.segment_count = self
            .segment_count
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        Ok(())
    }

    /// Closes the family against the exact row and segment totals in its manifest.
    pub fn finish(
        self,
        expected_rows: u64,
        expected_segments: u64,
    ) -> Result<(), SemanticPlaneRecordError> {
        if self.segment_open {
            return Err(SemanticPlaneRecordError::NonCanonicalSegmentBoundary);
        }
        if self.row_count != expected_rows {
            return Err(SemanticPlaneRecordError::BoundaryFamilyRowCount {
                expected: expected_rows,
                observed: self.row_count,
            });
        }
        if self.segment_count != expected_segments {
            return Err(SemanticPlaneRecordError::BoundaryFamilySegmentCount {
                expected: expected_segments,
                observed: self.segment_count,
            });
        }
        Ok(())
    }
}

pub(crate) fn stable_key_family_hash(family: SemanticIrPlane, key: &[u8; 32]) -> u64 {
    let mut hasher = blake3::Hasher::new_derive_key(STABLE_KEY_BOUNDARY_DOMAIN);
    match family {
        SemanticIrPlane::Core => hasher.update(&[1, 0, 0]),
        SemanticIrPlane::Types => hasher.update(&[2, 0, 0]),
        SemanticIrPlane::Relations => hasher.update(&[3, 0, 0]),
        SemanticIrPlane::Occurrences => hasher.update(&[4, 0, 0]),
        SemanticIrPlane::Documentation => hasher.update(&[5, 0, 0]),
        SemanticIrPlane::SourceProvenance => hasher.update(&[6, 0, 0]),
        SemanticIrPlane::LanguageExtensions(profile) => {
            let code = <[u8; 2]>::from(profile);
            hasher.update(&[7, code[0], code[1]])
        }
    };
    hasher.update(key);
    let digest = hasher.finalize();
    u64::from_be_bytes([
        digest.as_bytes()[0],
        digest.as_bytes()[1],
        digest.as_bytes()[2],
        digest.as_bytes()[3],
        digest.as_bytes()[4],
        digest.as_bytes()[5],
        digest.as_bytes()[6],
        digest.as_bytes()[7],
    ])
}

//! Structural validation shared by live sessions and durable checkpoints.

use std::collections::BTreeMap;

use crate::{ReplicationError, SparseCoverage, TransportLimits};

use super::super::protocol::ChunkChain;
use super::{ExtentId, StagedExtent};

pub(super) fn validate_extent_metadata(
    object_len: u64,
    claimed_coverage: &SparseCoverage,
    extents: impl IntoIterator<Item = StagedExtent>,
    limits: TransportLimits,
    max_extents: usize,
) -> Result<(), ReplicationError> {
    if max_extents == 0 || claimed_coverage.ranges().len() > limits.max_ranges {
        return Err(ReplicationError::CoverageLimit);
    }
    for range in claimed_coverage.ranges() {
        if range.end()? > object_len {
            return Err(ReplicationError::Range);
        }
    }
    let mut recomputed = SparseCoverage::new(limits.max_ranges)?;
    let mut by_sequence = BTreeMap::new();
    let mut retained_bytes = 0u64;
    let mut extent_count = 0usize;
    for extent in extents {
        extent_count = extent_count
            .checked_add(1)
            .ok_or(ReplicationError::Overflow)?;
        if extent_count > max_extents {
            return Err(ReplicationError::CoverageLimit);
        }
        if extent.len == 0 || extent.len > limits.max_chunk as u64 {
            return Err(ReplicationError::ChunkTooLarge);
        }
        if extent.sequence >= object_len {
            return Err(ReplicationError::Range);
        }
        if extent.id != ExtentId(extent.chain.0) {
            return Err(ReplicationError::IdentityMismatch);
        }
        if by_sequence.insert(extent.sequence, extent).is_some() {
            return Err(ReplicationError::ReplayConflict);
        }
        retained_bytes = retained_bytes
            .checked_add(extent.len)
            .ok_or(ReplicationError::Overflow)?;
        if retained_bytes > object_len {
            return Err(ReplicationError::CoverageLimit);
        }
        let range = extent.range()?;
        if range.end()? > object_len || recomputed.overlaps(range) {
            return Err(ReplicationError::ReplayConflict);
        }
        recomputed.insert(range)?;
    }
    if recomputed.ranges() != claimed_coverage.ranges() {
        return Err(ReplicationError::IdentityMismatch);
    }
    validate_chain_links(&by_sequence, object_len)
}

pub(super) fn validate_chain_links(
    extents: &BTreeMap<u64, StagedExtent>,
    object_len: u64,
) -> Result<(), ReplicationError> {
    if extents.is_empty() {
        return (object_len != 0)
            .then_some(())
            .ok_or(ReplicationError::Incomplete);
    }
    let Some(last_sequence) = extents.keys().next_back().copied() else {
        return Err(ReplicationError::Incomplete);
    };
    let expected_count = usize::try_from(last_sequence)
        .ok()
        .and_then(|sequence| sequence.checked_add(1))
        .ok_or(ReplicationError::Overflow)?;
    if expected_count != extents.len() || last_sequence >= object_len {
        return Err(ReplicationError::Incomplete);
    }
    let mut previous = ChunkChain([0; 32]);
    for sequence in 0..=last_sequence {
        let extent = extents.get(&sequence).ok_or(ReplicationError::Incomplete)?;
        if extent.previous_chain != previous {
            return Err(ReplicationError::CorruptFrame);
        }
        previous = extent.chain;
    }
    Ok(())
}

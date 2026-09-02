//! Atom ordering, deduplication, and output writing.

use core::cell::Cell;

use super::{
    accounting::{Counters, increment},
    error::{CanonicalDataError, DataCountLane},
    types::{DataOutput, DataResource, DataScratch, lane_index},
};
use compiler_ir_vocabulary::{AtomId, SemanticAtom};

pub(super) fn canonicalize_atoms<'output, 'bytes>(
    atoms: &[SemanticAtom<'bytes>],
    scratch: &mut DataScratch<'output>,
    counters: &Counters,
) -> Result<u32, CanonicalDataError> {
    for (ordinal, destination) in scratch.atom_order[..atoms.len()].iter_mut().enumerate() {
        *destination =
            AtomId::new(
                u32::try_from(ordinal).map_err(|source| CanonicalDataError::Count {
                    lane: DataCountLane::Atoms,
                    actual: ordinal,
                    source,
                })?,
            );
    }
    let order = &mut scratch.atom_order[..atoms.len()];
    let sort_overflow = Cell::new(false);
    order.sort_unstable_by(|left, right| {
        increment(&counters.sort_comparisons);
        if counters.sort_comparisons.get() == u64::MAX {
            sort_overflow.set(true);
        }
        atoms[lane_index(left.raw)]
            .bytes
            .cmp(atoms[lane_index(right.raw)].bytes)
            .then_with(|| left.raw.cmp(&right.raw))
    });
    if sort_overflow.get() {
        return Err(CanonicalDataError::ResourceCounterOverflow {
            resource: DataResource::SortComparisons,
        });
    }
    let mut canonical = 0_u32;
    let mut previous: Option<&[u8]> = None;
    for source in order.iter().copied() {
        let bytes = atoms[lane_index(source.raw)].bytes;
        if previous != Some(bytes) {
            canonical =
                canonical
                    .checked_add(1)
                    .ok_or(CanonicalDataError::CanonicalCountOverflow {
                        lane: DataCountLane::Atoms,
                    })?;
        }
        scratch.atom_to_canonical[lane_index(source.raw)] = canonical - 1;
        previous = Some(bytes);
    }
    Ok(canonical)
}

/// Count distinct atom byte values without touching caller scratch.  This is
/// used by physical admission so an undersized output lane is rejected before
/// any transactional scratch mutation.  The production sort still owns the
/// canonical order once admission succeeds.
pub(super) fn distinct_atom_count(atoms: &[SemanticAtom<'_>]) -> Result<u32, CanonicalDataError> {
    let mut count = 0_u32;
    for (ordinal, atom) in atoms.iter().enumerate() {
        let duplicate = atoms[..ordinal]
            .iter()
            .any(|previous| previous.bytes == atom.bytes);
        if !duplicate {
            count = count
                .checked_add(1)
                .ok_or(CanonicalDataError::CanonicalCountOverflow {
                    lane: DataCountLane::Atoms,
                })?;
        }
    }
    Ok(count)
}

pub(super) fn write_atoms<'output, 'bytes>(
    atoms: &[SemanticAtom<'bytes>],
    scratch: &DataScratch<'output>,
    output: &mut DataOutput<'output, 'bytes>,
) {
    let mut canonical = 0_usize;
    let mut previous: Option<&[u8]> = None;
    for source in scratch.atom_order[..atoms.len()].iter().copied() {
        let bytes = atoms[lane_index(source.raw)].bytes;
        if previous != Some(bytes) {
            output.atoms[canonical] = SemanticAtom { bytes };
            canonical += 1;
        }
        previous = Some(bytes);
    }
}

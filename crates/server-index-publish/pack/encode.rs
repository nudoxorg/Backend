//! Defines pack encode behavior for `server-index-publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the pack encode invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Canonical direct encoding of compiler-derived exact and lexical pack lanes.

use core::ops::Deref;

use backend_version::{ArtifactHasher, GenerationId, HASH_BYTES};
use server_index_build::PreparedIndex;
use backend_semantic::index_core::{
    ENTITY_DOCUMENT_ID_BYTES, ExactRow, ExactSegment, LexicalRow, LexicalRowValue, LexicalSegment,
    MAX_EXACT_ROWS, MAX_LEXICAL_ROWS,
};
use backend_semantic::index_vocabulary::{ExactSegmentId, IndexPackId, IndexSnapshotId, LexicalSegmentId};
use zerocopy::IntoBytes;

use crate::pack::error::{IndexPackEncodeError, IndexPackLane};
use crate::{OpenedCompilationSnapshot, pack::grammar};

/// Maximum selected segment bodies held by one immutable index snapshot.
pub(crate) const MAX_PACK_SEGMENTS: usize = backend_semantic::index_core::MAX_SELECTED_SEGMENTS;

/// Immutable public preflight facts for one direct pack encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexPackPlanFacts {
    /// Compiler-derived canonical generation retained by the snapshot.
    pub generation: GenerationId,
    /// Immutable snapshot selected from compiler-built exact and lexical segments.
    pub snapshot: IndexSnapshotId,
    /// Exact total output bytes required before caller output is touched.
    pub encoded_bytes: usize,
    /// Number of exact segment bodies selected by the snapshot.
    pub exact_segments: usize,
    /// Number of lexical segment bodies selected by the snapshot.
    pub lexical_segments: usize,
}

/// A preflighted compiler-derived pack whose private lanes are ready to write directly.
pub struct IndexPackPlan<'bytes> {
    facts: IndexPackPlanFacts,
    exact: [Option<ExactPlan<'bytes>>; MAX_PACK_SEGMENTS],
    lexical: [Option<LexicalPlan<'bytes>>; MAX_PACK_SEGMENTS],
}

impl Deref for IndexPackPlan<'_> {
    type Target = IndexPackPlanFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

#[derive(Clone, Copy)]
struct ExactPlan<'bytes> {
    segment: ExactSegment<'bytes>,
    bytes: usize,
}

#[derive(Clone, Copy)]
struct LexicalPlan<'bytes> {
    segment: LexicalSegment<'bytes>,
    bytes: usize,
}

pub(crate) enum IndexPackStreamError<WriteError> {
    Encode(IndexPackEncodeError),
    Write(WriteError),
}

impl<WriteError> From<IndexPackEncodeError> for IndexPackStreamError<WriteError> {
    fn from(source: IndexPackEncodeError) -> Self {
        Self::Encode(source)
    }
}

/// Preflights a pack from one compiler-derived immutable snapshot without allocating or writing.
///
/// Every snapshot directory identity is resolved to exactly one prepared source before the final
/// output length is derived. The returned plan borrows only the already-validated core segment
/// rows; it never owns a staging copy.
///
/// # Errors
///
/// Returns a typed missing, duplicate, or fixed-address-space plan cause without allocating.
pub fn plan_index_pack<'bytes>(
    snapshot: &OpenedCompilationSnapshot<'_, '_, '_, 'bytes, '_>,
) -> Result<IndexPackPlan<'bytes>, IndexPackEncodeError> {
    let mut exact = [None; MAX_PACK_SEGMENTS];
    for (ordinal, id) in snapshot.snapshot.exact.iter().copied().enumerate() {
        let slot = exact
            .get_mut(ordinal)
            .ok_or(IndexPackEncodeError::PlanSlot {
                lane: IndexPackLane::Exact,
                ordinal,
                capacity: MAX_PACK_SEGMENTS,
            })?;
        *slot = Some(ExactPlan {
            segment: select_exact(snapshot.indexes, id)?,
            bytes: grammar::TOMBSTONE_VALUE_BYTES,
        });
    }
    let mut lexical = [None; MAX_PACK_SEGMENTS];
    for (ordinal, id) in snapshot.snapshot.lexical.iter().copied().enumerate() {
        let slot = lexical
            .get_mut(ordinal)
            .ok_or(IndexPackEncodeError::PlanSlot {
                lane: IndexPackLane::Lexical,
                ordinal,
                capacity: MAX_PACK_SEGMENTS,
            })?;
        *slot = Some(LexicalPlan {
            segment: select_lexical(snapshot.indexes, id)?,
            bytes: grammar::TOMBSTONE_VALUE_BYTES,
        });
    }
    fill_exact_lengths(&mut exact)?;
    fill_lexical_lengths(&mut lexical)?;
    let body_bytes = exact
        .iter()
        .flatten()
        .map(|plan| plan.bytes)
        .chain(lexical.iter().flatten().map(|plan| plan.bytes))
        .try_fold(0_usize, checked_add)?;
    let directory_bytes = snapshot
        .snapshot
        .exact
        .len()
        .checked_add(snapshot.snapshot.lexical.len())
        .and_then(|count| count.checked_mul(grammar::INDEX_PACK_DIRECTORY_BYTES))
        .ok_or(IndexPackEncodeError::AddressSpace {
            observed: usize::MAX,
        })?;
    let encoded_bytes = grammar::INDEX_PACK_HEADER_BYTES
        .checked_add(directory_bytes)
        .and_then(|total| total.checked_add(body_bytes))
        .ok_or(IndexPackEncodeError::AddressSpace {
            observed: usize::MAX,
        })?;
    ensure_u32(encoded_bytes)?;
    Ok(IndexPackPlan {
        facts: IndexPackPlanFacts {
            generation: snapshot.snapshot.generation,
            snapshot: snapshot.snapshot.id,
            encoded_bytes,
            exact_segments: snapshot.snapshot.exact.len(),
            lexical_segments: snapshot.snapshot.lexical.len(),
        },
        exact,
        lexical,
    })
}

/// Encodes one preflighted pack into caller-owned output without any staging allocation.
///
/// Output capacity is checked before the first byte is written. On success the returned typed
/// artifact identity covers every output byte through the exact planned length.
///
/// # Errors
///
/// Returns a typed output-capacity or canonical-address-space cause, leaving unwritten caller
/// bytes untouched when capacity is insufficient.
pub fn encode_index_pack(
    plan: &IndexPackPlan<'_>,
    output: &mut [u8],
) -> Result<IndexPackId, IndexPackEncodeError> {
    if output.len() < plan.encoded_bytes {
        return Err(IndexPackEncodeError::OutputTooSmall {
            required: plan.encoded_bytes,
            available: output.len(),
        });
    }
    let mut cursor = 0_usize;
    let available = output.len();
    let identity = stream_index_pack(plan, |bytes| {
        let end = cursor
            .checked_add(bytes.len())
            .ok_or(IndexPackEncodeError::AddressSpace {
                observed: usize::MAX,
            })?;
        let target = output
            .get_mut(cursor..end)
            .ok_or(IndexPackEncodeError::OutputTooSmall {
                required: end,
                available,
            })?;
        target.copy_from_slice(bytes);
        cursor = end;
        Ok(())
    })
    .map_err(|error| match error {
        IndexPackStreamError::Encode(source) | IndexPackStreamError::Write(source) => source,
    })?;
    debug_assert_eq!(cursor, plan.encoded_bytes);
    Ok(identity)
}

/// Streams one preflighted canonical pack without staging the final byte image.
///
/// This is the filesystem publication seam. The closure is invoked in exact physical byte order;
/// it may write a file, hash a transport body, or lend caller output, while the pack identity is
/// derived over the identical emitted chunks.
pub(crate) fn stream_index_pack<WriteError>(
    plan: &IndexPackPlan<'_>,
    mut write: impl FnMut(&[u8]) -> Result<(), WriteError>,
) -> Result<IndexPackId, IndexPackStreamError<WriteError>> {
    let mut hasher = ArtifactHasher::new();
    let mut emit = |bytes: &[u8]| -> Result<(), IndexPackStreamError<WriteError>> {
        hasher.write_chunk(bytes);
        write(bytes).map_err(IndexPackStreamError::Write)
    };
    let header = grammar::canonical_header(
        as_u32(plan.encoded_bytes)?,
        plan.generation,
        plan.snapshot,
        as_u8(plan.exact_segments)?,
        as_u8(plan.lexical_segments)?,
    )?;
    emit(header.as_bytes())?;
    let body_start = grammar::INDEX_PACK_HEADER_BYTES
        .checked_add(directory_bytes(plan)?)
        .ok_or(IndexPackStreamError::Encode(
            IndexPackEncodeError::AddressSpace {
                observed: usize::MAX,
            },
        ))?;
    let mut next_body = body_start;
    for plan in plan.exact.iter().flatten() {
        emit_directory(*plan.segment.id, next_body, plan.bytes, &mut emit)?;
        next_body = next_body
            .checked_add(plan.bytes)
            .ok_or(IndexPackStreamError::Encode(
                IndexPackEncodeError::AddressSpace {
                    observed: usize::MAX,
                },
            ))?;
    }
    for plan in plan.lexical.iter().flatten() {
        emit_directory(*plan.segment.id, next_body, plan.bytes, &mut emit)?;
        next_body = next_body
            .checked_add(plan.bytes)
            .ok_or(IndexPackStreamError::Encode(
                IndexPackEncodeError::AddressSpace {
                    observed: usize::MAX,
                },
            ))?;
    }
    for plan in plan.exact.iter().flatten() {
        emit_exact_body(plan.segment, &mut emit)?;
    }
    for plan in plan.lexical.iter().flatten() {
        emit_lexical_body(plan.segment, &mut emit)?;
    }
    debug_assert_eq!(next_body, plan.encoded_bytes);
    Ok(hasher.finalize())
}

fn select_exact<'bytes>(
    indexes: &[PreparedIndex<'bytes>],
    selected: ExactSegmentId,
) -> Result<ExactSegment<'bytes>, IndexPackEncodeError> {
    let mut found = None;
    for index in indexes {
        if index.exact.id == selected && found.replace(index.exact).is_some() {
            return Err(IndexPackEncodeError::ExactSelection { id: selected });
        }
    }
    found.ok_or(IndexPackEncodeError::ExactSelection { id: selected })
}

fn select_lexical<'bytes>(
    indexes: &[PreparedIndex<'bytes>],
    selected: LexicalSegmentId,
) -> Result<LexicalSegment<'bytes>, IndexPackEncodeError> {
    let mut found = None;
    for index in indexes {
        if index.lexical.id == selected && found.replace(index.lexical).is_some() {
            return Err(IndexPackEncodeError::LexicalSelection { id: selected });
        }
    }
    found.ok_or(IndexPackEncodeError::LexicalSelection { id: selected })
}

fn fill_exact_lengths(plans: &mut [Option<ExactPlan<'_>>]) -> Result<(), IndexPackEncodeError> {
    for plan in plans.iter_mut().flatten() {
        plan.bytes = exact_body_bytes(plan.segment.rows)?;
    }
    Ok(())
}

fn fill_lexical_lengths(plans: &mut [Option<LexicalPlan<'_>>]) -> Result<(), IndexPackEncodeError> {
    for plan in plans.iter_mut().flatten() {
        plan.bytes = lexical_body_bytes(plan.segment.rows)?;
    }
    Ok(())
}

fn exact_body_bytes(rows: &[ExactRow<'_>]) -> Result<usize, IndexPackEncodeError> {
    debug_assert!(rows.len() <= MAX_EXACT_ROWS);
    let prefix = grammar::SEGMENT_ROW_COUNT_BYTES
        .checked_add(rows.len().checked_mul(grammar::ROW_OFFSET_BYTES).ok_or(
            IndexPackEncodeError::AddressSpace {
                observed: usize::MAX,
            },
        )?)
        .ok_or(IndexPackEncodeError::AddressSpace {
            observed: usize::MAX,
        })?;
    rows.iter().try_fold(prefix, |total, row| {
        let value = row
            .value_bytes()
            .map_or(grammar::TOMBSTONE_VALUE_BYTES, <[u8]>::len);
        let row_bytes = grammar::EXACT_ROW_PREFIX_BYTES
            .checked_add(row.key.len())
            .and_then(|bytes| bytes.checked_add(value))
            .ok_or(IndexPackEncodeError::AddressSpace {
                observed: usize::MAX,
            })?;
        ensure_u32(row.key.len())?;
        ensure_u32(value)?;
        total
            .checked_add(row_bytes)
            .ok_or(IndexPackEncodeError::AddressSpace {
                observed: usize::MAX,
            })
    })
}

fn lexical_body_bytes(rows: &[LexicalRow<'_>]) -> Result<usize, IndexPackEncodeError> {
    debug_assert!(rows.len() <= MAX_LEXICAL_ROWS);
    let prefix = grammar::SEGMENT_ROW_COUNT_BYTES
        .checked_add(rows.len().checked_mul(grammar::ROW_OFFSET_BYTES).ok_or(
            IndexPackEncodeError::AddressSpace {
                observed: usize::MAX,
            },
        )?)
        .ok_or(IndexPackEncodeError::AddressSpace {
            observed: usize::MAX,
        })?;
    rows.iter().try_fold(prefix, |total, row| {
        ensure_u32(row.term.len())?;
        let row_bytes = grammar::LEXICAL_ROW_PREFIX_BYTES
            .checked_add(row.term.len())
            .ok_or(IndexPackEncodeError::AddressSpace {
                observed: usize::MAX,
            })?;
        total
            .checked_add(row_bytes)
            .ok_or(IndexPackEncodeError::AddressSpace {
                observed: usize::MAX,
            })
    })
}

fn emit_exact_body<WriteError>(
    segment: ExactSegment<'_>,
    emit: &mut impl FnMut(&[u8]) -> Result<(), IndexPackStreamError<WriteError>>,
) -> Result<(), IndexPackStreamError<WriteError>> {
    let rows = segment.rows;
    let row_count_wire = grammar::canonical_rows(as_u16(rows.len())?);
    emit(row_count_wire.as_bytes())?;
    let mut offset = grammar::SEGMENT_ROW_COUNT_BYTES + rows.len() * grammar::ROW_OFFSET_BYTES;
    for row in rows {
        let offset_wire =
            zerocopy::byteorder::U32::<zerocopy::byteorder::LittleEndian>::new(as_u32(offset)?);
        emit(offset_wire.as_bytes())?;
        offset = offset
            .checked_add(exact_row_bytes(*row))
            .ok_or(IndexPackStreamError::Encode(
                IndexPackEncodeError::AddressSpace {
                    observed: usize::MAX,
                },
            ))?;
    }
    for row in rows {
        let (state, value) = match row.value_bytes() {
            Some(value) => (grammar::ROW_PRESENT, value),
            None => (grammar::ROW_TOMBSTONE, &[][..]),
        };
        let record_wire =
            grammar::canonical_exact_row(state, as_u32(row.key.len())?, as_u32(value.len())?);
        emit(record_wire.as_bytes())?;
        emit(row.key)?;
        emit(value)?;
    }
    Ok(())
}

fn emit_lexical_body<WriteError>(
    segment: LexicalSegment<'_>,
    emit: &mut impl FnMut(&[u8]) -> Result<(), IndexPackStreamError<WriteError>>,
) -> Result<(), IndexPackStreamError<WriteError>> {
    let rows = segment.rows;
    let row_count_wire = grammar::canonical_rows(as_u16(rows.len())?);
    emit(row_count_wire.as_bytes())?;
    let mut offset = grammar::SEGMENT_ROW_COUNT_BYTES + rows.len() * grammar::ROW_OFFSET_BYTES;
    for row in rows {
        let offset_wire =
            zerocopy::byteorder::U32::<zerocopy::byteorder::LittleEndian>::new(as_u32(offset)?);
        emit(offset_wire.as_bytes())?;
        offset =
            offset
                .checked_add(lexical_row_bytes(*row))
                .ok_or(IndexPackStreamError::Encode(
                    IndexPackEncodeError::AddressSpace {
                        observed: usize::MAX,
                    },
                ))?;
    }
    for row in rows {
        let (state, score) = match row.value {
            LexicalRowValue::Present(score) => (grammar::ROW_PRESENT, u32::from(score)),
            LexicalRowValue::Tombstone => (grammar::ROW_TOMBSTONE, grammar::TOMBSTONE_SCORE),
        };
        let document: [u8; ENTITY_DOCUMENT_ID_BYTES] = row.document.into();
        let record_wire =
            grammar::canonical_lexical_row(as_u32(row.term.len())?, document, state, score);
        emit(record_wire.as_bytes())?;
        emit(row.term)?;
    }
    Ok(())
}

fn exact_row_bytes(row: ExactRow<'_>) -> usize {
    grammar::EXACT_ROW_PREFIX_BYTES
        + row.key.len()
        + row
            .value_bytes()
            .map_or(grammar::TOMBSTONE_VALUE_BYTES, <[u8]>::len)
}

const fn lexical_row_bytes(row: LexicalRow<'_>) -> usize {
    grammar::LEXICAL_ROW_PREFIX_BYTES + row.term.len()
}

fn directory_bytes(plan: &IndexPackPlan<'_>) -> Result<usize, IndexPackEncodeError> {
    plan.exact_segments
        .checked_add(plan.lexical_segments)
        .and_then(|count| count.checked_mul(grammar::INDEX_PACK_DIRECTORY_BYTES))
        .ok_or(IndexPackEncodeError::AddressSpace {
            observed: usize::MAX,
        })
}

fn emit_directory<WriteError>(
    id: [u8; HASH_BYTES],
    start: usize,
    bytes: usize,
    emit: &mut impl FnMut(&[u8]) -> Result<(), IndexPackStreamError<WriteError>>,
) -> Result<(), IndexPackStreamError<WriteError>> {
    let directory = grammar::canonical_directory(id, as_u32(start)?, as_u32(bytes)?);
    emit(directory.as_bytes())
}

fn as_u32(value: usize) -> Result<u32, IndexPackEncodeError> {
    u32::try_from(value).map_err(|_| IndexPackEncodeError::AddressSpace { observed: value })
}

fn as_u16(value: usize) -> Result<u16, IndexPackEncodeError> {
    u16::try_from(value).map_err(|_| IndexPackEncodeError::AddressSpace { observed: value })
}

fn as_u8(value: usize) -> Result<u8, IndexPackEncodeError> {
    u8::try_from(value).map_err(|_| IndexPackEncodeError::AddressSpace { observed: value })
}

fn ensure_u32(value: usize) -> Result<(), IndexPackEncodeError> {
    as_u32(value).map(|_| ())
}

fn checked_add(left: usize, right: usize) -> Result<usize, IndexPackEncodeError> {
    left.checked_add(right)
        .ok_or(IndexPackEncodeError::AddressSpace {
            observed: usize::MAX,
        })
}

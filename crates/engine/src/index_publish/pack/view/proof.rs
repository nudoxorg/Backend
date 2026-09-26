//! Validates a content-addressed index pack and reborrows its proven rows.
//!
//! Directory order, segment ranges, and row offsets are proved once. Borrowed
//! query views then select those retained proofs without walking the grammar again.

use core::cmp::Ordering;

use backend_semantic::index_core::{
    EntityDocumentId, ExactRow, ExactSegmentError, ExactSegmentVerifier, IndexSnapshot, LexicalRow,
    LexicalScore, LexicalSegmentError, LexicalSegmentVerifier,
};
use backend_semantic::index_vocabulary::{ExactSegmentId, LexicalSegmentId};
use backend_version::HASH_BYTES;

use crate::index_publish::pack::{
    MAX_PACK_SEGMENTS,
    error::{IndexPackLane, IndexPackOpenError, IndexPackRegion, IndexPackRowInvariant},
    grammar::{self, ExactDirectory, LexicalDirectory, PackHeader, PackLayout, PackRange},
};

pub(super) fn validate(bytes: &[u8]) -> Result<PackLayout, IndexPackOpenError> {
    let header = grammar::parse_header(bytes)?;
    let mut exact = [None; MAX_PACK_SEGMENTS];
    let mut lexical = [None; MAX_PACK_SEGMENTS];
    let directory_end = grammar::directory_end(header)?;
    let mut body_cursor = directory_end;
    let mut directory_offset = grammar::INDEX_PACK_HEADER_BYTES;
    let mut exact_ids = [None; MAX_PACK_SEGMENTS];
    let mut lexical_ids = [None; MAX_PACK_SEGMENTS];
    validate_exact_directories(
        bytes,
        header,
        &mut exact,
        &mut exact_ids,
        &mut directory_offset,
        &mut body_cursor,
    )?;
    validate_lexical_directories(
        bytes,
        header,
        &mut lexical,
        &mut lexical_ids,
        &mut directory_offset,
        &mut body_cursor,
    )?;
    if body_cursor != header.total_bytes {
        return Err(IndexPackOpenError::TrailingBytes {
            offset: body_cursor,
            total: header.total_bytes,
        });
    }
    let observed = IndexSnapshot::canonical_identity_from_slots(
        header.generation,
        &exact_ids,
        header.exact_count,
        &lexical_ids,
        header.lexical_count,
    )
    .map_err(|source| IndexPackOpenError::SnapshotInvariant { source })?;
    if observed != header.snapshot {
        return Err(IndexPackOpenError::SnapshotIdentity {
            expected: header.snapshot,
            observed,
        });
    }
    Ok(PackLayout {
        header,
        exact,
        lexical,
    })
}

fn validate_exact_directories(
    bytes: &[u8],
    header: PackHeader,
    directories: &mut [Option<ExactDirectory>; MAX_PACK_SEGMENTS],
    ids: &mut [Option<ExactSegmentId>; MAX_PACK_SEGMENTS],
    directory_offset: &mut usize,
    body_cursor: &mut usize,
) -> Result<(), IndexPackOpenError> {
    let mut previous = None;
    for ordinal in 0..header.exact_count {
        let wire = grammar::parse_directory(bytes, *directory_offset, IndexPackLane::Exact)?;
        let raw = grammar::directory_identity(wire);
        let id = ExactSegmentId::try_from(raw).map_err(|source| {
            IndexPackOpenError::DirectoryAuthority {
                lane: IndexPackLane::Exact,
                ordinal,
                source,
            }
        })?;
        validate_order(IndexPackLane::Exact, ordinal, previous, raw)?;
        previous = Some(raw);
        let range = validate_range(
            grammar::directory_start(wire)?,
            grammar::directory_bytes(wire)?,
            IndexPackLane::Exact,
            ordinal,
            *body_cursor,
            header.total_bytes,
        )?;
        validate_exact_body(bytes, range, ordinal, id)?;
        let slot = directories
            .get_mut(ordinal)
            .ok_or(IndexPackOpenError::SegmentCount {
                lane: IndexPackLane::Exact,
                observed: ordinal + 1,
                maximum: MAX_PACK_SEGMENTS,
            })?;
        *slot = Some(ExactDirectory { id, range });
        let id_slot = ids
            .get_mut(ordinal)
            .ok_or(IndexPackOpenError::SegmentCount {
                lane: IndexPackLane::Exact,
                observed: ordinal + 1,
                maximum: MAX_PACK_SEGMENTS,
            })?;
        *id_slot = Some(id);
        *directory_offset = directory_offset
            .checked_add(grammar::INDEX_PACK_DIRECTORY_BYTES)
            .ok_or(IndexPackOpenError::TrailingBytes {
                offset: *directory_offset,
                total: header.total_bytes,
            })?;
        *body_cursor = range.end;
    }
    Ok(())
}

fn validate_lexical_directories(
    bytes: &[u8],
    header: PackHeader,
    directories: &mut [Option<LexicalDirectory>; MAX_PACK_SEGMENTS],
    ids: &mut [Option<LexicalSegmentId>; MAX_PACK_SEGMENTS],
    directory_offset: &mut usize,
    body_cursor: &mut usize,
) -> Result<(), IndexPackOpenError> {
    let mut previous = None;
    for ordinal in 0..header.lexical_count {
        let wire = grammar::parse_directory(bytes, *directory_offset, IndexPackLane::Lexical)?;
        let raw = grammar::directory_identity(wire);
        let id = LexicalSegmentId::try_from(raw).map_err(|source| {
            IndexPackOpenError::DirectoryAuthority {
                lane: IndexPackLane::Lexical,
                ordinal,
                source,
            }
        })?;
        validate_order(IndexPackLane::Lexical, ordinal, previous, raw)?;
        previous = Some(raw);
        let range = validate_range(
            grammar::directory_start(wire)?,
            grammar::directory_bytes(wire)?,
            IndexPackLane::Lexical,
            ordinal,
            *body_cursor,
            header.total_bytes,
        )?;
        validate_lexical_body(bytes, range, ordinal, id)?;
        let slot = directories
            .get_mut(ordinal)
            .ok_or(IndexPackOpenError::SegmentCount {
                lane: IndexPackLane::Lexical,
                observed: ordinal + 1,
                maximum: MAX_PACK_SEGMENTS,
            })?;
        *slot = Some(LexicalDirectory { id, range });
        let id_slot = ids
            .get_mut(ordinal)
            .ok_or(IndexPackOpenError::SegmentCount {
                lane: IndexPackLane::Lexical,
                observed: ordinal + 1,
                maximum: MAX_PACK_SEGMENTS,
            })?;
        *id_slot = Some(id);
        *directory_offset = directory_offset
            .checked_add(grammar::INDEX_PACK_DIRECTORY_BYTES)
            .ok_or(IndexPackOpenError::TrailingBytes {
                offset: *directory_offset,
                total: header.total_bytes,
            })?;
        *body_cursor = range.end;
    }
    Ok(())
}

fn validate_order(
    lane: IndexPackLane,
    ordinal: usize,
    previous: Option<[u8; HASH_BYTES]>,
    observed: [u8; HASH_BYTES],
) -> Result<(), IndexPackOpenError> {
    if let Some(previous) = previous
        && previous >= observed
    {
        return Err(IndexPackOpenError::DirectoryOrder {
            lane,
            ordinal,
            previous,
            observed,
        });
    }
    Ok(())
}

fn validate_range(
    start: usize,
    bytes: usize,
    lane: IndexPackLane,
    ordinal: usize,
    expected: usize,
    total: usize,
) -> Result<PackRange, IndexPackOpenError> {
    if start != expected {
        return Err(IndexPackOpenError::SegmentRange {
            lane,
            ordinal,
            expected,
            observed: start,
        });
    }
    let end = grammar::checked_end(start, bytes, lane, ordinal, total)?;
    Ok(PackRange { start, end })
}

fn validate_exact_body(
    bytes: &[u8],
    range: PackRange,
    segment: usize,
    expected: ExactSegmentId,
) -> Result<(), IndexPackOpenError> {
    let body = grammar::bytes_at(bytes, range, IndexPackRegion::Segment(IndexPackLane::Exact))?;
    let count = grammar::row_count(grammar::parse_rows(body, 0, IndexPackLane::Exact)?);
    let data_start = row_data_start(count, range, IndexPackLane::Exact)?;
    let mut verifier =
        ExactSegmentVerifier::new(count).map_err(|error| IndexPackOpenError::SegmentInvariant {
            lane: IndexPackLane::Exact,
            segment,
            invariant: exact_invariant(error),
        })?;
    let mut cursor = data_start;
    for ordinal in 0..count {
        let declared = grammar::parse_offset(
            body,
            row_offset_position(ordinal, range, IndexPackLane::Exact)?,
            IndexPackLane::Exact,
        )?;
        let expected_offset = cursor - range.start;
        if declared != expected_offset {
            return Err(IndexPackOpenError::RowOffset {
                lane: IndexPackLane::Exact,
                segment,
                row: ordinal,
                expected: expected_offset,
                observed: declared,
            });
        }
        let (row, end) = decode_exact_row(body, range, cursor - range.start, segment, ordinal)?;
        verifier
            .admit(row)
            .map_err(|error| IndexPackOpenError::SegmentInvariant {
                lane: IndexPackLane::Exact,
                segment,
                invariant: exact_invariant(error),
            })?;
        cursor = range.start + end;
    }
    if cursor != range.end {
        return Err(IndexPackOpenError::TrailingBytes {
            offset: cursor,
            total: range.end,
        });
    }
    let observed = verifier
        .finish()
        .map_err(|error| IndexPackOpenError::SegmentInvariant {
            lane: IndexPackLane::Exact,
            segment,
            invariant: exact_invariant(error),
        })?;
    if observed != expected {
        return Err(IndexPackOpenError::ExactSegmentIdentity {
            ordinal: segment,
            expected,
            observed,
        });
    }
    Ok(())
}

fn validate_lexical_body(
    bytes: &[u8],
    range: PackRange,
    segment: usize,
    expected: LexicalSegmentId,
) -> Result<(), IndexPackOpenError> {
    let body = grammar::bytes_at(
        bytes,
        range,
        IndexPackRegion::Segment(IndexPackLane::Lexical),
    )?;
    let count = grammar::row_count(grammar::parse_rows(body, 0, IndexPackLane::Lexical)?);
    let data_start = row_data_start(count, range, IndexPackLane::Lexical)?;
    let mut verifier = LexicalSegmentVerifier::new(count).map_err(|error| {
        IndexPackOpenError::SegmentInvariant {
            lane: IndexPackLane::Lexical,
            segment,
            invariant: lexical_invariant(error),
        }
    })?;
    let mut cursor = data_start;
    for ordinal in 0..count {
        let declared = grammar::parse_offset(
            body,
            row_offset_position(ordinal, range, IndexPackLane::Lexical)?,
            IndexPackLane::Lexical,
        )?;
        let expected_offset = cursor - range.start;
        if declared != expected_offset {
            return Err(IndexPackOpenError::RowOffset {
                lane: IndexPackLane::Lexical,
                segment,
                row: ordinal,
                expected: expected_offset,
                observed: declared,
            });
        }
        let (row, end) = decode_lexical_row(body, range, cursor - range.start, segment, ordinal)?;
        verifier
            .admit(row)
            .map_err(|error| IndexPackOpenError::SegmentInvariant {
                lane: IndexPackLane::Lexical,
                segment,
                invariant: lexical_invariant(error),
            })?;
        cursor = range.start + end;
    }
    if cursor != range.end {
        return Err(IndexPackOpenError::TrailingBytes {
            offset: cursor,
            total: range.end,
        });
    }
    let observed = verifier
        .finish()
        .map_err(|error| IndexPackOpenError::SegmentInvariant {
            lane: IndexPackLane::Lexical,
            segment,
            invariant: lexical_invariant(error),
        })?;
    if observed != expected {
        return Err(IndexPackOpenError::LexicalSegmentIdentity {
            ordinal: segment,
            expected,
            observed,
        });
    }
    Ok(())
}

fn decode_exact_row(
    body: &[u8],
    range: PackRange,
    offset: usize,
    segment: usize,
    ordinal: usize,
) -> Result<(ExactRow<'_>, usize), IndexPackOpenError> {
    let record = grammar::parse_exact_row(body, offset)?;
    let key_bytes = grammar::exact_key_bytes(record)?;
    let value_bytes = grammar::exact_value_bytes(record)?;
    let key_start = offset + grammar::EXACT_ROW_PREFIX_BYTES;
    let key = take_row(
        body,
        range.start,
        key_start,
        key_bytes,
        IndexPackLane::Exact,
    )?;
    let value_start =
        key_start
            .checked_add(key_bytes)
            .ok_or(IndexPackOpenError::TrailingBytes {
                offset: range.start + key_start,
                total: range.end,
            })?;
    let end = value_start
        .checked_add(value_bytes)
        .ok_or(IndexPackOpenError::TrailingBytes {
            offset: range.start + value_start,
            total: range.end,
        })?;
    match grammar::exact_state(record) {
        grammar::ROW_PRESENT => Ok((
            ExactRow::present(
                key,
                take_row(
                    body,
                    range.start,
                    value_start,
                    value_bytes,
                    IndexPackLane::Exact,
                )?,
            ),
            end,
        )),
        grammar::ROW_TOMBSTONE if value_bytes == grammar::TOMBSTONE_VALUE_BYTES => {
            Ok((ExactRow::tombstone(key), end))
        }
        grammar::ROW_TOMBSTONE => Err(IndexPackOpenError::TombstoneValue {
            lane: IndexPackLane::Exact,
            segment,
            row: ordinal,
            observed: value_bytes,
        }),
        observed => Err(IndexPackOpenError::RowState {
            lane: IndexPackLane::Exact,
            segment,
            row: ordinal,
            observed: observed.0,
        }),
    }
}

fn decode_lexical_row(
    body: &[u8],
    range: PackRange,
    offset: usize,
    segment: usize,
    ordinal: usize,
) -> Result<(LexicalRow<'_>, usize), IndexPackOpenError> {
    let record = grammar::parse_lexical_row(body, offset)?;
    let term_bytes = grammar::lexical_term_bytes(record)?;
    let term_start = offset + grammar::LEXICAL_ROW_PREFIX_BYTES;
    let term = take_row(
        body,
        range.start,
        term_start,
        term_bytes,
        IndexPackLane::Lexical,
    )?;
    let end = term_start
        .checked_add(term_bytes)
        .ok_or(IndexPackOpenError::TrailingBytes {
            offset: range.start + term_start,
            total: range.end,
        })?;
    let document = EntityDocumentId::try_from(grammar::lexical_document(record).as_slice())
        .map_err(|source| IndexPackOpenError::LexicalDocument {
            segment,
            row: ordinal,
            source,
        })?;
    match grammar::lexical_state(record) {
        grammar::ROW_PRESENT => Ok((
            LexicalRow::new(
                term,
                document,
                LexicalScore::from(grammar::lexical_score(record)),
            ),
            end,
        )),
        grammar::ROW_TOMBSTONE if grammar::lexical_score(record) == grammar::TOMBSTONE_SCORE => {
            Ok((LexicalRow::tombstone(term, document), end))
        }
        grammar::ROW_TOMBSTONE => Err(IndexPackOpenError::TombstoneScore {
            segment,
            row: ordinal,
            observed: grammar::lexical_score(record),
        }),
        observed => Err(IndexPackOpenError::RowState {
            lane: IndexPackLane::Lexical,
            segment,
            row: ordinal,
            observed: observed.0,
        }),
    }
}

fn row_data_start(
    count: usize,
    range: PackRange,
    lane: IndexPackLane,
) -> Result<usize, IndexPackOpenError> {
    let offset_bytes =
        count
            .checked_mul(grammar::ROW_OFFSET_BYTES)
            .ok_or(IndexPackOpenError::TrailingBytes {
                offset: range.start,
                total: range.end,
            })?;
    grammar::checked_end(
        range.start + grammar::SEGMENT_ROW_COUNT_BYTES,
        offset_bytes,
        lane,
        0,
        range.end,
    )
}

fn row_offset_position(
    ordinal: usize,
    range: PackRange,
    lane: IndexPackLane,
) -> Result<usize, IndexPackOpenError> {
    let relative = ordinal
        .checked_mul(grammar::ROW_OFFSET_BYTES)
        .and_then(|bytes| grammar::SEGMENT_ROW_COUNT_BYTES.checked_add(bytes))
        .ok_or(IndexPackOpenError::TrailingBytes {
            offset: range.start,
            total: range.end,
        })?;
    grammar::checked_end(range.start, relative, lane, ordinal, range.end)
        .map(|absolute| absolute - range.start)
}

pub(super) fn take_row(
    body: &[u8],
    body_start: usize,
    offset: usize,
    length: usize,
    lane: IndexPackLane,
) -> Result<&[u8], IndexPackOpenError> {
    let end = offset
        .checked_add(length)
        .ok_or(IndexPackOpenError::Truncated {
            region: IndexPackRegion::Row(lane),
            offset: body_start + offset,
            required: length,
            available: body_start + body.len(),
        })?;
    body.get(offset..end).ok_or(IndexPackOpenError::Truncated {
        region: IndexPackRegion::Row(lane),
        offset: body_start + offset,
        required: length,
        available: body_start + body.len(),
    })
}

const fn exact_invariant(error: ExactSegmentError<'_>) -> IndexPackRowInvariant {
    match error {
        ExactSegmentError::TooManyRows { .. } | ExactSegmentError::RowCount { .. } => {
            IndexPackRowInvariant::RowLimit
        }
        ExactSegmentError::PayloadBytesLimit { .. }
        | ExactSegmentError::PayloadBytesOverflow { .. } => IndexPackRowInvariant::PayloadLimit,
        ExactSegmentError::OutOfOrder { .. } => IndexPackRowInvariant::Order,
        ExactSegmentError::DuplicateKey { .. } => IndexPackRowInvariant::Duplicate,
    }
}

const fn lexical_invariant(error: LexicalSegmentError<'_>) -> IndexPackRowInvariant {
    match error {
        LexicalSegmentError::TooManyRows { .. } | LexicalSegmentError::RowCount { .. } => {
            IndexPackRowInvariant::RowLimit
        }
        LexicalSegmentError::PayloadBytesLimit { .. }
        | LexicalSegmentError::PayloadBytesOverflow { .. } => IndexPackRowInvariant::PayloadLimit,
        LexicalSegmentError::OutOfOrder { .. } => IndexPackRowInvariant::Order,
        LexicalSegmentError::DuplicateRow { .. } => IndexPackRowInvariant::Duplicate,
    }
}

pub(super) fn selected_exact(
    layout: &PackLayout,
    selected: ExactSegmentId,
) -> Result<Option<(usize, ExactDirectory)>, IndexPackOpenError> {
    let mut lower = 0;
    let mut upper = layout.header.exact_count;
    while lower < upper {
        let ordinal = lower + (upper - lower) / 2;
        let directory = exact_directory(layout, ordinal)?;
        match directory.id.cmp(&selected) {
            Ordering::Less => lower = ordinal + 1,
            Ordering::Equal => return Ok(Some((ordinal, directory))),
            Ordering::Greater => upper = ordinal,
        }
    }
    Ok(None)
}

pub(super) fn selected_lexical(
    layout: &PackLayout,
    selected: LexicalSegmentId,
) -> Result<Option<(usize, LexicalDirectory)>, IndexPackOpenError> {
    let mut lower = 0;
    let mut upper = layout.header.lexical_count;
    while lower < upper {
        let ordinal = lower + (upper - lower) / 2;
        let directory = lexical_directory(layout, ordinal)?;
        match directory.id.cmp(&selected) {
            Ordering::Less => lower = ordinal + 1,
            Ordering::Equal => return Ok(Some((ordinal, directory))),
            Ordering::Greater => upper = ordinal,
        }
    }
    Ok(None)
}

fn exact_directory(
    layout: &PackLayout,
    ordinal: usize,
) -> Result<ExactDirectory, IndexPackOpenError> {
    layout
        .exact
        .get(ordinal)
        .copied()
        .flatten()
        .ok_or(IndexPackOpenError::LayoutSlot {
            lane: IndexPackLane::Exact,
            ordinal,
        })
}

fn lexical_directory(
    layout: &PackLayout,
    ordinal: usize,
) -> Result<LexicalDirectory, IndexPackOpenError> {
    layout
        .lexical
        .get(ordinal)
        .copied()
        .flatten()
        .ok_or(IndexPackOpenError::LayoutSlot {
            lane: IndexPackLane::Lexical,
            ordinal,
        })
}

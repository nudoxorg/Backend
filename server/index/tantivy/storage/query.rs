//! Tantivy membership selection and canonical newest-first merge.
use std::{
    cmp::Ordering,
    sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
};

use server_index_core::{
    ENTITY_DOCUMENT_ID_BYTES, EntityDocumentId, IndexSnapshotId, LexicalOperation, LexicalRowValue,
    LexicalScore, LexicalSegmentId, MAX_SELECTED_SEGMENTS,
};
use tantivy::{
    TantivyDocument, Term,
    query::{FuzzyTermQuery, Query, TermQuery},
    schema::{IndexRecordOption, Value},
};

use super::{
    codec,
    model::{
        CandidateInsertion, StorePhase, TantivyCandidate, TantivyProvenance, TantivyRowOrdinal,
        TantivySegment, TantivySegmentHit, TantivySegmentStoreError,
    },
};

#[allow(
    clippy::indexing_slicing,
    reason = "all output and scratch ranges are preflighted against caller capacity"
)]
pub(crate) fn search_operation<'segment>(
    snapshot: IndexSnapshotId,
    segments: &[&'segment TantivySegment],
    operation: LexicalOperation<'_>,
    requested_limit: usize,
    output: &mut [Option<TantivySegmentHit<'segment>>],
    candidates: &mut [Option<TantivyCandidate<'segment>>],
    cancelled: Option<&AtomicBool>,
) -> Result<usize, TantivySegmentStoreError> {
    codec::validate_term_len(operation.term.len())?;
    if segments.len() > MAX_SELECTED_SEGMENTS {
        return Err(TantivySegmentStoreError::SegmentLimit {
            limit: MAX_SELECTED_SEGMENTS,
            observed: segments.len(),
        });
    }
    if requested_limit > output.len() {
        return Err(TantivySegmentStoreError::OutputCapacity {
            required: requested_limit,
            available: output.len(),
        });
    }
    let required_candidates = segments
        .iter()
        .try_fold(0_usize, |total, segment| {
            total.checked_add(segment.rows.len())
        })
        .ok_or(TantivySegmentStoreError::CompositionCapacity)?;
    if candidates.len() < required_candidates {
        return Err(TantivySegmentStoreError::CandidateCapacity {
            required: required_candidates,
            available: candidates.len(),
        });
    }
    for candidate in candidates.iter_mut() {
        *candidate = None;
    }
    for (segment_index, segment) in segments.iter().enumerate() {
        let segment_index =
            u8::try_from(segment_index).map_err(|_| TantivySegmentStoreError::CountOverflow)?;
        if cancelled.is_some_and(|flag| flag.load(AtomicOrdering::Acquire)) {
            return Err(TantivySegmentStoreError::Cancelled);
        }
        select(segment, operation, segment_index, candidates)?;
    }
    let mut live_count = 0_usize;
    for candidate in candidates.iter().flatten() {
        if cancelled.is_some_and(|flag| flag.load(AtomicOrdering::Acquire)) {
            return Err(TantivySegmentStoreError::Cancelled);
        }
        let segment = segments.get(usize::from(candidate.segment_index)).ok_or(
            TantivySegmentStoreError::CandidateSegment {
                segment: candidate.segment,
            },
        )?;
        if segment.id != candidate.segment {
            return Err(TantivySegmentStoreError::CandidateSegment {
                segment: candidate.segment,
            });
        }
        let row = segment.rows.get(candidate.row.as_usize()).ok_or(
            TantivySegmentStoreError::CandidateRow {
                segment: candidate.segment,
                row: candidate.row,
            },
        )?;
        if !matches!(row.value, LexicalRowValue::Tombstone) {
            live_count = live_count
                .checked_add(1)
                .ok_or(TantivySegmentStoreError::CompositionCapacity)?;
        }
    }
    let required = live_count.min(requested_limit);
    for key in candidates.iter().flatten() {
        if cancelled.is_some_and(|flag| flag.load(AtomicOrdering::Acquire)) {
            return Err(TantivySegmentStoreError::Cancelled);
        }
        let segment = segments.get(usize::from(key.segment_index)).ok_or(
            TantivySegmentStoreError::CandidateSegment {
                segment: key.segment,
            },
        )?;
        if segment.id != key.segment {
            return Err(TantivySegmentStoreError::CandidateSegment {
                segment: key.segment,
            });
        }
        let row =
            segment
                .rows
                .get(key.row.as_usize())
                .ok_or(TantivySegmentStoreError::CandidateRow {
                    segment: key.segment,
                    row: key.row,
                })?;
        if let Some(score) = row_score(row.value) {
            operation.relevance(score, row.term.len()).ok_or(
                TantivySegmentStoreError::ScoreDiscount {
                    term_bytes: row.term.len(),
                    prefix_bytes: operation.term.len(),
                },
            )?;
        }
    }
    let mut written = 0_usize;
    for key in candidates.iter().flatten() {
        let segment = segments.get(usize::from(key.segment_index)).ok_or(
            TantivySegmentStoreError::CandidateSegment {
                segment: key.segment,
            },
        )?;
        if segment.id != key.segment {
            return Err(TantivySegmentStoreError::CandidateSegment {
                segment: key.segment,
            });
        }
        let row =
            segment
                .rows
                .get(key.row.as_usize())
                .ok_or(TantivySegmentStoreError::CandidateRow {
                    segment: key.segment,
                    row: key.row,
                })?;
        let Some(score) = row_score(row.value) else {
            continue;
        };
        let hit = TantivySegmentHit {
            provenance: TantivyProvenance {
                snapshot,
                segment: segment.id,
                row: key.row,
                document: row.document,
            },
            term: &row.term,
            score: operation.relevance(score, row.term.len()).ok_or(
                TantivySegmentStoreError::ScoreDiscount {
                    term_bytes: row.term.len(),
                    prefix_bytes: operation.term.len(),
                },
            )?,
        };
        let position = match output[..written]
            .iter()
            .position(|current| current.is_some_and(|current| compare_hits(&hit, &current).is_lt()))
        {
            Some(position) => position,
            None => written,
        };
        if position < required {
            let new_count = (written + 1).min(required);
            for destination in (position + 1..new_count).rev() {
                output[destination] = output[destination - 1];
            }
            output[position] = Some(hit);
            written = new_count;
        }
    }
    for slot in output.iter_mut().skip(written) {
        *slot = None;
    }
    Ok(written)
}

pub(crate) fn remember_candidate<'segment>(
    table: &mut [Option<TantivyCandidate<'segment>>],
    segment: LexicalSegmentId,
    segment_index: u8,
    row: usize,
    term: &'segment [u8],
    document: EntityDocumentId,
) -> Result<CandidateInsertion, TantivySegmentStoreError> {
    if table.is_empty() {
        return Err(TantivySegmentStoreError::CompositionCapacity);
    }
    let mut slot = key_hash(document) % table.len();
    for _ in 0..table.len() {
        match table.get(slot).copied().flatten() {
            None => {
                let target = table
                    .get_mut(slot)
                    .ok_or(TantivySegmentStoreError::CompositionCapacity)?;
                let row = TantivyRowOrdinal::try_from(row)
                    .map_err(|_| TantivySegmentStoreError::CompositionCapacity)?;
                *target = Some(TantivyCandidate {
                    segment,
                    segment_index,
                    row,
                    term,
                    document,
                });
                return Ok(CandidateInsertion::Inserted);
            }
            Some(seen) if seen.document == document => {
                if segment_index == seen.segment_index && row < seen.row.as_usize() {
                    let target = table
                        .get_mut(slot)
                        .ok_or(TantivySegmentStoreError::CompositionCapacity)?;
                    let row = TantivyRowOrdinal::try_from(row)
                        .map_err(|_| TantivySegmentStoreError::CompositionCapacity)?;
                    *target = Some(TantivyCandidate {
                        segment,
                        segment_index,
                        row,
                        term,
                        document,
                    });
                    return Ok(CandidateInsertion::ReplacedSameSegment);
                }
                return Ok(CandidateInsertion::KeptExisting);
            }
            Some(_) => slot = (slot + 1) % table.len(),
        }
    }
    Err(TantivySegmentStoreError::CompositionCapacity)
}

fn key_hash(document: EntityDocumentId) -> usize {
    let bytes: [u8; ENTITY_DOCUMENT_ID_BYTES] = document.into();
    bytes.iter().fold(2_166_136_261_usize, |hash, byte| {
        hash.wrapping_mul(16_777_619)
            .wrapping_add(usize::from(*byte))
    })
}

pub(crate) const fn row_score(value: LexicalRowValue) -> Option<LexicalScore> {
    match value {
        LexicalRowValue::Present(score) => Some(score),
        LexicalRowValue::Tombstone => None,
    }
}

pub(crate) fn compare_hits(
    left: &TantivySegmentHit<'_>,
    right: &TantivySegmentHit<'_>,
) -> Ordering {
    compare(
        left.score,
        &left.provenance.document,
        left.term,
        &left.provenance.segment,
        right.score,
        &right.provenance.document,
        right.term,
        &right.provenance.segment,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "comparison receives the complete fixed ranking key without heap allocation"
)]
pub(crate) fn compare(
    left_score: LexicalScore,
    left_document: &EntityDocumentId,
    left_term: &[u8],
    left_segment: &LexicalSegmentId,
    right_score: LexicalScore,
    right_document: &EntityDocumentId,
    right_term: &[u8],
    right_segment: &LexicalSegmentId,
) -> Ordering {
    right_score
        .cmp(&left_score)
        .then_with(|| left_document.cmp(right_document))
        .then_with(|| left_term.cmp(right_term))
        .then_with(|| left_segment.cmp(right_segment))
}

pub(crate) fn select<'segment>(
    segment: &'segment TantivySegment,
    operation: LexicalOperation<'_>,
    segment_index: u8,
    candidates: &mut [Option<TantivyCandidate<'segment>>],
) -> Result<(), TantivySegmentStoreError> {
    codec::validate_term_len(operation.term.len())?;
    let query = codec::encode_term(operation.term)?;
    let term = Term::from_field_text(segment.body_field, &query);
    match operation.match_mode {
        server_index_core::LexicalMatch::Exact => {
            let parsed = TermQuery::new(term, IndexRecordOption::Basic);
            collect_backend_matches(segment, operation, segment_index, &parsed, candidates)
        }
        server_index_core::LexicalMatch::Prefix => {
            let parsed = FuzzyTermQuery::new_prefix(term, 0, true);
            collect_backend_matches(segment, operation, segment_index, &parsed, candidates)
        }
    }
}

fn collect_backend_matches<'segment, Q: Query>(
    segment: &'segment TantivySegment,
    operation: LexicalOperation<'_>,
    segment_index: u8,
    parsed: &Q,
    candidates: &mut [Option<TantivyCandidate<'segment>>],
) -> Result<(), TantivySegmentStoreError> {
    let searcher = segment.reader.searcher();
    let count = usize::try_from(searcher.num_docs())
        .map_err(|_| TantivySegmentStoreError::CountOverflow)?;
    if count > server_index_core::MAX_LEXICAL_ROWS {
        return Err(TantivySegmentStoreError::CompositionCapacity);
    }
    if count == 0 {
        return Ok(());
    }
    // `count` is the validated backend document count and is capped above by the core row bound;
    // this collector therefore cannot allocate an unbounded match set or depend on caller TopK.
    let collector = tantivy::collector::TopDocs::with_limit(count).order_by_score();
    let matches = searcher.search(parsed, &collector).map_err(|source| {
        TantivySegmentStoreError::Tantivy {
            phase: StorePhase::Search,
            path: segment.path.clone(),
            source,
        }
    })?;
    for (_, address) in matches {
        let document: TantivyDocument =
            searcher
                .doc(address)
                .map_err(|source| TantivySegmentStoreError::Tantivy {
                    phase: StorePhase::ReadDocument,
                    path: segment.path.clone(),
                    source,
                })?;
        let ordinal = document
            .get_first(segment.ordinal_field)
            .and_then(|value| value.as_u64())
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(TantivySegmentStoreError::Corrupt {
                path: segment.path.clone(),
                detail: "backend result ordinal missing",
            })?;
        let row = segment
            .rows
            .get(ordinal)
            .ok_or(TantivySegmentStoreError::Corrupt {
                path: segment.path.clone(),
                detail: "backend result ordinal out of range",
            })?;
        let selected = match operation.match_mode {
            server_index_core::LexicalMatch::Exact => row.term == operation.term,
            server_index_core::LexicalMatch::Prefix => row.term.starts_with(operation.term),
        };
        if !selected {
            return Err(TantivySegmentStoreError::Corrupt {
                path: segment.path.clone(),
                detail: "backend result disagrees with canonical row",
            });
        }
        let _ = remember_candidate(
            candidates,
            segment.id,
            segment_index,
            ordinal,
            &row.term,
            row.document,
        )?;
    }
    Ok(())
}

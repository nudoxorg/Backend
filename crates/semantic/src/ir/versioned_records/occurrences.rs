//! Canonical source-site evidence rows for stable graph relations.

use alloc::vec::Vec;

use crate::ir::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, Confidence, FactAvailability,
    LinkOccurrence, LinkOccurrenceId, SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError,
    SemanticReader, SourceSpan,
};

use super::{
    relations::relation_key,
    wire::{Cursor, put_bytes},
};

const OCCURRENCE_TAG: u8 = 4;
const OCCURRENCE_BASE_DOMAIN: &[u8] = b"backend.semantic.ir.occurrence-base.v1\0";
const OCCURRENCE_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.occurrence-row.v1\0";

#[derive(Clone, Copy)]
struct OccurrencePlanRow {
    id: LinkOccurrenceId,
    occurrence: LinkOccurrence,
    base_digest: [u8; 32],
    duplicate_rank: u32,
}

/// Opaque reader-local occurrence handle used by the public row encoder trait.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct OccurrenceHandle {
    id: LinkOccurrenceId,
    occurrence: LinkOccurrence,
    base_digest: [u8; 32],
    duplicate_rank: u32,
}

/// Stable-key encoder for site-local evidence on canonical graph relations.
#[derive(Clone, Copy, Debug, Default)]
pub struct OccurrenceRows;

impl CanonicalPlaneRowEncoder for OccurrenceRows {
    type Handle = OccurrenceHandle;
    type Plan = ();

    fn kind(&self) -> SemanticPlaneKind {
        SemanticPlaneKind::Ir(SemanticIrPlane::Occurrences)
    }

    fn build_plan<Reader: SemanticReader + ?Sized>(
        &self,
        _reader: &Reader,
    ) -> Result<Self::Plan, SemanticPlaneRecordError> {
        Ok(())
    }

    fn collect_keys<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        sink: &mut CanonicalSemanticPlaneKeySink<Self::Handle>,
    ) -> Result<(), SemanticPlaneRecordError> {
        let mut rows = Vec::new();
        let mut base_bytes = Vec::new();
        for (id, occurrence) in reader.link_occurrences() {
            sink.check_row_count(rows.len())?;
            rows.try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            base_bytes.clear();
            encode_occurrence_base(reader, id, occurrence, &mut base_bytes)?;
            rows.push(OccurrencePlanRow {
                id,
                occurrence,
                base_digest: occurrence_digest_from_bytes(&base_bytes),
                duplicate_rank: 0,
            });
        }
        rows.sort_unstable_by(|left, right| {
            left.base_digest
                .cmp(&right.base_digest)
                .then_with(|| left.id.cmp(&right.id))
        });
        assign_duplicate_ranks(reader, &mut rows)?;
        for row in rows {
            let handle = OccurrenceHandle {
                id: row.id,
                occurrence: row.occurrence,
                base_digest: row.base_digest,
                duplicate_rank: row.duplicate_rank,
            };
            sink.push(
                occurrence_row_key(row.base_digest, row.duplicate_rank),
                handle,
            )?;
        }
        Ok(())
    }

    fn encode_row<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        handle: Self::Handle,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        let before = out.len();
        encode_occurrence_base(reader, handle.id, handle.occurrence, out)?;
        if occurrence_digest_from_bytes(&out[before..]) != handle.base_digest {
            return Err(SemanticPlaneRecordError::StableKeyMismatch);
        }
        out.extend_from_slice(&handle.duplicate_rank.to_be_bytes());
        Ok(OCCURRENCE_TAG)
    }
}

fn assign_duplicate_ranks<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    rows: &mut [OccurrencePlanRow],
) -> Result<(), SemanticPlaneRecordError> {
    let mut start = 0;
    let mut first_bytes = Vec::new();
    let mut candidate = Vec::new();
    while start < rows.len() {
        let digest = rows[start].base_digest;
        let mut end = start + 1;
        while end < rows.len() && rows[end].base_digest == digest {
            end += 1;
        }
        first_bytes.clear();
        encode_occurrence_base(
            reader,
            rows[start].id,
            rows[start].occurrence,
            &mut first_bytes,
        )?;
        for row in &rows[start + 1..end] {
            candidate.clear();
            encode_occurrence_base(reader, row.id, row.occurrence, &mut candidate)?;
            if candidate != first_bytes {
                return Err(SemanticPlaneRecordError::StableKeyCollision);
            }
        }
        for (rank, row) in rows[start..end].iter_mut().enumerate() {
            row.duplicate_rank =
                u32::try_from(rank).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        }
        start = end;
    }
    Ok(())
}

fn occurrence_digest_from_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(OCCURRENCE_BASE_DOMAIN);
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn occurrence_row_key(base_digest: [u8; 32], duplicate_rank: u32) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(OCCURRENCE_KEY_DOMAIN);
    hasher.update(&base_digest);
    hasher.update(&duplicate_rank.to_be_bytes());
    *hasher.finalize().as_bytes()
}

fn encode_occurrence_base<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    id: LinkOccurrenceId,
    occurrence: LinkOccurrence,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let link = reader
        .link(occurrence.link)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    out.extend_from_slice(&relation_key(reader, link)?);
    out.push(occurrence.confidence as u8);
    let authority = reader
        .occurrence_authority(id)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    out.push(availability_code(authority.source));
    match occurrence.source {
        None => out.push(0),
        Some(span) => {
            out.push(1);
            encode_span(reader, span, out)?;
        }
    }
    Ok(())
}

fn encode_span<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    span: SourceSpan,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let path = reader
        .atom(span.file())
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    put_bytes(out, path)?;
    out.extend_from_slice(&span.start().to_be_bytes());
    out.extend_from_slice(&span.end().to_be_bytes());
    Ok(())
}

const fn availability_code(value: FactAvailability) -> u8 {
    match value {
        FactAvailability::Captured => 1,
        FactAvailability::Unavailable => 0,
    }
}

pub(super) fn validate_record(
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    if tag != OCCURRENCE_TAG {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut cursor = Cursor::new(payload);
    let _relation = cursor.take(32)?;
    let confidence = cursor.u8()?;
    if confidence > Confidence::Compiler as u8 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let availability = cursor.u8()?;
    if availability > 1 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let has_source = match cursor.u8()? {
        0 => false,
        1 => {
            let _ = cursor.bytes32()?;
            let start = cursor.u32()?;
            let end = cursor.u32()?;
            if start > end {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            true
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    };
    if (availability == 1) != has_source {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let rank_offset = payload
        .len()
        .checked_sub(4)
        .ok_or(SemanticPlaneRecordError::Truncated)?;
    let duplicate_rank = cursor.u32()?;
    if !cursor.is_empty() {
        return Err(SemanticPlaneRecordError::RowTrailingBytes);
    }
    let base_digest = occurrence_digest_from_bytes(&payload[..rank_offset]);
    if occurrence_row_key(base_digest, duplicate_rank) != key {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(())
}

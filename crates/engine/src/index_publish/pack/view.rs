//! Defines pack view behavior for `backend-engine index_publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the pack view invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Owner-generic validation and short-lived borrowed exact and lexical query views.

use core::{cmp::Ordering, ops::Deref};

use backend_semantic::index_core::{EntityDocumentId, LexicalScore};
use backend_semantic::index_vocabulary::{
    ExactSegmentId, IndexPackId, IndexSnapshotId, LexicalSegmentId,
};
use backend_version::ArtifactId;

use crate::index_publish::pack::{
    error::{IndexPackLane, IndexPackOpenError, IndexPackRegion, RejectedIndexPack},
    grammar::{self, PackLayout},
};

mod proof;

use proof::{selected_exact, selected_lexical, take_row, validate};

/// Immutable readable facts of one validated content-addressed index pack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexPackFacts {
    /// Physical artifact identity derived from every complete pack byte.
    pub id: IndexPackId,
    /// Compiler-derived generation bound by the canonical snapshot.
    pub generation: backend_version::GenerationId,
    /// Immutable exact-and-lexical snapshot identity.
    pub snapshot: IndexSnapshotId,
    /// Selected exact segment count.
    pub exact_segments: usize,
    /// Selected lexical segment count.
    pub lexical_segments: usize,
}

/// A validated immutable byte owner with compact fixed-capacity layout metadata.
pub struct IndexPack<Owner>
where
    Owner: AsRef<[u8]>,
{
    facts: IndexPackFacts,
    owner: Owner,
    layout: PackLayout,
}

impl<Owner> Deref for IndexPack<Owner>
where
    Owner: AsRef<[u8]>,
{
    type Target = IndexPackFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<Owner> AsRef<[u8]> for IndexPack<Owner>
where
    Owner: AsRef<[u8]>,
{
    fn as_ref(&self) -> &[u8] {
        self.owner.as_ref()
    }
}

impl<Owner> IndexPack<Owner>
where
    Owner: AsRef<[u8]>,
{
    /// Validates an arbitrary immutable owner against the identity supplied by its receipt or path.
    ///
    /// Validation retains only fixed inline directory metadata. The owner stays independent, so an
    /// mmap, caller slab, boxed bytes, or `Vec<u8>` can all use the same no-self-reference view.
    ///
    /// # Errors
    ///
    /// Returns the caller's unchanged owner with exact identity, grammar, authority, or semantic
    /// validation evidence.
    #[allow(
        clippy::result_large_err,
        reason = "rejected owner-generic validation returns the original immutable byte owner unchanged"
    )]
    pub fn open(owner: Owner, expected: IndexPackId) -> Result<Self, RejectedIndexPack<Owner>> {
        let bytes = owner.as_ref();
        let observed = ArtifactId::from_encoded_bytes(bytes);
        if observed != expected {
            return Err(RejectedIndexPack {
                error: IndexPackOpenError::Identity { expected, observed },
                owner,
            });
        }
        let layout = match validate(bytes) {
            Ok(layout) => layout,
            Err(error) => return Err(RejectedIndexPack { error, owner }),
        };
        Ok(Self {
            facts: IndexPackFacts {
                id: expected,
                generation: layout.header.generation,
                snapshot: layout.header.snapshot,
                exact_segments: layout.header.exact_count,
                lexical_segments: layout.header.lexical_count,
            },
            owner,
            layout,
        })
    }

    /// Borrows this immutable owner through the validated pack grammar without revalidation.
    #[must_use]
    pub fn view(&self) -> IndexPackView<'_> {
        IndexPackView {
            facts: self.facts,
            bytes: self.owner.as_ref(),
            layout: &self.layout,
        }
    }
}

/// A short-lived borrowed pack query view independent of its concrete immutable byte owner.
pub struct IndexPackView<'bytes> {
    /// Immutable public pack facts.
    pub facts: IndexPackFacts,
    bytes: &'bytes [u8],
    layout: &'bytes PackLayout,
}

impl Deref for IndexPackView<'_> {
    type Target = IndexPackFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<'bytes> IndexPackView<'bytes> {
    /// Selects one exact segment by its typed immutable identity without allocation.
    ///
    /// # Errors
    ///
    /// Returns the exact preserved structural cause if a privately retained layout proof cannot
    /// reborrow its declared body range.
    pub fn exact(
        &self,
        selected: ExactSegmentId,
    ) -> Result<Option<ExactPackSegment<'bytes>>, IndexPackOpenError> {
        let Some((ordinal, directory)) = selected_exact(self.layout, selected)? else {
            return Ok(None);
        };
        Ok(Some(ExactPackSegment {
            id: directory.id,
            ordinal,
            body: grammar::bytes_at(
                self.bytes,
                directory.range,
                IndexPackRegion::Segment(IndexPackLane::Exact),
            )?,
            body_start: directory.range.start,
        }))
    }

    /// Selects one lexical segment by its typed immutable identity without allocation.
    ///
    /// # Errors
    ///
    /// Returns the exact preserved structural cause if a privately retained layout proof cannot
    /// reborrow its declared body range.
    pub fn lexical(
        &self,
        selected: LexicalSegmentId,
    ) -> Result<Option<LexicalPackSegment<'bytes>>, IndexPackOpenError> {
        let Some((ordinal, directory)) = selected_lexical(self.layout, selected)? else {
            return Ok(None);
        };
        Ok(Some(LexicalPackSegment {
            id: directory.id,
            ordinal,
            body: grammar::bytes_at(
                self.bytes,
                directory.range,
                IndexPackRegion::Segment(IndexPackLane::Lexical),
            )?,
            body_start: directory.range.start,
        }))
    }
}

/// One borrowed exact segment body proven by an immutable pack directory.
pub struct ExactPackSegment<'bytes> {
    /// Typed exact segment identity named by the pack snapshot.
    pub id: ExactSegmentId,
    ordinal: usize,
    body: &'bytes [u8],
    body_start: usize,
}

impl<'bytes> ExactPackSegment<'bytes> {
    /// Performs an allocation-free binary search over canonical variable-width exact rows.
    ///
    /// # Errors
    ///
    /// Returns a concrete row or layout cause rather than treating a malformed retained body as
    /// an absent key.
    pub fn lookup(&self, key: &[u8]) -> Result<Option<ExactPackRow<'bytes>>, IndexPackOpenError> {
        let count = self.row_count()?;
        let mut lower = 0;
        let mut upper = count;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            let row = self.row(middle)?;
            match row.key.cmp(key) {
                Ordering::Less => lower = middle + 1,
                Ordering::Equal => return Ok(Some(row)),
                Ordering::Greater => upper = middle,
            }
        }
        Ok(None)
    }

    /// Reborrows one exact row without allocating or rebuilding any semantic ID.
    ///
    /// # Errors
    ///
    /// Returns an exact row-ordinal, row-state, or retained-body structural cause.
    pub fn row(&self, ordinal: usize) -> Result<ExactPackRow<'bytes>, IndexPackOpenError> {
        let count = self.row_count()?;
        let offset = self.row_offset(ordinal, count)?;
        let record = grammar::parse_exact_row(self.body, offset)?;
        let state = grammar::exact_state(record);
        let key_bytes = grammar::exact_key_bytes(record)?;
        let value_bytes = grammar::exact_value_bytes(record)?;
        let key_start = offset.checked_add(grammar::EXACT_ROW_PREFIX_BYTES).ok_or(
            IndexPackOpenError::TrailingBytes {
                offset: self.body_start + offset,
                total: self.body_start + self.body.len(),
            },
        )?;
        let key = take_row(
            self.body,
            self.body_start,
            key_start,
            key_bytes,
            IndexPackLane::Exact,
        )?;
        let value_start =
            key_start
                .checked_add(key_bytes)
                .ok_or(IndexPackOpenError::TrailingBytes {
                    offset: self.body_start + key_start,
                    total: self.body_start + self.body.len(),
                })?;
        match state {
            grammar::ROW_PRESENT => Ok(ExactPackRow {
                key,
                value: ExactPackValue::Present(take_row(
                    self.body,
                    self.body_start,
                    value_start,
                    value_bytes,
                    IndexPackLane::Exact,
                )?),
            }),
            grammar::ROW_TOMBSTONE if value_bytes == grammar::TOMBSTONE_VALUE_BYTES => {
                Ok(ExactPackRow {
                    key,
                    value: ExactPackValue::Tombstone,
                })
            }
            grammar::ROW_TOMBSTONE => Err(IndexPackOpenError::TombstoneValue {
                lane: IndexPackLane::Exact,
                segment: self.ordinal,
                row: ordinal,
                observed: value_bytes,
            }),
            observed => Err(IndexPackOpenError::RowState {
                lane: IndexPackLane::Exact,
                segment: self.ordinal,
                row: ordinal,
                observed: observed.0,
            }),
        }
    }

    fn row_count(&self) -> Result<usize, IndexPackOpenError> {
        grammar::parse_rows(self.body, 0, IndexPackLane::Exact).map(grammar::row_count)
    }

    fn row_offset(&self, ordinal: usize, count: usize) -> Result<usize, IndexPackOpenError> {
        if ordinal >= count {
            return Err(IndexPackOpenError::RowOrdinal {
                lane: IndexPackLane::Exact,
                segment: self.ordinal,
                observed: ordinal,
                count,
            });
        }
        let table_offset = grammar::SEGMENT_ROW_COUNT_BYTES
            .checked_add(ordinal.checked_mul(grammar::ROW_OFFSET_BYTES).ok_or(
                IndexPackOpenError::TrailingBytes {
                    offset: self.body_start,
                    total: self.body_start + self.body.len(),
                },
            )?)
            .ok_or(IndexPackOpenError::TrailingBytes {
                offset: self.body_start,
                total: self.body_start + self.body.len(),
            })?;
        grammar::parse_offset(self.body, table_offset, IndexPackLane::Exact)
    }
}

/// One borrowed exact row, with a typed present/tombstone state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactPackRow<'bytes> {
    /// Canonical key bytes borrowed from the immutable pack owner.
    pub key: &'bytes [u8],
    /// Present value or immutable tombstone state.
    pub value: ExactPackValue<'bytes>,
}

/// Exact value state borrowed from one immutable packed row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactPackValue<'bytes> {
    /// Canonical value bytes borrowed from the immutable pack owner.
    Present(&'bytes [u8]),
    /// Immutable deletion fact for this key.
    Tombstone,
}

/// One borrowed lexical segment body proven by an immutable pack directory.
pub struct LexicalPackSegment<'bytes> {
    /// Typed lexical segment identity named by the pack snapshot.
    pub id: LexicalSegmentId,
    ordinal: usize,
    body: &'bytes [u8],
    body_start: usize,
}

impl<'bytes> LexicalPackSegment<'bytes> {
    /// Returns the contiguous row interval for one exact term without allocating.
    ///
    /// # Errors
    ///
    /// Returns the exact retained-row structural cause if the validated body cannot be reborrowed.
    pub fn term_range(&self, term: &[u8]) -> Result<core::ops::Range<usize>, IndexPackOpenError> {
        let count = self.row_count()?;
        let first = self.lower_bound(term, count)?;
        let last = self.upper_bound(term, count)?;
        Ok(first..last)
    }

    /// Reborrows one lexical row without allocation or document deserialization beyond its ID wire.
    ///
    /// # Errors
    ///
    /// Returns an exact row-ordinal, document-authority, row-state, or structural cause.
    pub fn row(&self, ordinal: usize) -> Result<LexicalPackRow<'bytes>, IndexPackOpenError> {
        let count = self.row_count()?;
        let offset = self.row_offset(ordinal, count)?;
        let record = grammar::parse_lexical_row(self.body, offset)?;
        let term_bytes = grammar::lexical_term_bytes(record)?;
        let term_start = offset
            .checked_add(grammar::LEXICAL_ROW_PREFIX_BYTES)
            .ok_or(IndexPackOpenError::TrailingBytes {
                offset: self.body_start + offset,
                total: self.body_start + self.body.len(),
            })?;
        let term = take_row(
            self.body,
            self.body_start,
            term_start,
            term_bytes,
            IndexPackLane::Lexical,
        )?;
        let document = EntityDocumentId::try_from(grammar::lexical_document(record).as_slice())
            .map_err(|source| IndexPackOpenError::LexicalDocument {
                segment: self.ordinal,
                row: ordinal,
                source,
            })?;
        let value = match grammar::lexical_state(record) {
            grammar::ROW_PRESENT => {
                LexicalPackValue::Present(LexicalScore::from(grammar::lexical_score(record)))
            }
            grammar::ROW_TOMBSTONE
                if grammar::lexical_score(record) == grammar::TOMBSTONE_SCORE =>
            {
                LexicalPackValue::Tombstone
            }
            grammar::ROW_TOMBSTONE => {
                return Err(IndexPackOpenError::TombstoneScore {
                    segment: self.ordinal,
                    row: ordinal,
                    observed: grammar::lexical_score(record),
                });
            }
            observed => {
                return Err(IndexPackOpenError::RowState {
                    lane: IndexPackLane::Lexical,
                    segment: self.ordinal,
                    row: ordinal,
                    observed: observed.0,
                });
            }
        };
        Ok(LexicalPackRow {
            term,
            document,
            value,
        })
    }

    fn row_count(&self) -> Result<usize, IndexPackOpenError> {
        grammar::parse_rows(self.body, 0, IndexPackLane::Lexical).map(grammar::row_count)
    }

    fn row_offset(&self, ordinal: usize, count: usize) -> Result<usize, IndexPackOpenError> {
        if ordinal >= count {
            return Err(IndexPackOpenError::RowOrdinal {
                lane: IndexPackLane::Lexical,
                segment: self.ordinal,
                observed: ordinal,
                count,
            });
        }
        let table_offset = grammar::SEGMENT_ROW_COUNT_BYTES
            .checked_add(ordinal.checked_mul(grammar::ROW_OFFSET_BYTES).ok_or(
                IndexPackOpenError::TrailingBytes {
                    offset: self.body_start,
                    total: self.body_start + self.body.len(),
                },
            )?)
            .ok_or(IndexPackOpenError::TrailingBytes {
                offset: self.body_start,
                total: self.body_start + self.body.len(),
            })?;
        grammar::parse_offset(self.body, table_offset, IndexPackLane::Lexical)
    }

    fn lower_bound(&self, term: &[u8], count: usize) -> Result<usize, IndexPackOpenError> {
        self.bound(term, count, TermBound::First)
    }

    fn upper_bound(&self, term: &[u8], count: usize) -> Result<usize, IndexPackOpenError> {
        self.bound(term, count, TermBound::PastLast)
    }

    fn bound(
        &self,
        term: &[u8],
        count: usize,
        bound: TermBound,
    ) -> Result<usize, IndexPackOpenError> {
        let mut lower = 0;
        let mut upper = count;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            let row = self.row(middle)?;
            let comparison = row.term.cmp(term);
            let before_bound = match bound {
                TermBound::First => comparison == Ordering::Less,
                TermBound::PastLast => comparison != Ordering::Greater,
            };
            if before_bound {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        Ok(lower)
    }
}

#[derive(Clone, Copy)]
enum TermBound {
    First,
    PastLast,
}

/// One lexical row borrowed from the immutable packed bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalPackRow<'bytes> {
    /// Canonical term bytes borrowed from the immutable pack owner.
    pub term: &'bytes [u8],
    /// Globally addressable immutable entity document identity.
    pub document: EntityDocumentId,
    /// Present score or immutable deletion state.
    pub value: LexicalPackValue,
}

/// Lexical membership state retained by a packed row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexicalPackValue {
    /// Deterministic recipe score.
    Present(LexicalScore),
    /// Immutable deletion fact for this term/document membership.
    Tombstone,
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use zerocopy::{
        IntoBytes,
        byteorder::{LittleEndian, U32},
    };

    use backend_semantic::index_core::{ExactRow, IndexSnapshot};

    use super::*;
    use crate::index_publish::pack::{
        MAX_PACK_SEGMENTS,
        grammar::{self, ExactDirectory, LexicalDirectory, PackHeader},
    };

    #[derive(Debug, thiserror::Error)]
    enum PackTestError {
        #[error("exact fixture row was rejected")]
        Exact,
        #[error("fixture snapshot was rejected")]
        Snapshot(#[source] backend_semantic::index_core::IndexSnapshotError),
        #[error("fixture pack exceeded its fixed grammar address space")]
        Address(#[source] core::num::TryFromIntError),
        #[error("fixture pack header could not encode its fixed address space")]
        Encoding(#[from] crate::index_publish::pack::error::IndexPackEncodeError),
        #[error("fixture byte geometry was inconsistent")]
        Geometry,
        #[error("validated pack layout retained unexpected row scratch")]
        Layout,
        #[error("malformed pack unexpectedly validated")]
        Accepted,
    }

    #[test]
    fn hostile_pack_geometry_retains_specific_structural_causes() -> Result<(), PackTestError> {
        let valid = valid_exact_pack()?;
        let valid_id = IndexPackId::from_encoded_bytes(&valid);
        if IndexPack::open(valid, valid_id).is_err() {
            return Err(PackTestError::Accepted);
        }

        let mut wrong_authority = valid_exact_pack()?;
        let lexical = LexicalSegmentId::from_canonical_bytes(b"foreign-directory-authority");
        write_at(
            &mut wrong_authority,
            grammar::INDEX_PACK_HEADER_BYTES,
            lexical.as_ref(),
        )?;
        match rejected(wrong_authority)? {
            IndexPackOpenError::DirectoryAuthority {
                lane: IndexPackLane::Exact,
                ordinal: 0,
                ..
            } => {}
            _ => return Err(PackTestError::Accepted),
        }

        let mut overlapping_row = valid_exact_pack()?;
        let offset = grammar::INDEX_PACK_HEADER_BYTES
            + grammar::INDEX_PACK_DIRECTORY_BYTES
            + grammar::SEGMENT_ROW_COUNT_BYTES;
        write_at(
            &mut overlapping_row,
            offset,
            U32::<LittleEndian>::new(0).as_bytes(),
        )?;
        match rejected(overlapping_row)? {
            IndexPackOpenError::RowOffset {
                lane: IndexPackLane::Exact,
                segment: 0,
                row: 0,
                ..
            } => {}
            _ => return Err(PackTestError::Accepted),
        }

        let mut truncated = valid_exact_pack()?;
        let declared = truncated
            .len()
            .checked_sub(1)
            .ok_or(PackTestError::Geometry)?;
        truncated.pop().ok_or(PackTestError::Geometry)?;
        let header = grammar::canonical_header(
            u32::try_from(declared).map_err(PackTestError::Address)?,
            generation(),
            exact_snapshot()?,
            1,
            0,
        );
        write_at(&mut truncated, 0, header?.as_bytes())?;
        match rejected(truncated)? {
            IndexPackOpenError::SegmentEnd {
                lane: IndexPackLane::Exact,
                ordinal: 0,
                ..
            } => {}
            _ => return Err(PackTestError::Accepted),
        }

        match rejected(duplicate_directory_pack()?)? {
            IndexPackOpenError::DirectoryOrder {
                lane: IndexPackLane::Exact,
                ordinal: 1,
                ..
            } => Ok(()),
            _ => Err(PackTestError::Accepted),
        }
    }

    #[test]
    fn validated_layout_is_inline_and_rowless() -> Result<(), PackTestError> {
        let fixed_layout_bytes = size_of::<PackHeader>()
            + MAX_PACK_SEGMENTS
                * (size_of::<Option<ExactDirectory>>() + size_of::<Option<LexicalDirectory>>());
        if size_of::<PackLayout>() != fixed_layout_bytes
            || size_of::<PackLayout>() >= grammar::MAX_INDEX_PACK_BYTES
        {
            return Err(PackTestError::Layout);
        }
        Ok(())
    }

    fn valid_exact_pack() -> Result<Vec<u8>, PackTestError> {
        let generation = generation();
        let snapshot = exact_snapshot()?;
        let rows = [ExactRow::present(b"alpha", b"value")];
        let segment = backend_semantic::index_core::ExactSegment::new(&rows)
            .map_err(|_| PackTestError::Exact)?;
        let body = exact_body(&rows)?;
        let total = grammar::INDEX_PACK_HEADER_BYTES
            .checked_add(grammar::INDEX_PACK_DIRECTORY_BYTES)
            .and_then(|value| value.checked_add(body.len()))
            .ok_or(PackTestError::Geometry)?;
        let header = grammar::canonical_header(
            u32::try_from(total).map_err(PackTestError::Address)?,
            generation,
            snapshot,
            1,
            0,
        );
        let directory = grammar::canonical_directory(
            *segment.id,
            u32::try_from(grammar::INDEX_PACK_HEADER_BYTES + grammar::INDEX_PACK_DIRECTORY_BYTES)
                .map_err(PackTestError::Address)?,
            u32::try_from(body.len()).map_err(PackTestError::Address)?,
        );
        let mut bytes = Vec::with_capacity(total);
        bytes.extend_from_slice(header?.as_bytes());
        bytes.extend_from_slice(directory.as_bytes());
        bytes.extend_from_slice(&body);
        if bytes.len() != total {
            return Err(PackTestError::Geometry);
        }
        Ok(bytes)
    }

    fn duplicate_directory_pack() -> Result<Vec<u8>, PackTestError> {
        let original = valid_exact_pack()?;
        let body_start = grammar::INDEX_PACK_HEADER_BYTES + grammar::INDEX_PACK_DIRECTORY_BYTES;
        let body = original.get(body_start..).ok_or(PackTestError::Geometry)?;
        let rows = [ExactRow::present(b"alpha", b"value")];
        let segment = backend_semantic::index_core::ExactSegment::new(&rows)
            .map_err(|_| PackTestError::Exact)?;
        let id = segment.id;
        let duplicate_body_start = body_start
            .checked_add(grammar::INDEX_PACK_DIRECTORY_BYTES)
            .ok_or(PackTestError::Geometry)?;
        let total = duplicate_body_start
            .checked_add(body.len().checked_mul(2).ok_or(PackTestError::Geometry)?)
            .ok_or(PackTestError::Geometry)?;
        let header = grammar::canonical_header(
            u32::try_from(total).map_err(PackTestError::Address)?,
            generation(),
            IndexSnapshotId::from_canonical_bytes(b"duplicate directory reaches order first"),
            2,
            0,
        );
        let first = grammar::canonical_directory(
            *id,
            u32::try_from(duplicate_body_start).map_err(PackTestError::Address)?,
            u32::try_from(body.len()).map_err(PackTestError::Address)?,
        );
        let second_start = duplicate_body_start
            .checked_add(body.len())
            .ok_or(PackTestError::Geometry)?;
        let second = grammar::canonical_directory(
            *id,
            u32::try_from(second_start).map_err(PackTestError::Address)?,
            u32::try_from(body.len()).map_err(PackTestError::Address)?,
        );
        let mut bytes = Vec::with_capacity(total);
        bytes.extend_from_slice(header?.as_bytes());
        bytes.extend_from_slice(first.as_bytes());
        bytes.extend_from_slice(second.as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(body);
        Ok(bytes)
    }

    fn exact_body(rows: &[ExactRow<'_>]) -> Result<Vec<u8>, PackTestError> {
        let data_start = grammar::SEGMENT_ROW_COUNT_BYTES
            .checked_add(
                rows.len()
                    .checked_mul(grammar::ROW_OFFSET_BYTES)
                    .ok_or(PackTestError::Geometry)?,
            )
            .ok_or(PackTestError::Geometry)?;
        let row = rows.first().ok_or(PackTestError::Geometry)?;
        let value = row.value_bytes().ok_or(PackTestError::Geometry)?;
        let record_wire = grammar::canonical_exact_row(
            grammar::ROW_PRESENT,
            u32::try_from(row.key.len()).map_err(PackTestError::Address)?,
            u32::try_from(value.len()).map_err(PackTestError::Address)?,
        );
        let row_count_wire =
            grammar::canonical_rows(u16::try_from(rows.len()).map_err(PackTestError::Address)?);
        let offset =
            U32::<LittleEndian>::new(u32::try_from(data_start).map_err(PackTestError::Address)?);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(row_count_wire.as_bytes());
        bytes.extend_from_slice(offset.as_bytes());
        bytes.extend_from_slice(record_wire.as_bytes());
        bytes.extend_from_slice(row.key);
        bytes.extend_from_slice(value);
        Ok(bytes)
    }

    fn exact_snapshot() -> Result<IndexSnapshotId, PackTestError> {
        let rows = [ExactRow::present(b"alpha", b"value")];
        let segment = backend_semantic::index_core::ExactSegment::new(&rows)
            .map_err(|_| PackTestError::Exact)?;
        let exact = [segment.id];
        IndexSnapshot::new(generation(), &exact, &[])
            .map(|snapshot| snapshot.id)
            .map_err(PackTestError::Snapshot)
    }

    fn generation() -> backend_version::GenerationId {
        backend_version::GenerationId::from_canonical_bytes(b"index-pack-grammar-test-generation")
    }

    fn rejected(bytes: Vec<u8>) -> Result<IndexPackOpenError, PackTestError> {
        let id = IndexPackId::from_encoded_bytes(&bytes);
        match IndexPack::open(bytes, id) {
            Ok(_pack) => Err(PackTestError::Accepted),
            Err(rejected) => Ok(rejected.error),
        }
    }

    fn write_at(target: &mut [u8], offset: usize, source: &[u8]) -> Result<(), PackTestError> {
        let end = offset
            .checked_add(source.len())
            .ok_or(PackTestError::Geometry)?;
        let destination = target.get_mut(offset..end).ok_or(PackTestError::Geometry)?;
        destination.copy_from_slice(source);
        Ok(())
    }
}

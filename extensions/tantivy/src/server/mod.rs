//! The `backend-extension-tantivy` crate exists to adapt immutable lexical segments to Tantivy indexing and search.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//!
//! Real, bounded Tantivy lexical-membership projection over one immutable index snapshot.
//!
//! Tantivy establishes term membership only. Fixed-point scoring, update reconciliation, and
//! global deterministic ranking remain in `backend-semantic::index_core`; backend floating scores never cross
//! this adapter boundary.

mod storage;

pub use self::storage::{
    TantivyCandidate, TantivyProvenance, TantivyRowOrdinal, TantivySegment, TantivySegmentHit,
    TantivySegmentStore, TantivySegmentStoreError, TantivySnapshot,
};

use core::str::Utf8Error;

use backend_semantic::index_core::{
    ENTITY_DOCUMENT_ID_BYTES, EntityDocumentId, EntityDocumentIdError, IndexSnapshotId,
    LexicalManifest, LexicalOrderKey, LexicalSegmentId,
};
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::Column;
use tantivy::{
    DocId, Index, IndexReader, Score, TantivyDocument,
    doc,
    query::{QueryParser, QueryParserError},
    schema::{FAST, Field, STORED, Schema, TEXT, Value},
};

/// Tantivy's minimum writer heap in bytes for this bounded nested build.
const WRITER_MEMORY_BYTES: usize = 15_000_000;

/// Maximum documents admitted by one in-memory nested projection.
pub const MAX_TANTIVY_DOCUMENTS: usize = 256;

/// Maximum aggregate UTF-8 document bytes admitted by one projection.
pub const MAX_TANTIVY_TEXT_BYTES: usize = 1_048_576;

/// Maximum UTF-8 query bytes parsed by one operation.
pub const MAX_TANTIVY_QUERY_BYTES: usize = 4_096;

/// One deterministic adapter hit. Backend floating scores never become identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TantivyHit {
    /// Stable package document identity; ties are ordered by this value.
    pub document: EntityDocumentId,
}

/// Complete immutable lexical terminal retaining the pinned snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TantivyTerminal {
    /// Exact immutable snapshot searched by Tantivy.
    pub snapshot: IndexSnapshotId,
    /// Number of initialized result slots.
    pub written: usize,
}

/// Structured build/query failure retaining its causal Tantivy source.
#[derive(Debug, thiserror::Error)]
pub enum TantivyAdapterError {
    /// Nested Tantivy publication requires the complete healthy canonical selection.
    #[error(
        "Tantivy projection requires a complete healthy manifest; missing={missing}, degraded={degraded}"
    )]
    IncompleteManifest {
        /// Selected lexical segments unavailable to the manifest.
        missing: usize,
        /// Whether the manifest used a degraded route.
        degraded: bool,
    },
    /// Caller attempted to query a different immutable snapshot.
    #[error("Tantivy projection snapshot mismatch")]
    WrongSnapshot {
        /// Snapshot owning this immutable Tantivy index.
        expected: IndexSnapshotId,
        /// Rejected query snapshot.
        observed: IndexSnapshotId,
    },
    /// Caller output cannot retain requested `TopK`.
    #[error("Tantivy output has {available} slots, requested {required}")]
    InsufficientOutput {
        /// Requested `TopK`.
        required: usize,
        /// Caller-provided slots.
        available: usize,
    },
    /// The corpus exceeded the fixed document-count admission bound.
    #[error("Tantivy corpus has {observed} documents, limit is {limit}")]
    DocumentLimit {
        /// Fixed document-count limit.
        limit: usize,
        /// Complete observed document count.
        observed: usize,
    },
    /// The corpus exceeded the fixed aggregate text-byte admission bound.
    #[error("Tantivy corpus has {observed} text bytes, limit is {limit}")]
    TextBytesLimit {
        /// Fixed aggregate text-byte limit.
        limit: usize,
        /// Complete observed text bytes.
        observed: usize,
    },
    /// Summing hostile document widths overflowed the platform counter.
    #[error("Tantivy corpus text width overflowed at document {index}")]
    TextBytesOverflow {
        /// Document whose width crossed the representable range.
        index: usize,
    },
    /// The query exceeded the fixed parser-work admission bound.
    #[error("Tantivy query has {observed} bytes, limit is {limit}")]
    QueryBytesLimit {
        /// Fixed query byte limit.
        limit: usize,
        /// Complete observed query bytes.
        observed: usize,
    },
    /// A canonical lexical term was not valid UTF-8 for Tantivy's text field.
    #[error("Tantivy term at segment {segment:?} row {row} is not valid UTF-8")]
    NonUtf8Term {
        /// Immutable segment containing the rejected term.
        segment: LexicalSegmentId,
        /// Row within the immutable segment.
        row: usize,
        /// Original UTF-8 conversion failure.
        #[source]
        source: Utf8Error,
    },
    /// Tantivy failed while building, committing, opening, or searching.
    #[error("Tantivy {phase:?} failed")]
    Tantivy {
        /// Exact operation phase.
        phase: TantivyPhase,
        /// Original Tantivy source.
        #[source]
        source: tantivy::TantivyError,
    },
    /// Tantivy rejected the caller's lexical grammar.
    #[error("Tantivy query parsing failed")]
    Query {
        /// Original query parser source.
        #[source]
        source: QueryParserError,
    },
    /// A fast ordinal did not name the stored document identity.
    #[error(
        "Tantivy result at segment {segment} document {document} disagreed with its ordinal identity"
    )]
    OrdinalIdentity {
        /// Tantivy segment ordinal.
        segment: u32,
        /// Tantivy document ordinal.
        document: u32,
    },
    /// A stored document lacked or malformed its complete immutable identity field.
    #[error("Tantivy result at segment {segment} document {document} had an invalid identity")]
    StoredIdentity {
        /// Tantivy segment ordinal.
        segment: u32,
        /// Tantivy document ordinal.
        document: u32,
        /// Typed reason the stored identity could not be recovered.
        #[source]
        source: StoredIdentityError,
    },
    /// Tantivy reported more documents than this target can address.
    #[error("Tantivy document count {observed} exceeds this target")]
    DocumentCountOverflow {
        /// Complete backend document count.
        observed: u64,
        /// Original checked-integer conversion failure.
        #[source]
        source: core::num::TryFromIntError,
    },
}

/// Typed rejection while recovering a complete immutable document identity from Tantivy storage.
#[derive(Debug, thiserror::Error)]
pub enum StoredIdentityError {
    /// The stored document did not retain the required fixed-width bytes field.
    #[error("identity field was absent or not bytes")]
    Missing,
    /// The stored byte field did not decode as a typed global document identity.
    #[error("identity field was malformed")]
    Malformed {
        /// Complete fixed-key decoding cause.
        #[source]
        source: EntityDocumentIdError,
    },
}

/// Exact causal phase for a Tantivy source error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TantivyPhase {
    /// In-memory index writer creation.
    Writer,
    /// Immutable document admission.
    AddDocument,
    /// Segment commit.
    Commit,
    /// Reader construction.
    Reader,
    /// Query execution.
    Search,
    /// Stored identity readback.
    ReadDocument,
}

/// Nested Tantivy index pinned to one immutable server snapshot.
pub struct TantivyLexical {
    snapshot: IndexSnapshotId,
    index: Index,
    reader: IndexReader,
    body_field: Field,
    ordinal_field: Field,
    identities: Vec<EntityDocumentId>,
}

impl TantivyLexical {
    /// Builds a real in-memory index directly from a validated immutable lexical manifest.
    ///
    /// Each newest live canonical lexical membership becomes one nested Tantivy document. The
    /// snapshot authority is therefore inherited from content-derived segment identities rather
    /// than accepted as an unrelated caller label.
    ///
    /// # Errors
    ///
    /// Returns a typed adapter error when manifest, corpus, or Tantivy admission fails.
    pub fn build(manifest: LexicalManifest<'_, '_>) -> Result<Self, TantivyAdapterError> {
        if !manifest.is_complete() {
            return Err(TantivyAdapterError::IncompleteManifest {
                missing: manifest.missing.len(),
                degraded: manifest.is_degraded(),
            });
        }
        let document_count = manifest
            .segments
            .iter()
            .map(|segment| segment.rows.len())
            .sum::<usize>();
        if document_count > MAX_TANTIVY_DOCUMENTS {
            return Err(TantivyAdapterError::DocumentLimit {
                limit: MAX_TANTIVY_DOCUMENTS,
                observed: document_count,
            });
        }
        let mut text_bytes = 0_usize;
        let mut document_index = 0_usize;
        for segment in manifest.segments {
            for row in segment.rows {
                text_bytes = text_bytes.checked_add(row.term.len()).ok_or(
                    TantivyAdapterError::TextBytesOverflow {
                        index: document_index,
                    },
                )?;
                document_index += 1;
            }
        }
        if text_bytes > MAX_TANTIVY_TEXT_BYTES {
            return Err(TantivyAdapterError::TextBytesLimit {
                limit: MAX_TANTIVY_TEXT_BYTES,
                observed: text_bytes,
            });
        }
        let mut schema = Schema::builder();
        let body_field = schema.add_text_field("body", TEXT);
        let document_field = schema.add_bytes_field("document", STORED);
        let ordinal_field = schema.add_u64_field("ordinal", FAST);
        let index = Index::create_in_ram(schema.build());
        let mut writer =
            index
                .writer(WRITER_MEMORY_BYTES)
                .map_err(|source| TantivyAdapterError::Tantivy {
                    phase: TantivyPhase::Writer,
                    source,
                })?;
        let mut identities = Vec::new();
        for (segment_position, segment) in manifest.segments.iter().enumerate() {
            for (row_index, row) in segment.rows.iter().copied().enumerate() {
                let key = LexicalOrderKey::from(row);
                let shadowed = manifest
                    .segments
                    .iter()
                    .take(segment_position)
                    .any(|newer| {
                        newer
                            .rows
                            .binary_search_by(|candidate| {
                                LexicalOrderKey::from(*candidate).cmp(&key)
                            })
                            .is_ok()
                    });
                if shadowed || row.is_tombstone() {
                    continue;
                }
                let text = core::str::from_utf8(row.term).map_err(|source| {
                    TantivyAdapterError::NonUtf8Term {
                        segment: segment.id,
                        row: row_index,
                        source,
                    }
                })?;
                let document: [u8; ENTITY_DOCUMENT_ID_BYTES] = row.document.into();
                let ordinal = u64::try_from(identities.len()).map_err(|source| {
                    TantivyAdapterError::DocumentCountOverflow {
                        observed: u64::MAX,
                        source,
                    }
                })?;
                writer
                    .add_document(doc!(
                        body_field => text,
                        document_field => document.to_vec(),
                        ordinal_field => ordinal,
                    ))
                    .map_err(|source| TantivyAdapterError::Tantivy {
                        phase: TantivyPhase::AddDocument,
                        source,
                    })?;
                identities.push(row.document);
            }
        }
        writer
            .commit()
            .map_err(|source| TantivyAdapterError::Tantivy {
                phase: TantivyPhase::Commit,
                source,
            })?;
        let reader = index
            .reader()
            .map_err(|source| TantivyAdapterError::Tantivy {
                phase: TantivyPhase::Reader,
                source,
            })?;
        validate_ordinals(&reader, document_field, ordinal_field, &identities)?;
        Ok(Self {
            snapshot: manifest.snapshot,
            index,
            reader,
            body_field,
            ordinal_field,
            identities,
        })
    }

    /// Executes Tantivy, then applies the portable recipe's deterministic identity tie order.
    ///
    /// # Errors
    ///
    /// Returns a typed adapter error when the snapshot, query, output, or stored identity is invalid.
    #[allow(
        clippy::indexing_slicing,
        reason = "requested output capacity is checked before slicing"
    )]
    pub fn search(
        &self,
        snapshot: IndexSnapshotId,
        query: &str,
        requested_limit: usize,
        output: &mut [Option<TantivyHit>],
    ) -> Result<TantivyTerminal, TantivyAdapterError> {
        if snapshot != self.snapshot {
            return Err(TantivyAdapterError::WrongSnapshot {
                expected: self.snapshot,
                observed: snapshot,
            });
        }
        if requested_limit > output.len() {
            return Err(TantivyAdapterError::InsufficientOutput {
                required: requested_limit,
                available: output.len(),
            });
        }
        if query.len() > MAX_TANTIVY_QUERY_BYTES {
            return Err(TantivyAdapterError::QueryBytesLimit {
                limit: MAX_TANTIVY_QUERY_BYTES,
                observed: query.len(),
            });
        }
        let parser = QueryParser::for_index(&self.index, vec![self.body_field]);
        let query = parser
            .parse_query(query)
            .map_err(|source| TantivyAdapterError::Query { source })?;
        let searcher = self.reader.searcher();
        let observed_documents = searcher.num_docs();
        let available_documents = usize::try_from(observed_documents).map_err(|source| {
            TantivyAdapterError::DocumentCountOverflow {
                observed: observed_documents,
                source,
            }
        })?;
        if available_documents == 0 || requested_limit == 0 {
            return Ok(TantivyTerminal {
                snapshot: self.snapshot,
                written: 0,
            });
        }
        let field = searcher.schema().get_field_name(self.ordinal_field);
        let collected = searcher
            .search(
                &query,
                &OrdinalCollector {
                    limit: available_documents,
                    field,
                },
            )
            .map_err(|source| TantivyAdapterError::Tantivy {
                phase: TantivyPhase::Search,
                source,
            })?;
        if let Some((segment, document)) = collected.missing {
            return Err(TantivyAdapterError::StoredIdentity {
                segment,
                document,
                source: StoredIdentityError::Missing,
            });
        }
        if collected.ordinals.len() > available_documents {
            return Err(TantivyAdapterError::DocumentLimit {
                limit: available_documents,
                observed: collected.ordinals.len(),
            });
        }
        let mut written = 0;
        for ordinal in collected.ordinals {
            let ordinal = usize::try_from(ordinal).map_err(|source| {
                TantivyAdapterError::DocumentCountOverflow {
                    observed: ordinal,
                    source,
                }
            })?;
            let identity = self.identities.get(ordinal).copied().ok_or(
                TantivyAdapterError::OrdinalIdentity {
                    segment: 0,
                    document: u32::try_from(ordinal).unwrap_or(u32::MAX),
                },
            )?;
            insert_document(&mut output[..requested_limit], &mut written, identity);
        }
        Ok(TantivyTerminal {
            snapshot: self.snapshot,
            written,
        })
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "output capacity is checked and insertion indices are bounded"
)]
fn insert_document(
    output: &mut [Option<TantivyHit>],
    written: &mut usize,
    document: EntityDocumentId,
) {
    if output.is_empty() {
        return;
    }
    let candidate = TantivyHit { document };
    let occupied = (*written).min(output.len());
    if output[..occupied].contains(&Some(candidate)) {
        return;
    }
    let mut position = occupied;
    for (index, hit) in output.iter().copied().take(occupied).enumerate() {
        if hit.is_some_and(|hit| candidate < hit) {
            position = index;
            break;
        }
    }
    if position == output.len() {
        return;
    }
    let new_written = (occupied + 1).min(output.len());
    for index in (position + 1..new_written).rev() {
        output[index] = output[index - 1];
    }
    output[position] = Some(candidate);
    *written = new_written;
}

fn validate_ordinals(
    reader: &IndexReader,
    document_field: Field,
    ordinal_field: Field,
    identities: &[EntityDocumentId],
) -> Result<(), TantivyAdapterError> {
    let searcher = reader.searcher();
    let mut seen = vec![false; identities.len()];
    let mut live = 0_usize;
    for (segment_ord, segment_reader) in searcher.segment_readers().iter().enumerate() {
        let segment_ord = u32::try_from(segment_ord).map_err(|source| {
            TantivyAdapterError::DocumentCountOverflow {
                observed: searcher.num_docs(),
                source,
            }
        })?;
        let field = searcher.schema().get_field_name(ordinal_field);
        let ordinals = segment_reader.fast_fields().u64(field).map_err(|source| {
            TantivyAdapterError::Tantivy {
                phase: TantivyPhase::ReadDocument,
                source,
            }
        })?;
        for doc in 0..segment_reader.max_doc() {
            if segment_reader.is_deleted(doc) {
                continue;
            }
            live = live.checked_add(1).ok_or(TantivyAdapterError::DocumentLimit {
                limit: identities.len(),
                observed: identities.len(),
            })?;
            let ordinal = ordinals.first(doc).ok_or(TantivyAdapterError::StoredIdentity {
                segment: segment_ord,
                document: doc,
                source: StoredIdentityError::Missing,
            })?;
            let index = usize::try_from(ordinal).map_err(|source| {
                TantivyAdapterError::DocumentCountOverflow {
                    observed: ordinal,
                    source,
                }
            })?;
            let expected = identities.get(index).copied().ok_or(
                TantivyAdapterError::OrdinalIdentity {
                    segment: segment_ord,
                    document: doc,
                },
            )?;
            let slot = seen.get_mut(index).ok_or(TantivyAdapterError::OrdinalIdentity {
                segment: segment_ord,
                document: doc,
            })?;
            if *slot {
                return Err(TantivyAdapterError::OrdinalIdentity {
                    segment: segment_ord,
                    document: doc,
                });
            }
            *slot = true;
            let stored: TantivyDocument = searcher
                .doc(tantivy::DocAddress {
                    segment_ord,
                    doc_id: doc,
                })
                .map_err(|source| TantivyAdapterError::Tantivy {
                    phase: TantivyPhase::ReadDocument,
                    source,
                })?;
            let identity = stored
                .get_first(document_field)
                .and_then(|value| value.as_bytes())
                .ok_or(TantivyAdapterError::StoredIdentity {
                    segment: segment_ord,
                    document: doc,
                    source: StoredIdentityError::Missing,
                })
                .and_then(|bytes| {
                    EntityDocumentId::try_from(bytes).map_err(|source| {
                        TantivyAdapterError::StoredIdentity {
                            segment: segment_ord,
                            document: doc,
                            source: StoredIdentityError::Malformed { source },
                        }
                    })
                })?;
            if identity != expected {
                return Err(TantivyAdapterError::OrdinalIdentity {
                    segment: segment_ord,
                    document: doc,
                });
            }
        }
    }
    if live != identities.len() || seen.iter().any(|present| !present) {
        return Err(TantivyAdapterError::DocumentLimit {
            limit: identities.len(),
            observed: live,
        });
    }
    Ok(())
}

struct OrdinalFruit {
    ordinals: Vec<u64>,
    missing: Option<(u32, u32)>,
}

struct OrdinalCollector<'segment> {
    limit: usize,
    field: &'segment str,
}

struct OrdinalSegment {
    segment_ord: u32,
    ordinals: Column<u64>,
    found: Vec<u64>,
    missing: Option<(u32, u32)>,
}

impl SegmentCollector for OrdinalSegment {
    type Fruit = OrdinalFruit;

    fn collect(&mut self, doc: DocId, _score: Score) {
        if self.missing.is_some() {
            return;
        }
        match self.ordinals.first(doc) {
            Some(ordinal) => self.found.push(ordinal),
            None => self.missing = Some((self.segment_ord, doc)),
        }
    }

    fn harvest(self) -> Self::Fruit {
        OrdinalFruit {
            ordinals: self.found,
            missing: self.missing,
        }
    }
}

impl Collector for OrdinalCollector<'_> {
    type Fruit = OrdinalFruit;
    type Child = OrdinalSegment;

    fn for_segment(
        &self,
        segment_local_id: tantivy::SegmentOrdinal,
        segment: &tantivy::SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let mut found = Vec::new();
        found.reserve(self.limit);
        Ok(OrdinalSegment {
            segment_ord: segment_local_id,
            ordinals: segment.fast_fields().u64(self.field)?,
            found,
            missing: None,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, segment_fruits: Vec<OrdinalFruit>) -> tantivy::Result<OrdinalFruit> {
        let mut ordinals = Vec::new();
        let mut missing = None;
        for fruit in segment_fruits {
            if missing.is_none() {
                missing = fruit.missing;
            }
            ordinals.extend(fruit.ordinals);
        }
        Ok(OrdinalFruit { ordinals, missing })
    }
}

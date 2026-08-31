#![forbid(unsafe_code)]
//! Real, bounded Tantivy lexical-membership projection over one immutable index snapshot.
//!
//! Tantivy establishes term membership only. Fixed-point scoring, update reconciliation, and
//! global deterministic ranking remain in `nudox-index-core`; backend floating scores never cross
//! this adapter boundary.

use core::str::Utf8Error;

use nudox_index_core::{
    ENTITY_DOCUMENT_ID_BYTES, EntityDocumentId, EntityDocumentIdError, IndexSnapshotId,
    LexicalManifest, LexicalSegmentId,
};
use tantivy::{
    Index, IndexReader, TantivyDocument,
    collector::TopDocs,
    doc,
    query::{QueryParser, QueryParserError},
    schema::{Field, STORED, Schema, TEXT, Value},
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
    /// Caller output cannot retain requested TopK.
    #[error("Tantivy output has {available} slots, requested {required}")]
    InsufficientOutput {
        /// Requested TopK.
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

/// Nested Tantivy index pinned to one immutable Nudox snapshot.
pub struct TantivyLexical {
    snapshot: IndexSnapshotId,
    index: Index,
    reader: IndexReader,
    body_field: Field,
    document_field: Field,
}

impl TantivyLexical {
    /// Builds a real in-memory index directly from a validated immutable lexical manifest.
    ///
    /// Each newest live canonical lexical membership becomes one nested Tantivy document. The
    /// snapshot authority is therefore inherited from content-derived segment identities rather
    /// than accepted as an unrelated caller label.
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
        let index = Index::create_in_ram(schema.build());
        let mut writer =
            index
                .writer(WRITER_MEMORY_BYTES)
                .map_err(|source| TantivyAdapterError::Tantivy {
                    phase: TantivyPhase::Writer,
                    source,
                })?;
        for (segment_position, segment) in manifest.segments.iter().enumerate() {
            for (row_index, row) in segment.rows.iter().copied().enumerate() {
                let shadowed = manifest
                    .segments
                    .iter()
                    .take(segment_position)
                    .flat_map(|newer| newer.rows)
                    .any(|newer| newer.term == row.term && newer.document == row.document);
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
                writer
                    .add_document(doc!(
                        body_field => text,
                        document_field => document.to_vec(),
                    ))
                    .map_err(|source| TantivyAdapterError::Tantivy {
                        phase: TantivyPhase::AddDocument,
                        source,
                    })?;
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
        Ok(Self {
            snapshot: manifest.snapshot,
            index,
            reader,
            body_field,
            document_field,
        })
    }

    /// Executes Tantivy, then applies the portable recipe's deterministic identity tie order.
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
        let matches = searcher
            .search(
                &query,
                &TopDocs::with_limit(available_documents).order_by_score(),
            )
            .map_err(|source| TantivyAdapterError::Tantivy {
                phase: TantivyPhase::Search,
                source,
            })?;
        let mut written = 0;
        for (_, address) in matches {
            let document: TantivyDocument =
                searcher
                    .doc(address)
                    .map_err(|source| TantivyAdapterError::Tantivy {
                        phase: TantivyPhase::ReadDocument,
                        source,
                    })?;
            let identity = document
                .get_first(self.document_field)
                .and_then(|value| value.as_bytes())
                .ok_or(TantivyAdapterError::StoredIdentity {
                    segment: address.segment_ord,
                    document: address.doc_id,
                    source: StoredIdentityError::Missing,
                })
                .and_then(|bytes| {
                    EntityDocumentId::try_from(bytes).map_err(|source| {
                        TantivyAdapterError::StoredIdentity {
                            segment: address.segment_ord,
                            document: address.doc_id,
                            source: StoredIdentityError::Malformed { source },
                        }
                    })
                })?;
            insert_document(&mut output[..requested_limit], &mut written, identity);
        }
        Ok(TantivyTerminal {
            snapshot: self.snapshot,
            written,
        })
    }
}

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

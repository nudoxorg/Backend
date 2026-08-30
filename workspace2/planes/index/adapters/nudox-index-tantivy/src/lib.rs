#![forbid(unsafe_code)]
//! Real, bounded Tantivy lexical-membership projection over one immutable index snapshot.
//!
//! Tantivy establishes term membership only. Fixed-point scoring, update reconciliation, and
//! global deterministic ranking remain in `nudox-index-core`; backend floating scores never cross
//! this adapter boundary.

use nudox_index_core::IndexSnapshotId;
use tantivy::{
    Index, IndexReader, TantivyDocument,
    collector::TopDocs,
    doc,
    query::{QueryParser, QueryParserError},
    schema::{Field, INDEXED, STORED, Schema, TEXT, Value},
};

/// Tantivy's minimum writer heap in bytes for this bounded nested build.
const WRITER_MEMORY_BYTES: usize = 15_000_000;

/// Maximum documents admitted by one in-memory nested projection.
pub const MAX_TANTIVY_DOCUMENTS: usize = 256;

/// Maximum aggregate UTF-8 document bytes admitted by one projection.
pub const MAX_TANTIVY_TEXT_BYTES: usize = 1_048_576;

/// Maximum UTF-8 query bytes parsed by one operation.
pub const MAX_TANTIVY_QUERY_BYTES: usize = 4_096;

/// One immutable document projected into Tantivy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TantivyDocumentInput<'text> {
    /// Stable local document identity shared with the portable lexical core.
    pub document: u32,
    /// Borrowed UTF-8 body indexed by Tantivy's default tokenizer.
    pub text: &'text str,
}

/// One deterministic adapter hit. Backend floating scores never become identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TantivyHit {
    /// Stable local document identity; ties are ordered by this value.
    pub document: u32,
}

/// Complete immutable lexical terminal retaining the pinned snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TantivyTerminal {
    snapshot: IndexSnapshotId,
    written: usize,
}

impl TantivyTerminal {
    /// Returns the exact immutable snapshot searched by Tantivy.
    #[must_use]
    pub const fn snapshot(self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Returns the number of initialized result slots.
    #[must_use]
    pub const fn written(self) -> usize {
        self.written
    }
}

/// Structured build/query failure retaining its causal Tantivy source.
#[derive(Debug, thiserror::Error)]
pub enum TantivyAdapterError {
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
    /// One input document identity occurred more than once.
    #[error("duplicate document {document} at {first_index} and {index}")]
    DuplicateDocument {
        /// First occurrence.
        first_index: usize,
        /// Later occurrence.
        index: usize,
        /// Repeated identity.
        document: u32,
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
    /// A stored document lacked or overflowed its stable identity field.
    #[error("Tantivy result at segment {segment} document {document} has no valid identity")]
    StoredIdentity {
        /// Tantivy segment ordinal.
        segment: u32,
        /// Tantivy document ordinal.
        document: u32,
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
    /// Builds and commits a real in-memory Tantivy index for one immutable snapshot.
    pub fn build(
        snapshot: IndexSnapshotId,
        documents: &[TantivyDocumentInput<'_>],
    ) -> Result<Self, TantivyAdapterError> {
        if documents.len() > MAX_TANTIVY_DOCUMENTS {
            return Err(TantivyAdapterError::DocumentLimit {
                limit: MAX_TANTIVY_DOCUMENTS,
                observed: documents.len(),
            });
        }
        let mut text_bytes = 0_usize;
        for (index, document) in documents.iter().enumerate() {
            text_bytes = text_bytes
                .checked_add(document.text.len())
                .ok_or(TantivyAdapterError::TextBytesOverflow { index })?;
        }
        if text_bytes > MAX_TANTIVY_TEXT_BYTES {
            return Err(TantivyAdapterError::TextBytesLimit {
                limit: MAX_TANTIVY_TEXT_BYTES,
                observed: text_bytes,
            });
        }
        reject_duplicate_documents(documents)?;
        let mut schema = Schema::builder();
        let body_field = schema.add_text_field("body", TEXT);
        let document_field = schema.add_u64_field("document", STORED | INDEXED);
        let index = Index::create_in_ram(schema.build());
        let mut writer =
            index
                .writer(WRITER_MEMORY_BYTES)
                .map_err(|source| TantivyAdapterError::Tantivy {
                    phase: TantivyPhase::Writer,
                    source,
                })?;
        for document in documents {
            writer
                .add_document(doc!(
                    body_field => document.text,
                    document_field => u64::from(document.document),
                ))
                .map_err(|source| TantivyAdapterError::Tantivy {
                    phase: TantivyPhase::AddDocument,
                    source,
                })?;
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
            snapshot,
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
            let Some(identity) = document
                .get_first(self.document_field)
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok())
            else {
                return Err(TantivyAdapterError::StoredIdentity {
                    segment: address.segment_ord,
                    document: address.doc_id,
                });
            };
            insert_document(&mut output[..requested_limit], &mut written, identity);
        }
        Ok(TantivyTerminal {
            snapshot: self.snapshot,
            written,
        })
    }
}

fn reject_duplicate_documents(
    documents: &[TantivyDocumentInput<'_>],
) -> Result<(), TantivyAdapterError> {
    for index in 0..documents.len() {
        for first_index in 0..index {
            if documents[first_index].document == documents[index].document {
                return Err(TantivyAdapterError::DuplicateDocument {
                    first_index,
                    index,
                    document: documents[index].document,
                });
            }
        }
    }
    Ok(())
}

fn insert_document(output: &mut [Option<TantivyHit>], written: &mut usize, document: u32) {
    if output.is_empty() {
        return;
    }
    let candidate = TantivyHit { document };
    let occupied = (*written).min(output.len());
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

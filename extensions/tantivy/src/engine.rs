//! Real Tantivy-backed lexical source with canonical ranking at the adapter boundary.

use crate::{
    Binding, Cursor, DocumentState, Error, FieldSelection, LexicalPage, LexicalSource, Limits,
    MatchMode, OverlayLimits, Query, QueryRequest, QueryVersion, RankedHit, Relevance,
    SchemaVersion, compare_ranked_hits,
};
use backend_semantic::EntityId;
use backend_version::CoverageWitness;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::{
    collections::BTreeMap, fs, fs::File, io::Read, path::Path,
    sync::atomic::Ordering as AtomicOrdering,
};
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::ColumnValues;
use tantivy::{
    DocId, Index, IndexReader, Score, Term, doc,
    query::{BooleanQuery, FuzzyTermQuery, Occur, Query as TantivyQuery, TermQuery},
    schema::{FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema},
};

const WRITER_MEMORY_BYTES: usize = 15_000_000;
const BINDING_FILE: &str = "backend-binding-v2";
const INTEGRITY_FILE: &str = "backend-files-v2";
const ORDINAL_MAP_FILE: &str = "backend-ordinals-v1";
const INTEGRITY_MAGIC: &[u8] = b"backend-tantivy-files-v2\0";
const ORDINAL_MAP_MAGIC: &[u8] = b"backend-tantivy-ordinals-v1\0";
const DURABLE_ROOTS_DIRECTORY: &str = "v2";
const DURABLE_ROOT_LEASE: &str = ".backend-root-reader.lock";
const MAX_RETAINED_DURABLE_ROOTS: usize = 4;
const DEFAULT_DURABLE_CACHE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_PROJECTION_FILES: usize = 65_536;
const MAX_PROJECTION_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ORDINAL_MAP_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ORDINAL_SLOTS: usize = 4_000_000;
const ORDINAL_MAP_RECORD_BYTES: usize = 8 + 32 + 32 + 4 + 32;
const DEFAULT_RANK_SCRATCH_BYTES: usize = 1024 * 1024 * 1024;
const DEFAULT_RETAINED_RANK_BYTES: usize = 64 * 1024 * 1024;
const RANK_SCRATCH_BYTES_PER_MATCH: usize = 320;
const MAX_DURABLE_ROOT_SCAN_ENTRIES: usize = 65_536;
static NEXT_DURABLE_STAGE: AtomicU64 = AtomicU64::new(0);

/// How a resident Tantivy projection absorbed a new document snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionKind {
    /// Every indexed field matched. Only the binding stamp moved.
    Rebound,
    /// A bounded set of documents was deleted, rewritten, or appended.
    Revised,
}

/// Posting movement performed by one in-place projection update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionRevision {
    /// Whether the update rewrote any document.
    pub kind: ProjectionKind,
    /// Documents whose identity was added, removed, or rewritten.
    pub rewritten_documents: usize,
    /// Token postings removed from the resident index.
    pub retired_postings: u64,
    /// Token postings appended for rewritten or new documents.
    pub added_postings: u64,
}

/// Result of asking a resident projection to absorb a new document state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintainOutcome {
    /// The resident index now serves `next`.
    Applied(ProjectionRevision),
    /// The edit is larger than the maintenance budget. The index is unchanged.
    RebuildRequired,
}

/// How a root-selected persistent lexical projection became available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableProjectionAction {
    /// The complete selected root already existed and passed binding and file checks.
    Opened,
    /// The selected root was built in a staging directory and atomically published.
    Built,
    /// A delta was written into a copy-on-write stage and atomically published.
    Revised,
}

/// Maximum bytes retained by one local durable Tantivy cache.
///
/// This bounds both a selected projection while it is verified and the set of
/// retained generations. A root which exceeds the configured budget is
/// reported as a capacity refusal and is never treated as corrupt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurableCacheBudget {
    max_bytes: u64,
}

impl DurableCacheBudget {
    /// Creates a nonzero byte budget.
    #[must_use]
    pub const fn new(max_bytes: u64) -> Option<Self> {
        if max_bytes == 0 {
            None
        } else {
            Some(Self { max_bytes })
        }
    }

    /// Returns the maximum retained bytes.
    #[must_use]
    pub const fn max_bytes(self) -> u64 {
        self.max_bytes
    }
}

impl Default for DurableCacheBudget {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_DURABLE_CACHE_BYTES,
        }
    }
}

/// Per-source bounds for exact lexical rank construction and retention.
///
/// The scratch allowance covers collector maps, intersections, and the
/// compact rank snapshot under construction. Each source retains at most one
/// query snapshot; independently retained source snapshots consume their own
/// configured allowance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankSnapshotBudget {
    max_scratch_bytes: usize,
    max_retained_bytes: usize,
}

impl RankSnapshotBudget {
    /// Creates nonzero scratch and retained-rank allowances.
    #[must_use]
    pub const fn new(max_scratch_bytes: usize, max_retained_bytes: usize) -> Option<Self> {
        if max_scratch_bytes == 0 || max_retained_bytes == 0 {
            None
        } else {
            Some(Self {
                max_scratch_bytes,
                max_retained_bytes,
            })
        }
    }

    /// Returns the exact-query build scratch allowance.
    #[must_use]
    pub const fn max_scratch_bytes(self) -> usize {
        self.max_scratch_bytes
    }

    /// Returns the retained compact-rank allowance.
    #[must_use]
    pub const fn max_retained_bytes(self) -> usize {
        self.max_retained_bytes
    }
}

impl Default for RankSnapshotBudget {
    fn default() -> Self {
        Self {
            max_scratch_bytes: DEFAULT_RANK_SCRATCH_BYTES,
            max_retained_bytes: DEFAULT_RETAINED_RANK_BYTES,
        }
    }
}

/// One live ordinal in the resident projection.
///
/// Ordinals are stable for the life of the index. A removal leaves a hole so
/// every untouched document keeps the posting term it was indexed under.
#[derive(Clone, Copy)]
struct LiveDocument {
    id: EntityId,
    fields_digest: [u8; 32],
    postings: u32,
}

/// Compact live-document storage. Stable Tantivy ordinals may contain holes,
/// so resident memory scales with live rows rather than historical slots.
#[derive(Clone, Default)]
struct DocumentTable {
    slot_count: u32,
    live: Vec<OrdinalDocument>,
}

#[derive(Clone, Copy)]
struct OrdinalDocument {
    ordinal: u32,
    document: LiveDocument,
}

#[derive(Clone, Copy)]
struct IdentityOrdinal {
    fingerprint: [u8; 16],
    ordinal: u32,
}

impl DocumentTable {
    fn from_live(slot_count: u32, live: Vec<OrdinalDocument>) -> Result<Self, Error> {
        let mut previous: Option<u32> = None;
        for entry in &live {
            if entry.ordinal >= slot_count || previous.is_some_and(|value| value >= entry.ordinal) {
                return Err(Error::MalformedInput);
            }
            previous = Some(entry.ordinal);
        }
        Ok(Self { slot_count, live })
    }

    fn get(&self, ordinal: usize) -> Option<&LiveDocument> {
        let ordinal = u32::try_from(ordinal).ok()?;
        self.live
            .binary_search_by_key(&ordinal, |entry| entry.ordinal)
            .ok()
            .map(|index| &self.live[index].document)
    }

    fn iter(&self) -> impl Iterator<Item = (usize, &LiveDocument)> {
        self.live
            .iter()
            .map(|entry| (entry.ordinal as usize, &entry.document))
    }

    fn identity_index(&self) -> Vec<IdentityOrdinal> {
        let mut identities = self
            .live
            .iter()
            .map(|entry| IdentityOrdinal {
                fingerprint: entity_fingerprint(entry.document.id),
                ordinal: entry.ordinal,
            })
            .collect::<Vec<_>>();
        identities.sort_unstable_by_key(|entry| (entry.fingerprint, entry.ordinal));
        identities
    }

    fn ordinal_for_entity(&self, identities: &[IdentityOrdinal], id: EntityId) -> Option<usize> {
        let fingerprint = entity_fingerprint(id);
        let start = identities.partition_point(|entry| entry.fingerprint < fingerprint);
        identities[start..]
            .iter()
            .take_while(|entry| entry.fingerprint == fingerprint)
            .find_map(|entry| {
                self.get(entry.ordinal as usize)
                    .filter(|document| document.id == id)
                    .map(|_| entry.ordinal as usize)
            })
    }

    fn merge_replacements(mut self, mut replacements: Vec<OrdinalDocument>) -> Result<Self, Error> {
        replacements.sort_unstable_by_key(|entry| entry.ordinal);
        if replacements
            .windows(2)
            .any(|pair| pair[0].ordinal == pair[1].ordinal)
        {
            return Err(Error::MalformedInput);
        }
        let mut merged = Vec::with_capacity(self.live.len().saturating_add(replacements.len()));
        let mut old = self.live.into_iter().peekable();
        let mut new = replacements.into_iter().peekable();
        loop {
            match (old.peek(), new.peek()) {
                (Some(left), Some(right)) if left.ordinal == right.ordinal => {
                    return Err(Error::MalformedInput);
                }
                (Some(left), Some(right)) if left.ordinal < right.ordinal => {
                    merged.push(old.next().ok_or(Error::MalformedInput)?);
                }
                (Some(_), Some(_)) => merged.push(new.next().ok_or(Error::MalformedInput)?),
                (Some(_), None) => merged.push(old.next().ok_or(Error::MalformedInput)?),
                (None, Some(_)) => merged.push(new.next().ok_or(Error::MalformedInput)?),
                (None, None) => break,
            }
        }
        self.live = merged;
        Ok(self)
    }
}

fn entity_fingerprint(id: EntityId) -> [u8; 16] {
    let digest = blake3::hash(id.as_bytes());
    let mut fingerprint = [0_u8; 16];
    fingerprint.copy_from_slice(&digest.as_bytes()[..16]);
    fingerprint
}

/// A real in-memory Tantivy projection pinned to one exact lexical binding.
pub struct TantivySource {
    binding: Binding,
    coverage: CoverageWitness,
    limits: Limits,
    _index: Index,
    reader: IndexReader,
    raw_token: Field,
    folded_token: Field,
    ranking_token: Field,
    field_name: Field,
    ordinal: Field,
    rank_weight: Field,
    rank_bytes: Field,
    documents: DocumentTable,
    identity_ordinals: Vec<IdentityOrdinal>,
    _root_lease: Option<File>,
    poisoned: bool,
    rank_budget: RankSnapshotBudget,
    rank_cache: Mutex<Option<CachedRank>>,
    rank_evaluations: AtomicU64,
}

struct CachedRank {
    binding: Binding,
    query: QueryVersion,
    total: usize,
    ranks: RankStorage,
}

#[derive(Clone, Copy)]
struct CompactRelevance([u8; 9]);

impl CompactRelevance {
    fn from_relevance(relevance: Relevance) -> Self {
        let (matched_bytes, term_bytes, field_weight, matched_clauses) = relevance.rank_parts();
        let mut bytes = [0_u8; 9];
        bytes[..4].copy_from_slice(&matched_bytes.to_le_bytes());
        bytes[4..8].copy_from_slice(&term_bytes.to_le_bytes());
        // Current field weights are 1..=4 and query admission caps clauses at
        // sixteen, so both values fit losslessly in the final byte.
        bytes[8] = u8::try_from(field_weight).unwrap_or(u8::MAX)
            | (u8::try_from(matched_clauses.saturating_add(1))
                .unwrap_or(u8::MAX)
                .checked_shl(3)
                .unwrap_or(u8::MAX));
        Self(bytes)
    }

    fn relevance(self) -> Result<Relevance, TantivySourceError> {
        let matched_bytes =
            u32::from_le_bytes(self.0[..4].try_into().map_err(|_| Error::MalformedInput)?);
        let term_bytes =
            u32::from_le_bytes(self.0[4..8].try_into().map_err(|_| Error::MalformedInput)?);
        let field_weight = u16::from(self.0[8] & 0b0000_0111);
        let matched_clauses = u16::from(self.0[8] >> 3)
            .checked_sub(1)
            .ok_or(Error::MalformedInput)?;
        if matched_clauses == 0 {
            return Ok(Relevance::all_documents());
        }
        Relevance::from_rank_parts(matched_bytes, term_bytes, field_weight, matched_clauses)
            .map_err(Into::into)
    }
}

#[derive(Clone, Copy)]
struct CompactRankEntry {
    ordinal: u32,
    relevance: CompactRelevance,
}

enum RankStorage {
    Sparse {
        by_ordinal: Box<[CompactRankEntry]>,
        rank_order: Box<[u32]>,
    },
    Dense {
        by_ordinal: Box<[[u8; 10]]>,
        rank_order: Box<[u32]>,
    },
}

impl RankStorage {
    fn from_entries(
        mut entries: Vec<(usize, Relevance)>,
        documents: &DocumentTable,
        budget: RankSnapshotBudget,
    ) -> Result<Self, TantivySourceError> {
        for (ordinal, _) in &entries {
            if documents.get(*ordinal).is_none() {
                return Err(Error::MalformedInput.into());
            }
        }
        entries.sort_unstable_by(
            |(left_ordinal, left_relevance), (right_ordinal, right_relevance)| {
                let left_id = documents.get(*left_ordinal).map(|document| document.id);
                let right_id = documents.get(*right_ordinal).map(|document| document.id);
                match (left_id, right_id) {
                    (Some(left_id), Some(right_id)) => compare_ranked_hits(
                        RankedHit {
                            document: left_id,
                            relevance: *left_relevance,
                        },
                        RankedHit {
                            document: right_id,
                            relevance: *right_relevance,
                        },
                    ),
                    _ => std::cmp::Ordering::Equal,
                }
            },
        );
        let sparse_bytes = entries
            .len()
            .checked_mul(
                std::mem::size_of::<CompactRankEntry>()
                    .checked_add(std::mem::size_of::<u32>())
                    .ok_or(Error::SizeLimit)?,
            )
            .ok_or(Error::SizeLimit)?;
        let dense_bytes = usize::try_from(documents.slot_count)
            .map_err(|_| Error::SizeLimit)?
            .checked_mul(10)
            .and_then(|bytes| {
                entries
                    .len()
                    .checked_mul(std::mem::size_of::<u32>())
                    .and_then(|order| bytes.checked_add(order))
            })
            .ok_or(Error::SizeLimit)?;
        let retained_bytes = dense_bytes.min(sparse_bytes);
        if retained_bytes > budget.max_retained_bytes {
            return Err(TantivySourceError::RankSnapshotBudgetExceeded {
                budget_bytes: budget.max_retained_bytes,
                required_bytes: retained_bytes,
            });
        }
        let rank_order = entries
            .iter()
            .map(|(ordinal, _)| u32::try_from(*ordinal).map_err(|_| Error::SizeLimit))
            .collect::<Result<Vec<_>, _>>()?;
        if dense_bytes < sparse_bytes {
            let mut dense = vec![[0_u8; 10]; documents.slot_count as usize];
            for (ordinal, relevance) in entries {
                let slot = dense.get_mut(ordinal).ok_or(Error::SizeLimit)?;
                slot[..9].copy_from_slice(&CompactRelevance::from_relevance(relevance).0);
                slot[9] = 1;
            }
            Ok(Self::Dense {
                by_ordinal: dense.into_boxed_slice(),
                rank_order: rank_order.into_boxed_slice(),
            })
        } else {
            let mut sparse = Vec::with_capacity(entries.len());
            for (ordinal, relevance) in entries {
                sparse.push(CompactRankEntry {
                    ordinal: u32::try_from(ordinal).map_err(|_| Error::SizeLimit)?,
                    relevance: CompactRelevance::from_relevance(relevance),
                });
            }
            sparse.sort_unstable_by_key(|entry| entry.ordinal);
            Ok(Self::Sparse {
                by_ordinal: sparse.into_boxed_slice(),
                rank_order: rank_order.into_boxed_slice(),
            })
        }
    }

    fn get(&self, ordinal: usize) -> Result<Option<Relevance>, TantivySourceError> {
        match self {
            Self::Sparse { by_ordinal, .. } => {
                let ordinal = u32::try_from(ordinal).map_err(|_| Error::SizeLimit)?;
                by_ordinal
                    .binary_search_by_key(&ordinal, |entry| entry.ordinal)
                    .ok()
                    .map(|index| by_ordinal[index].relevance.relevance())
                    .transpose()
            }
            Self::Dense { by_ordinal, .. } => {
                let Some(entry) = by_ordinal.get(ordinal) else {
                    return Ok(None);
                };
                if entry[9] == 0 {
                    return Ok(None);
                }
                let mut compact = [0_u8; 9];
                compact.copy_from_slice(&entry[..9]);
                CompactRelevance(compact).relevance().map(Some)
            }
        }
    }

    fn ranked_at(&self, position: usize) -> Result<Option<(usize, Relevance)>, TantivySourceError> {
        let rank_order = match self {
            Self::Sparse { rank_order, .. } | Self::Dense { rank_order, .. } => rank_order,
        };
        let Some(ordinal) = rank_order.get(position) else {
            return Ok(None);
        };
        let ordinal = usize::try_from(*ordinal).map_err(|_| Error::SizeLimit)?;
        Ok(self.get(ordinal)?.map(|relevance| (ordinal, relevance)))
    }

    #[cfg(test)]
    fn byte_len(&self) -> usize {
        match self {
            Self::Sparse {
                by_ordinal,
                rank_order,
            } => {
                by_ordinal.len() * std::mem::size_of::<CompactRankEntry>()
                    + rank_order.len() * std::mem::size_of::<u32>()
            }
            Self::Dense {
                by_ordinal,
                rank_order,
            } => {
                by_ordinal.len() * std::mem::size_of::<[u8; 10]>()
                    + rank_order.len() * std::mem::size_of::<u32>()
            }
        }
    }

    fn for_each(
        &self,
        mut visit: impl FnMut(usize, Relevance) -> Result<(), TantivySourceError>,
    ) -> Result<(), TantivySourceError> {
        match self {
            Self::Sparse { by_ordinal, .. } => {
                for entry in by_ordinal.iter() {
                    visit(
                        usize::try_from(entry.ordinal).map_err(|_| Error::SizeLimit)?,
                        entry.relevance.relevance()?,
                    )?;
                }
            }
            Self::Dense { by_ordinal, .. } => {
                for (ordinal, entry) in by_ordinal.iter().enumerate() {
                    if entry[9] == 0 {
                        continue;
                    }
                    let mut compact = [0_u8; 9];
                    compact.copy_from_slice(&entry[..9]);
                    visit(ordinal, CompactRelevance(compact).relevance()?)?;
                }
            }
        }
        Ok(())
    }
}

struct DurableCacheLock {
    _file: File,
}

impl DurableCacheLock {
    fn acquire(cache_root: &Path) -> Result<Self, std::io::Error> {
        let lock = backend_platform::durability::open_or_create_regular_file_nofollow(
            &cache_root.join(".search-index-v2.lock"),
        )?;
        lock.lock()?;
        Ok(Self { _file: lock })
    }
}

/// Fully admitted local query adapter backed by a concrete Tantivy index.
pub type TantivyAdapter = crate::Adapter<TantivySource>;

/// Typed failure from concrete Tantivy construction or execution.
#[derive(Debug)]
pub enum TantivySourceError {
    /// Portable admission rejected the request or state.
    Contract(Error),
    /// Tantivy rejected index construction or query execution.
    Backend(tantivy::TantivyError),
    /// The durable projection directory could not be read or committed.
    Io(std::io::Error),
    /// Tantivy returned a document outside the verified projection mapping.
    Corrupt(&'static str),
    /// A complete projection does not fit the configured durable cache budget.
    BudgetExceeded {
        /// Configured maximum cache bytes.
        budget_bytes: u64,
        /// Bytes required by the selected root or retained cache.
        required_bytes: u64,
    },
    /// The selected state cannot fit its identity map within the format bound.
    OrdinalMapCapacityExceeded {
        /// Maximum encoded sidecar size.
        maximum_bytes: u64,
        /// Encoded size required by the selected live documents.
        required_bytes: u64,
    },
    /// An exact query's compact rank state or bounded construction scratch
    /// exceeds this source's configured query-memory budget.
    RankSnapshotBudgetExceeded {
        /// Configured scratch or retained-rank allowance.
        budget_bytes: usize,
        /// Bytes required by the exact query snapshot or conservative build bound.
        required_bytes: usize,
    },
}

impl std::fmt::Display for TantivySourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contract(error) => error.fmt(formatter),
            Self::Backend(error) => write!(formatter, "Tantivy backend failed: {error}"),
            Self::Io(error) => write!(formatter, "Tantivy projection I/O failed: {error}"),
            Self::Corrupt(detail) => write!(formatter, "Tantivy projection is corrupt: {detail}"),
            Self::BudgetExceeded {
                budget_bytes,
                required_bytes,
            } => write!(
                formatter,
                "Tantivy projection needs {required_bytes} bytes, above the {budget_bytes}-byte cache budget",
            ),
            Self::OrdinalMapCapacityExceeded {
                maximum_bytes,
                required_bytes,
            } => write!(
                formatter,
                "Tantivy ordinal identity map needs {required_bytes} bytes, above its {maximum_bytes}-byte format bound",
            ),
            Self::RankSnapshotBudgetExceeded {
                budget_bytes,
                required_bytes,
            } => write!(
                formatter,
                "exact lexical rank needs {required_bytes} bytes, above its {budget_bytes}-byte query budget",
            ),
        }
    }
}

impl std::error::Error for TantivySourceError {}

impl From<Error> for TantivySourceError {
    fn from(error: Error) -> Self {
        Self::Contract(error)
    }
}

impl From<tantivy::TantivyError> for TantivySourceError {
    fn from(error: tantivy::TantivyError) -> Self {
        Self::Backend(error)
    }
}

impl From<std::io::Error> for TantivySourceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl TantivySource {
    /// Builds the standard in-memory local-service adapter in one admitted step.
    ///
    /// # Errors
    ///
    /// Returns a typed contract or Tantivy construction failure.
    pub fn local_adapter(
        state: &DocumentState,
        limits: Limits,
    ) -> Result<TantivyAdapter, TantivySourceError> {
        Ok(TantivyAdapter::new(Self::build(state, limits)?, limits)?)
    }

    /// Builds and commits a concrete Tantivy projection from complete reconciled state.
    ///
    /// # Errors
    ///
    /// Returns a typed contract or Tantivy construction failure.
    pub fn build(state: &DocumentState, limits: Limits) -> Result<Self, TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        let projected = projection_schema();
        let index = Index::create_in_ram(projected.schema);
        Self::populate(state, limits, index, projected.fields)
    }

    /// Reopens a committed directory only when its schema and complete state
    /// binding match the supplied authoritative snapshot.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, schema, binding, coverage, or Tantivy failure.
    pub fn open_in_dir(
        state: &DocumentState,
        limits: Limits,
        directory: impl AsRef<Path>,
    ) -> Result<Self, TantivySourceError> {
        Self::open_in_dir_with_budget(state, limits, directory, DurableCacheBudget::default())
    }

    /// Reopens a committed directory while applying an explicit cache budget.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::open_in_dir`], plus
    /// [`TantivySourceError::BudgetExceeded`] when the verified root is larger
    /// than `budget`.
    pub fn open_in_dir_with_budget(
        state: &DocumentState,
        limits: Limits,
        directory: impl AsRef<Path>,
        budget: DurableCacheBudget,
    ) -> Result<Self, TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        preflight_ordinal_map_capacity(state.iter().count())?;
        let directory = directory.as_ref();
        let _directory_handle =
            backend_platform::durability::open_directory_readonly_nofollow(directory)?;
        verify_projection_manifest(directory, projection_fingerprint(state.binding()), budget)?;
        let persisted = read_binding_stamp(directory)?;
        if persisted != projection_fingerprint(state.binding()) {
            return Err(Error::StaleRoot.into());
        }
        let index = Index::open_in_dir(directory)?;
        let projected = projection_schema();
        if index.schema() != projected.schema {
            return Err(Error::SchemaDrift.into());
        }
        let reader = index.reader()?;
        let expected_tokens = state
            .iter()
            .flat_map(|(_, fields)| fields.iter())
            .flat_map(|(_, text)| searchable_tokens(&text))
            .count();
        let indexed_tokens =
            usize::try_from(reader.searcher().num_docs()).map_err(|_| Error::SizeLimit)?;
        if indexed_tokens != expected_tokens {
            return Err(Self::corrupt(
                "indexed token count does not match bound state",
            ));
        }
        let documents =
            read_ordinal_map(state, projection_fingerprint(state.binding()), directory)?;
        let fields = projected.fields;
        let identity_ordinals = documents.identity_index();
        Ok(Self {
            binding: state.binding(),
            coverage: state.coverage(),
            limits,
            _index: index,
            reader,
            raw_token: fields.raw_token,
            folded_token: fields.folded_token,
            ranking_token: fields.ranking_token,
            field_name: fields.field_name,
            ordinal: fields.ordinal,
            rank_weight: fields.rank_weight,
            rank_bytes: fields.rank_bytes,
            documents,
            identity_ordinals,
            _root_lease: None,
            poisoned: false,
            rank_budget: RankSnapshotBudget::default(),
            rank_cache: Mutex::new(None),
            rank_evaluations: AtomicU64::new(0),
        })
    }

    fn corrupt(detail: &'static str) -> TantivySourceError {
        TantivySourceError::Corrupt(detail)
    }

    /// Builds and commits the projection in a durable Tantivy directory.
    ///
    /// The supplied state remains the authority for semantic identities and the
    /// complete root binding; Tantivy stores only the replaceable search projection.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, contract, or Tantivy construction failure.
    pub fn build_in_dir(
        state: &DocumentState,
        limits: Limits,
        directory: impl AsRef<Path>,
    ) -> Result<Self, TantivySourceError> {
        Self::build_in_dir_with_budget(state, limits, directory, DurableCacheBudget::default())
    }

    /// Builds a durable projection under an explicit cache byte budget.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::build_in_dir`], plus
    /// [`TantivySourceError::BudgetExceeded`] when its completed projection
    /// exceeds `budget`.
    pub fn build_in_dir_with_budget(
        state: &DocumentState,
        limits: Limits,
        directory: impl AsRef<Path>,
        budget: DurableCacheBudget,
    ) -> Result<Self, TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        preflight_ordinal_map_capacity(state.iter().count())?;
        let projected = projection_schema();
        let index = Index::create_in_dir(directory.as_ref(), projected.schema)?;
        let source = Self::populate(state, limits, index, projected.fields)?;
        write_ordinal_map(
            directory.as_ref(),
            projection_fingerprint(state.binding()),
            &source.documents,
        )?;
        write_binding_stamp(directory.as_ref(), projection_fingerprint(state.binding()))?;
        write_projection_manifest(
            directory.as_ref(),
            projection_fingerprint(state.binding()),
            budget,
        )?;
        Ok(source)
    }

    /// Opens or atomically publishes the durable projection selected by the
    /// complete lexical binding. The cache root contains immutable
    /// content-addressed generations, so a failed refresh cannot replace the
    /// last usable generation. The owner-supplied document state remains the
    /// authority; this method never admits a posting set by fingerprint alone.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, contract, schema, binding, coverage, or Tantivy
    /// failure. Incomplete staging directories are ignored and removed on the
    /// next call. At most four complete bindings are retained for rollback.
    pub fn open_or_build_in_dir(
        state: &DocumentState,
        limits: Limits,
        cache_root: impl AsRef<Path>,
    ) -> Result<Self, TantivySourceError> {
        Self::open_or_build_in_dir_with_action(state, limits, cache_root).map(|(source, _)| source)
    }

    /// Opens or publishes the durable projection and reports whether the
    /// selected generation was restored from disk or newly created.
    pub fn open_or_build_in_dir_with_action(
        state: &DocumentState,
        limits: Limits,
        cache_root: impl AsRef<Path>,
    ) -> Result<(Self, DurableProjectionAction), TantivySourceError> {
        Self::open_or_build_in_dir_with_budget_and_action(
            state,
            limits,
            cache_root,
            DurableCacheBudget::default(),
        )
    }

    /// Opens or publishes the selected root under an explicit cache budget.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::open_or_build_in_dir_with_action`],
    /// plus [`TantivySourceError::BudgetExceeded`] when a root or the pinned
    /// retained set does not fit `budget`.
    pub fn open_or_build_in_dir_with_budget_and_action(
        state: &DocumentState,
        limits: Limits,
        cache_root: impl AsRef<Path>,
        budget: DurableCacheBudget,
    ) -> Result<(Self, DurableProjectionAction), TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }

        fs::create_dir_all(cache_root.as_ref())?;
        let _cache_directory =
            backend_platform::durability::open_directory_readonly_nofollow(cache_root.as_ref())?;
        let _cache_lock = DurableCacheLock::acquire(cache_root.as_ref())?;
        Self::open_or_build_locked(state, limits, cache_root.as_ref(), budget)
    }

    fn open_or_build_locked(
        state: &DocumentState,
        limits: Limits,
        cache_root: &Path,
        budget: DurableCacheBudget,
    ) -> Result<(Self, DurableProjectionAction), TantivySourceError> {
        // The public entrypoints keep the process-safe cache lock alive across
        // this whole operation, including stale-stage cleanup and pruning.
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        preflight_ordinal_map_capacity(state.iter().count())?;
        let version_root = cache_root.join(DURABLE_ROOTS_DIRECTORY);
        fs::create_dir_all(&version_root)?;
        let _version_directory =
            backend_platform::durability::open_directory_readonly_nofollow(&version_root)?;
        remove_incomplete_stages(&version_root)?;
        let key = hex_fingerprint(projection_fingerprint(state.binding()));
        let selected = version_root.join(&key);

        if path_exists(&selected)? && !path_is_real_directory(&selected)? {
            remove_projection_path(&selected)?;
        }
        if path_exists(&selected)? {
            match Self::open_in_dir_with_budget(state, limits, &selected, budget) {
                Ok(mut source) => {
                    pin_durable_root(&mut source, &selected)?;
                    touch_durable_root(&selected)?;
                    prune_durable_roots(&version_root, &selected, budget)?;
                    return Ok((source, DurableProjectionAction::Opened));
                }
                Err(error) if is_definitively_corrupt_root(&error) => {
                    remove_unpinned_projection_root(&selected)?
                }
                Err(error) => return Err(error),
            }
        }

        let stage_id = NEXT_DURABLE_STAGE.fetch_add(1, AtomicOrdering::Relaxed);
        let staging =
            version_root.join(format!(".{key}.building-{}-{stage_id}", std::process::id()));
        fs::create_dir(&staging)?;
        let built = match Self::build_in_dir_with_budget(state, limits, &staging, budget) {
            Ok(source) => source,
            Err(error) => {
                let _ = remove_projection_path(&staging);
                return Err(error);
            }
        };
        drop(built);
        sync_directory(&staging)?;
        fs::rename(&staging, &selected)?;
        sync_directory(&version_root)?;

        let mut source = Self::open_in_dir_with_budget(state, limits, &selected, budget)?;
        pin_durable_root(&mut source, &selected)?;
        touch_durable_root(&selected)?;
        prune_durable_roots(&version_root, &selected, budget)?;
        Ok((source, DurableProjectionAction::Built))
    }

    /// Publishes a selected delta as a new durable generation. The previous
    /// root is copied with copy-on-write file links, revised under the same
    /// document deletion/update budget as the resident index, and exposed only
    /// after the complete target binding commits. A prior generation remains
    /// usable for rollback.
    ///
    /// If the target root is already durable, it is reopened directly and the
    /// revision is `None`. A budget refusal falls back to a complete build and
    /// also returns `None`.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, contract, schema, binding, coverage, or Tantivy
    /// failure. The previous generation is left untouched on failure.
    pub fn open_or_advance_in_dir(
        previous: &DocumentState,
        next: &DocumentState,
        limits: Limits,
        budget: OverlayLimits,
        cache_root: impl AsRef<Path>,
    ) -> Result<(Self, Option<ProjectionRevision>), TantivySourceError> {
        Self::open_or_advance_in_dir_with_action(previous, next, limits, budget, cache_root)
            .map(|(source, revision, _)| (source, revision))
    }

    /// Advances a durable binding and returns whether it reused an existing
    /// root, built a complete root, or published a delta generation.
    pub fn open_or_advance_in_dir_with_action(
        previous: &DocumentState,
        next: &DocumentState,
        limits: Limits,
        budget: OverlayLimits,
        cache_root: impl AsRef<Path>,
    ) -> Result<(Self, Option<ProjectionRevision>, DurableProjectionAction), TantivySourceError>
    {
        Self::open_or_advance_in_dir_with_budget_and_action(
            previous,
            next,
            limits,
            budget,
            cache_root,
            DurableCacheBudget::default(),
        )
    }

    /// Advances a durable binding under an explicit cache byte budget.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::open_or_advance_in_dir_with_action`],
    /// plus [`TantivySourceError::BudgetExceeded`] when a root or the pinned
    /// retained set does not fit `cache_budget`.
    pub fn open_or_advance_in_dir_with_budget_and_action(
        previous: &DocumentState,
        next: &DocumentState,
        limits: Limits,
        budget: OverlayLimits,
        cache_root: impl AsRef<Path>,
        cache_budget: DurableCacheBudget,
    ) -> Result<(Self, Option<ProjectionRevision>, DurableProjectionAction), TantivySourceError>
    {
        let limits = limits.validate()?;
        if !matches!(previous.coverage(), CoverageWitness::Complete(_))
            || !matches!(next.coverage(), CoverageWitness::Complete(_))
        {
            return Err(Error::IncompleteCoverage.into());
        }
        fs::create_dir_all(cache_root.as_ref())?;
        let _cache_directory =
            backend_platform::durability::open_directory_readonly_nofollow(cache_root.as_ref())?;
        let _cache_lock = DurableCacheLock::acquire(cache_root.as_ref())?;
        Self::open_or_advance_locked(
            previous,
            next,
            limits,
            budget,
            cache_root.as_ref(),
            cache_budget,
        )
    }

    fn open_or_advance_locked(
        previous: &DocumentState,
        next: &DocumentState,
        limits: Limits,
        budget: OverlayLimits,
        cache_root: &Path,
        cache_budget: DurableCacheBudget,
    ) -> Result<(Self, Option<ProjectionRevision>, DurableProjectionAction), TantivySourceError>
    {
        let limits = limits.validate()?;
        if !matches!(previous.coverage(), CoverageWitness::Complete(_))
            || !matches!(next.coverage(), CoverageWitness::Complete(_))
        {
            return Err(Error::IncompleteCoverage.into());
        }
        preflight_ordinal_map_capacity(next.iter().count())?;
        if previous.binding() == next.binding() {
            return Self::open_or_build_locked(next, limits, cache_root, cache_budget)
                .map(|(source, action)| (source, None, action));
        }
        let version_root = cache_root.join(DURABLE_ROOTS_DIRECTORY);
        fs::create_dir_all(&version_root)?;
        let _version_directory =
            backend_platform::durability::open_directory_readonly_nofollow(&version_root)?;
        remove_incomplete_stages(&version_root)?;

        let next_key = hex_fingerprint(projection_fingerprint(next.binding()));
        let selected = version_root.join(&next_key);
        if path_exists(&selected)? && !path_is_real_directory(&selected)? {
            remove_projection_path(&selected)?;
        }
        if path_exists(&selected)? {
            match Self::open_in_dir_with_budget(next, limits, &selected, cache_budget) {
                Ok(mut source) => {
                    pin_durable_root(&mut source, &selected)?;
                    touch_durable_root(&selected)?;
                    prune_durable_roots(&version_root, &selected, cache_budget)?;
                    return Ok((source, None, DurableProjectionAction::Opened));
                }
                Err(error) if is_definitively_corrupt_root(&error) => {
                    remove_unpinned_projection_root(&selected)?
                }
                Err(error) => return Err(error),
            }
        }

        let previous_key = hex_fingerprint(projection_fingerprint(previous.binding()));
        let previous_path = version_root.join(previous_key);
        let (previous_source, _) =
            Self::open_or_build_locked(previous, limits, cache_root, cache_budget)?;
        drop(previous_source);

        let stage_id = NEXT_DURABLE_STAGE.fetch_add(1, AtomicOrdering::Relaxed);
        let staging = version_root.join(format!(
            ".{next_key}.building-{}-{stage_id}",
            std::process::id()
        ));
        if let Err(error) = copy_projection_tree(&previous_path, &staging) {
            let _ = remove_projection_path(&staging);
            return Err(error.into());
        }
        let mut staged =
            match Self::open_in_dir_with_budget(previous, limits, &staging, cache_budget) {
                Ok(source) => source,
                Err(error) => {
                    let _ = remove_projection_path(&staging);
                    return Err(error);
                }
            };
        let revision = match staged.maintain(next, budget) {
            Ok(MaintainOutcome::Applied(revision)) => revision,
            Ok(MaintainOutcome::RebuildRequired) => {
                drop(staged);
                remove_projection_path(&staging)?;
                return Self::open_or_build_locked(next, limits, cache_root, cache_budget)
                    .map(|(source, action)| (source, None, action));
            }
            Err(error) => {
                drop(staged);
                let _ = remove_projection_path(&staging);
                return Err(error);
            }
        };
        write_ordinal_map(
            &staging,
            projection_fingerprint(next.binding()),
            &staged.documents,
        )?;
        write_binding_stamp(&staging, projection_fingerprint(next.binding()))?;
        drop(staged);
        write_projection_manifest(
            &staging,
            projection_fingerprint(next.binding()),
            cache_budget,
        )?;
        sync_directory(&staging)?;
        fs::rename(&staging, &selected)?;
        sync_directory(&version_root)?;

        let mut source = Self::open_in_dir_with_budget(next, limits, &selected, cache_budget)?;
        pin_durable_root(&mut source, &selected)?;
        touch_durable_root(&selected)?;
        prune_durable_roots(&version_root, &selected, cache_budget)?;
        Ok((source, Some(revision), DurableProjectionAction::Revised))
    }

    fn populate(
        state: &DocumentState,
        limits: Limits,
        index: Index,
        fields: ProjectionFields,
    ) -> Result<Self, TantivySourceError> {
        if state.iter().count() > MAX_ORDINAL_SLOTS {
            return Err(Error::SizeLimit.into());
        }
        let mut writer = index.writer(WRITER_MEMORY_BYTES)?;
        let mut live = Vec::new();
        for (document, document_fields) in state.iter() {
            let document_ordinal = u64::try_from(live.len()).map_err(|_| Error::SizeLimit)?;
            let postings = write_fields(&writer, &fields, document_ordinal, document_fields)?;
            live.push(OrdinalDocument {
                ordinal: u32::try_from(document_ordinal).map_err(|_| Error::SizeLimit)?,
                document: LiveDocument {
                    id: document,
                    fields_digest: document_fields_digest(document_fields),
                    postings,
                },
            });
        }
        let documents = DocumentTable::from_live(
            u32::try_from(live.len()).map_err(|_| Error::SizeLimit)?,
            live,
        )?;
        let identity_ordinals = documents.identity_index();
        writer.commit()?;
        // Join background merges so no thread is still rewriting the index
        // directory once the source is handed out.
        writer.wait_merging_threads()?;
        let reader = index.reader()?;
        Ok(Self {
            binding: state.binding(),
            coverage: state.coverage(),
            limits,
            _index: index,
            reader,
            raw_token: fields.raw_token,
            folded_token: fields.folded_token,
            ranking_token: fields.ranking_token,
            field_name: fields.field_name,
            ordinal: fields.ordinal,
            rank_weight: fields.rank_weight,
            rank_bytes: fields.rank_bytes,
            documents,
            identity_ordinals,
            _root_lease: None,
            poisoned: false,
            rank_budget: RankSnapshotBudget::default(),
            rank_cache: Mutex::new(None),
            rank_evaluations: AtomicU64::new(0),
        })
    }

    /// Returns the number of token postings visible to the current reader.
    #[must_use]
    pub fn indexed_postings(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// Replaces the per-source exact-query scratch and retained-rank budget.
    /// Configure this before the first query. The source retains only one
    /// query snapshot at a time; separately live sources have separate
    /// allowances.
    #[must_use]
    pub fn with_rank_snapshot_budget(mut self, budget: RankSnapshotBudget) -> Self {
        self.rank_budget = budget;
        self
    }

    /// Counts full ranking passes caused by a page miss.
    ///
    /// A later page of the same binding and query reuses the retained rank.
    #[must_use]
    pub fn rank_evaluations(&self) -> u64 {
        self.rank_evaluations.load(Ordering::Relaxed)
    }

    /// Drops the retained rank so the next page computes it again.
    ///
    /// # Errors
    ///
    /// Returns a corrupt-projection error when the rank lock is poisoned.
    pub fn clear_rank_cache(&self) -> Result<(), TantivySourceError> {
        self.rank_cache
            .lock()
            .map_err(|_| Self::corrupt("rank cache lock poisoned"))?
            .take();
        Ok(())
    }

    fn ensure_live(&self) -> Result<(), TantivySourceError> {
        if self.poisoned {
            Err(Self::corrupt(
                "projection commit was not admitted by the reader",
            ))
        } else {
            Ok(())
        }
    }

    /// Absorbs a complete document snapshot into this resident projection.
    ///
    /// Identical documents keep their ordinals and postings. A changed view
    /// binding is stamped onto the same index. Document edits delete and
    /// append only the affected ordinals. An edit past `budget` leaves this
    /// projection untouched so the caller can build a replacement.
    ///
    /// # Errors
    ///
    /// Returns a typed coverage, size, or Tantivy failure. A failure leaves
    /// the resident binding and ordinal map unchanged.
    pub fn maintain(
        &mut self,
        next: &DocumentState,
        budget: OverlayLimits,
    ) -> Result<MaintainOutcome, TantivySourceError> {
        self.ensure_live()?;
        self.rank_cache
            .lock()
            .map_err(|_| Self::corrupt("rank cache lock poisoned"))?
            .take();
        let Some(plan) = self.plan_revision(next, budget)? else {
            return Ok(MaintainOutcome::RebuildRequired);
        };
        if plan.deletes.is_empty() && plan.writes.is_empty() {
            self.binding = next.binding();
            self.coverage = next.coverage();
            return Ok(MaintainOutcome::Applied(ProjectionRevision {
                kind: ProjectionKind::Rebound,
                rewritten_documents: 0,
                retired_postings: 0,
                added_postings: 0,
            }));
        }
        self.commit_revision(next, plan)
    }

    fn plan_revision(
        &self,
        next: &DocumentState,
        budget: OverlayLimits,
    ) -> Result<Option<RevisionPlan>, TantivySourceError> {
        let budget = budget.validate()?;
        if !matches!(next.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        if next.binding().workspace != self.binding.workspace {
            return Ok(None);
        }
        let mut next_fields = BTreeMap::new();
        for (id, fields) in next.iter() {
            if next_fields.insert(id, fields).is_some() {
                return Err(Error::MalformedInput.into());
            }
        }
        let mut rewritten = Vec::new();
        let mut removed = Vec::new();
        for (_, current) in self.documents.iter() {
            let id = current.id;
            match next_fields.get(&id) {
                Some(fields) if document_fields_digest(fields) == current.fields_digest => {}
                Some(_) => rewritten.push(id),
                None => removed.push(id),
            }
        }
        let mut added = Vec::new();
        for id in next_fields.keys() {
            if self
                .documents
                .ordinal_for_entity(&self.identity_ordinals, *id)
                .is_none()
            {
                added.push(*id);
            }
        }
        let rewritten_documents = rewritten
            .len()
            .saturating_add(removed.len())
            .saturating_add(added.len());
        if rewritten_documents == 0 {
            return Ok(Some(RevisionPlan {
                rewritten_documents: 0,
                retired_postings: 0,
                deletes: Vec::new(),
                writes: Vec::new(),
                documents: self.documents.clone(),
            }));
        }
        if rewritten_documents > budget.max_changed_documents {
            return Ok(None);
        }
        let mut term_count = 0usize;
        for id in rewritten.iter().chain(added.iter()) {
            let Some(fields) = next_fields.get(id) else {
                return Err(Self::corrupt("planned document is missing its fields"));
            };
            term_count = term_count
                .checked_add(usize::try_from(posting_count(fields)?).map_err(|_| Error::SizeLimit)?)
                .ok_or(Error::SizeLimit)?;
        }
        if term_count > budget.max_terms {
            return Ok(None);
        }
        let resulting_slots = (self.documents.slot_count as usize).saturating_add(added.len());
        // Stable ordinals leave holes after deletion. Compact through a
        // complete rebuild before the sparse ordinal map grows without bound.
        if resulting_slots > MAX_ORDINAL_SLOTS
            || resulting_slots > next_fields.len().saturating_mul(2).saturating_add(65_536)
        {
            return Ok(None);
        }
        let mut documents = self.documents.clone();
        let mut deletes = Vec::new();
        let mut removed_ordinals = Vec::new();
        let mut retired_postings = 0u64;
        for id in removed.iter().chain(rewritten.iter()) {
            let Some(ordinal) = self
                .documents
                .ordinal_for_entity(&self.identity_ordinals, *id)
            else {
                return Err(Self::corrupt("planned document has no ordinal"));
            };
            let Some(current) = self.documents.get(ordinal).copied() else {
                return Err(Self::corrupt("planned ordinal is already empty"));
            };
            retired_postings = retired_postings
                .checked_add(u64::from(current.postings))
                .ok_or(Error::SizeLimit)?;
            deletes.push(u64::try_from(ordinal).map_err(|_| Error::SizeLimit)?);
            removed_ordinals.push(u32::try_from(ordinal).map_err(|_| Error::SizeLimit)?);
        }
        removed_ordinals.sort_unstable();
        removed_ordinals.dedup();
        documents
            .live
            .retain(|entry| removed_ordinals.binary_search(&entry.ordinal).is_err());
        let mut writes = Vec::new();
        for id in rewritten {
            let Some(ordinal) = self
                .documents
                .ordinal_for_entity(&self.identity_ordinals, id)
            else {
                return Err(Self::corrupt("revised document has no ordinal"));
            };
            let Some(fields) = next_fields.get(&id) else {
                return Err(Self::corrupt("revised document is missing its fields"));
            };
            writes.push(PlannedWrite {
                ordinal: u64::try_from(ordinal).map_err(|_| Error::SizeLimit)?,
                id,
                fields_digest: document_fields_digest(fields),
                fields: fields.to_vec(),
            });
        }
        for id in added {
            let ordinal = u64::from(documents.slot_count);
            let Some(fields) = next_fields.get(&id) else {
                return Err(Self::corrupt("added document is missing its fields"));
            };
            documents.slot_count = documents
                .slot_count
                .checked_add(1)
                .ok_or(Error::SizeLimit)?;
            writes.push(PlannedWrite {
                ordinal,
                id,
                fields_digest: document_fields_digest(fields),
                fields: fields.to_vec(),
            });
        }
        Ok(Some(RevisionPlan {
            rewritten_documents,
            retired_postings,
            deletes,
            writes,
            documents,
        }))
    }

    fn commit_revision(
        &mut self,
        next: &DocumentState,
        mut plan: RevisionPlan,
    ) -> Result<MaintainOutcome, TantivySourceError> {
        let mut writer = self._index.writer(WRITER_MEMORY_BYTES)?;
        for ordinal in &plan.deletes {
            let _opstamp = writer.delete_term(Term::from_field_u64(self.ordinal, *ordinal));
        }
        let mut added_postings = 0u64;
        let fields = ProjectionFields {
            raw_token: self.raw_token,
            folded_token: self.folded_token,
            ranking_token: self.ranking_token,
            field_name: self.field_name,
            ordinal: self.ordinal,
            rank_weight: self.rank_weight,
            rank_bytes: self.rank_bytes,
        };
        let mut replacements = Vec::with_capacity(plan.writes.len());
        for write in plan.writes {
            let postings = write_fields(&writer, &fields, write.ordinal, &write.fields)?;
            added_postings = added_postings
                .checked_add(u64::from(postings))
                .ok_or(Error::SizeLimit)?;
            replacements.push(OrdinalDocument {
                ordinal: u32::try_from(write.ordinal).map_err(|_| Error::SizeLimit)?,
                document: LiveDocument {
                    id: write.id,
                    fields_digest: write.fields_digest,
                    postings,
                },
            });
        }
        writer.commit()?;
        if let Err(error) = writer.wait_merging_threads() {
            self.poisoned = true;
            return Err(error.into());
        }
        if let Err(error) = self.reader.reload() {
            self.poisoned = true;
            return Err(error.into());
        }
        self.documents = plan.documents.merge_replacements(replacements)?;
        self.identity_ordinals = self.documents.identity_index();
        self.binding = next.binding();
        self.coverage = next.coverage();
        Ok(MaintainOutcome::Applied(ProjectionRevision {
            kind: ProjectionKind::Revised,
            rewritten_documents: plan.rewritten_documents,
            retired_postings: plan.retired_postings,
            added_postings,
        }))
    }

    /// Materializes every exact match in canonical rank order.
    ///
    /// # Errors
    ///
    /// Returns a typed query-admission, index-read, or projection-integrity failure.
    pub fn search(&self, query: &Query) -> Result<Vec<RankedHit>, TantivySourceError> {
        self.ensure_live()?;
        query.validate(self.limits)?;
        self.with_ranked_snapshot(query, |snapshot| {
            let mut hits = Vec::with_capacity(snapshot.total);
            snapshot.ranks.for_each(|ordinal, relevance| {
                let document = self
                    .documents
                    .get(ordinal)
                    .map(|document| document.id)
                    .ok_or_else(|| Self::corrupt("rank ordinal is outside the binding"))?;
                hits.push(RankedHit {
                    document,
                    relevance,
                });
                Ok(())
            })?;
            hits.sort_unstable_by(|left, right| compare_ranked_hits(*left, *right));
            Ok(hits)
        })
    }

    /// Visits exact query matches once in stable ordinal order without
    /// constructing a result array. This is not display order; each hit
    /// carries its exact rank for bounded caller-side composition.
    ///
    /// The cached query rank is compact ordinal metadata scoped to this
    /// selected binding; callers can compose a bounded answer in one pass.
    ///
    /// # Errors
    ///
    /// Returns a typed query-admission, Tantivy, or projection-integrity failure.
    pub fn for_each_ranked_hit(
        &self,
        query: &Query,
        mut visit: impl FnMut(RankedHit),
    ) -> Result<usize, TantivySourceError> {
        self.ensure_live()?;
        query.validate(self.limits)?;
        self.with_ranked_snapshot(query, |snapshot| {
            snapshot.ranks.for_each(|ordinal, relevance| {
                let document = self
                    .documents
                    .get(ordinal)
                    .map(|document| document.id)
                    .ok_or_else(|| Self::corrupt("rank ordinal is outside the binding"))?;
                visit(RankedHit {
                    document,
                    relevance,
                });
                Ok(())
            })?;
            Ok(snapshot.total)
        })
    }

    /// Resolves a bounded set of candidate identities against exact lexical
    /// relevance without paging through the result set.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SizeLimit`] when the candidate set exceeds the query
    /// page bound, and a typed query or projection failure otherwise.
    pub fn relevance_for_candidates(
        &self,
        query: &Query,
        candidates: &[EntityId],
    ) -> Result<BTreeMap<EntityId, Relevance>, TantivySourceError> {
        self.ensure_live()?;
        query.validate(self.limits)?;
        if candidates.len() > self.limits.max_page {
            return Err(Error::SizeLimit.into());
        }
        if candidates.is_empty() {
            return Ok(BTreeMap::new());
        }
        self.with_ranked_snapshot(query, |snapshot| {
            let mut relevance = BTreeMap::new();
            for candidate in candidates.iter().copied() {
                let Some(ordinal) = self
                    .documents
                    .ordinal_for_entity(&self.identity_ordinals, candidate)
                else {
                    continue;
                };
                if let Some(score) = snapshot.ranks.get(ordinal)? {
                    relevance.insert(candidate, score);
                }
            }
            Ok(relevance)
        })
    }

    fn query_clause_candidates(
        &self,
        term: &str,
        query: &Query,
        max_rows: usize,
    ) -> Result<BTreeMap<u64, Relevance>, TantivySourceError> {
        let engine_query = self.compile_clause(term, query);
        let searcher = self.reader.searcher();
        if usize::try_from(searcher.num_docs()).is_err() {
            return Err(Error::SizeLimit.into());
        }
        if searcher.num_docs() == 0 {
            return Ok(BTreeMap::new());
        }
        let collector = ClauseRankCollector {
            term_bytes: term.len(),
            max_rows,
            admitted_rows: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let ranked_ordinals = searcher
            .search(engine_query.as_ref(), &collector)?
            .map_err(|error| match error {
                ClauseRankError::Capacity => {
                    let required_bytes = max_rows
                        .saturating_add(1)
                        .saturating_mul(RANK_SCRATCH_BYTES_PER_MATCH);
                    TantivySourceError::RankSnapshotBudgetExceeded {
                        budget_bytes: self.rank_budget.max_scratch_bytes,
                        required_bytes,
                    }
                }
                ClauseRankError::Contract(error) => TantivySourceError::Contract(error),
            })?;
        for ordinal in ranked_ordinals.keys() {
            let ordinal = usize::try_from(*ordinal).map_err(|_| Error::SizeLimit)?;
            if self.documents.get(ordinal).is_none() {
                return Err(Self::corrupt("posting ordinal is outside the binding"));
            }
        }
        Ok(ranked_ordinals)
    }

    fn compile_clause(&self, term: &str, query: &Query) -> Box<dyn TantivyQuery> {
        let token_field = match query.case {
            crate::CaseSensitivity::Sensitive => self.raw_token,
            crate::CaseSensitivity::FoldAscii => self.folded_token,
        };
        let token = Term::from_field_text(token_field, term);
        let token_query: Box<dyn TantivyQuery> = match query.match_mode {
            MatchMode::Exact => Box::new(TermQuery::new(token, IndexRecordOption::Basic)),
            MatchMode::Prefix => Box::new(FuzzyTermQuery::new_prefix(token, 0, true)),
        };
        match &query.fields {
            FieldSelection::All => token_query,
            FieldSelection::Only(field) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, token_query),
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.field_name, field),
                        IndexRecordOption::Basic,
                    )),
                ),
            ])),
        }
    }

    fn with_ranked_snapshot<R>(
        &self,
        query: &Query,
        use_snapshot: impl FnOnce(&CachedRank) -> Result<R, TantivySourceError>,
    ) -> Result<R, TantivySourceError> {
        let mut guard = self
            .rank_cache
            .lock()
            .map_err(|_| Self::corrupt("rank cache lock poisoned"))?;
        if !guard
            .as_ref()
            .is_some_and(|cached| cached.binding == self.binding && cached.query == query.version)
        {
            // Release a different query's retained arrays before allocating
            // the next bounded rank snapshot.
            guard.take();
            let snapshot = self.build_rank_snapshot(query)?;
            *guard = Some(snapshot);
            self.rank_evaluations.fetch_add(1, Ordering::Relaxed);
        }
        let snapshot = guard
            .as_ref()
            .ok_or_else(|| Self::corrupt("rank snapshot was not retained"))?;
        use_snapshot(snapshot)
    }

    fn build_rank_snapshot(&self, query: &Query) -> Result<CachedRank, TantivySourceError> {
        let max_rows = self.rank_budget.max_scratch_bytes / RANK_SCRATCH_BYTES_PER_MATCH;
        let entries = if query.terms.is_empty() {
            let live_rows = self.documents.live.len();
            ensure_rank_scratch_capacity(live_rows, self.rank_budget.max_scratch_bytes)?;
            self.documents
                .iter()
                .map(|(ordinal, _)| (ordinal, Relevance::all_documents()))
                .collect::<Vec<_>>()
        } else {
            let mut candidates = self.query_clause_candidates(&query.terms[0], query, max_rows)?;
            for term in query.terms.iter().skip(1) {
                let posting = self.query_clause_candidates(term, query, max_rows)?;
                let mut intersection = BTreeMap::new();
                for (ordinal, relevance) in candidates {
                    if let Some(next) = posting.get(&ordinal) {
                        intersection.insert(ordinal, relevance.combine(*next)?);
                    }
                }
                candidates = intersection;
            }
            candidates
                .into_iter()
                .map(|(ordinal, relevance)| {
                    Ok((
                        usize::try_from(ordinal).map_err(|_| Error::SizeLimit)?,
                        relevance,
                    ))
                })
                .collect::<Result<Vec<_>, Error>>()?
        };
        let total = entries.len();
        ensure_rank_scratch_capacity(total, self.rank_budget.max_scratch_bytes)?;
        let ranks = RankStorage::from_entries(entries, &self.documents, self.rank_budget)?;
        Ok(CachedRank {
            binding: self.binding,
            query: query.version,
            total,
            ranks,
        })
    }
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct SearchableToken {
    searchable: String,
    ranking: String,
}

fn searchable_tokens(text: &str) -> Vec<SearchableToken> {
    // The projection only needs canonical order after tokenization.  A tree
    // allocates one node per token while this bounded vector can sort and
    // deduplicate in place, retaining the same `(searchable, ranking)` set
    // with fewer allocations and better locality during cold ingest.
    let mut tokens = Vec::new();
    for raw in text.split_whitespace().filter(|token| !token.is_empty()) {
        tokens.push(SearchableToken {
            searchable: raw.to_owned(),
            ranking: raw.to_owned(),
        });
        for identifier in raw.split(|character: char| !character.is_alphanumeric()) {
            if identifier.is_empty() {
                continue;
            }
            tokens.push(SearchableToken {
                searchable: identifier.to_owned(),
                ranking: raw.to_owned(),
            });
            tokens.extend(identifier_words(identifier).map(|word| SearchableToken {
                searchable: word.to_owned(),
                ranking: raw.to_owned(),
            }));
        }
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

fn identifier_words(identifier: &str) -> impl Iterator<Item = &str> {
    let mut words = Vec::new();
    let mut characters = identifier.char_indices().peekable();
    let Some((_, mut previous)) = characters.next() else {
        return words.into_iter();
    };
    let mut start = 0;
    while let Some((index, current)) = characters.next() {
        let next = characters.peek().map(|(_, character)| *character);
        let case_boundary = (previous.is_lowercase() && current.is_uppercase())
            || (previous.is_uppercase()
                && current.is_uppercase()
                && next.is_some_and(char::is_lowercase));
        let class_boundary = previous.is_numeric() != current.is_numeric();
        if case_boundary || class_boundary {
            let end = index;
            if start < end {
                words.push(&identifier[start..end]);
            }
            start = end;
        }
        previous = current;
    }
    if start < identifier.len() {
        words.push(&identifier[start..]);
    }
    words.into_iter()
}

struct ProjectedSchema {
    schema: Schema,
    fields: ProjectionFields,
}

struct ProjectionFields {
    raw_token: Field,
    folded_token: Field,
    ranking_token: Field,
    field_name: Field,
    ordinal: Field,
    rank_weight: Field,
    rank_bytes: Field,
}

fn projection_schema() -> ProjectedSchema {
    let mut schema = Schema::builder();
    let raw_token = schema.add_text_field("raw_token", STRING | STORED);
    let folded_token = schema.add_text_field("folded_token", STRING);
    let ranking_token = schema.add_text_field("ranking_token", STORED);
    let field_name = schema.add_text_field("field_name", STRING | STORED);
    // Indexed so a revision can delete one document's postings by ordinal.
    // Fast columns carry the ordinal, field weight, and ranking length so a
    // query can rank without loading stored fields.
    let ordinal = schema.add_u64_field("document_ordinal", INDEXED | STORED | FAST);
    let rank_weight = schema.add_u64_field("rank_weight", FAST);
    let rank_bytes = schema.add_u64_field("rank_bytes", FAST);
    ProjectedSchema {
        schema: schema.build(),
        fields: ProjectionFields {
            raw_token,
            folded_token,
            ranking_token,
            field_name,
            ordinal,
            rank_weight,
            rank_bytes,
        },
    }
}

fn projection_fingerprint(binding: Binding) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-extension-tantivy/projection/v2");
    hasher.update(binding.workspace.as_bytes());
    hasher.update(binding.root.as_bytes());
    hasher.update(binding.recipe.as_bytes());
    hasher.update(binding.authority.as_bytes());
    hasher.update(binding.read_manifest.as_bytes());
    hasher.update(binding.frontier.as_bytes());
    *hasher.finalize().as_bytes()
}

fn hex_fingerprint(fingerprint: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in fingerprint {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn write_binding_stamp(directory: &Path, fingerprint: [u8; 32]) -> Result<(), std::io::Error> {
    let stamp = directory.join(BINDING_FILE);
    let staging = directory.join(format!(".{BINDING_FILE}.tmp"));
    let mut file = backend_platform::durability::open_or_truncate_regular_file_nofollow(&staging)?;
    std::io::Write::write_all(&mut file, &fingerprint)?;
    file.sync_all()?;
    backend_platform::durable::replace_file(&staging, &stamp)?;
    sync_directory(directory)
}

fn read_binding_stamp(directory: &Path) -> Result<[u8; 32], TantivySourceError> {
    let path = directory.join(BINDING_FILE);
    let mut file = match backend_platform::durability::open_regular_file_nofollow(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(TantivySource::corrupt(
                "durable projection has no binding stamp",
            ));
        }
        Err(error) if is_nofollow_rejection(&error) => {
            return Err(TantivySource::corrupt(
                "durable projection binding is not a regular file",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() != 32 {
        return Err(TantivySource::corrupt(
            "durable projection binding has an invalid size",
        ));
    }
    let mut binding = [0_u8; 32];
    file.read_exact(&mut binding)?;
    Ok(binding)
}

fn write_ordinal_map(
    directory: &Path,
    fingerprint: [u8; 32],
    documents: &DocumentTable,
) -> Result<(), TantivySourceError> {
    if documents.slot_count as usize > MAX_ORDINAL_SLOTS {
        return Err(Error::SizeLimit.into());
    }
    let live_count = documents.live.len();
    let total_bytes = preflight_ordinal_map_capacity(live_count)?;
    let mut bytes = Vec::with_capacity(total_bytes);
    bytes.extend_from_slice(ORDINAL_MAP_MAGIC);
    bytes.extend_from_slice(&fingerprint);
    bytes.extend_from_slice(&u64::from(documents.slot_count).to_le_bytes());
    bytes.extend_from_slice(
        &u64::try_from(live_count)
            .map_err(|_| Error::SizeLimit)?
            .to_le_bytes(),
    );
    for entry in &documents.live {
        let ordinal = u64::from(entry.ordinal);
        let document = &entry.document;
        bytes.extend_from_slice(&ordinal.to_le_bytes());
        bytes.extend_from_slice(document.id.as_bytes());
        bytes.extend_from_slice(&document.fields_digest);
        bytes.extend_from_slice(&document.postings.to_le_bytes());
        bytes.extend_from_slice(&ordinal_witness(fingerprint, ordinal, document));
    }
    debug_assert_eq!(bytes.len(), total_bytes);
    let staging = directory.join(format!(".{ORDINAL_MAP_FILE}.tmp"));
    let mut file = backend_platform::durability::open_or_truncate_regular_file_nofollow(&staging)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()?;
    backend_platform::durable::replace_file(&staging, &directory.join(ORDINAL_MAP_FILE))?;
    sync_directory(directory)?;
    Ok(())
}

fn preflight_ordinal_map_capacity(live_count: usize) -> Result<usize, TantivySourceError> {
    let required_bytes = live_count
        .checked_mul(ORDINAL_MAP_RECORD_BYTES)
        .and_then(|records| {
            ORDINAL_MAP_MAGIC
                .len()
                .checked_add(48)?
                .checked_add(records)
        })
        .ok_or(Error::SizeLimit)?;
    if required_bytes as u64 > MAX_ORDINAL_MAP_BYTES {
        return Err(TantivySourceError::OrdinalMapCapacityExceeded {
            maximum_bytes: MAX_ORDINAL_MAP_BYTES,
            required_bytes: u64::try_from(required_bytes).map_err(|_| Error::SizeLimit)?,
        });
    }
    Ok(required_bytes)
}

fn ordinal_witness(fingerprint: [u8; 32], ordinal: u64, document: &LiveDocument) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-tantivy-ordinal-witness-v1\0");
    hasher.update(&fingerprint);
    hasher.update(&ordinal.to_le_bytes());
    hasher.update(document.id.as_bytes());
    hasher.update(&document.fields_digest);
    hasher.update(&document.postings.to_le_bytes());
    *hasher.finalize().as_bytes()
}

fn read_ordinal_map(
    state: &DocumentState,
    fingerprint: [u8; 32],
    directory: &Path,
) -> Result<DocumentTable, TantivySourceError> {
    let path = directory.join(ORDINAL_MAP_FILE);
    let bytes = match read_bounded_regular_file(&path, MAX_ORDINAL_MAP_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(TantivySource::corrupt(
                "durable projection has no ordinal map",
            ));
        }
        Err(error) if is_nofollow_rejection(&error) => {
            return Err(TantivySource::corrupt(
                "durable projection ordinal map is malformed",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if !bytes.starts_with(ORDINAL_MAP_MAGIC) {
        return Err(TantivySource::corrupt(
            "durable projection ordinal map is malformed",
        ));
    }
    let mut offset = ORDINAL_MAP_MAGIC.len();
    if take_bytes::<32>(&bytes, &mut offset) != Some(fingerprint) {
        return Err(TantivySource::corrupt(
            "durable projection ordinal map has another root",
        ));
    }
    let Some(slot_count) =
        take_u64(&bytes, &mut offset).and_then(|count| usize::try_from(count).ok())
    else {
        return Err(TantivySource::corrupt(
            "durable projection ordinal map has an invalid slot count",
        ));
    };
    let Some(live_count) =
        take_u64(&bytes, &mut offset).and_then(|count| usize::try_from(count).ok())
    else {
        return Err(TantivySource::corrupt(
            "durable projection ordinal map has an invalid live count",
        ));
    };
    let Some(expected_bytes) = live_count
        .checked_mul(8 + 32 + 32 + 4 + 32)
        .and_then(|records| offset.checked_add(records))
    else {
        return Err(TantivySource::corrupt(
            "durable projection ordinal map size overflows",
        ));
    };
    let expected = state.iter().collect::<Vec<_>>();
    if slot_count > MAX_ORDINAL_SLOTS
        || live_count > slot_count
        || live_count != expected.len()
        || expected_bytes != bytes.len()
    {
        return Err(TantivySource::corrupt(
            "durable projection ordinal map has inconsistent counts",
        ));
    }
    let mut seen = vec![false; expected.len()];
    let mut documents = Vec::with_capacity(live_count);
    let mut previous_ordinal = None;
    for _ in 0..live_count {
        let Some(ordinal) =
            take_u64(&bytes, &mut offset).and_then(|ordinal| usize::try_from(ordinal).ok())
        else {
            return Err(TantivySource::corrupt(
                "durable projection ordinal map is truncated",
            ));
        };
        let Some(id_bytes) = take_bytes::<32>(&bytes, &mut offset) else {
            return Err(TantivySource::corrupt(
                "durable projection ordinal identity is truncated",
            ));
        };
        let Some(fields_digest) = take_bytes::<32>(&bytes, &mut offset) else {
            return Err(TantivySource::corrupt(
                "durable projection ordinal digest is truncated",
            ));
        };
        let Some(postings) = take_u32(&bytes, &mut offset) else {
            return Err(TantivySource::corrupt(
                "durable projection ordinal posting count is truncated",
            ));
        };
        let Some(witness) = take_bytes::<32>(&bytes, &mut offset) else {
            return Err(TantivySource::corrupt(
                "durable projection ordinal witness is truncated",
            ));
        };
        if ordinal >= slot_count || previous_ordinal.is_some_and(|previous| previous >= ordinal) {
            return Err(TantivySource::corrupt(
                "durable projection ordinal is duplicated or outside the map",
            ));
        }
        previous_ordinal = Some(ordinal);
        let expected_index = expected
            .binary_search_by(|(id, _)| id.as_bytes().cmp(&id_bytes))
            .map_err(|_| {
                TantivySource::corrupt("durable projection ordinal names another document")
            })?;
        if seen[expected_index] {
            return Err(TantivySource::corrupt(
                "durable projection ordinal document is duplicated",
            ));
        }
        let (id, fields) = expected[expected_index];
        let expected_digest = document_fields_digest(fields);
        let expected_postings = posting_count(fields)?;
        if fields_digest != expected_digest || postings != expected_postings {
            return Err(TantivySource::corrupt(
                "durable projection ordinal does not match its bound document",
            ));
        }
        let live = LiveDocument {
            id,
            fields_digest,
            postings,
        };
        if witness
            != ordinal_witness(
                fingerprint,
                u64::try_from(ordinal).map_err(|_| Error::SizeLimit)?,
                &live,
            )
        {
            return Err(TantivySource::corrupt(
                "durable projection ordinal witness does not match its slot",
            ));
        }
        seen[expected_index] = true;
        documents.push(OrdinalDocument {
            ordinal: u32::try_from(ordinal).map_err(|_| Error::SizeLimit)?,
            document: live,
        });
    }
    if offset != bytes.len() || seen.iter().any(|admitted| !admitted) {
        return Err(TantivySource::corrupt(
            "durable projection ordinal map omits a bound document",
        ));
    }
    DocumentTable::from_live(
        u32::try_from(slot_count).map_err(|_| Error::SizeLimit)?,
        documents,
    )
    .map_err(|_| TantivySource::corrupt("durable projection ordinal table is malformed"))
}

fn write_projection_manifest(
    directory: &Path,
    fingerprint: [u8; 32],
    budget: DurableCacheBudget,
) -> Result<(), TantivySourceError> {
    let files = projection_file_fingerprints(directory, budget)?;
    let content_bytes = files.values().try_fold(0_u64, |total, (size, _)| {
        total.checked_add(*size).ok_or(Error::SizeLimit)
    })?;
    let count = u32::try_from(files.len()).map_err(|_| Error::SizeLimit)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(INTEGRITY_MAGIC);
    bytes.extend_from_slice(&fingerprint);
    bytes.extend_from_slice(&count.to_le_bytes());
    for (path, (size, digest)) in files {
        let path = path.as_bytes();
        if path.is_empty() || path.len() > 255 {
            return Err(Error::SizeLimit.into());
        }
        let path_length = u32::try_from(path.len()).map_err(|_| Error::SizeLimit)?;
        bytes.extend_from_slice(&path_length.to_le_bytes());
        bytes.extend_from_slice(path);
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&digest);
    }
    if bytes.len() as u64 > MAX_PROJECTION_MANIFEST_BYTES {
        return Err(Error::SizeLimit.into());
    }
    // Reserve the integrity record and the fixed last-used marker before the
    // root can be selected. Root leases are zero-length markers; transient
    // writer locks are likewise zero-length on supported Tantivy versions.
    let required_bytes = content_bytes
        .checked_add(u64::try_from(bytes.len()).map_err(|_| Error::SizeLimit)?)
        .and_then(|total| total.checked_add(16))
        .ok_or(Error::SizeLimit)?;
    if required_bytes > budget.max_bytes() {
        return Err(TantivySourceError::BudgetExceeded {
            budget_bytes: budget.max_bytes(),
            required_bytes,
        });
    }
    let staging = directory.join(format!(".{INTEGRITY_FILE}.tmp"));
    let mut file = backend_platform::durability::open_or_truncate_regular_file_nofollow(&staging)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()?;
    backend_platform::durable::replace_file(&staging, &directory.join(INTEGRITY_FILE))?;
    sync_directory(directory)?;
    Ok(())
}

fn verify_projection_manifest(
    directory: &Path,
    fingerprint: [u8; 32],
    budget: DurableCacheBudget,
) -> Result<(), TantivySourceError> {
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(TantivySourceError::Corrupt(
                "durable projection directory is missing",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_dir() {
        return Err(TantivySourceError::Corrupt(
            "durable projection path is not a directory",
        ));
    }
    let manifest_path = directory.join(INTEGRITY_FILE);
    let bytes = match read_bounded_regular_file(&manifest_path, MAX_PROJECTION_MANIFEST_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(TantivySourceError::Corrupt(
                "durable projection has no integrity manifest",
            ));
        }
        Err(error) if is_nofollow_rejection(&error) => {
            return Err(TantivySource::corrupt(
                "durable projection integrity manifest is malformed",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if !bytes.starts_with(INTEGRITY_MAGIC) {
        return Err(TantivySourceError::Corrupt(
            "durable projection integrity manifest is malformed",
        ));
    }
    let mut offset = INTEGRITY_MAGIC.len();
    if take_bytes::<32>(&bytes, &mut offset) != Some(fingerprint) {
        return Err(Error::StaleRoot.into());
    }
    let Some(count) = take_u32(&bytes, &mut offset) else {
        return Err(TantivySourceError::Corrupt(
            "durable projection integrity manifest is truncated",
        ));
    };
    let mut expected = BTreeMap::new();
    if count as usize > MAX_PROJECTION_FILES {
        return Err(TantivySource::corrupt(
            "durable projection integrity file count exceeds its bound",
        ));
    }
    for _ in 0..count {
        let Some(path_length) = take_u32(&bytes, &mut offset) else {
            return Err(TantivySourceError::Corrupt(
                "durable projection integrity manifest is truncated",
            ));
        };
        let Some(path_bytes) = take_slice(&bytes, &mut offset, path_length as usize) else {
            return Err(TantivySourceError::Corrupt(
                "durable projection integrity path is truncated",
            ));
        };
        let Ok(path) = std::str::from_utf8(path_bytes) else {
            return Err(TantivySourceError::Corrupt(
                "durable projection integrity path is not UTF-8",
            ));
        };
        let Some(size) = take_u64(&bytes, &mut offset) else {
            return Err(TantivySourceError::Corrupt(
                "durable projection integrity file size is truncated",
            ));
        };
        let Some(digest) = take_bytes::<32>(&bytes, &mut offset) else {
            return Err(TantivySourceError::Corrupt(
                "durable projection integrity digest is truncated",
            ));
        };
        if path.is_empty()
            || path.len() > 255
            || path.starts_with('/')
            || path.contains('/')
            || !is_projection_file_name(path)
        {
            return Err(TantivySourceError::Corrupt(
                "durable projection integrity path escapes its root",
            ));
        }
        if expected.insert(path.to_owned(), (size, digest)).is_some() {
            return Err(TantivySourceError::Corrupt(
                "durable projection integrity path is duplicated",
            ));
        }
    }
    if offset != bytes.len() || expected != projection_file_fingerprints(directory, budget)? {
        return Err(TantivySourceError::Corrupt(
            "durable projection files do not match their integrity manifest",
        ));
    }
    let required_bytes = durable_root_size(directory)?;
    if required_bytes > budget.max_bytes() {
        return Err(TantivySourceError::BudgetExceeded {
            budget_bytes: budget.max_bytes(),
            required_bytes,
        });
    }
    Ok(())
}

fn take_slice<'a>(bytes: &'a [u8], offset: &mut usize, length: usize) -> Option<&'a [u8]> {
    let end = offset.checked_add(length)?;
    let value = bytes.get(*offset..end)?;
    *offset = end;
    Some(value)
}

fn take_u32(bytes: &[u8], offset: &mut usize) -> Option<u32> {
    Some(u32::from_le_bytes(take_bytes(bytes, offset)?))
}

fn take_u64(bytes: &[u8], offset: &mut usize) -> Option<u64> {
    Some(u64::from_le_bytes(take_bytes(bytes, offset)?))
}

fn take_bytes<const N: usize>(bytes: &[u8], offset: &mut usize) -> Option<[u8; N]> {
    take_slice(bytes, offset, N)?.try_into().ok()
}

fn projection_file_fingerprints(
    root: &Path,
    budget: DurableCacheBudget,
) -> Result<BTreeMap<String, (u64, [u8; 32])>, TantivySourceError> {
    let mut files = BTreeMap::new();
    let mut total_bytes = 0_u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| TantivySource::corrupt("durable projection filename is not UTF-8"))?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_file() {
            return Err(TantivySource::corrupt(
                "durable projection contains a directory, symlink, or special file",
            ));
        }
        if is_volatile_projection_file(&name) {
            continue;
        }
        if !is_projection_file_name(&name) {
            return Err(TantivySource::corrupt(
                "durable projection contains an unrecognized file",
            ));
        }
        if files.len() >= MAX_PROJECTION_FILES {
            return Err(TantivySource::corrupt(
                "durable projection file count exceeds its bound",
            ));
        }
        let path = entry.path();
        let mut file = match backend_platform::durability::open_regular_file_nofollow(&path) {
            Ok(file) => file,
            Err(error) if is_nofollow_rejection(&error) => {
                return Err(TantivySource::corrupt(
                    "durable projection file changed to a link or non-file",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let opened_metadata = file.metadata()?;
        if opened_metadata.len() != metadata.len() {
            return Err(TantivySource::corrupt(
                "durable projection file changed while being opened",
            ));
        }
        total_bytes = total_bytes
            .checked_add(opened_metadata.len())
            .ok_or(Error::SizeLimit)?;
        if total_bytes > budget.max_bytes() {
            return Err(TantivySourceError::BudgetExceeded {
                budget_bytes: budget.max_bytes(),
                required_bytes: total_bytes,
            });
        }
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut size = 0_u64;
        let mut bounded = file.take(opened_metadata.len().saturating_add(1));
        loop {
            let read = bounded.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            size = size.checked_add(read as u64).ok_or(Error::SizeLimit)?;
            hasher.update(&buffer[..read]);
        }
        if size != opened_metadata.len() {
            return Err(TantivySource::corrupt(
                "durable projection file changed while being hashed",
            ));
        }
        files.insert(name, (size, *hasher.finalize().as_bytes()));
    }
    Ok(files)
}

fn read_bounded_regular_file(path: &Path, maximum: u64) -> Result<Vec<u8>, std::io::Error> {
    let mut file =
        backend_platform::durability::open_regular_file_nofollow(path).map_err(|error| {
            if is_nofollow_rejection(&error) {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error)
            } else {
                error
            }
        })?;
    let initial_length = file.metadata()?.len();
    if initial_length > maximum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable projection metadata exceeds its read bound",
        ));
    }
    let capacity = usize::try_from(initial_length)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "file too large"))?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum || bytes.len() as u64 != initial_length {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable projection metadata changed or exceeds its read bound",
        ));
    }
    Ok(bytes)
}

fn is_volatile_projection_file(name: &str) -> bool {
    matches!(
        name,
        INTEGRITY_FILE
            | DURABLE_ROOT_LEASE
            | ".last-used"
            | ".tantivy-writer.lock"
            | ".tantivy-meta.lock"
    ) || name == format!(".{BINDING_FILE}.tmp")
        || name == format!(".{INTEGRITY_FILE}.tmp")
        || name == format!(".{ORDINAL_MAP_FILE}.tmp")
}

fn is_nofollow_rejection(error: &std::io::Error) -> bool {
    let loop_error = {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            error.raw_os_error() == Some(40)
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            error.raw_os_error() == Some(62)
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )))]
        {
            false
        }
    };
    error.kind() == std::io::ErrorKind::InvalidData || loop_error
}

fn is_projection_file_name(name: &str) -> bool {
    if matches!(
        name,
        BINDING_FILE | ORDINAL_MAP_FILE | "meta.json" | ".managed.json"
    ) {
        return true;
    }
    let Some((segment, component)) = name.split_once('.') else {
        return false;
    };
    if segment.len() != 32 || !segment.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    matches!(
        component,
        "idx" | "pos" | "term" | "store" | "fast" | "fieldnorm"
    ) || component.strip_suffix(".del").is_some_and(|opstamp| {
        !opstamp.is_empty() && opstamp.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn path_exists(path: &Path) -> Result<bool, std::io::Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn path_is_real_directory(path: &Path) -> Result<bool, std::io::Error> {
    fs::symlink_metadata(path).map(|metadata| metadata.file_type().is_dir())
}

fn is_definitively_corrupt_root(error: &TantivySourceError) -> bool {
    match error {
        TantivySourceError::Contract(Error::StaleRoot | Error::SchemaDrift)
        | TantivySourceError::Corrupt(_) => true,
        TantivySourceError::Backend(
            tantivy::TantivyError::DataCorruption(_) | tantivy::TantivyError::IncompatibleIndex(_),
        ) => true,
        TantivySourceError::BudgetExceeded { .. } => false,
        TantivySourceError::OrdinalMapCapacityExceeded { .. } => false,
        TantivySourceError::RankSnapshotBudgetExceeded { .. } => false,
        TantivySourceError::Contract(_)
        | TantivySourceError::Backend(_)
        | TantivySourceError::Io(_) => false,
    }
}

fn remove_projection_path(path: &Path) -> Result<(), std::io::Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn remove_incomplete_stages(root: &Path) -> Result<(), std::io::Error> {
    let mut count = 0_usize;
    for entry in fs::read_dir(root)? {
        count = count.saturating_add(1);
        if count > MAX_DURABLE_ROOT_SCAN_ENTRIES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable root directory exceeds its entry bound",
            ));
        }
        let entry = entry?;
        let name = entry.file_name();
        let name = name.into_string().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid cache filename")
        })?;
        if is_projection_stage_name(&name) {
            remove_projection_path(&entry.path())?;
        }
    }
    Ok(())
}

fn copy_projection_tree(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    fs::create_dir(destination)?;
    let mut count = 0_usize;
    for entry in fs::read_dir(source)? {
        count = count.saturating_add(1);
        if count > MAX_PROJECTION_FILES.saturating_add(8) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable Tantivy source exceeds its copy entry bound",
            ));
        }
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if !metadata.file_type().is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable Tantivy projection contains a directory, link, or special entry",
            ));
        }
        let name = entry.file_name();
        let name = name.into_string().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid filename")
        })?;
        if !is_projection_file_name(&name) && !is_volatile_projection_file(&name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable Tantivy projection contains an unrecognized file",
            ));
        }
        if name == ".last-used"
            || name == DURABLE_ROOT_LEASE
            || name == format!(".{BINDING_FILE}.tmp")
            || name == format!(".{INTEGRITY_FILE}.tmp")
            || name == format!(".{ORDINAL_MAP_FILE}.tmp")
        {
            continue;
        }
        if name == BINDING_FILE
            || name == ORDINAL_MAP_FILE
            || matches!(name.as_str(), ".tantivy-writer.lock" | ".tantivy-meta.lock")
        {
            copy_projection_file_nofollow(&source_path, &destination_path, metadata.len())?;
            continue;
        }
        let immutable_segment = is_immutable_segment_file_name(&name);
        if immutable_segment {
            let source_file =
                backend_platform::durability::open_regular_file_nofollow(&source_path)?;
            if source_file.metadata()?.len() != metadata.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "durable Tantivy segment changed while opening for copy-on-write",
                ));
            }
            if fs::hard_link(&source_path, &destination_path).is_ok() {
                continue;
            }
        }
        copy_projection_file_nofollow(&source_path, &destination_path, metadata.len())?;
    }
    sync_directory(destination)
}

fn is_immutable_segment_file_name(name: &str) -> bool {
    let Some((segment_id, component)) = name.split_once('.') else {
        return false;
    };
    segment_id.len() == 32
        && segment_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        && matches!(
            component,
            "store" | "idx" | "term" | "pos" | "fieldnorm" | "fast"
        )
}

fn is_projection_stage_name(name: &str) -> bool {
    let Some(name) = name.strip_prefix('.') else {
        return false;
    };
    let Some((root, suffix)) = name.split_once(".building-") else {
        return false;
    };
    let Some((process, stage)) = suffix.split_once('-') else {
        return false;
    };
    root.len() == 64
        && root.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !process.is_empty()
        && process.bytes().all(|byte| byte.is_ascii_digit())
        && !stage.is_empty()
        && stage.bytes().all(|byte| byte.is_ascii_digit())
}

fn copy_projection_file_nofollow(
    source: &Path,
    destination: &Path,
    expected_length: u64,
) -> Result<(), std::io::Error> {
    let mut source_file = backend_platform::durability::open_regular_file_nofollow(source)?;
    if source_file.metadata()?.len() != expected_length {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable Tantivy file changed while opening for copy",
        ));
    }
    let mut destination_file =
        backend_platform::durability::open_or_truncate_regular_file_nofollow(destination)?;
    let copied = std::io::copy(
        &mut source_file.take(expected_length.saturating_add(1)),
        &mut destination_file,
    )?;
    if copied != expected_length {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable Tantivy file changed while being copied",
        ));
    }
    destination_file.sync_all()
}

fn touch_durable_root(path: &Path) -> Result<(), std::io::Error> {
    let stamp = path.join(".last-used");
    let mut file = backend_platform::durability::open_or_truncate_regular_file_nofollow(&stamp)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::io::Write::write_all(&mut file, &now.to_le_bytes())?;
    file.sync_all()
}

fn pin_durable_root(source: &mut TantivySource, path: &Path) -> Result<(), TantivySourceError> {
    let lease = open_root_lease(path)?;
    lease.lock_shared()?;
    source._root_lease = Some(lease);
    Ok(())
}

fn prune_durable_roots(
    root: &Path,
    selected: &Path,
    budget: DurableCacheBudget,
) -> Result<(), TantivySourceError> {
    struct Candidate {
        path: std::path::PathBuf,
        last_used: u128,
        bytes: u64,
        selected: bool,
        retained: bool,
    }

    let mut candidates = Vec::new();
    let mut pinned_bytes = 0_u64;
    let mut entries_seen = 0_usize;
    for entry in fs::read_dir(root)? {
        entries_seen = entries_seen.saturating_add(1);
        if entries_seen > MAX_DURABLE_ROOT_SCAN_ENTRIES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable root directory exceeds its entry bound",
            )
            .into());
        }
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid durable root name")
        })?;
        if !name.bytes().all(|byte| byte.is_ascii_hexdigit()) || name.len() != 64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable root directory contains an unrecognized entry",
            )
            .into());
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_dir() {
            remove_projection_path(&entry.path())?;
            continue;
        }
        let bytes = durable_root_size(&entry.path())?;
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        let last_used = durable_root_last_used(&entry.path(), modified)?;
        if entry.path() == selected {
            candidates.push(Candidate {
                path: entry.path(),
                last_used,
                bytes,
                selected: true,
                retained: true,
            });
            continue;
        }
        let lease = open_root_lease(&entry.path())?;
        match lease.try_lock() {
            Ok(()) => candidates.push(Candidate {
                path: entry.path(),
                last_used,
                bytes,
                selected: false,
                retained: false,
            }),
            Err(std::fs::TryLockError::WouldBlock) => {
                pinned_bytes = pinned_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| std::io::Error::other("durable cache size overflow"))?;
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    candidates.sort_by(|left, right| {
        right
            .selected
            .cmp(&left.selected)
            .then_with(|| right.last_used.cmp(&left.last_used))
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut retained_bytes = pinned_bytes;
    let mut retained_roots = 0_usize;
    for candidate in &mut candidates {
        if candidate.selected {
            retained_roots = retained_roots.saturating_add(1);
            retained_bytes = retained_bytes
                .checked_add(candidate.bytes)
                .ok_or_else(|| std::io::Error::other("durable cache size overflow"))?;
            continue;
        }
        if retained_roots < MAX_RETAINED_DURABLE_ROOTS
            && retained_bytes
                .checked_add(candidate.bytes)
                .is_some_and(|required| required <= budget.max_bytes())
        {
            candidate.retained = true;
            retained_roots = retained_roots.saturating_add(1);
            retained_bytes = retained_bytes
                .checked_add(candidate.bytes)
                .ok_or_else(|| std::io::Error::other("durable cache size overflow"))?;
        }
    }
    for candidate in candidates.iter().filter(|candidate| !candidate.retained) {
        match remove_unpinned_projection_root(&candidate.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                // A reader may have pinned the root after classification. Keep
                // it; reader leases are outside the root-count limit but remain
                // inside the total byte quota.
                retained_bytes = retained_bytes
                    .checked_add(candidate.bytes)
                    .ok_or_else(|| std::io::Error::other("durable cache size overflow"))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    if retained_bytes > budget.max_bytes() {
        return Err(TantivySourceError::BudgetExceeded {
            budget_bytes: budget.max_bytes(),
            required_bytes: retained_bytes,
        });
    }
    sync_directory(root)?;
    Ok(())
}

fn open_root_lease(root: &Path) -> Result<File, std::io::Error> {
    backend_platform::durability::open_or_create_regular_file_nofollow(
        &root.join(DURABLE_ROOT_LEASE),
    )
}

fn durable_root_last_used(
    root: &Path,
    fallback: std::time::SystemTime,
) -> Result<u128, std::io::Error> {
    let path = root.join(".last-used");
    let mut file = match backend_platform::durability::open_regular_file_nofollow(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(fallback
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos());
        }
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() != 16 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable root last-used stamp has an invalid size",
        ));
    }
    let mut bytes = [0_u8; 16];
    file.read_exact(&mut bytes)?;
    Ok(u128::from_le_bytes(bytes))
}

fn remove_unpinned_projection_root(path: &Path) -> Result<(), std::io::Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_dir() {
        return remove_projection_path(path);
    }
    let lease = open_root_lease(path)?;
    match lease.try_lock() {
        Ok(()) => remove_projection_path(path),
        Err(std::fs::TryLockError::WouldBlock) => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "selected Tantivy root is held by an active reader",
        )),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

fn durable_root_size(root: &Path) -> Result<u64, std::io::Error> {
    let mut total = 0_u64;
    let mut count = 0_usize;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid filename")
        })?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable root contains a directory, symlink, or special file",
            ));
        }
        if !is_volatile_projection_file(&name) && !is_projection_file_name(&name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable root contains an unrecognized file",
            ));
        }
        count = count.saturating_add(1);
        if count > MAX_PROJECTION_FILES.saturating_add(8) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable root contains too many files",
            ));
        }
        total = total
            .checked_add(metadata.len())
            .ok_or_else(|| std::io::Error::other("durable root size overflow"))?;
    }
    Ok(total)
}

fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    backend_platform::durability::open_directory_nofollow(path)?.sync_all()
}

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) const BINDING_FILE: &str = super::BINDING_FILE;
    pub(crate) const DURABLE_ROOTS_DIRECTORY: &str = super::DURABLE_ROOTS_DIRECTORY;
    pub(crate) const INTEGRITY_FILE: &str = super::INTEGRITY_FILE;
    pub(crate) const MAX_RETAINED_DURABLE_ROOTS: usize = super::MAX_RETAINED_DURABLE_ROOTS;
    pub(crate) const MAX_PROJECTION_MANIFEST_BYTES: u64 = super::MAX_PROJECTION_MANIFEST_BYTES;
    pub(crate) const MAX_ORDINAL_MAP_BYTES: u64 = super::MAX_ORDINAL_MAP_BYTES;
    pub(crate) const ORDINAL_MAP_FILE: &str = super::ORDINAL_MAP_FILE;
    pub(crate) const ORDINAL_MAP_MAGIC: &[u8] = super::ORDINAL_MAP_MAGIC;

    pub(crate) fn hex_fingerprint(fingerprint: [u8; 32]) -> String {
        super::hex_fingerprint(fingerprint)
    }

    pub(crate) fn projection_fingerprint(binding: crate::Binding) -> [u8; 32] {
        super::projection_fingerprint(binding)
    }

    pub(crate) fn ordinal_map_capacity(
        live_count: usize,
    ) -> Result<usize, super::TantivySourceError> {
        super::preflight_ordinal_map_capacity(live_count)
    }

    pub(crate) fn write_projection_manifest(
        directory: &std::path::Path,
        fingerprint: [u8; 32],
        budget: super::DurableCacheBudget,
    ) -> Result<(), super::TantivySourceError> {
        super::write_projection_manifest(directory, fingerprint, budget)
    }

    pub(crate) fn rank_cache_bytes(
        adapter: &super::TantivyAdapter,
    ) -> Result<usize, super::TantivySourceError> {
        let guard = adapter
            .source()
            .rank_cache
            .lock()
            .map_err(|_| super::TantivySource::corrupt("rank cache lock poisoned"))?;
        Ok(guard.as_ref().map_or(0, |rank| rank.ranks.byte_len()))
    }

    pub(crate) fn ordinal_residency(source: &super::TantivySource) -> (u32, usize, usize) {
        let live_bytes = source
            .documents
            .live
            .capacity()
            .saturating_mul(std::mem::size_of::<super::OrdinalDocument>());
        let identity_bytes = source
            .identity_ordinals
            .capacity()
            .saturating_mul(std::mem::size_of::<super::IdentityOrdinal>());
        (
            source.documents.slot_count,
            source.documents.live.len(),
            live_bytes.saturating_add(identity_bytes),
        )
    }

    pub(crate) fn write_sparse_durable_fixture(
        state: &super::DocumentState,
        directory: &std::path::Path,
        slot_count: u32,
        ordinal: u32,
    ) -> Result<(), super::TantivySourceError> {
        let mut rows = state.iter();
        let Some((id, fields)) = rows.next() else {
            return Err(super::Error::MalformedInput.into());
        };
        if rows.next().is_some() || ordinal >= slot_count {
            return Err(super::Error::MalformedInput.into());
        }
        let projected = super::projection_schema();
        let index = super::Index::create_in_dir(directory, projected.schema)?;
        let writer = index.writer(super::WRITER_MEMORY_BYTES)?;
        let postings = super::write_fields(&writer, &projected.fields, u64::from(ordinal), fields)?;
        writer.commit()?;
        writer.wait_merging_threads()?;
        let fingerprint = super::projection_fingerprint(state.binding());
        let documents = super::DocumentTable::from_live(
            slot_count,
            vec![super::OrdinalDocument {
                ordinal,
                document: super::LiveDocument {
                    id,
                    fields_digest: super::document_fields_digest(fields),
                    postings,
                },
            }],
        )?;
        super::write_ordinal_map(directory, fingerprint, &documents)?;
        super::write_binding_stamp(directory, fingerprint)?;
        super::write_projection_manifest(
            directory,
            fingerprint,
            super::DurableCacheBudget::default(),
        )?;
        Ok(())
    }

    pub(crate) fn prune_durable_roots_for_test(
        root: &std::path::Path,
        selected: &std::path::Path,
        budget: super::DurableCacheBudget,
    ) -> Result<(), super::TantivySourceError> {
        super::prune_durable_roots(root, selected, budget)
    }

    pub(crate) fn pin_durable_root_for_test(
        root: &std::path::Path,
    ) -> Result<std::fs::File, std::io::Error> {
        let lease = super::open_root_lease(root)?;
        lease.lock_shared()?;
        Ok(lease)
    }

    pub(crate) fn durable_root_bytes_for_test(
        root: &std::path::Path,
    ) -> Result<u64, std::io::Error> {
        super::durable_root_size(root)
    }
}

impl LexicalSource for TantivySource {
    type Error = TantivySourceError;

    fn fetch(&self, request: &QueryRequest) -> Result<LexicalPage, Self::Error> {
        self.ensure_live()?;
        if request.binding != self.binding {
            return Err(Error::StaleRoot.into());
        }
        if request.limit == 0 || request.limit > self.limits.max_page {
            return Err(Error::SizeLimit.into());
        }
        if let Some(cursor) = request.cursor
            && (cursor.binding() != self.binding || cursor.query() != request.query.version)
        {
            return Err(Error::StaleCursor.into());
        }
        request.query.validate(self.limits)?;
        if request
            .cursor
            .is_some_and(|cursor| cursor.after_hit().is_none())
        {
            return Err(Error::InvalidCursor.into());
        }
        self.with_ranked_snapshot(&request.query, |snapshot| {
            let offset = request.cursor.map_or(0, Cursor::offset);
            let after = request.cursor.and_then(Cursor::after_hit);
            if offset > snapshot.total {
                return Err(Error::InvalidCursor.into());
            }
            match after {
                Some(after) => {
                    if offset == 0 {
                        return Err(Error::InvalidCursor.into());
                    }
                    let (ordinal, relevance) = snapshot
                        .ranks
                        .ranked_at(offset - 1)?
                        .ok_or(Error::InvalidCursor)?;
                    let selected = self
                        .documents
                        .get(ordinal)
                        .ok_or_else(|| Self::corrupt("rank ordinal is outside the binding"))?;
                    let boundary = RankedHit {
                        document: selected.id,
                        relevance,
                    };
                    if boundary != after {
                        return Err(Error::InvalidCursor.into());
                    }
                }
                None if offset != 0 => return Err(Error::InvalidCursor.into()),
                None => {}
            }
            let end = offset
                .checked_add(request.limit)
                .ok_or(Error::SizeLimit)?
                .min(snapshot.total);
            let mut hits = Vec::with_capacity(end.saturating_sub(offset));
            for position in offset..end {
                let (ordinal, relevance) = snapshot
                    .ranks
                    .ranked_at(position)?
                    .ok_or_else(|| Self::corrupt("rank ordering is incomplete"))?;
                let selected = self
                    .documents
                    .get(ordinal)
                    .ok_or_else(|| Self::corrupt("rank ordinal is outside the binding"))?;
                hits.push(RankedHit {
                    document: selected.id,
                    relevance,
                });
            }
            let next = if end < snapshot.total {
                let after = hits.last().copied().ok_or(Error::InvalidCursor)?;
                Some(Cursor::after(
                    self.binding,
                    request.query.version,
                    end,
                    after,
                ))
            } else {
                None
            };
            Ok(LexicalPage {
                schema: SchemaVersion::CURRENT,
                binding: self.binding,
                query: request.query.version,
                hits,
                next,
                total: snapshot.total,
                coverage: self.coverage,
            })
        })
    }
}

struct RevisionPlan {
    rewritten_documents: usize,
    retired_postings: u64,
    deletes: Vec<u64>,
    writes: Vec<PlannedWrite>,
    documents: DocumentTable,
}

struct PlannedWrite {
    ordinal: u64,
    id: EntityId,
    fields_digest: [u8; 32],
    fields: Vec<(String, String)>,
}

fn document_fields_digest(fields: &[(String, String)]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-extension-tantivy/document-fields/v1");
    hash_len(&mut hasher, fields.len());
    for (field, text) in fields {
        hash_len(&mut hasher, field.len());
        hasher.update(field.as_bytes());
        hash_len(&mut hasher, text.len());
        hasher.update(text.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

fn hash_len(hasher: &mut blake3::Hasher, len: usize) {
    match u64::try_from(len) {
        Ok(len) => {
            hasher.update(&len.to_le_bytes());
        }
        Err(_) => {
            hasher.update(&u64::MAX.to_le_bytes());
        }
    }
}

fn posting_count(fields: &[(String, String)]) -> Result<u32, Error> {
    let mut postings = 0u32;
    for (_, text) in fields {
        let count = u32::try_from(searchable_tokens(text).len()).map_err(|_| Error::SizeLimit)?;
        postings = postings.checked_add(count).ok_or(Error::SizeLimit)?;
    }
    Ok(postings)
}

fn write_fields(
    writer: &tantivy::IndexWriter,
    fields: &ProjectionFields,
    document_ordinal: u64,
    document_fields: &[(String, String)],
) -> Result<u32, TantivySourceError> {
    let mut postings = 0u32;
    for (field, text) in document_fields {
        let weight = u64::from(field_weight(field));
        for token in searchable_tokens(text) {
            postings = postings.checked_add(1).ok_or(Error::SizeLimit)?;
            let ranking_bytes = u64::try_from(token.ranking.len()).map_err(|_| Error::SizeLimit)?;
            writer.add_document(doc!(
                fields.raw_token => token.searchable.as_str(),
                fields.folded_token => token.searchable.to_ascii_lowercase(),
                fields.ranking_token => token.ranking.as_str(),
                fields.field_name => field.as_str(),
                fields.ordinal => document_ordinal,
                fields.rank_weight => weight,
                fields.rank_bytes => ranking_bytes,
            ))?;
        }
    }
    Ok(postings)
}

const ORDINAL_FIELD: &str = "document_ordinal";
const WEIGHT_FIELD: &str = "rank_weight";
const RANK_BYTES_FIELD: &str = "rank_bytes";

fn ensure_rank_scratch_capacity(
    row_count: usize,
    budget_bytes: usize,
) -> Result<(), TantivySourceError> {
    let required_bytes = row_count
        .checked_mul(RANK_SCRATCH_BYTES_PER_MATCH)
        .ok_or(Error::SizeLimit)?;
    if required_bytes > budget_bytes {
        return Err(TantivySourceError::RankSnapshotBudgetExceeded {
            budget_bytes,
            required_bytes,
        });
    }
    Ok(())
}

enum ClauseRankError {
    Contract(Error),
    Capacity,
}

struct ClauseRankCollector {
    term_bytes: usize,
    max_rows: usize,
    admitted_rows: Arc<std::sync::atomic::AtomicUsize>,
}

struct ClauseRankSegment {
    term_bytes: usize,
    ordinals: Arc<dyn ColumnValues<u64>>,
    weights: Arc<dyn ColumnValues<u64>>,
    ranking_bytes: Arc<dyn ColumnValues<u64>>,
    best: BTreeMap<u64, Relevance>,
    error: Option<Error>,
    capacity_exceeded: bool,
    max_rows: usize,
    admitted_rows: Arc<std::sync::atomic::AtomicUsize>,
}

impl SegmentCollector for ClauseRankSegment {
    type Fruit = Result<BTreeMap<u64, Relevance>, ClauseRankError>;

    fn collect(&mut self, doc: DocId, _score: Score) {
        if self.error.is_some() || self.capacity_exceeded {
            return;
        }
        let ordinal = self.ordinals.get_val(doc);
        let weight = self.weights.get_val(doc);
        let ranking_bytes = self.ranking_bytes.get_val(doc);
        if weight == 0 || ranking_bytes == 0 {
            self.error = Some(Error::MalformedInput);
            return;
        }
        let Ok(weight) = u16::try_from(weight) else {
            self.error = Some(Error::SizeLimit);
            return;
        };
        let Ok(ranking_bytes) = usize::try_from(ranking_bytes) else {
            self.error = Some(Error::SizeLimit);
            return;
        };
        let relevance = match Relevance::new(self.term_bytes, ranking_bytes, weight, 1) {
            Ok(relevance) => relevance,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.best
            .entry(ordinal)
            .and_modify(|current| *current = (*current).max(relevance));
        if !self.best.contains_key(&ordinal) {
            let admitted =
                self.admitted_rows
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                        (current < self.max_rows).then_some(current.saturating_add(1))
                    });
            if admitted.is_err() {
                self.capacity_exceeded = true;
                return;
            }
            self.best.insert(ordinal, relevance);
        }
    }

    fn harvest(self) -> Self::Fruit {
        if self.capacity_exceeded {
            return Err(ClauseRankError::Capacity);
        }
        match self.error {
            Some(error) => Err(ClauseRankError::Contract(error)),
            None => Ok(self.best),
        }
    }
}

impl Collector for ClauseRankCollector {
    type Fruit = Result<BTreeMap<u64, Relevance>, ClauseRankError>;
    type Child = ClauseRankSegment;

    fn for_segment(
        &self,
        _segment_local_id: tantivy::SegmentOrdinal,
        segment: &tantivy::SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(ClauseRankSegment {
            term_bytes: self.term_bytes,
            ordinals: fast_column(segment, ORDINAL_FIELD)?,
            weights: fast_column(segment, WEIGHT_FIELD)?,
            ranking_bytes: fast_column(segment, RANK_BYTES_FIELD)?,
            best: BTreeMap::new(),
            error: None,
            capacity_exceeded: false,
            max_rows: self.max_rows,
            admitted_rows: Arc::clone(&self.admitted_rows),
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<<Self::Child as SegmentCollector>::Fruit>,
    ) -> tantivy::Result<Self::Fruit> {
        let mut merged: BTreeMap<u64, Relevance> = BTreeMap::new();
        for fruit in segment_fruits {
            let segment = match fruit {
                Ok(segment) => segment,
                Err(error) => return Ok(Err(error)),
            };
            for (ordinal, relevance) in segment {
                merged
                    .entry(ordinal)
                    .and_modify(|current: &mut Relevance| *current = (*current).max(relevance))
                    .or_insert(relevance);
            }
        }
        Ok(Ok(merged))
    }
}

fn fast_column(
    segment: &tantivy::SegmentReader,
    name: &str,
) -> tantivy::Result<Arc<dyn ColumnValues<u64>>> {
    let column = segment
        .fast_fields()
        .u64_lenient(name)?
        .ok_or_else(|| tantivy::TantivyError::FieldNotFound(name.to_owned()))?;
    Ok(column.0.first_or_default_col(0))
}

impl crate::Adapter<TantivySource> {
    /// Absorbs a complete document snapshot into the resident Tantivy index.
    ///
    /// # Errors
    ///
    /// Returns the source failure. The resident projection stays on its
    /// previous binding when maintenance is refused or fails before commit.
    pub fn maintain(
        &mut self,
        next: &DocumentState,
        budget: OverlayLimits,
    ) -> Result<MaintainOutcome, TantivySourceError> {
        self.source_mut().maintain(next, budget)
    }

    /// Returns token postings visible to the resident reader.
    #[must_use]
    pub fn indexed_postings(&self) -> u64 {
        self.source().indexed_postings()
    }

    /// Counts full ranking passes caused by a page miss on this adapter.
    #[must_use]
    pub fn rank_evaluations(&self) -> u64 {
        self.source().rank_evaluations()
    }

    /// Visits every exact lexical hit while retaining only the source's
    /// compact ordinal rank snapshot and caller-owned bounded state.
    ///
    /// # Errors
    ///
    /// Returns the typed Tantivy or selected-projection failure.
    pub fn for_each_ranked_hit(
        &self,
        query: &Query,
        visit: impl FnMut(RankedHit),
    ) -> Result<usize, TantivySourceError> {
        self.source().for_each_ranked_hit(query, visit)
    }

    /// Looks up exact relevance for a bounded candidate identity set without
    /// walking paged lexical results.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SizeLimit`] when candidates exceed the query page
    /// bound, and a typed query or selected-projection failure otherwise.
    pub fn relevance_for_candidates(
        &self,
        query: &Query,
        candidates: &[EntityId],
    ) -> Result<BTreeMap<EntityId, Relevance>, TantivySourceError> {
        self.source().relevance_for_candidates(query, candidates)
    }

    /// Drops the retained rank so the next page computes it again.
    ///
    /// # Errors
    ///
    /// Returns the source failure when the rank lock is poisoned.
    pub fn clear_rank_cache(&self) -> Result<(), TantivySourceError> {
        self.source().clear_rank_cache()
    }
}

fn field_weight(field: &str) -> u16 {
    match field {
        "name" => 4,
        "signature" => 3,
        "documentation" => 2,
        _ => 1,
    }
}

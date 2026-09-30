//! Real Tantivy-backed lexical source with canonical ranking at the adapter boundary.

use crate::{
    Binding, Cursor, DocumentState, Error, FieldSelection, LexicalPage, LexicalSource, Limits,
    MatchMode, OverlayLimits, Query, QueryRequest, RankedHit, Relevance, SchemaVersion,
    compare_ranked_hits,
};
use backend_semantic::EntityId;
use backend_version::CoverageWitness;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    fs::File,
    io::Read,
    path::Path,
    sync::atomic::Ordering as AtomicOrdering,
};
use tantivy::columnar::BytesColumn;
use tantivy::{
    DocAddress, DocId, DocSet, Index, IndexReader, ReloadPolicy, TERMINATED, TantivyDocument, Term,
    query::{
        AllQuery, BooleanQuery, EnableScoring, FuzzyTermQuery, Occur, Query as TantivyQuery,
        Scorer, TermQuery,
    },
    schema::{BytesOptions, FAST, Field, INDEXED, IndexRecordOption, STRING, Schema},
};

const WRITER_MEMORY_BYTES: usize = 15_000_000;
const BINDING_FILE: &str = "backend-binding-v3";
const INTEGRITY_FILE: &str = "backend-files-v3";
const ORDINAL_MAP_FILE: &str = "backend-ordinals-v1";
const INTEGRITY_MAGIC: &[u8] = b"backend-tantivy-files-v3\0";
const ORDINAL_MAP_MAGIC: &[u8] = b"backend-tantivy-ordinals-v1\0";
const DURABLE_ROOTS_DIRECTORY: &str = "v3";
const DURABLE_ROOT_LEASE: &str = ".backend-root-reader.lock";
const MAX_RETAINED_DURABLE_ROOTS: usize = 4;
const DEFAULT_DURABLE_CACHE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_PROJECTION_FILES: usize = 65_536;
const MAX_PROJECTION_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ORDINAL_MAP_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ORDINAL_SLOTS: usize = 4_000_000;
const ORDINAL_MAP_RECORD_BYTES: usize = 8 + 32 + 32 + 4 + 32;
const RANK_MATERIAL_MAGIC: &[u8] = b"tantivy-rank-v1\0";
const RANK_MATERIAL_FIELD: &str = "rank_material";
const RANK_MATERIAL_LENGTH_FIELD: &str = "rank_material_bytes";
const FIELD_RAW_TOKEN: &str = "field_raw_token";
const FIELD_FOLDED_TOKEN: &str = "field_folded_token";
const MAX_RANK_MATERIAL_BYTES: usize = 64 * 1024 * 1024;
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

/// Per-source bounds for exact lexical query scratch and explicit
/// all-results materialization.
///
/// Page queries retain only one row payload and their bounded top page. The
/// retained allowance applies to [`TantivySource::search`], which explicitly
/// returns every exact hit. Independently live sources have separate limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankSnapshotBudget {
    max_scratch_bytes: usize,
    max_retained_bytes: usize,
}

impl RankSnapshotBudget {
    /// Creates nonzero scratch and all-results allowances.
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

    /// Returns the exact-query page scratch allowance.
    #[must_use]
    pub const fn max_scratch_bytes(self) -> usize {
        self.max_scratch_bytes
    }

    /// Returns the all-results search allowance.
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
    address: Option<DocAddress>,
    segment_id: Option<tantivy::SegmentId>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BindingWork {
    payload_rows_scanned: usize,
    source_posting_checks: usize,
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
        self.get_entry(ordinal).map(|entry| &entry.document)
    }

    fn get_entry(&self, ordinal: usize) -> Option<&OrdinalDocument> {
        let ordinal = u32::try_from(ordinal).ok()?;
        self.live
            .binary_search_by_key(&ordinal, |entry| entry.ordinal)
            .ok()
            .map(|index| &self.live[index])
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
    field_raw_token: Field,
    field_folded_token: Field,
    ordinal: Field,
    rank_material: Field,
    rank_material_len: Field,
    documents: DocumentTable,
    identity_ordinals: Vec<IdentityOrdinal>,
    durable: bool,
    _root_lease: Option<File>,
    poisoned: bool,
    rank_budget: RankSnapshotBudget,
    rank_evaluations: AtomicU64,
    rank_docs_visited: AtomicU64,
    rank_peak_scratch_bytes: AtomicU64,
    #[cfg(test)]
    last_binding_work: BindingWork,
}

struct TopHits {
    hits: Vec<RankedHit>,
    limit: usize,
}

impl TopHits {
    fn new(limit: usize) -> Result<Self, TantivySourceError> {
        let mut hits = Vec::new();
        hits.try_reserve_exact(limit)
            .map_err(|_| Error::SizeLimit)?;
        Ok(Self { hits, limit })
    }

    fn consider(&mut self, hit: RankedHit) {
        if self.hits.len() < self.limit {
            self.hits.push(hit);
            let mut child = self.hits.len() - 1;
            while child > 0 {
                let parent = (child - 1) / 2;
                if !compare_ranked_hits(self.hits[parent], self.hits[child]).is_lt() {
                    break;
                }
                self.hits.swap(parent, child);
                child = parent;
            }
            return;
        }
        if self
            .hits
            .first()
            .is_some_and(|worst| compare_ranked_hits(hit, *worst).is_lt())
        {
            self.hits[0] = hit;
            let mut parent = 0;
            loop {
                let left = parent * 2 + 1;
                if left >= self.hits.len() {
                    break;
                }
                let right = left + 1;
                let worse_child = if right < self.hits.len()
                    && compare_ranked_hits(self.hits[left], self.hits[right]).is_lt()
                {
                    right
                } else {
                    left
                };
                if !compare_ranked_hits(self.hits[parent], self.hits[worse_child]).is_lt() {
                    break;
                }
                self.hits.swap(parent, worse_child);
                parent = worse_child;
            }
        }
    }

    fn into_sorted(mut self) -> Vec<RankedHit> {
        self.hits
            .sort_unstable_by(|left, right| compare_ranked_hits(*left, *right));
        self.hits
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
    /// An exact query's bounded scratch or explicit all-results output
    /// exceeds this source's configured query-memory budget.
    RankSnapshotBudgetExceeded {
        /// Configured query-scratch or all-results allowance.
        budget_bytes: usize,
        /// Bytes required by the exact query snapshot or conservative build bound.
        required_bytes: usize,
    },
    /// Durable projections are immutable; updates must publish a new root.
    DurableProjectionImmutable,
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
                "exact lexical query needs {required_bytes} bytes, above its {budget_bytes}-byte query budget",
            ),
            Self::DurableProjectionImmutable => write!(
                formatter,
                "durable Tantivy projections are immutable; publish an updated root",
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
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let mut documents =
            read_ordinal_map(state, projection_fingerprint(state.binding()), directory)?;
        let binding_work =
            bind_document_addresses(&reader, &mut documents, limits, state, projected.fields)?;
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
            field_raw_token: fields.field_raw_token,
            field_folded_token: fields.field_folded_token,
            ordinal: fields.ordinal,
            rank_material: fields.rank_material,
            rank_material_len: fields.rank_material_len,
            documents,
            identity_ordinals,
            durable: true,
            _root_lease: None,
            poisoned: false,
            rank_budget: RankSnapshotBudget::default(),
            rank_evaluations: AtomicU64::new(0),
            rank_docs_visited: AtomicU64::new(0),
            rank_peak_scratch_bytes: AtomicU64::new(0),
            #[cfg(test)]
            last_binding_work: binding_work,
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
        let mut source = Self::populate(state, limits, index, projected.fields)?;
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
        source.durable = true;
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
        let revision = match staged.maintain_for_publication(next, budget) {
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
            let postings = write_document(
                &writer,
                &fields,
                document_ordinal,
                document,
                document_fields,
                rank_material_limit(limits),
            )?;
            live.push(OrdinalDocument {
                ordinal: u32::try_from(document_ordinal).map_err(|_| Error::SizeLimit)?,
                document: LiveDocument {
                    id: document,
                    fields_digest: document_fields_digest(document_fields),
                    postings,
                },
                address: None,
                segment_id: None,
            });
        }
        let mut documents = DocumentTable::from_live(
            u32::try_from(live.len()).map_err(|_| Error::SizeLimit)?,
            live,
        )?;
        writer.commit()?;
        // Join background merges so no thread is still rewriting the index
        // directory once the source is handed out.
        writer.wait_merging_threads()?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let binding_work = bind_document_addresses(&reader, &mut documents, limits, state, fields)?;
        let identity_ordinals = documents.identity_index();
        Ok(Self {
            binding: state.binding(),
            coverage: state.coverage(),
            limits,
            _index: index,
            reader,
            raw_token: fields.raw_token,
            folded_token: fields.folded_token,
            field_raw_token: fields.field_raw_token,
            field_folded_token: fields.field_folded_token,
            ordinal: fields.ordinal,
            rank_material: fields.rank_material,
            rank_material_len: fields.rank_material_len,
            documents,
            identity_ordinals,
            durable: false,
            _root_lease: None,
            poisoned: false,
            rank_budget: RankSnapshotBudget::default(),
            rank_evaluations: AtomicU64::new(0),
            rank_docs_visited: AtomicU64::new(0),
            rank_peak_scratch_bytes: AtomicU64::new(0),
            #[cfg(test)]
            last_binding_work: binding_work,
        })
    }

    /// Returns the exact token-posting count represented by live entity rows.
    #[must_use]
    pub fn indexed_postings(&self) -> u64 {
        self.documents.live.iter().fold(0_u64, |total, entry| {
            total.saturating_add(u64::from(entry.document.postings))
        })
    }

    /// Replaces the per-source page-scratch and all-results memory budget.
    #[must_use]
    pub fn with_rank_snapshot_budget(mut self, budget: RankSnapshotBudget) -> Self {
        self.rank_budget = budget;
        self
    }

    /// Counts complete exact Tantivy scans performed for query answers.
    #[must_use]
    pub fn rank_evaluations(&self) -> u64 {
        self.rank_evaluations.load(Ordering::Relaxed)
    }

    /// Counts matched Tantivy row documents scored across exact query scans.
    #[must_use]
    pub fn rank_docs_visited(&self) -> u64 {
        self.rank_docs_visited.load(Ordering::Relaxed)
    }

    /// Highest source-owned query scratch observed across completed scans.
    #[must_use]
    pub fn rank_peak_scratch_bytes(&self) -> u64 {
        self.rank_peak_scratch_bytes.load(Ordering::Relaxed)
    }

    /// Retained rank snapshots are no longer used; this compatibility method
    /// is intentionally a no-op.
    ///
    /// # Errors
    ///
    /// Retained rank snapshots are not used by this source.
    pub fn clear_rank_cache(&self) -> Result<(), TantivySourceError> {
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
    /// the resident binding and ordinal map unchanged. Durable sources return
    /// [`TantivySourceError::DurableProjectionImmutable`]; update those through
    /// [`Self::open_or_advance_in_dir`] so a new root is published atomically.
    pub fn maintain(
        &mut self,
        next: &DocumentState,
        budget: OverlayLimits,
    ) -> Result<MaintainOutcome, TantivySourceError> {
        if self.durable {
            return Err(TantivySourceError::DurableProjectionImmutable);
        }
        self.maintain_projection(next, budget)
    }

    fn maintain_for_publication(
        &mut self,
        next: &DocumentState,
        budget: OverlayLimits,
    ) -> Result<MaintainOutcome, TantivySourceError> {
        if !self.durable || self._root_lease.is_some() {
            return Err(Self::corrupt(
                "durable publication must mutate an unpinned staging projection",
            ));
        }
        self.maintain_projection(next, budget)
    }

    fn maintain_projection(
        &mut self,
        next: &DocumentState,
        budget: OverlayLimits,
    ) -> Result<MaintainOutcome, TantivySourceError> {
        self.ensure_live()?;
        let Some(plan) = self.plan_revision(next, budget)? else {
            return Ok(MaintainOutcome::RebuildRequired);
        };
        match plan {
            RevisionPlan::Rebound => {
                self.binding = next.binding();
                self.coverage = next.coverage();
                #[cfg(test)]
                {
                    self.last_binding_work = BindingWork::default();
                }
                Ok(MaintainOutcome::Applied(ProjectionRevision {
                    kind: ProjectionKind::Rebound,
                    rewritten_documents: 0,
                    retired_postings: 0,
                    added_postings: 0,
                }))
            }
            RevisionPlan::Changed(plan) => self.commit_revision(next, plan),
        }
    }

    fn plan_revision<'next>(
        &self,
        next: &'next DocumentState,
        budget: OverlayLimits,
    ) -> Result<Option<RevisionPlan<'next>>, TantivySourceError> {
        let budget = budget.validate()?;
        if !matches!(next.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        if next.binding().workspace != self.binding.workspace {
            return Ok(None);
        }
        let live_count = self.documents.live.len();
        let bitmap_words = live_count.checked_add(63).ok_or(Error::SizeLimit)? / 64;
        let mut present = Vec::new();
        present
            .try_reserve_exact(bitmap_words)
            .map_err(|_| Error::SizeLimit)?;
        present.resize(bitmap_words, 0_u64);
        let mut rewritten_slots = Vec::new();
        rewritten_slots
            .try_reserve_exact(bitmap_words)
            .map_err(|_| Error::SizeLimit)?;
        rewritten_slots.resize(bitmap_words, 0_u64);
        let mut rewritten = Vec::new();
        let mut added = Vec::new();
        let mut slot_count = self.documents.slot_count;
        let mut next_document_count = 0usize;
        let mut term_count = 0usize;
        for (id, fields) in next.iter() {
            next_document_count = next_document_count.checked_add(1).ok_or(Error::SizeLimit)?;
            if let Some(ordinal) = self
                .documents
                .ordinal_for_entity(&self.identity_ordinals, id)
            {
                let ordinal = u32::try_from(ordinal).map_err(|_| Error::SizeLimit)?;
                let index = self
                    .documents
                    .live
                    .binary_search_by_key(&ordinal, |entry| entry.ordinal)
                    .map_err(|_| Self::corrupt("selected identity ordinal is missing"))?;
                present[index / 64] |= 1_u64 << (index % 64);
                let current = &self.documents.live[index].document;
                if document_fields_digest(fields) != current.fields_digest {
                    if rewritten.len().saturating_add(added.len()) == budget.max_changed_documents {
                        return Ok(None);
                    }
                    term_count = term_count
                        .checked_add(
                            usize::try_from(posting_count(fields)?)
                                .map_err(|_| Error::SizeLimit)?,
                        )
                        .ok_or(Error::SizeLimit)?;
                    if term_count > budget.max_terms {
                        return Ok(None);
                    }
                    rewritten.try_reserve(1).map_err(|_| Error::SizeLimit)?;
                    rewritten.push((id, ordinal));
                    rewritten_slots[index / 64] |= 1_u64 << (index % 64);
                }
            } else {
                if rewritten.len().saturating_add(added.len()) == budget.max_changed_documents {
                    return Ok(None);
                }
                term_count = term_count
                    .checked_add(
                        usize::try_from(posting_count(fields)?).map_err(|_| Error::SizeLimit)?,
                    )
                    .ok_or(Error::SizeLimit)?;
                if term_count > budget.max_terms {
                    return Ok(None);
                }
                added.try_reserve(1).map_err(|_| Error::SizeLimit)?;
                added.push((id, slot_count));
                slot_count = slot_count.checked_add(1).ok_or(Error::SizeLimit)?;
            }
        }
        let mut removed_ordinals = Vec::new();
        let mut retired_postings = 0_u64;
        let mut removed_documents = 0usize;
        for (index, entry) in self.documents.live.iter().enumerate() {
            let is_present = present[index / 64] & (1_u64 << (index % 64)) != 0;
            let was_rewritten = rewritten_slots[index / 64] & (1_u64 << (index % 64)) != 0;
            if is_present && !was_rewritten {
                continue;
            }
            if !is_present {
                removed_documents = removed_documents.checked_add(1).ok_or(Error::SizeLimit)?;
                if rewritten
                    .len()
                    .saturating_add(added.len())
                    .saturating_add(removed_documents)
                    > budget.max_changed_documents
                {
                    return Ok(None);
                }
            }
            removed_ordinals
                .try_reserve(1)
                .map_err(|_| Error::SizeLimit)?;
            removed_ordinals.push(entry.ordinal);
            retired_postings = retired_postings
                .checked_add(u64::from(entry.document.postings))
                .ok_or(Error::SizeLimit)?;
        }
        let rewritten_documents = rewritten
            .len()
            .saturating_add(added.len())
            .saturating_add(removed_documents);
        if rewritten_documents == 0 {
            return Ok(Some(RevisionPlan::Rebound));
        }
        let resulting_slots = slot_count as usize;
        // Stable ordinals leave holes after deletion. Compact through a
        // complete rebuild before the sparse ordinal map grows without bound.
        if resulting_slots > MAX_ORDINAL_SLOTS
            || resulting_slots > next_document_count.saturating_mul(2).saturating_add(65_536)
        {
            return Ok(None);
        }
        let mut writes = Vec::new();
        writes
            .try_reserve_exact(rewritten.len().saturating_add(added.len()))
            .map_err(|_| Error::SizeLimit)?;
        for (id, ordinal) in rewritten {
            let Some(fields) = next.fields_for(id) else {
                return Err(Self::corrupt("revised document is missing its fields"));
            };
            writes.push(PlannedWrite {
                ordinal: u64::from(ordinal),
                id,
                fields_digest: document_fields_digest(fields),
                fields,
            });
        }
        for (id, ordinal) in added {
            let Some(fields) = next.fields_for(id) else {
                return Err(Self::corrupt("added document is missing its fields"));
            };
            writes.push(PlannedWrite {
                ordinal: u64::from(ordinal),
                id,
                fields_digest: document_fields_digest(fields),
                fields,
            });
        }
        Ok(Some(RevisionPlan::Changed(RevisionChanges {
            rewritten_documents,
            retired_postings,
            removed_ordinals,
            writes,
            slot_count,
        })))
    }

    fn commit_revision(
        &mut self,
        next: &DocumentState,
        plan: RevisionChanges<'_>,
    ) -> Result<MaintainOutcome, TantivySourceError> {
        let RevisionChanges {
            rewritten_documents,
            retired_postings,
            removed_ordinals,
            writes,
            slot_count,
        } = plan;
        let additional_live = writes.len().saturating_sub(removed_ordinals.len());
        self.documents
            .live
            .try_reserve_exact(additional_live)
            .map_err(|_| Error::SizeLimit)?;
        self.identity_ordinals
            .try_reserve_exact(additional_live)
            .map_err(|_| Error::SizeLimit)?;
        let mut replacements = Vec::new();
        replacements
            .try_reserve_exact(writes.len())
            .map_err(|_| Error::SizeLimit)?;
        let mut changed_ordinals = Vec::new();
        changed_ordinals
            .try_reserve_exact(writes.len())
            .map_err(|_| Error::SizeLimit)?;
        for write in &writes {
            changed_ordinals.push(u32::try_from(write.ordinal).map_err(|_| Error::SizeLimit)?);
        }
        changed_ordinals.sort_unstable();
        let old_searcher = self.reader.searcher();
        let mut old_segment_ids = HashSet::new();
        old_segment_ids
            .try_reserve(old_searcher.segment_readers().len())
            .map_err(|_| Error::SizeLimit)?;
        for segment in old_searcher.segment_readers() {
            if !old_segment_ids.insert(segment.segment_id()) {
                return Err(
                    Self::corrupt("resident Tantivy segment identity is duplicated").into(),
                );
            }
        }
        drop(old_searcher);

        let mut writer = self._index.writer(WRITER_MEMORY_BYTES)?;
        for ordinal in &removed_ordinals {
            let _opstamp =
                writer.delete_term(Term::from_field_u64(self.ordinal, u64::from(*ordinal)));
        }
        let mut added_postings = 0u64;
        let fields = ProjectionFields {
            raw_token: self.raw_token,
            folded_token: self.folded_token,
            field_raw_token: self.field_raw_token,
            field_folded_token: self.field_folded_token,
            ordinal: self.ordinal,
            rank_material: self.rank_material,
            rank_material_len: self.rank_material_len,
        };
        for write in writes {
            let postings = write_document(
                &writer,
                &fields,
                write.ordinal,
                write.id,
                write.fields,
                rank_material_limit(self.limits),
            )?;
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
                address: None,
                segment_id: None,
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
        let mut documents = std::mem::take(&mut self.documents);
        documents
            .live
            .retain(|entry| removed_ordinals.binary_search(&entry.ordinal).is_err());
        documents.slot_count = slot_count;
        documents.live.extend(replacements.iter().copied());
        documents.live.sort_unstable_by_key(|entry| entry.ordinal);
        let mut documents = match DocumentTable::from_live(slot_count, documents.live) {
            Ok(documents) => documents,
            Err(error) => {
                self.poisoned = true;
                return Err(error.into());
            }
        };
        let binding_work = match bind_resident_document_addresses(
            &self.reader,
            &mut documents,
            self.limits,
            next,
            fields,
            &old_segment_ids,
            &changed_ordinals,
        ) {
            Ok(work) => work,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        #[cfg(test)]
        {
            self.last_binding_work = binding_work;
        }
        self.identity_ordinals
            .retain(|identity| removed_ordinals.binary_search(&identity.ordinal).is_err());
        for replacement in &replacements {
            self.identity_ordinals.push(IdentityOrdinal {
                fingerprint: entity_fingerprint(replacement.document.id),
                ordinal: replacement.ordinal,
            });
        }
        self.identity_ordinals
            .sort_unstable_by_key(|entry| (entry.fingerprint, entry.ordinal));
        self.documents = documents;
        self.binding = next.binding();
        self.coverage = next.coverage();
        Ok(MaintainOutcome::Applied(ProjectionRevision {
            kind: ProjectionKind::Revised,
            rewritten_documents,
            retired_postings,
            added_postings,
        }))
    }

    /// Materializes every exact match in canonical rank order, subject to the
    /// explicitly configured all-results memory budget.
    ///
    /// # Errors
    ///
    /// Returns a typed query-admission, index-read, projection-integrity, or
    /// result-memory budget failure.
    pub fn search(&self, query: &Query) -> Result<Vec<RankedHit>, TantivySourceError> {
        self.ensure_live()?;
        query.validate(self.limits)?;
        preflight_query_scratch(query, 0, self.rank_budget.max_scratch_bytes)?;
        let max_hits = self
            .documents
            .live
            .len()
            .min(self.rank_budget.max_retained_bytes / std::mem::size_of::<RankedHit>());
        let mut hits = Vec::new();
        hits.try_reserve_exact(max_hits.min(64))
            .map_err(|_| Error::SizeLimit)?;
        self.scan_ranked_hits(query, 0, |hit| {
            if hits.len() == max_hits {
                return Err(TantivySourceError::RankSnapshotBudgetExceeded {
                    budget_bytes: self.rank_budget.max_retained_bytes,
                    required_bytes: hits
                        .len()
                        .saturating_add(1)
                        .saturating_mul(std::mem::size_of::<RankedHit>()),
                });
            }
            if hits.len() == hits.capacity() {
                let next_capacity = hits.capacity().max(8).saturating_mul(2).min(max_hits);
                hits.try_reserve_exact(next_capacity.saturating_sub(hits.len()))
                    .map_err(|_| Error::SizeLimit)?;
            }
            hits.push(hit);
            Ok(())
        })?;
        hits.sort_unstable_by(|left, right| compare_ranked_hits(*left, *right));
        Ok(hits)
    }

    /// Visits each exact match once in Tantivy's stable document traversal
    /// order. The scanner retains one row payload and no all-hit rank map.
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
        preflight_query_scratch(query, 0, self.rank_budget.max_scratch_bytes)?;
        let mut total = 0usize;
        self.scan_ranked_hits(query, 0, |hit| {
            visit(hit);
            total = total.checked_add(1).ok_or(Error::SizeLimit)?;
            Ok(())
        })?;
        Ok(total)
    }

    /// Resolves exact query relevance for a bounded candidate set by direct
    /// ordinal-to-document lookup, without scanning the query result set.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SizeLimit`] when the candidate set exceeds the query
    /// page bound, and a typed query or projection failure otherwise.
    pub fn relevance_for_candidates(
        &self,
        query: &Query,
        candidates: &[EntityId],
    ) -> Result<Vec<(EntityId, Relevance)>, TantivySourceError> {
        self.ensure_live()?;
        query.validate(self.limits)?;
        if candidates.len() > self.limits.max_page {
            return Err(Error::SizeLimit.into());
        }
        let clause_bytes = query
            .terms
            .len()
            .checked_mul(std::mem::size_of::<Option<Relevance>>())
            .ok_or(Error::SizeLimit)?;
        let result_bound_bytes = candidates
            .len()
            .checked_mul(std::mem::size_of::<(EntityId, Relevance)>())
            .ok_or(Error::SizeLimit)?;
        preflight_query_scratch(
            query,
            result_bound_bytes,
            self.rank_budget.max_scratch_bytes,
        )?;
        let searcher = self.reader.searcher();
        let mut relevance = Vec::new();
        relevance
            .try_reserve_exact(candidates.len())
            .map_err(|_| Error::SizeLimit)?;
        let result_capacity_bytes = relevance
            .capacity()
            .checked_mul(std::mem::size_of::<(EntityId, Relevance)>())
            .ok_or(Error::SizeLimit)?;
        let mut scratch = Vec::new();
        let mut best = Vec::new();
        best.try_reserve_exact(query.terms.len())
            .map_err(|_| Error::SizeLimit)?;
        let base_scratch_bytes = query_scratch_bytes(query, result_capacity_bytes)?;
        let actual_clause_bytes = best
            .capacity()
            .checked_mul(std::mem::size_of::<Option<Relevance>>())
            .ok_or(Error::SizeLimit)?;
        let base_scratch_bytes = base_scratch_bytes
            .checked_add(actual_clause_bytes.saturating_sub(clause_bytes))
            .ok_or(Error::SizeLimit)?;
        ensure_rank_scratch_capacity(base_scratch_bytes, self.rank_budget.max_scratch_bytes)?;
        self.record_rank_scratch(base_scratch_bytes);
        for candidate in candidates.iter().copied() {
            let Some(ordinal) = self
                .documents
                .ordinal_for_entity(&self.identity_ordinals, candidate)
            else {
                continue;
            };
            let Some(entry) = self.documents.live.get(
                self.documents
                    .live
                    .binary_search_by_key(&(ordinal as u32), |entry| entry.ordinal)
                    .map_err(|_| Self::corrupt("candidate ordinal is absent"))?,
            ) else {
                return Err(Self::corrupt("candidate ordinal is absent").into());
            };
            let address = entry
                .address
                .ok_or_else(|| Self::corrupt("candidate document address is not bound"))?;
            let segment = searcher
                .segment_readers()
                .get(address.segment_ord as usize)
                .ok_or_else(|| Self::corrupt("candidate segment address is missing"))?;
            let payloads = segment
                .fast_fields()
                .bytes(RANK_MATERIAL_FIELD_NAME)?
                .ok_or_else(|| Self::corrupt("rank payload fast field is missing"))?;
            let payload_length = segment
                .fast_fields()
                .u64(RANK_MATERIAL_LENGTH_FIELD_NAME)?
                .values
                .get_val(address.doc_id);
            let payload_length = usize::try_from(payload_length).map_err(|_| Error::SizeLimit)?;
            if payload_length == 0 || payload_length > MAX_RANK_MATERIAL_BYTES {
                return Err(Self::corrupt("rank payload length is outside its bound").into());
            }
            let required = base_scratch_bytes
                .checked_add(payload_length)
                .ok_or(Error::SizeLimit)?;
            ensure_rank_scratch_capacity(required, self.rank_budget.max_scratch_bytes)?;
            if scratch.capacity() < payload_length {
                scratch
                    .try_reserve_exact(payload_length.saturating_sub(scratch.len()))
                    .map_err(|_| Error::SizeLimit)?;
            }
            let retained_scratch = base_scratch_bytes
                .checked_add(scratch.capacity())
                .ok_or(Error::SizeLimit)?;
            ensure_rank_scratch_capacity(retained_scratch, self.rank_budget.max_scratch_bytes)?;
            self.record_rank_scratch(retained_scratch);
            let material = material_for_doc(&payloads, address.doc_id, &mut scratch)?;
            if material.len() != payload_length {
                return Err(Self::corrupt("rank payload length disagrees with its column").into());
            }
            let score =
                self.score_rank_material(query, ordinal as u32, address, material, &mut best)?;
            if let Some(score) = score {
                relevance.push((candidate, score.relevance));
            }
        }
        relevance.sort_unstable_by_key(|(entity, _)| *entity);
        relevance.dedup_by_key(|(entity, _)| *entity);
        Ok(relevance)
    }

    fn compile_query(&self, query: &Query) -> Box<dyn TantivyQuery> {
        if query.terms.is_empty() {
            return Box::new(AllQuery);
        }
        let clauses = query
            .terms
            .iter()
            .map(|term| {
                let (field, value) = match (&query.fields, query.case) {
                    (FieldSelection::All, crate::CaseSensitivity::Sensitive) => {
                        (self.raw_token, term.clone())
                    }
                    (FieldSelection::All, crate::CaseSensitivity::FoldAscii) => {
                        (self.folded_token, term.clone())
                    }
                    (FieldSelection::Only(field), crate::CaseSensitivity::Sensitive) => {
                        (self.field_raw_token, field_token_prefix(field, term))
                    }
                    (FieldSelection::Only(field), crate::CaseSensitivity::FoldAscii) => {
                        (self.field_folded_token, field_token_prefix(field, term))
                    }
                };
                let token = Term::from_field_text(field, &value);
                let token_query: Box<dyn TantivyQuery> = match query.match_mode {
                    MatchMode::Exact => Box::new(TermQuery::new(token, IndexRecordOption::Basic)),
                    MatchMode::Prefix => Box::new(FuzzyTermQuery::new_prefix(token, 0, true)),
                };
                (Occur::Must, token_query)
            })
            .collect();
        Box::new(BooleanQuery::new(clauses))
    }

    fn scan_ranked_hits(
        &self,
        query: &Query,
        extra_scratch_bytes: usize,
        mut visit: impl FnMut(RankedHit) -> Result<(), TantivySourceError>,
    ) -> Result<usize, TantivySourceError> {
        let base_scratch_bytes = query_scratch_bytes(query, extra_scratch_bytes)?;
        ensure_rank_scratch_capacity(base_scratch_bytes, self.rank_budget.max_scratch_bytes)?;
        self.record_rank_scratch(base_scratch_bytes);
        let mut best = Vec::new();
        best.try_reserve_exact(query.terms.len())
            .map_err(|_| Error::SizeLimit)?;
        let clause_bytes = query
            .terms
            .len()
            .checked_mul(std::mem::size_of::<Option<Relevance>>())
            .ok_or(Error::SizeLimit)?;
        let actual_clause_bytes = best
            .capacity()
            .checked_mul(std::mem::size_of::<Option<Relevance>>())
            .ok_or(Error::SizeLimit)?;
        let actual_base_scratch_bytes = base_scratch_bytes
            .checked_add(actual_clause_bytes.saturating_sub(clause_bytes))
            .ok_or(Error::SizeLimit)?;
        ensure_rank_scratch_capacity(
            actual_base_scratch_bytes,
            self.rank_budget.max_scratch_bytes,
        )?;
        self.record_rank_scratch(actual_base_scratch_bytes);
        // Admit the scorer allocations only after both the query representation
        // and the per-clause rank scratch fit the source's declared allowance.
        let searcher = self.reader.searcher();
        let engine_query = self.compile_query(query);
        let weight = engine_query.weight(EnableScoring::disabled_from_searcher(&searcher))?;
        let mut material = Vec::new();
        let mut total = 0usize;
        self.rank_evaluations.fetch_add(1, Ordering::Relaxed);
        for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
            let ordinals = segment.fast_fields().u64(ORDINAL_FIELD)?;
            let payload_lengths = segment.fast_fields().u64(RANK_MATERIAL_LENGTH_FIELD)?;
            let payloads = segment
                .fast_fields()
                .bytes(RANK_MATERIAL_FIELD_NAME)?
                .ok_or_else(|| Self::corrupt("rank payload fast field is missing"))?;
            let mut scorer = weight.scorer(segment, 1.0)?;
            let mut doc = scorer.doc();
            while doc != TERMINATED {
                self.rank_docs_visited.fetch_add(1, Ordering::Relaxed);
                // A raw Weight scorer can include tombstoned doc IDs. Collector
                // APIs normally apply the segment's live-doc bitset; this
                // direct streaming collector must do so before resolving the
                // stable ordinal against the selected generation.
                if segment.is_deleted(doc) {
                    doc = scorer.advance();
                    continue;
                }
                let address = DocAddress::new(
                    u32::try_from(segment_ord).map_err(|_| Error::SizeLimit)?,
                    doc,
                );
                let length = usize::try_from(payload_lengths.values.get_val(doc))
                    .map_err(|_| Error::SizeLimit)?;
                if length == 0 || length > MAX_RANK_MATERIAL_BYTES {
                    return Err(Self::corrupt("rank payload length is outside its bound").into());
                }
                let required = actual_base_scratch_bytes
                    .checked_add(length)
                    .ok_or(Error::SizeLimit)?;
                ensure_rank_scratch_capacity(required, self.rank_budget.max_scratch_bytes)?;
                self.record_rank_scratch(required);
                if material.capacity() < length {
                    material
                        .try_reserve_exact(length.saturating_sub(material.len()))
                        .map_err(|_| Error::SizeLimit)?;
                }
                let retained_scratch = actual_base_scratch_bytes
                    .checked_add(material.capacity())
                    .ok_or(Error::SizeLimit)?;
                ensure_rank_scratch_capacity(retained_scratch, self.rank_budget.max_scratch_bytes)?;
                self.record_rank_scratch(retained_scratch);
                let ordinal =
                    u32::try_from(ordinals.values.get_val(doc)).map_err(|_| Error::SizeLimit)?;
                let bytes = material_for_doc(&payloads, doc, &mut material)?;
                if bytes.len() != length {
                    return Err(
                        Self::corrupt("rank payload length disagrees with its column").into(),
                    );
                }
                if let Some(hit) =
                    self.score_rank_material(query, ordinal, address, bytes, &mut best)?
                {
                    total = total.checked_add(1).ok_or(Error::SizeLimit)?;
                    visit(hit)?;
                }
                doc = scorer.advance();
            }
        }
        Ok(total)
    }

    fn record_rank_scratch(&self, bytes: usize) {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        self.rank_peak_scratch_bytes
            .fetch_max(bytes, Ordering::Relaxed);
    }

    fn score_rank_material(
        &self,
        query: &Query,
        ordinal: u32,
        address: DocAddress,
        bytes: &[u8],
        best: &mut Vec<Option<Relevance>>,
    ) -> Result<Option<RankedHit>, TantivySourceError> {
        let Some(entry) = self.documents.get_entry(ordinal as usize) else {
            return Err(Self::corrupt("Tantivy ordinal is outside the selected binding").into());
        };
        if entry.address != Some(address) {
            return Err(
                Self::corrupt("Tantivy row address disagrees with the selected ordinal").into(),
            );
        }
        let (payload_ordinal, id, digest, field_count, mut offset) =
            parse_rank_material_header(bytes)?;
        if payload_ordinal != ordinal
            || id != *entry.document.id.as_bytes()
            || digest != entry.document.fields_digest
        {
            return Err(Self::corrupt(
                "Tantivy row identity disagrees with the selected generation",
            )
            .into());
        }
        if query.terms.is_empty() {
            let postings = validate_rank_material_tail(bytes, offset, field_count, self.limits)?;
            if postings != entry.document.postings {
                return Err(
                    Self::corrupt("rank payload posting count differs from ordinal map").into(),
                );
            }
            return Ok(Some(RankedHit {
                document: entry.document.id,
                relevance: Relevance::all_documents(),
            }));
        }
        if field_count > self.limits.max_fields_per_document {
            return Err(Self::corrupt("rank payload has too many fields").into());
        }
        best.clear();
        best.resize(query.terms.len(), None::<Relevance>);
        let mut total_tokens = 0u32;
        for _ in 0..field_count {
            let field_len = read_material_u32(bytes, &mut offset)? as usize;
            if field_len == 0 || field_len > self.limits.max_field_bytes {
                return Err(Self::corrupt("rank payload field length is invalid").into());
            }
            let field_bytes = read_material_slice(bytes, &mut offset, field_len)?;
            let field = std::str::from_utf8(field_bytes)
                .map_err(|_| Self::corrupt("rank field name is not UTF-8"))?;
            let weight = u16::from(read_material_u8(bytes, &mut offset)?);
            if !(1..=4).contains(&weight) {
                return Err(Self::corrupt("rank payload field weight is invalid").into());
            }
            let token_count = read_material_u32(bytes, &mut offset)?;
            for _ in 0..token_count {
                total_tokens = total_tokens.checked_add(1).ok_or(Error::SizeLimit)?;
                let token_len = read_material_u32(bytes, &mut offset)? as usize;
                if token_len == 0 || token_len > self.limits.max_field_bytes {
                    return Err(Self::corrupt("rank payload token length is invalid").into());
                }
                let token_bytes = read_material_slice(bytes, &mut offset, token_len)?;
                let token = std::str::from_utf8(token_bytes)
                    .map_err(|_| Self::corrupt("rank token is not UTF-8"))?;
                let ranking_bytes = read_material_u32(bytes, &mut offset)? as usize;
                if ranking_bytes == 0 || ranking_bytes > self.limits.max_field_bytes {
                    return Err(Self::corrupt("rank payload ranking length is invalid").into());
                }
                if !matches!(&query.fields, FieldSelection::Only(selected) if selected != field) {
                    for (clause, term) in query.terms.iter().enumerate() {
                        if query_token_matches(query, term, token) {
                            let score = Relevance::new(term.len(), ranking_bytes, weight, 1)?;
                            best[clause] =
                                Some(best[clause].map_or(score, |current| current.max(score)));
                        }
                    }
                }
            }
        }
        if offset != bytes.len() || total_tokens != entry.document.postings {
            return Err(
                Self::corrupt("rank payload is malformed or has another posting count").into(),
            );
        }
        let Some(mut relevance) = best.first().and_then(|value| *value) else {
            return Ok(None);
        };
        for clause in best.iter().skip(1) {
            let Some(clause) = *clause else {
                return Ok(None);
            };
            relevance = relevance.combine(clause)?;
        }
        Ok(Some(RankedHit {
            document: entry.document.id,
            relevance,
        }))
    }
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct SearchableToken<'a> {
    searchable: &'a str,
    ranking_bytes: usize,
}

fn searchable_tokens(text: &str) -> Vec<SearchableToken<'_>> {
    // Keep token text borrowed from the already-bounded source field. A tree
    // allocates one node per token, while owned token strings duplicate the
    // same input bytes many times for punctuation and identifier splits.
    let mut tokens = Vec::new();
    for raw in text.split_whitespace().filter(|token| !token.is_empty()) {
        let first = tokens.len();
        tokens.push(SearchableToken {
            searchable: raw,
            ranking_bytes: raw.len(),
        });
        for identifier in raw.split(|character: char| !character.is_alphanumeric()) {
            if identifier.is_empty() {
                continue;
            }
            tokens.push(SearchableToken {
                searchable: identifier,
                ranking_bytes: raw.len(),
            });
            tokens.extend(identifier_words(identifier).map(|word| SearchableToken {
                searchable: word,
                ranking_bytes: raw.len(),
            }));
        }
        // Most plain identifiers yield the same term through all three
        // paths. Deduplicate each whitespace token before retaining the
        // field-wide canonical set, limiting transient duplicate entries.
        tokens[first..].sort_unstable();
        let mut unique_end = first;
        for index in first..tokens.len() {
            let token = tokens[index];
            if unique_end == first || token != tokens[unique_end - 1] {
                tokens[unique_end] = token;
                unique_end += 1;
            }
        }
        tokens.truncate(unique_end);
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

#[derive(Clone, Copy)]
struct ProjectionFields {
    raw_token: Field,
    folded_token: Field,
    field_raw_token: Field,
    field_folded_token: Field,
    ordinal: Field,
    rank_material: Field,
    rank_material_len: Field,
}

fn projection_schema() -> ProjectedSchema {
    let mut schema = Schema::builder();
    let raw_token = schema.add_text_field("raw_token", STRING);
    let folded_token = schema.add_text_field("folded_token", STRING);
    let field_raw_token = schema.add_text_field(FIELD_RAW_TOKEN, STRING);
    let field_folded_token = schema.add_text_field(FIELD_FOLDED_TOKEN, STRING);
    // Indexed so a revision can delete one document's postings by ordinal.
    // A row-native document contains every token for one semantic entity. The
    // compact rank payload is stored as a bounded fast bytes column.
    let ordinal = schema.add_u64_field("document_ordinal", INDEXED | FAST);
    let rank_material =
        schema.add_bytes_field(RANK_MATERIAL_FIELD, BytesOptions::default().set_fast());
    let rank_material_len = schema.add_u64_field(RANK_MATERIAL_LENGTH_FIELD, FAST);
    ProjectedSchema {
        schema: schema.build(),
        fields: ProjectionFields {
            raw_token,
            folded_token,
            field_raw_token,
            field_folded_token,
            ordinal,
            rank_material,
            rank_material_len,
        },
    }
}

fn projection_fingerprint(binding: Binding) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-extension-tantivy/projection/v3");
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
            address: None,
            segment_id: None,
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
        TantivySourceError::DurableProjectionImmutable => false,
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
    source.durable = true;
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
        _adapter: &super::TantivyAdapter,
    ) -> Result<usize, super::TantivySourceError> {
        Ok(0)
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

    pub(crate) fn resident_table_addresses(source: &super::TantivySource) -> (usize, usize) {
        (
            source.documents.live.as_ptr() as usize,
            source.identity_ordinals.as_ptr() as usize,
        )
    }

    pub(crate) fn binding_work(source: &super::TantivySource) -> (usize, usize) {
        (
            source.last_binding_work.payload_rows_scanned,
            source.last_binding_work.source_posting_checks,
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
        let mut writer = index.writer(super::WRITER_MEMORY_BYTES)?;
        let postings = super::write_document(
            &writer,
            &projected.fields,
            u64::from(ordinal),
            id,
            fields,
            super::MAX_RANK_MATERIAL_BYTES,
        )?;
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
                address: None,
                segment_id: None,
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

    pub(crate) fn write_projection_mismatch_fixture(
        state: &super::DocumentState,
        directory: &std::path::Path,
        indexed_fields: &[(String, String)],
        rank_fields: &[(String, String)],
    ) -> Result<(), super::TantivySourceError> {
        let mut rows = state.iter();
        let Some((id, fields)) = rows.next() else {
            return Err(super::Error::MalformedInput.into());
        };
        if rows.next().is_some()
            || indexed_fields.len() != fields.len()
            || rank_fields.len() != fields.len()
        {
            return Err(super::Error::MalformedInput.into());
        }
        let projected = super::projection_schema();
        let index = super::Index::create_in_dir(directory, projected.schema)?;
        let mut writer = index.writer(super::WRITER_MEMORY_BYTES)?;
        let mut document = super::TantivyDocument::default();
        let mut postings = 0_u32;
        for (field, text) in indexed_fields {
            for token in super::searchable_tokens(text) {
                postings = postings.checked_add(1).ok_or(super::Error::SizeLimit)?;
                let folded = token.searchable.to_ascii_lowercase();
                document.add_text(projected.fields.raw_token, token.searchable);
                document.add_text(projected.fields.folded_token, folded.as_str());
                document.add_text(
                    projected.fields.field_raw_token,
                    super::field_token_value(field, token.searchable, false),
                );
                document.add_text(
                    projected.fields.field_folded_token,
                    super::field_token_value(field, token.searchable, true),
                );
            }
        }
        let mut material = Vec::new();
        super::append_rank_material(
            &mut material,
            super::RANK_MATERIAL_MAGIC,
            super::MAX_RANK_MATERIAL_BYTES,
        )?;
        super::append_rank_material(
            &mut material,
            &0_u64.to_le_bytes(),
            super::MAX_RANK_MATERIAL_BYTES,
        )?;
        super::append_rank_material(&mut material, id.as_bytes(), super::MAX_RANK_MATERIAL_BYTES)?;
        super::append_rank_material(
            &mut material,
            &super::document_fields_digest(fields),
            super::MAX_RANK_MATERIAL_BYTES,
        )?;
        super::append_rank_material_len(
            &mut material,
            rank_fields.len(),
            super::MAX_RANK_MATERIAL_BYTES,
        )?;
        for (field, text) in rank_fields {
            let tokens = super::searchable_tokens(text);
            super::append_rank_material_len(
                &mut material,
                field.len(),
                super::MAX_RANK_MATERIAL_BYTES,
            )?;
            super::append_rank_material(
                &mut material,
                field.as_bytes(),
                super::MAX_RANK_MATERIAL_BYTES,
            )?;
            super::append_rank_material(
                &mut material,
                &[
                    u8::try_from(super::field_weight(field))
                        .map_err(|_| super::Error::SizeLimit)?,
                ],
                super::MAX_RANK_MATERIAL_BYTES,
            )?;
            super::append_rank_material_len(
                &mut material,
                tokens.len(),
                super::MAX_RANK_MATERIAL_BYTES,
            )?;
            for token in tokens {
                super::append_rank_material_len(
                    &mut material,
                    token.searchable.len(),
                    super::MAX_RANK_MATERIAL_BYTES,
                )?;
                super::append_rank_material(
                    &mut material,
                    token.searchable.as_bytes(),
                    super::MAX_RANK_MATERIAL_BYTES,
                )?;
                super::append_rank_material_len(
                    &mut material,
                    token.ranking_bytes,
                    super::MAX_RANK_MATERIAL_BYTES,
                )?;
            }
        }
        document.add_u64(projected.fields.ordinal, 0);
        document.add_bytes(projected.fields.rank_material, &material);
        document.add_u64(
            projected.fields.rank_material_len,
            u64::try_from(material.len()).map_err(|_| super::Error::SizeLimit)?,
        );
        writer.add_document(document)?;
        writer.commit()?;
        writer.wait_merging_threads()?;

        let fingerprint = super::projection_fingerprint(state.binding());
        let documents = super::DocumentTable::from_live(
            1,
            vec![super::OrdinalDocument {
                ordinal: 0,
                document: super::LiveDocument {
                    id,
                    fields_digest: super::document_fields_digest(fields),
                    postings,
                },
                address: None,
                segment_id: None,
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

    pub(crate) fn projection_files_for_test(
        root: &std::path::Path,
    ) -> Result<std::collections::BTreeMap<String, (u64, [u8; 32])>, super::TantivySourceError>
    {
        super::projection_file_fingerprints(root, super::DurableCacheBudget::default())
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
        let offset = request.cursor.map_or(0, Cursor::offset);
        let after = request.cursor.and_then(Cursor::after_hit);
        if after.is_none() && offset != 0 || after.is_some() && offset == 0 {
            return Err(Error::InvalidCursor.into());
        }
        let heap_bytes = request
            .limit
            .checked_mul(std::mem::size_of::<RankedHit>())
            .ok_or(Error::SizeLimit)?;
        preflight_query_scratch(
            &request.query,
            heap_bytes,
            self.rank_budget.max_scratch_bytes,
        )?;
        let mut page = TopHits::new(request.limit)?;
        let heap_bytes = page
            .hits
            .capacity()
            .checked_mul(std::mem::size_of::<RankedHit>())
            .ok_or(Error::SizeLimit)?;
        preflight_query_scratch(
            &request.query,
            heap_bytes,
            self.rank_budget.max_scratch_bytes,
        )?;
        let mut eligible = 0usize;
        let mut before_boundary = 0usize;
        let mut boundary_seen = false;
        let total = self.scan_ranked_hits(&request.query, heap_bytes, |hit| {
            let after_boundary = match after {
                Some(boundary) => match compare_ranked_hits(hit, boundary) {
                    std::cmp::Ordering::Less => {
                        before_boundary = before_boundary.checked_add(1).ok_or(Error::SizeLimit)?;
                        false
                    }
                    std::cmp::Ordering::Equal => {
                        if hit != boundary || boundary_seen {
                            return Err(Error::InvalidCursor.into());
                        }
                        boundary_seen = true;
                        false
                    }
                    std::cmp::Ordering::Greater => true,
                },
                None => true,
            };
            if !after_boundary {
                return Ok(());
            }
            eligible = eligible.checked_add(1).ok_or(Error::SizeLimit)?;
            page.consider(hit);
            Ok(())
        })?;
        if offset > total
            || after.is_some_and(|_| !boundary_seen || before_boundary != offset.saturating_sub(1))
        {
            return Err(Error::InvalidCursor.into());
        }
        if eligible != total.saturating_sub(offset) {
            return Err(
                Self::corrupt("keyset cursor does not partition the exact result set").into(),
            );
        }
        let hits = page.into_sorted();
        let next = if eligible > hits.len() {
            let last = hits.last().copied().ok_or(Error::InvalidCursor)?;
            Some(Cursor::after(
                self.binding,
                request.query.version,
                offset.checked_add(hits.len()).ok_or(Error::SizeLimit)?,
                last,
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
            total,
            coverage: self.coverage,
        })
    }
}

enum RevisionPlan<'next> {
    Rebound,
    Changed(RevisionChanges<'next>),
}

struct RevisionChanges<'next> {
    rewritten_documents: usize,
    retired_postings: u64,
    removed_ordinals: Vec<u32>,
    writes: Vec<PlannedWrite<'next>>,
    slot_count: u32,
}

struct PlannedWrite<'next> {
    ordinal: u64,
    id: EntityId,
    fields_digest: [u8; 32],
    fields: &'next [(String, String)],
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

fn rank_material_limit(limits: Limits) -> usize {
    limits
        .max_total_text_bytes
        .saturating_mul(4)
        .saturating_add(limits.max_fields_per_document.saturating_mul(16))
        .min(MAX_RANK_MATERIAL_BYTES)
}

fn field_token_prefix(field: &str, token: &str) -> String {
    format!("{}:{field}{token}", field.len())
}

fn field_token_value(field: &str, token: &str, folded: bool) -> String {
    if folded {
        field_token_prefix(field, &token.to_ascii_lowercase())
    } else {
        field_token_prefix(field, token)
    }
}

fn append_rank_material(
    output: &mut Vec<u8>,
    value: &[u8],
    maximum: usize,
) -> Result<(), TantivySourceError> {
    let required = output
        .len()
        .checked_add(value.len())
        .ok_or(Error::SizeLimit)?;
    if required > maximum {
        return Err(Error::SizeLimit.into());
    }
    output
        .try_reserve(value.len())
        .map_err(|_| Error::SizeLimit)?;
    output.extend_from_slice(value);
    Ok(())
}

fn append_rank_material_len(
    output: &mut Vec<u8>,
    value: usize,
    maximum: usize,
) -> Result<(), TantivySourceError> {
    let mut value = u32::try_from(value).map_err(|_| Error::SizeLimit)?;
    loop {
        let continuation = value > 0x7f;
        let byte = (value as u8 & 0x7f) | if continuation { 0x80 } else { 0 };
        append_rank_material(output, &[byte], maximum)?;
        if !continuation {
            return Ok(());
        }
        value >>= 7;
    }
}

fn write_document(
    writer: &tantivy::IndexWriter,
    fields: &ProjectionFields,
    document_ordinal: u64,
    document_id: EntityId,
    document_fields: &[(String, String)],
    maximum_material_bytes: usize,
) -> Result<u32, TantivySourceError> {
    if document_fields.len() > u32::MAX as usize {
        return Err(Error::SizeLimit.into());
    }
    let mut document = TantivyDocument::default();
    let mut material = Vec::new();
    append_rank_material(&mut material, RANK_MATERIAL_MAGIC, maximum_material_bytes)?;
    append_rank_material(
        &mut material,
        &document_ordinal.to_le_bytes(),
        maximum_material_bytes,
    )?;
    append_rank_material(
        &mut material,
        document_id.as_bytes(),
        maximum_material_bytes,
    )?;
    append_rank_material(
        &mut material,
        &document_fields_digest(document_fields),
        maximum_material_bytes,
    )?;
    append_rank_material_len(&mut material, document_fields.len(), maximum_material_bytes)?;
    let mut postings = 0u32;
    for (field, text) in document_fields {
        let field_weight = field_weight(field);
        if field.is_empty() || field_weight == 0 {
            return Err(Error::MalformedInput.into());
        }
        let tokens = searchable_tokens(text);
        append_rank_material_len(&mut material, field.len(), maximum_material_bytes)?;
        append_rank_material(&mut material, field.as_bytes(), maximum_material_bytes)?;
        append_rank_material(
            &mut material,
            &[u8::try_from(field_weight).map_err(|_| Error::SizeLimit)?],
            maximum_material_bytes,
        )?;
        append_rank_material_len(&mut material, tokens.len(), maximum_material_bytes)?;
        for token in tokens {
            postings = postings.checked_add(1).ok_or(Error::SizeLimit)?;
            let folded = token.searchable.to_ascii_lowercase();
            document.add_text(fields.raw_token, token.searchable);
            document.add_text(fields.folded_token, folded.as_str());
            document.add_text(
                fields.field_raw_token,
                field_token_value(field, token.searchable, false),
            );
            document.add_text(
                fields.field_folded_token,
                field_token_value(field, token.searchable, true),
            );
            append_rank_material_len(
                &mut material,
                token.searchable.len(),
                maximum_material_bytes,
            )?;
            append_rank_material(
                &mut material,
                token.searchable.as_bytes(),
                maximum_material_bytes,
            )?;
            append_rank_material_len(&mut material, token.ranking_bytes, maximum_material_bytes)?;
        }
    }
    document.add_u64(fields.ordinal, document_ordinal);
    document.add_bytes(fields.rank_material, &material);
    document.add_u64(
        fields.rank_material_len,
        u64::try_from(material.len()).map_err(|_| Error::SizeLimit)?,
    );
    writer.add_document(document)?;
    Ok(postings)
}

const ORDINAL_FIELD: &str = "document_ordinal";
const RANK_MATERIAL_FIELD_NAME: &str = "rank_material";
const RANK_MATERIAL_LENGTH_FIELD_NAME: &str = "rank_material_bytes";

fn ensure_rank_scratch_capacity(
    required_bytes: usize,
    budget_bytes: usize,
) -> Result<(), TantivySourceError> {
    if required_bytes > budget_bytes {
        return Err(TantivySourceError::RankSnapshotBudgetExceeded {
            budget_bytes,
            required_bytes,
        });
    }
    Ok(())
}

fn query_scratch_bytes(query: &Query, extra_bytes: usize) -> Result<usize, TantivySourceError> {
    let clause_bytes = query
        .terms
        .len()
        .checked_mul(std::mem::size_of::<Option<Relevance>>())
        .ok_or(Error::SizeLimit)?;
    let field_prefix_bytes = match &query.fields {
        FieldSelection::All => 0,
        FieldSelection::Only(field) => field
            .len()
            .checked_add(1)
            .and_then(|bytes| bytes.checked_add(decimal_digits(field.len())))
            .ok_or(Error::SizeLimit)?,
    };
    let term_setup_bytes = query.terms.iter().try_fold(0_usize, |total, term| {
        // `Term::from_field_text` retains an owned copy while the builder's
        // `String` remains live. Field-qualified terms duplicate the complete
        // encoded field prefix, including the decimal byte-length header.
        let term_value_bytes = term
            .len()
            .checked_add(field_prefix_bytes)
            .ok_or(Error::SizeLimit)?;
        let owned_and_building_bytes = term_value_bytes.checked_mul(2).ok_or(Error::SizeLimit)?;
        total
            .checked_add(owned_and_building_bytes)
            .and_then(|bytes| bytes.checked_add(256))
            .ok_or(Error::SizeLimit)
    })?;
    clause_bytes
        .checked_add(term_setup_bytes)
        .and_then(|bytes| bytes.checked_add(extra_bytes))
        .ok_or_else(|| Error::SizeLimit.into())
}

fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn preflight_query_scratch(
    query: &Query,
    extra_bytes: usize,
    budget_bytes: usize,
) -> Result<(), TantivySourceError> {
    ensure_rank_scratch_capacity(query_scratch_bytes(query, extra_bytes)?, budget_bytes)
}

fn material_for_doc<'a>(
    payloads: &BytesColumn,
    document: DocId,
    material: &'a mut Vec<u8>,
) -> Result<&'a [u8], TantivySourceError> {
    let mut ords = payloads.term_ords(document);
    let Some(term_ord) = ords.next() else {
        return Err(TantivySource::corrupt("rank payload is missing").into());
    };
    if ords.next().is_some() {
        return Err(TantivySource::corrupt("rank payload has multiple values").into());
    }
    material.clear();
    if !payloads.ord_to_bytes(term_ord, material)? {
        return Err(TantivySource::corrupt("rank payload term is missing").into());
    }
    Ok(material)
}

fn parse_rank_material_header(
    bytes: &[u8],
) -> Result<(u32, [u8; 32], [u8; 32], usize, usize), TantivySourceError> {
    if !bytes.starts_with(RANK_MATERIAL_MAGIC) {
        return Err(TantivySource::corrupt("rank payload has an invalid format").into());
    }
    let mut offset = RANK_MATERIAL_MAGIC.len();
    let ordinal = read_material_u32_64(bytes, &mut offset)?;
    let id = read_material_array(bytes, &mut offset)?;
    let digest = read_material_array(bytes, &mut offset)?;
    let field_count =
        usize::try_from(read_material_u32(bytes, &mut offset)?).map_err(|_| Error::SizeLimit)?;
    Ok((ordinal, id, digest, field_count, offset))
}

fn read_material_u8(bytes: &[u8], offset: &mut usize) -> Result<u8, TantivySourceError> {
    let Some(value) = bytes.get(*offset).copied() else {
        return Err(TantivySource::corrupt("rank payload is truncated").into());
    };
    *offset += 1;
    Ok(value)
}

fn read_material_u32(bytes: &[u8], offset: &mut usize) -> Result<u32, TantivySourceError> {
    let mut value = 0_u32;
    for shift in (0..35).step_by(7) {
        let byte = read_material_u8(bytes, offset)?;
        if shift == 28 && byte & 0xf0 != 0 {
            return Err(TantivySource::corrupt("rank payload integer overflows u32").into());
        }
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            if shift > 0 && byte == 0 {
                return Err(TantivySource::corrupt("rank payload integer is not canonical").into());
            }
            return Ok(value);
        }
    }
    Err(TantivySource::corrupt("rank payload integer is too long").into())
}

fn read_material_u32_64(bytes: &[u8], offset: &mut usize) -> Result<u32, TantivySourceError> {
    let end = offset.checked_add(8).ok_or(Error::SizeLimit)?;
    let value = bytes
        .get(*offset..end)
        .and_then(|slice| slice.try_into().ok())
        .map(u64::from_le_bytes)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| TantivySource::corrupt("rank payload ordinal is invalid"))?;
    *offset = end;
    Ok(value)
}

fn read_material_array<const N: usize>(
    bytes: &[u8],
    offset: &mut usize,
) -> Result<[u8; N], TantivySourceError> {
    let end = offset.checked_add(N).ok_or(Error::SizeLimit)?;
    let value = bytes
        .get(*offset..end)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| TantivySource::corrupt("rank payload is truncated"))?;
    *offset = end;
    Ok(value)
}

fn read_material_slice<'a>(
    bytes: &'a [u8],
    offset: &mut usize,
    length: usize,
) -> Result<&'a [u8], TantivySourceError> {
    let end = offset.checked_add(length).ok_or(Error::SizeLimit)?;
    let value = bytes
        .get(*offset..end)
        .ok_or_else(|| TantivySource::corrupt("rank payload is truncated"))?;
    *offset = end;
    Ok(value)
}

fn validate_rank_material_tail(
    bytes: &[u8],
    mut offset: usize,
    field_count: usize,
    limits: Limits,
) -> Result<u32, TantivySourceError> {
    if field_count > limits.max_fields_per_document {
        return Err(TantivySource::corrupt("rank payload has too many fields").into());
    }
    let mut postings = 0u32;
    for _ in 0..field_count {
        let field_len = usize::try_from(read_material_u32(bytes, &mut offset)?)
            .map_err(|_| Error::SizeLimit)?;
        if field_len == 0 || field_len > limits.max_field_bytes {
            return Err(TantivySource::corrupt("rank payload field length is invalid").into());
        }
        let field = read_material_slice(bytes, &mut offset, field_len)?;
        std::str::from_utf8(field)
            .map_err(|_| TantivySource::corrupt("rank field name is not UTF-8"))?;
        let weight = read_material_u8(bytes, &mut offset)?;
        if !(1..=4).contains(&weight) {
            return Err(TantivySource::corrupt("rank payload field weight is invalid").into());
        }
        let token_count = usize::try_from(read_material_u32(bytes, &mut offset)?)
            .map_err(|_| Error::SizeLimit)?;
        for _ in 0..token_count {
            postings = postings.checked_add(1).ok_or(Error::SizeLimit)?;
            let token_len = usize::try_from(read_material_u32(bytes, &mut offset)?)
                .map_err(|_| Error::SizeLimit)?;
            if token_len == 0 || token_len > limits.max_field_bytes {
                return Err(TantivySource::corrupt("rank payload token length is invalid").into());
            }
            let token = read_material_slice(bytes, &mut offset, token_len)?;
            std::str::from_utf8(token)
                .map_err(|_| TantivySource::corrupt("rank token is not UTF-8"))?;
            let ranking_bytes = usize::try_from(read_material_u32(bytes, &mut offset)?)
                .map_err(|_| Error::SizeLimit)?;
            if ranking_bytes == 0 || ranking_bytes > limits.max_field_bytes {
                return Err(TantivySource::corrupt("rank payload token weight is invalid").into());
            }
        }
    }
    if offset != bytes.len() {
        return Err(TantivySource::corrupt("rank payload has trailing bytes").into());
    }
    Ok(postings)
}

fn hash_rank_material_length(
    hasher: &mut blake3::Hasher,
    length: usize,
) -> Result<(), TantivySourceError> {
    let mut value = u32::try_from(length).map_err(|_| Error::SizeLimit)?;
    loop {
        let continuation = value > 0x7f;
        hasher.update(&[(value as u8 & 0x7f) | if continuation { 0x80 } else { 0 }]);
        if !continuation {
            return Ok(());
        }
        value >>= 7;
    }
}

fn canonical_rank_material_tail(
    fields: &[(String, String)],
) -> Result<(u32, [u8; 32]), TantivySourceError> {
    let mut hasher = blake3::Hasher::new();
    let mut postings = 0_u32;
    for (field, text) in fields {
        let weight = field_weight(field);
        if field.is_empty() || weight == 0 {
            return Err(Error::MalformedInput.into());
        }
        let tokens = searchable_tokens(text);
        hash_rank_material_length(&mut hasher, field.len())?;
        hasher.update(field.as_bytes());
        hasher.update(&[u8::try_from(weight).map_err(|_| Error::SizeLimit)?]);
        hash_rank_material_length(&mut hasher, tokens.len())?;
        for token in tokens {
            postings = postings.checked_add(1).ok_or(Error::SizeLimit)?;
            hash_rank_material_length(&mut hasher, token.searchable.len())?;
            hasher.update(token.searchable.as_bytes());
            hash_rank_material_length(&mut hasher, token.ranking_bytes)?;
        }
    }
    Ok((postings, *hasher.finalize().as_bytes()))
}

fn query_token_matches(query: &Query, term: &str, token: &str) -> bool {
    let term = term.as_bytes();
    let token = token.as_bytes();
    match (query.case, query.match_mode) {
        (crate::CaseSensitivity::Sensitive, MatchMode::Exact) => token == term,
        (crate::CaseSensitivity::Sensitive, MatchMode::Prefix) => token.starts_with(term),
        (crate::CaseSensitivity::FoldAscii, MatchMode::Exact) => token.eq_ignore_ascii_case(term),
        (crate::CaseSensitivity::FoldAscii, MatchMode::Prefix) => token
            .get(..term.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(term)),
    }
}

fn require_document_posting(
    segment: &tantivy::SegmentReader,
    field: Field,
    value: &str,
    doc_id: DocId,
) -> Result<(), TantivySourceError> {
    let term = Term::from_field_text(field, value);
    let inverted_index = segment.inverted_index(field)?;
    let Some(mut postings) = inverted_index.read_postings(&term, IndexRecordOption::Basic)? else {
        return Err(
            TantivySource::corrupt("Tantivy term dictionary omits a source-bound token").into(),
        );
    };
    if postings.seek(doc_id) != doc_id {
        return Err(
            TantivySource::corrupt("Tantivy postings omit a source-bound document token").into(),
        );
    }
    Ok(())
}

fn bind_document_addresses(
    reader: &IndexReader,
    documents: &mut DocumentTable,
    limits: Limits,
    state: &DocumentState,
    fields: ProjectionFields,
) -> Result<BindingWork, TantivySourceError> {
    for entry in &mut documents.live {
        entry.address = None;
        entry.segment_id = None;
    }
    let searcher = reader.searcher();
    let expected_docs = u64::try_from(documents.live.len()).map_err(|_| Error::SizeLimit)?;
    if searcher.num_docs() != expected_docs {
        return Err(TantivySource::corrupt("Tantivy row count disagrees with ordinal map").into());
    }
    let mut bound = 0usize;
    let mut work = BindingWork::default();
    let mut material = Vec::new();
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let segment_id = segment.segment_id();
        let ordinals = segment.fast_fields().u64(ORDINAL_FIELD)?;
        let payload_lengths = segment.fast_fields().u64(RANK_MATERIAL_LENGTH_FIELD_NAME)?;
        let payloads = segment
            .fast_fields()
            .bytes(RANK_MATERIAL_FIELD_NAME)?
            .ok_or_else(|| TantivySource::corrupt("rank payload fast field is missing"))?;
        for doc in segment.doc_ids_alive() {
            work.payload_rows_scanned = work
                .payload_rows_scanned
                .checked_add(1)
                .ok_or(Error::SizeLimit)?;
            let ordinal =
                u32::try_from(ordinals.values.get_val(doc)).map_err(|_| Error::SizeLimit)?;
            let address = DocAddress::new(
                u32::try_from(segment_ord).map_err(|_| Error::SizeLimit)?,
                doc,
            );
            let payload_len = usize::try_from(payload_lengths.values.get_val(doc))
                .map_err(|_| Error::SizeLimit)?;
            if payload_len == 0 || payload_len > MAX_RANK_MATERIAL_BYTES {
                return Err(TantivySource::corrupt("rank payload length is invalid").into());
            }
            if material.capacity() < payload_len {
                material
                    .try_reserve_exact(payload_len.saturating_sub(material.len()))
                    .map_err(|_| Error::SizeLimit)?;
            }
            let bytes = material_for_doc(&payloads, doc, &mut material)?;
            if bytes.len() != payload_len {
                return Err(TantivySource::corrupt(
                    "rank payload length disagrees with its fast field",
                )
                .into());
            }
            let (payload_ordinal, id, digest, field_count, offset) =
                parse_rank_material_header(bytes)?;
            if payload_ordinal != ordinal {
                return Err(TantivySource::corrupt(
                    "rank payload ordinal disagrees with its fast field",
                )
                .into());
            }
            let postings = validate_rank_material_tail(bytes, offset, field_count, limits)?;
            let index = documents
                .live
                .binary_search_by_key(&ordinal, |entry| entry.ordinal)
                .map_err(|_| {
                    TantivySource::corrupt("Tantivy row ordinal is not in the selected map")
                })?;
            let entry = documents
                .live
                .get_mut(index)
                .ok_or_else(|| TantivySource::corrupt("Tantivy row ordinal is missing"))?;
            let source_fields = state.fields_for(entry.document.id).ok_or_else(|| {
                TantivySource::corrupt(
                    "Tantivy row identity is outside the selected document state",
                )
            })?;
            let (source_postings, source_tail_digest) =
                canonical_rank_material_tail(source_fields)?;
            if entry.address.is_some()
                || entry.segment_id.is_some()
                || id != *entry.document.id.as_bytes()
                || digest != entry.document.fields_digest
                || postings != entry.document.postings
                || postings != source_postings
                || field_count != source_fields.len()
                || blake3::hash(&bytes[offset..]).as_bytes() != &source_tail_digest
            {
                return Err(TantivySource::corrupt(
                    "Tantivy row identity or rank material disagrees with the selected generation",
                )
                .into());
            }
            for (field, text) in source_fields {
                for token in searchable_tokens(text) {
                    require_document_posting(segment, fields.raw_token, token.searchable, doc)?;
                    let folded = token.searchable.to_ascii_lowercase();
                    require_document_posting(segment, fields.folded_token, &folded, doc)?;
                    let qualified_raw = field_token_value(field, token.searchable, false);
                    require_document_posting(segment, fields.field_raw_token, &qualified_raw, doc)?;
                    let qualified_folded = field_token_value(field, token.searchable, true);
                    require_document_posting(
                        segment,
                        fields.field_folded_token,
                        &qualified_folded,
                        doc,
                    )?;
                    work.source_posting_checks = work
                        .source_posting_checks
                        .checked_add(4)
                        .ok_or(Error::SizeLimit)?;
                }
            }
            entry.address = Some(address);
            entry.segment_id = Some(segment_id);
            bound = bound.checked_add(1).ok_or(Error::SizeLimit)?;
        }
    }
    if bound != documents.live.len() || documents.live.iter().any(|entry| entry.address.is_none()) {
        return Err(TantivySource::corrupt(
            "selected ordinal map has no exact Tantivy row address",
        )
        .into());
    }
    Ok(work)
}

fn bind_resident_document_addresses(
    reader: &IndexReader,
    documents: &mut DocumentTable,
    limits: Limits,
    state: &DocumentState,
    fields: ProjectionFields,
    old_segment_ids: &HashSet<tantivy::SegmentId>,
    changed_ordinals: &[u32],
) -> Result<BindingWork, TantivySourceError> {
    let searcher = reader.searcher();
    let expected_docs = u64::try_from(documents.live.len()).map_err(|_| Error::SizeLimit)?;
    if searcher.num_docs() != expected_docs {
        return Err(TantivySource::corrupt("Tantivy row count disagrees with ordinal map").into());
    }
    let segment_readers = searcher.segment_readers();
    let mut segment_positions = HashMap::new();
    segment_positions
        .try_reserve(segment_readers.len())
        .map_err(|_| Error::SizeLimit)?;
    for (segment_ord, segment) in segment_readers.iter().enumerate() {
        let segment_ord = u32::try_from(segment_ord).map_err(|_| Error::SizeLimit)?;
        if segment_positions
            .insert(segment.segment_id(), segment_ord)
            .is_some()
        {
            return Err(TantivySource::corrupt("Tantivy segment identity is duplicated").into());
        }
    }

    for entry in &mut documents.live {
        let Some((segment_id, address)) = entry.segment_id.zip(entry.address) else {
            entry.segment_id = None;
            entry.address = None;
            continue;
        };
        let Some(segment_ord) = segment_positions.get(&segment_id).copied() else {
            entry.segment_id = None;
            entry.address = None;
            continue;
        };
        let segment = segment_readers
            .get(segment_ord as usize)
            .ok_or_else(|| TantivySource::corrupt("resident segment address is missing"))?;
        if segment.is_deleted(address.doc_id) {
            return Err(TantivySource::corrupt(
                "unchanged selected row became deleted during resident revision",
            )
            .into());
        }
        entry.address = Some(DocAddress::new(segment_ord, address.doc_id));
    }

    let mut work = BindingWork::default();
    let mut material = Vec::new();
    for (segment_ord, segment) in segment_readers.iter().enumerate() {
        let segment_id = segment.segment_id();
        if old_segment_ids.contains(&segment_id) {
            continue;
        }
        let ordinals = segment.fast_fields().u64(ORDINAL_FIELD)?;
        let payload_lengths = segment.fast_fields().u64(RANK_MATERIAL_LENGTH_FIELD_NAME)?;
        let payloads = segment
            .fast_fields()
            .bytes(RANK_MATERIAL_FIELD_NAME)?
            .ok_or_else(|| TantivySource::corrupt("rank payload fast field is missing"))?;
        for doc in segment.doc_ids_alive() {
            work.payload_rows_scanned = work
                .payload_rows_scanned
                .checked_add(1)
                .ok_or(Error::SizeLimit)?;
            let ordinal =
                u32::try_from(ordinals.values.get_val(doc)).map_err(|_| Error::SizeLimit)?;
            let payload_len = usize::try_from(payload_lengths.values.get_val(doc))
                .map_err(|_| Error::SizeLimit)?;
            if payload_len == 0 || payload_len > MAX_RANK_MATERIAL_BYTES {
                return Err(TantivySource::corrupt("rank payload length is invalid").into());
            }
            if material.capacity() < payload_len {
                material
                    .try_reserve_exact(payload_len.saturating_sub(material.len()))
                    .map_err(|_| Error::SizeLimit)?;
            }
            let bytes = material_for_doc(&payloads, doc, &mut material)?;
            if bytes.len() != payload_len {
                return Err(TantivySource::corrupt(
                    "rank payload length disagrees with its fast field",
                )
                .into());
            }
            let (payload_ordinal, id, digest, field_count, offset) =
                parse_rank_material_header(bytes)?;
            if payload_ordinal != ordinal {
                return Err(TantivySource::corrupt(
                    "rank payload ordinal disagrees with its fast field",
                )
                .into());
            }
            let index = documents
                .live
                .binary_search_by_key(&ordinal, |entry| entry.ordinal)
                .map_err(|_| {
                    TantivySource::corrupt("Tantivy row ordinal is not in the selected map")
                })?;
            let entry = documents
                .live
                .get(index)
                .ok_or_else(|| TantivySource::corrupt("Tantivy row ordinal is missing"))?;
            if entry.address.is_some()
                || entry.segment_id.is_some()
                || id != *entry.document.id.as_bytes()
                || digest != entry.document.fields_digest
            {
                return Err(TantivySource::corrupt(
                    "resident Tantivy row identity disagrees with the selected generation",
                )
                .into());
            }
            if changed_ordinals.binary_search(&ordinal).is_ok() {
                let source_fields = state.fields_for(entry.document.id).ok_or_else(|| {
                    TantivySource::corrupt(
                        "revised Tantivy row is outside the selected document state",
                    )
                })?;
                let source_digest = document_fields_digest(source_fields);
                let (source_postings, source_tail_digest) =
                    canonical_rank_material_tail(source_fields)?;
                let postings = validate_rank_material_tail(bytes, offset, field_count, limits)?;
                if source_digest != entry.document.fields_digest
                    || postings != entry.document.postings
                    || postings != source_postings
                    || field_count != source_fields.len()
                    || blake3::hash(&bytes[offset..]).as_bytes() != &source_tail_digest
                {
                    return Err(TantivySource::corrupt(
                        "new Tantivy row rank material disagrees with the selected generation",
                    )
                    .into());
                }
                for (field, text) in source_fields {
                    for token in searchable_tokens(text) {
                        require_document_posting(segment, fields.raw_token, token.searchable, doc)?;
                        let folded = token.searchable.to_ascii_lowercase();
                        require_document_posting(segment, fields.folded_token, &folded, doc)?;
                        let qualified_raw = field_token_value(field, token.searchable, false);
                        require_document_posting(
                            segment,
                            fields.field_raw_token,
                            &qualified_raw,
                            doc,
                        )?;
                        let qualified_folded = field_token_value(field, token.searchable, true);
                        require_document_posting(
                            segment,
                            fields.field_folded_token,
                            &qualified_folded,
                            doc,
                        )?;
                        work.source_posting_checks = work
                            .source_posting_checks
                            .checked_add(4)
                            .ok_or(Error::SizeLimit)?;
                    }
                }
            } else {
                let source_fields = state.fields_for(entry.document.id).ok_or_else(|| {
                    TantivySource::corrupt(
                        "merged Tantivy row is outside the selected document state",
                    )
                })?;
                if field_count != source_fields.len() {
                    return Err(TantivySource::corrupt(
                        "merged Tantivy row field count disagrees with selected state",
                    )
                    .into());
                }
            }
            let entry = documents
                .live
                .get_mut(index)
                .ok_or_else(|| TantivySource::corrupt("Tantivy row ordinal is missing"))?;
            entry.address = Some(DocAddress::new(
                u32::try_from(segment_ord).map_err(|_| Error::SizeLimit)?,
                doc,
            ));
            entry.segment_id = Some(segment_id);
        }
    }
    if documents
        .live
        .iter()
        .any(|entry| entry.address.is_none() || entry.segment_id.is_none())
    {
        return Err(TantivySource::corrupt(
            "selected ordinal map has no exact resident Tantivy row address",
        )
        .into());
    }
    Ok(work)
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

    /// Returns the exact token-posting count represented by live rows.
    #[must_use]
    pub fn indexed_postings(&self) -> u64 {
        self.source().indexed_postings()
    }

    /// Counts complete exact Tantivy query scans on this adapter.
    #[must_use]
    pub fn rank_evaluations(&self) -> u64 {
        self.source().rank_evaluations()
    }

    /// Counts query-matching row documents advanced by exact Tantivy scorers.
    #[must_use]
    pub fn rank_docs_visited(&self) -> u64 {
        self.source().rank_docs_visited()
    }

    /// Returns the largest measured query scratch allocation for this adapter.
    #[must_use]
    pub fn rank_peak_scratch_bytes(&self) -> u64 {
        self.source().rank_peak_scratch_bytes()
    }

    /// Visits every exact lexical hit while retaining only one row payload.
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
    ) -> Result<Vec<(EntityId, Relevance)>, TantivySourceError> {
        self.source().relevance_for_candidates(query, candidates)
    }

    /// Compatibility no-op; query pages do not retain all-match rank state.
    ///
    /// # Errors
    ///
    /// Returns the source's no-op cache-clear result.
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

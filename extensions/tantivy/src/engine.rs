//! Real Tantivy-backed lexical source with canonical ranking at the adapter boundary.

use crate::{
    Binding, Cursor, DocumentState, Error, FieldSelection, LexicalPage, LexicalSource, Limits,
    MatchMode, OverlayLimits, Query, QueryRequest, QueryVersion, RankedHit, Relevance,
    SchemaVersion,
};
use backend_semantic::EntityId;
use backend_version::CoverageWitness;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::{collections::BTreeMap, path::Path};
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::ColumnValues;
use tantivy::{
    DocId, Index, IndexReader, Score, Term, doc,
    query::{BooleanQuery, FuzzyTermQuery, Occur, Query as TantivyQuery, TermQuery},
    schema::{FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema},
};

const WRITER_MEMORY_BYTES: usize = 15_000_000;
const BINDING_FILE: &str = "backend-binding-v2";

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
    documents: Vec<Option<LiveDocument>>,
    poisoned: bool,
    rank_cache: Mutex<Option<CachedRank>>,
    rank_evaluations: AtomicU64,
}

struct CachedRank {
    binding: Binding,
    query: QueryVersion,
    hits: Arc<[RankedHit]>,
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
}

impl std::fmt::Display for TantivySourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contract(error) => error.fmt(formatter),
            Self::Backend(error) => write!(formatter, "Tantivy backend failed: {error}"),
            Self::Io(error) => write!(formatter, "Tantivy projection I/O failed: {error}"),
            Self::Corrupt(detail) => write!(formatter, "Tantivy projection is corrupt: {detail}"),
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
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        let persisted = std::fs::read(directory.as_ref().join(BINDING_FILE))?;
        if persisted.as_slice() != projection_fingerprint(state.binding()) {
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
        let mut documents = Vec::new();
        for (document, fields) in state.iter() {
            documents.push(Some(LiveDocument {
                id: document,
                fields_digest: document_fields_digest(fields),
                postings: posting_count(fields)?,
            }));
        }
        let fields = projected.fields;
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
            poisoned: false,
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
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        let projected = projection_schema();
        let index = Index::create_in_dir(directory.as_ref(), projected.schema)?;
        let source = Self::populate(state, limits, index, projected.fields)?;
        std::fs::write(
            directory.as_ref().join(BINDING_FILE),
            projection_fingerprint(state.binding()),
        )?;
        Ok(source)
    }

    fn populate(
        state: &DocumentState,
        limits: Limits,
        index: Index,
        fields: ProjectionFields,
    ) -> Result<Self, TantivySourceError> {
        let mut writer = index.writer(WRITER_MEMORY_BYTES)?;
        let mut documents = Vec::new();
        for (document, document_fields) in state.iter() {
            let document_ordinal = u64::try_from(documents.len()).map_err(|_| Error::SizeLimit)?;
            let postings = write_fields(&writer, &fields, document_ordinal, document_fields)?;
            documents.push(Some(LiveDocument {
                id: document,
                fields_digest: document_fields_digest(document_fields),
                postings,
            }));
        }
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
            poisoned: false,
            rank_cache: Mutex::new(None),
            rank_evaluations: AtomicU64::new(0),
        })
    }

    /// Returns the number of token postings visible to the current reader.
    #[must_use]
    pub fn indexed_postings(&self) -> u64 {
        self.reader.searcher().num_docs()
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
        let mut ordinals = BTreeMap::new();
        for (ordinal, document) in self.documents.iter().enumerate() {
            let Some(document) = document else {
                continue;
            };
            if ordinals.insert(document.id, ordinal).is_some() {
                return Err(Self::corrupt("duplicate live document identity"));
            }
        }
        let mut next_fields = BTreeMap::new();
        for (id, fields) in next.iter() {
            if next_fields.insert(id, fields).is_some() {
                return Err(Error::MalformedInput.into());
            }
        }
        let mut rewritten = Vec::new();
        let mut removed = Vec::new();
        for (id, ordinal) in &ordinals {
            let Some(current) = self.documents.get(*ordinal).copied().flatten() else {
                return Err(Self::corrupt("live ordinal is empty"));
            };
            match next_fields.get(id) {
                Some(fields) if document_fields_digest(fields) == current.fields_digest => {}
                Some(_) => rewritten.push(*id),
                None => removed.push(*id),
            }
        }
        let mut added = Vec::new();
        for id in next_fields.keys() {
            if !ordinals.contains_key(id) {
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
        let mut documents = self.documents.clone();
        let mut deletes = Vec::new();
        let mut retired_postings = 0u64;
        for id in removed.iter().chain(rewritten.iter()) {
            let Some(ordinal) = ordinals.get(id).copied() else {
                return Err(Self::corrupt("planned document has no ordinal"));
            };
            let Some(slot) = documents.get_mut(ordinal) else {
                return Err(Self::corrupt("planned ordinal is outside the projection"));
            };
            let Some(current) = *slot else {
                return Err(Self::corrupt("planned ordinal is already empty"));
            };
            retired_postings = retired_postings
                .checked_add(u64::from(current.postings))
                .ok_or(Error::SizeLimit)?;
            deletes.push(u64::try_from(ordinal).map_err(|_| Error::SizeLimit)?);
            *slot = None;
        }
        let mut writes = Vec::new();
        for id in rewritten {
            let Some(ordinal) = ordinals.get(&id).copied() else {
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
            let ordinal = u64::try_from(documents.len()).map_err(|_| Error::SizeLimit)?;
            let Some(fields) = next_fields.get(&id) else {
                return Err(Self::corrupt("added document is missing its fields"));
            };
            documents.push(None);
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
        for write in plan.writes {
            let postings = write_fields(&writer, &fields, write.ordinal, &write.fields)?;
            added_postings = added_postings
                .checked_add(u64::from(postings))
                .ok_or(Error::SizeLimit)?;
            let ordinal = usize::try_from(write.ordinal).map_err(|_| Error::SizeLimit)?;
            let Some(slot) = plan.documents.get_mut(ordinal) else {
                return Err(Error::SizeLimit.into());
            };
            *slot = Some(LiveDocument {
                id: write.id,
                fields_digest: write.fields_digest,
                postings,
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
        self.documents = plan.documents;
        self.binding = next.binding();
        self.coverage = next.coverage();
        Ok(MaintainOutcome::Applied(ProjectionRevision {
            kind: ProjectionKind::Revised,
            rewritten_documents: plan.rewritten_documents,
            retired_postings: plan.retired_postings,
            added_postings,
        }))
    }

    /// Executes every clause against Tantivy, reconciles identities globally, then ranks.
    ///
    /// # Errors
    ///
    /// Returns a typed query-admission, index-read, or projection-integrity failure.
    pub fn search(&self, query: &Query) -> Result<Vec<RankedHit>, TantivySourceError> {
        self.ensure_live()?;
        query.validate(self.limits)?;
        if query.terms.is_empty() {
            return Ok(self
                .documents
                .iter()
                .filter_map(|document| document.map(|document| document.id))
                .map(|document| RankedHit {
                    document,
                    relevance: Relevance::all_documents(),
                })
                .collect());
        }
        let mut candidates = self.query_clause_candidates(&query.terms[0], query)?;
        for term in query.terms.iter().skip(1) {
            let posting = self.query_clause_candidates(term, query)?;
            let mut intersection = BTreeMap::new();
            for (document, relevance) in candidates {
                if let Some(next) = posting.get(&document) {
                    intersection.insert(document, relevance.combine(*next)?);
                }
            }
            candidates = intersection;
        }
        let mut hits = candidates
            .into_iter()
            .map(|(document, relevance)| RankedHit {
                document,
                relevance,
            })
            .collect::<Vec<_>>();
        hits.sort_unstable_by(|left, right| {
            right
                .relevance
                .cmp(&left.relevance)
                .then_with(|| left.document.cmp(&right.document))
        });
        Ok(hits)
    }

    fn query_clause_candidates(
        &self,
        term: &str,
        query: &Query,
    ) -> Result<BTreeMap<EntityId, Relevance>, TantivySourceError> {
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
        };
        let ranked_ordinals = searcher.search(engine_query.as_ref(), &collector)??;
        let mut ranked = BTreeMap::new();
        for (ordinal, relevance) in ranked_ordinals {
            let ordinal = usize::try_from(ordinal).map_err(|_| Error::SizeLimit)?;
            let document = self
                .documents
                .get(ordinal)
                .and_then(|document| document.map(|document| document.id))
                .ok_or_else(|| Self::corrupt("document ordinal is outside the binding"))?;
            ranked
                .entry(document)
                .and_modify(|current: &mut Relevance| *current = (*current).max(relevance))
                .or_insert(relevance);
        }
        Ok(ranked)
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

    fn ranked_hits(&self, query: &Query) -> Result<Arc<[RankedHit]>, TantivySourceError> {
        if let Some(hits) = self.cached_rank(query.version)? {
            return Ok(hits);
        }
        let hits = Arc::<[RankedHit]>::from(self.search(query)?);
        self.remember_rank(query.version, Arc::clone(&hits))?;
        self.rank_evaluations.fetch_add(1, Ordering::Relaxed);
        Ok(hits)
    }

    fn cached_rank(
        &self,
        query: QueryVersion,
    ) -> Result<Option<Arc<[RankedHit]>>, TantivySourceError> {
        let guard = self
            .rank_cache
            .lock()
            .map_err(|_| Self::corrupt("rank cache lock poisoned"))?;
        Ok(guard.as_ref().and_then(|cached| {
            (cached.binding == self.binding && cached.query == query)
                .then(|| Arc::clone(&cached.hits))
        }))
    }

    fn remember_rank(
        &self,
        query: QueryVersion,
        hits: Arc<[RankedHit]>,
    ) -> Result<(), TantivySourceError> {
        let mut guard = self
            .rank_cache
            .lock()
            .map_err(|_| Self::corrupt("rank cache lock poisoned"))?;
        *guard = Some(CachedRank {
            binding: self.binding,
            query,
            hits,
        });
        Ok(())
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
        let hits = self.ranked_hits(&request.query)?;
        let offset = request.cursor.map_or(0, Cursor::offset);
        if offset > hits.len() {
            return Err(Error::InvalidCursor.into());
        }
        let end = offset
            .checked_add(request.limit)
            .ok_or(Error::SizeLimit)?
            .min(hits.len());
        Ok(LexicalPage {
            schema: SchemaVersion::CURRENT,
            binding: self.binding,
            query: request.query.version,
            hits: hits[offset..end].to_vec(),
            next: (end < hits.len()).then(|| Cursor::new(self.binding, request.query.version, end)),
            total: hits.len(),
            coverage: self.coverage,
        })
    }
}

struct RevisionPlan {
    rewritten_documents: usize,
    retired_postings: u64,
    deletes: Vec<u64>,
    writes: Vec<PlannedWrite>,
    documents: Vec<Option<LiveDocument>>,
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

struct ClauseRankCollector {
    term_bytes: usize,
}

struct ClauseRankSegment {
    term_bytes: usize,
    ordinals: Arc<dyn ColumnValues<u64>>,
    weights: Arc<dyn ColumnValues<u64>>,
    ranking_bytes: Arc<dyn ColumnValues<u64>>,
    best: BTreeMap<u64, Relevance>,
    error: Option<Error>,
}

impl SegmentCollector for ClauseRankSegment {
    type Fruit = Result<BTreeMap<u64, Relevance>, Error>;

    fn collect(&mut self, doc: DocId, _score: Score) {
        if self.error.is_some() {
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
            .and_modify(|current| *current = (*current).max(relevance))
            .or_insert(relevance);
    }

    fn harvest(self) -> Self::Fruit {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.best),
        }
    }
}

impl Collector for ClauseRankCollector {
    type Fruit = Result<BTreeMap<u64, Relevance>, Error>;
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

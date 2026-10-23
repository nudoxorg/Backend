use backend_engine::{CoverageCapability, Row, RowId, ViewRoot, WorkspaceRoot};
use backend_extension_tantivy as lexical;
use backend_extension_trustfall::{
    SemanticQueryCorpus, SemanticQueryEvidence, SemanticQueryFact, SemanticQueryPresentation,
};
#[cfg(test)]
use backend_semantic::ir::SemanticCoreReader;
use backend_semantic::{Entity, EntityId, Source};
use backend_version::{CoverageWitness, RelationState};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const MAX_SEMANTIC_DOCUMENT_PAGE_ROWS: usize = 256;

/// One whitespace clause whose lexical term is the leaf and whose owner
/// segments must match the presentation parent chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QualifiedClause {
    leaf: String,
    owners: Vec<String>,
}

/// A validated bounded local text query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalQuery {
    lexical: lexical::Query,
    limit: usize,
    semantic_limit: usize,
    qualified: Vec<QualifiedClause>,
    /// The words as typed (case kept): what a match is placed by.
    words: Vec<String>,
}

impl LocalQuery {
    /// Creates a folded prefix query. Clauses use the same punctuation and
    /// identifier boundaries as indexed text, and every resulting term must
    /// match; valid dotted and `::` clauses keep their owner constraint.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::EmptyQuery`] for blank input and
    /// [`QueryError::InvalidLimit`] when text or page bounds are exceeded.
    pub fn prefix(text: &str, limit: usize) -> Result<Self, QueryError> {
        if text.trim().is_empty() {
            return Err(QueryError::EmptyQuery);
        }
        let limits = lexical::Limits::default();
        if text.len() > limits.max_field_bytes {
            return Err(QueryError::InvalidLimit);
        }
        if limit == 0 || limit > limits.max_page {
            return Err(QueryError::InvalidLimit);
        }
        let mut qualified = Vec::new();
        let words = text.split_whitespace().map(str::to_owned).collect();
        let mut searchable = String::with_capacity(text.len());
        for clause in text.split_whitespace() {
            if let Some(parsed) = parse_qualified_clause(clause) {
                searchable.push_str(&parsed.leaf);
                qualified.push(parsed);
            } else {
                searchable.push_str(clause);
            }
            searchable.push(' ');
        }
        let terms =
            lexical::normalize_query_terms(&searchable, limits).map_err(QueryError::Lexical)?;
        if terms.is_empty() {
            return Err(QueryError::EmptyQuery);
        }
        let lexical = lexical::Query::prefix(terms, limits).map_err(QueryError::Lexical)?;
        Ok(Self {
            lexical,
            limit,
            semantic_limit: limit,
            qualified,
            words,
        })
    }

    /// Maximum displayed rows.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    pub(crate) const fn semantic_limit(&self) -> usize {
        self.semantic_limit
    }

    pub(crate) const fn lexical(&self) -> &lexical::Query {
        &self.lexical
    }

    pub(crate) fn qualified_clauses(&self) -> &[QualifiedClause] {
        &self.qualified
    }

    /// The words as typed.
    #[must_use]
    pub fn words(&self) -> &[String] {
        &self.words
    }
}

/// What a match is placed by, before its lexical relevance: a person who
/// types `toml Value` means the declaration named `Value` in the package
/// named `toml`, not every row whose words start with both.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Placement {
    /// The row's own name.
    name: String,
    /// The name of the package it belongs to (`toml` for a registry tree
    /// `…/toml-0.8.23`), when it belongs to one.
    package: Option<String>,
    /// A declaration of somewhere else that this package links to.
    external: bool,
    /// The selected canonical row retains an actual source declaration site.
    sourced: bool,
    /// How much the row's kind is the thing its name names ([`kind_weight`]).
    kind: u8,
}

/// How much a row of `kind` is the thing its name names: a type (or the
/// package itself) before a callable, before a field or a module, before a
/// `use` that only brings the name in.
fn kind_weight(kind: &str) -> u8 {
    if kind == "project" {
        4
    } else {
        backend_library::declaration_name_priority(backend_library::DeclarationKind::from_name(
            kind,
        ))
    }
}

impl Placement {
    #[cfg(test)]
    pub(crate) fn for_test(name: &str, package: &str, kind: &str, external: bool) -> Self {
        Self {
            name: name.to_owned(),
            package: Some(package.to_owned()),
            external,
            sourced: false,
            kind: kind_weight(kind),
        }
    }

    /// The key a match is ordered by (higher first): a declaration before a
    /// link to one elsewhere; a row whose whole name is one of the words (as
    /// typed, then in any case); a row whose package another word names; an
    /// actual source declaration before an inferred helper type; a kind that
    /// is the named thing before one that only mentions it.
    pub(crate) fn key(&self, words: &[String]) -> (bool, u8, bool, bool, u8) {
        placement_key(
            &self.name,
            self.package.as_deref(),
            self.external,
            self.sourced,
            self.kind,
            words,
        )
    }
}

fn placement_key(
    name: &str,
    package: Option<&str>,
    external: bool,
    sourced: bool,
    kind: u8,
    words: &[String],
) -> (bool, u8, bool, bool, u8) {
    let named = words
        .iter()
        .position(|word| word == name)
        .map(|at| (2, at))
        .or_else(|| {
            words
                .iter()
                .position(|word| word.eq_ignore_ascii_case(name))
                .map(|at| (1, at))
        });
    let package_named = package.is_some_and(|package| {
        words.iter().enumerate().any(|(at, word)| {
            named.is_none_or(|(_, name_at)| at != name_at) && word.eq_ignore_ascii_case(package)
        })
    });
    (
        !external,
        named.map_or(0, |(strength, _)| strength),
        package_named,
        sourced,
        kind,
    )
}

/// The retained local prefix stores exactly the rank-budgeted identity/relevance
/// pairs. Its greatest (weakest) hit is at the root, so a complete hit scan
/// performs logarithmic retention work instead of shifting the entire prefix
/// for every match. The caller supplies one ordering for retention and output.
struct RetainedLocalRankPrefix {
    hits: Vec<(EntityId, lexical::Relevance)>,
    limit: usize,
}

impl RetainedLocalRankPrefix {
    fn new(limit: usize) -> Result<Self, std::collections::TryReserveError> {
        let mut hits = Vec::new();
        hits.try_reserve_exact(limit)?;
        Ok(Self { hits, limit })
    }

    fn offer(
        &mut self,
        hit: (EntityId, lexical::Relevance),
        compare: &impl Fn(&(EntityId, lexical::Relevance), &(EntityId, lexical::Relevance)) -> Ordering,
    ) {
        if self.limit == 0 {
            return;
        }
        if self.hits.len() < self.limit {
            self.hits.push(hit);
            let mut child = self.hits.len() - 1;
            while child > 0 {
                let parent = (child - 1) / 2;
                if !compare(&self.hits[parent], &self.hits[child]).is_lt() {
                    break;
                }
                self.hits.swap(parent, child);
                child = parent;
            }
            return;
        }
        if !compare(&hit, &self.hits[0]).is_lt() {
            return;
        }
        self.hits[0] = hit;
        let mut parent = 0;
        // Only a node before len/2 has a child. This also bounds 2*parent+1.
        while parent < self.hits.len() / 2 {
            let left = parent * 2 + 1;
            let right = left + 1;
            let child = if right < self.hits.len()
                && compare(&self.hits[left], &self.hits[right]).is_lt()
            {
                right
            } else {
                left
            };
            if !compare(&self.hits[parent], &self.hits[child]).is_lt() {
                break;
            }
            self.hits.swap(parent, child);
            parent = child;
        }
    }

    fn into_sorted_controlled<E>(
        mut self,
        compare: impl Fn(&(EntityId, lexical::Relevance), &(EntityId, lexical::Relevance)) -> Ordering,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Vec<(EntityId, lexical::Relevance)>, E> {
        super::controlled_sort::sort_unstable_by_in_place_checked(
            &mut self.hits,
            compare,
            checkpoint,
        )?;
        Ok(self.hits)
    }

    fn into_sorted(
        mut self,
        compare: impl Fn(&(EntityId, lexical::Relevance), &(EntityId, lexical::Relevance)) -> Ordering,
    ) -> Vec<(EntityId, lexical::Relevance)> {
        self.hits.sort_unstable_by(compare);
        self.hits
    }
}

/// A package's name from its label: the last part of its path, less the
/// version a registry tree carries (`…/toml-0.8.23` → `toml`,
/// `…/proc-macro2-1.0.107` → `proc-macro2`, `pkg:cargo/toml@0.8.23` →
/// `toml`); a person's own folder keeps its name (`toml_pin`).
pub(crate) fn package_name(label: &str) -> &str {
    let last = label.rsplit(['/', '\\']).next().unwrap_or(label);
    let last = last.split_once('@').map_or(last, |(name, _)| name);
    match last.rsplit_once('-') {
        Some((name, version))
            if !name.is_empty() && version.starts_with(|c: char| c.is_ascii_digit()) =>
        {
            name
        }
        _ => last,
    }
}

fn parse_qualified_clause(clause: &str) -> Option<QualifiedClause> {
    if !clause.contains('.') && !clause.contains("::") {
        return None;
    }
    let normalized = clause.replace("::", ".");
    let segments: Vec<&str> = normalized.split('.').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    let leaf = segments.last()?.to_ascii_lowercase();
    let owners = segments[..segments.len() - 1]
        .iter()
        .map(|segment| segment.to_ascii_lowercase())
        .collect();
    Some(QualifiedClause { leaf, owners })
}

/// Query lane identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lane {
    /// Canonical rows selected by the published local view.
    Canonical,
    /// Deterministic lexical ranking over every selected row.
    Lexical,
    /// Optional semantic ordering supplied by a bound ANN projection.
    Semantic,
}

/// What a lane is proven to cover.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverageBasis {
    /// Every row in the selected published view was considered.
    CompleteView {
        /// Cardinality of the closed canonical input relation.
        selected_rows: usize,
    },
    /// Every row with search evidence was considered; the view and its
    /// evidence disagree about the rest, which were left out rather than
    /// failing every query.
    PartialView {
        /// Rows considered.
        selected_rows: usize,
        /// What was left out, and why.
        left_out: LeftOut,
    },
    /// A lane may rank candidates but cannot change result coverage.
    CandidateSubset {
        /// Candidates admitted into the local result identity set.
        admitted: usize,
        /// Provider candidates absent from the local result identity set.
        suppressed: usize,
        /// Canonical rows contributed beyond the lexical match set.
        augmented: usize,
        /// Provider recall/approximation contract retained without upgrade.
        quality: backend_extension_qdrant::SearchQuality,
    },
    /// Optional work was not usable.
    Unavailable,
}

/// Freshness relative to the immutable view selected for this query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Freshness {
    /// The lane is tied to the exact selected view.
    Current,
    /// The optional lane named another workspace/view basis.
    Stale,
    /// The optional lane was not requested or failed before admission.
    Unknown,
}

/// Exact source inputs retained for auditing a lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceBasis {
    /// The owner-selected canonical view.
    Canonical {
        /// Exact workspace selected by the owner.
        workspace: WorkspaceRoot,
        /// Immutable published view version.
        view: backend_engine::ViewVersion,
        /// Canonical visible row relation root.
        relation: backend_engine::ViewStateRoot,
    },
    /// The complete typed lexical materialization and canonical query.
    Lexical {
        /// Complete materialization input binding.
        binding: lexical::Binding,
        /// Digest of the admitted compiler/structural evidence corpus.
        evidence: [u8; 32],
        /// Canonical lexical query identity.
        query: lexical::QueryVersion,
        /// Whether a bounded exact overlay was present.
        overlay: bool,
    },
    /// Exact current and immutable-base vector bindings plus model identity.
    Semantic {
        /// Immutable ANN base binding searched remotely.
        base: Box<backend_extension_qdrant::Binding>,
        /// Exact local vector facts binding used to rerank.
        current: Box<backend_extension_qdrant::Binding>,
        /// Immutable embedding model revision.
        model: backend_extension_qdrant::ModelVersion,
        /// Provider recall/approximation evidence retained after reranking.
        quality: backend_extension_qdrant::SearchQuality,
    },
}

/// Evidence for one independently observable lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneReport {
    /// Lane being reported.
    pub lane: Lane,
    /// Scope actually covered by the lane.
    pub coverage: CoverageBasis,
    /// Freshness against the selected view.
    pub freshness: Freshness,
    /// Exact source basis, when the lane produced admitted output.
    pub source: Option<SourceBasis>,
}

/// One ranked canonical row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RankedRow {
    /// Canonical row; the provider never supplies row payloads.
    pub row: Row,
    /// Deterministic lexical relevance retained after semantic ordering.
    pub lexical_relevance: Option<lexical::Relevance>,
}

/// One canonical document input selected from the immutable view.
///
/// The text is derived from the row's label, signature, and document
/// fragments in stable field order.  It is handed to a manifest-bound
/// embedding producer only after the coordinator has admitted the complete
/// selected view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticDocument {
    /// Canonical row identity that will become the vector candidate identity.
    pub row: RowId,
    /// Bounded document text supplied to the document-side model treatment.
    pub text: String,
}

/// Re-iterable bounded materialization of the document texts owned by one
/// selected corpus. Only the current page allocates strings.
pub(crate) struct SemanticDocumentPages<'a> {
    corpus: &'a Corpus,
    offset: usize,
    page_rows: NonZeroUsize,
}

impl Iterator for SemanticDocumentPages<'_> {
    type Item = Vec<SemanticDocument>;

    fn next(&mut self) -> Option<Self::Item> {
        let selected = &self.corpus.selected;
        let order = &selected.document_order;
        if self.offset >= order.len() {
            return None;
        }
        let end = self
            .offset
            .saturating_add(self.page_rows.get())
            .min(order.len());
        let mut page = Vec::with_capacity(end - self.offset);
        for selected_index in &order[self.offset..end] {
            let selected = selected.entities[*selected_index];
            let fact = &self.corpus.semantic_evidence.facts()[selected.fact_index];
            page.push(SemanticDocument {
                row: selected.row,
                text: semantic_text(fact.presentation()),
            });
        }
        self.offset = end;
        Some(page)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self
            .corpus
            .selected
            .document_order
            .len()
            .saturating_sub(self.offset);
        let pages = remaining.div_ceil(self.page_rows.get());
        (pages, Some(pages))
    }
}

impl ExactSizeIterator for SemanticDocumentPages<'_> {}

/// Local document pages reject the exact text extent before allocating a String.
/// The caller reserves page storage from the shared embedding owner first.
pub(crate) struct BoundedSemanticDocumentPages<'a> {
    corpus: &'a Corpus,
    offset: usize,
    page_rows: NonZeroUsize,
    maximum_text_bytes: usize,
}

impl BoundedSemanticDocumentPages<'_> {
    /// Computes the next page's exact UTF-8 extent without forming owned text.
    pub(crate) fn next_page_extent(&self) -> Result<Option<(usize, usize)>, QueryError> {
        let order = &self.corpus.selected.document_order;
        if self.offset >= order.len() {
            return Ok(None);
        }
        let end = self
            .offset
            .saturating_add(self.page_rows.get())
            .min(order.len());
        let mut bytes = 0usize;
        for index in &order[self.offset..end] {
            let selected = self.corpus.selected.entities[*index];
            let row = self.corpus.semantic_evidence.facts()[selected.fact_index].presentation();
            bytes = bytes
                .checked_add(semantic_text_extent(
                    row,
                    selected.row,
                    self.maximum_text_bytes,
                )?)
                .ok_or(QueryError::CorpusLimit)?;
        }
        Ok(Some((end - self.offset, bytes)))
    }
}

impl Iterator for BoundedSemanticDocumentPages<'_> {
    type Item = Result<Vec<SemanticDocument>, QueryError>;
    fn next(&mut self) -> Option<Self::Item> {
        let order = &self.corpus.selected.document_order;
        if self.offset >= order.len() {
            return None;
        }
        let end = self
            .offset
            .saturating_add(self.page_rows.get())
            .min(order.len());
        let result = (|| {
            let mut page = Vec::new();
            page.try_reserve_exact(end - self.offset)
                .map_err(|_| QueryError::CorpusLimit)?;
            for index in &order[self.offset..end] {
                let selected = self.corpus.selected.entities[*index];
                let row = self.corpus.semantic_evidence.facts()[selected.fact_index].presentation();
                page.push(SemanticDocument {
                    row: selected.row,
                    text: bounded_semantic_text(row, selected.row, self.maximum_text_bytes)?,
                });
            }
            Ok(page)
        })();
        self.offset = end;
        Some(result)
    }
}

pub(crate) struct Corpus {
    pub(crate) workspace: WorkspaceRoot,
    pub(crate) view: ViewRoot,
    pub(crate) coverage: CoverageWitness,
    pub(crate) lexical: lexical::TantivyAdapter,
    pub(crate) lexical_binding: lexical::Binding,
    // The exact admitted state used to build or advance `lexical`. Keeping its
    // persistent tree lets a later durable revision diff from the prior view
    // without rejoining that view's rows and semantic evidence.
    document_state: lexical::DocumentState,
    selected: SelectedCorpus,
    pub(crate) semantic_evidence: SemanticQueryCorpus,
}

/// Search placement uses the selected canonical source evidence, never a
/// rendered signature such as `unknown(unannotated)` or an external's label.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum SearchOrigin {
    ExternalTarget,
    Unsourced,
    SourceDeclaration,
}

impl SearchOrigin {
    fn from_source(source: &backend_library::SourceAvailability) -> Self {
        match source {
            backend_library::SourceAvailability::Captured(_)
            | backend_library::SourceAvailability::NotHydrated
            | backend_library::SourceAvailability::StaleFile { .. } => Self::SourceDeclaration,
            backend_library::SourceAvailability::NotCaptured
            | backend_library::SourceAvailability::Unconfigured => Self::Unsourced,
        }
    }
}

#[derive(Clone, Copy)]
struct SelectedEntity {
    entity: EntityId,
    row: RowId,
    candidate: backend_extension_qdrant::CandidateId,
    fact_index: usize,
    origin: SearchOrigin,
}

struct SelectedCorpus {
    entities: Box<[SelectedEntity]>,
    candidate_order: Box<[usize]>,
    document_order: Box<[usize]>,
    fact_order: Box<[usize]>,
    left_out: LeftOut,
}

impl SelectedCorpus {
    fn entity(&self, entity: EntityId) -> Option<SelectedEntity> {
        self.entities
            .binary_search_by_key(&entity, |entry| entry.entity)
            .ok()
            .map(|index| self.entities[index])
    }

    fn row(&self, row: RowId, workspace: WorkspaceRoot) -> Option<SelectedEntity> {
        let entity = entity_id(workspace, row).ok()?;
        self.entity(entity).filter(|entry| entry.row == row)
    }

    fn candidate(
        &self,
        candidate: backend_extension_qdrant::CandidateId,
    ) -> Option<SelectedEntity> {
        let order_index = self
            .candidate_order
            .binary_search_by_key(&candidate, |index| self.entities[*index].candidate)
            .ok()?;
        self.entities
            .get(self.candidate_order[order_index])
            .copied()
    }

    fn fact_index(&self, evidence: &SemanticQueryCorpus, id: &str) -> Option<usize> {
        let order_index = self
            .fact_order
            .binary_search_by(|index| evidence.facts()[*index].presentation().id.as_str().cmp(id))
            .ok()?;
        self.fact_order.get(order_index).copied()
    }
}

impl Corpus {
    /// The lexical revision already completed on this exclusively borrowed
    /// corpus. Move the same admitted document selection into place once.
    fn install_prepared(&mut self, prepared: PreparedCorpus) {
        self.workspace = prepared.workspace;
        self.view = prepared.view;
        self.coverage = prepared.coverage;
        self.lexical_binding = prepared.binding;
        self.selected = prepared.selected;
        self.semantic_evidence = prepared.semantic_evidence;
    }

    fn placement_key(
        &self,
        entity: EntityId,
        words: &[String],
    ) -> Option<(bool, u8, bool, bool, u8)> {
        let selected = self.selected.entity(entity)?;
        let presentation = self.semantic_evidence.facts()[selected.fact_index].presentation();
        let package = presentation
            .project
            .as_deref()
            .and_then(|project| self.selected.fact_index(&self.semantic_evidence, project))
            .map(|index| package_name(&self.semantic_evidence.facts()[index].presentation().name));
        let name = if presentation.kind == "project" {
            package_name(&presentation.name)
        } else {
            presentation.name.as_str()
        };
        Some(placement_key(
            name,
            package,
            selected.origin == SearchOrigin::ExternalTarget,
            selected.origin == SearchOrigin::SourceDeclaration,
            kind_weight(&presentation.kind),
            words,
        ))
    }

    pub(super) fn semantic_binding_matches(
        &self,
        binding: &backend_extension_qdrant::Binding,
    ) -> bool {
        binding.workspace == self.workspace
            && binding.authority == self.semantic_authority()
            && binding.read_manifest == self.semantic_read_manifest()
            && binding.frontier == self.semantic_frontier()
    }

    fn semantic_authority(&self) -> backend_extension_qdrant::Authority {
        backend_extension_qdrant::Authority::from_value(&self.semantic_evidence.evidence_digest())
    }

    fn semantic_read_manifest(&self) -> backend_extension_qdrant::ReadManifest {
        backend_extension_qdrant::ReadManifest::from_value(&semantic_read_identity(
            &self.view,
            self.semantic_evidence.evidence_digest(),
        ))
    }

    fn semantic_frontier(&self) -> backend_extension_qdrant::Frontier {
        backend_extension_qdrant::Frontier::from_value(self.view.frontier().root.as_bytes())
    }
}

/// Where the selected view and its typed search evidence did not pair up.
///
/// Each row of the view should have exactly one fact and each fact one row.
/// A disagreement is a producer defect, but it is one package's, and it does
/// not take search away from every other row: what does not pair is left
/// out, counted here, reported in the lane coverage, and said on the owner's
/// standard error once per selected view.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LeftOut {
    /// View rows no search evidence describes.
    pub rows_without_evidence: usize,
    /// Search evidence for rows the view does not hold.
    pub evidence_without_row: usize,
}

impl LeftOut {
    /// Whether everything paired.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.rows_without_evidence == 0 && self.evidence_without_row == 0
    }
}

/// A complete local answer. It is immediately renderable and is the only
/// input accepted by optional semantic acceleration.
#[derive(Clone)]
pub struct LocalAnswer {
    pub(crate) corpus: Arc<Corpus>,
    pub(crate) query: LocalQuery,
    /// Bounded display-page membership sorted by entity for point lookup.
    pub(crate) matches: Vec<(EntityId, lexical::Relevance)>,
    /// First bounded local page.
    pub rows: Vec<RankedRow>,
    /// Total complete local lexical matches before paging. Semantic
    /// augmentation does not rewrite this coverage fact.
    pub total_matches: usize,
    /// Canonical and lexical lane evidence.
    pub lanes: Vec<LaneReport>,
}

/// Exact scan receipt transferred to one semantic worker. Only a complete
/// LocalAnswer can construct it; it retains the identical admitted corpus Arc.
/// The original score allocation moves once, while display descriptors carry
/// canonical IDs rather than copied row/source payloads.
pub(super) struct LexicalSeed {
    corpus: Arc<Corpus>,
    query: LocalQuery,
    matches: Vec<(EntityId, lexical::Relevance)>,
    display: Vec<(RowId, Option<lexical::Relevance>)>,
    total_matches: usize,
    lanes: Vec<LaneReport>,
    _memory: backend_compile::EmbeddingMemoryReservation,
}
impl LexicalSeed {
    pub(super) fn admit_selection(&self, coordinator: &QueryCoordinator) -> Result<(), QueryError> {
        if Arc::ptr_eq(&self.corpus, &coordinator.corpus) {
            Ok(())
        } else {
            Err(QueryError::StaleViewBinding)
        }
    }
    pub(super) fn limit(&self) -> usize {
        self.query.limit()
    }
    pub(super) fn restore(
        self,
        coordinator: &QueryCoordinator,
        checkpoint: &mut impl FnMut() -> Result<(), super::LocalSemanticError>,
    ) -> Result<(LocalAnswer, backend_compile::EmbeddingMemoryReservation), super::LocalSemanticError>
    {
        checkpoint()?;
        self.admit_selection(coordinator)?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(self.display.len())?;
        for (id, relevance) in self.display {
            checkpoint()?;
            let row = self.corpus.view.row(id).ok_or(QueryError::InvalidView)?;
            rows.push(RankedRow {
                row,
                lexical_relevance: relevance,
            });
        }
        checkpoint()?;
        Ok((
            LocalAnswer {
                corpus: self.corpus,
                query: self.query,
                matches: self.matches,
                rows,
                total_matches: self.total_matches,
                lanes: self.lanes,
            },
            self._memory,
        ))
    }
}

/// Canonical lexical scores for an arbitrary candidate collection. Callers
/// can ask for one score without relying on the vector's sort order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CandidateLexicalScores(Vec<(EntityId, lexical::Relevance)>);

impl CandidateLexicalScores {
    fn from_unsorted(mut scores: Vec<(EntityId, lexical::Relevance)>) -> Self {
        scores.sort_unstable_by_key(|(entity, _)| *entity);
        scores.dedup_by_key(|(entity, _)| *entity);
        Self(scores)
    }

    pub(super) fn score(&self, entity: EntityId) -> Option<lexical::Relevance> {
        self.0
            .binary_search_by_key(&entity, |(candidate, _)| *candidate)
            .ok()
            .map(|index| self.0[index].1)
    }

    #[cfg(test)]
    pub(super) fn as_slice(&self) -> &[(EntityId, lexical::Relevance)] {
        &self.0
    }
}

impl LocalAnswer {
    /// Reserves the transferred score allocation and new bounded ID descriptors
    /// before moving them into the worker. The initial page keeps its rows.
    pub(super) fn take_lexical_seed(
        &mut self,
        budget: &backend_compile::EmbeddingMemoryBudget,
        checkpoint: &mut impl FnMut() -> Result<(), super::LocalSemanticError>,
    ) -> Result<LexicalSeed, super::LocalSemanticError> {
        checkpoint()?;
        let bytes = self
            .matches
            .capacity()
            .checked_mul(std::mem::size_of::<(EntityId, lexical::Relevance)>())
            .and_then(|bytes| {
                bytes.checked_add(
                    self.rows
                        .len()
                        .checked_mul(std::mem::size_of::<(RowId, Option<lexical::Relevance>)>())?,
                )
            })
            .and_then(|bytes| {
                bytes.checked_add(
                    self.lanes
                        .len()
                        .checked_mul(std::mem::size_of::<LaneReport>())?,
                )
            })
            .and_then(|bytes| bytes.checked_add(3 * 256))
            .ok_or(super::LocalSemanticError::CorpusEnvelope)?;
        let memory = budget
            .reserve(u64::try_from(bytes).map_err(|_| super::LocalSemanticError::CorpusEnvelope)?)
            .map_err(super::LocalSemanticError::Memory)?;
        let mut display = Vec::new();
        display.try_reserve_exact(self.rows.len())?;
        for row in &self.rows {
            checkpoint()?;
            display.push((row.row.id, row.lexical_relevance));
        }
        let mut lanes = Vec::new();
        lanes.try_reserve_exact(self.lanes.len())?;
        for lane in &self.lanes {
            checkpoint()?;
            lanes.push(lane.clone());
        }
        checkpoint()?;
        Ok(LexicalSeed {
            corpus: Arc::clone(&self.corpus),
            query: self.query.clone(),
            matches: std::mem::take(&mut self.matches),
            display,
            total_matches: self.total_matches,
            lanes,
            _memory: memory,
        })
    }

    /// Returns the bounded local page's canonical identities in display order.
    ///
    /// The coordinator resolves provider identities through its selected
    /// immutable view, so missing projection entries cannot cross into a
    /// command reply.
    #[must_use]
    pub fn ranked_row_ids(&self) -> Vec<RowId> {
        self.rows.iter().map(|ranked| ranked.row.id).collect()
    }

    pub(crate) fn lexical_relevance(&self, entity: EntityId) -> Option<lexical::Relevance> {
        self.matches
            .binary_search_by_key(&entity, |(candidate, _)| *candidate)
            .ok()
            .map(|index| self.matches[index].1)
    }

    /// Resolves a bounded ANN page against the exact lexical query without
    /// retaining the full lexical hit set in this answer.
    pub(super) fn lexical_relevance_for_candidates(
        &self,
        entities: &[EntityId],
    ) -> Result<CandidateLexicalScores, QueryError> {
        let mut remaining = BTreeSet::new();
        let mut relevance = Vec::new();
        relevance
            .try_reserve_exact(entities.len())
            .map_err(|error| QueryError::LexicalProvider {
                phase: LexicalPhase::CandidateScores,
                cause: LexicalFailureCause::Allocation {
                    requested: entities.len(),
                    error,
                },
            })?;
        for entity in entities {
            if let Some(score) = self.lexical_relevance(*entity) {
                relevance.push((*entity, score));
            } else {
                remaining.insert(*entity);
            }
        }
        if !remaining.is_empty() {
            let requested = remaining.into_iter().collect::<Vec<_>>();
            for (entity, score) in self
                .corpus
                .lexical
                .relevance_for_candidates(self.query.lexical(), &requested)
                .map_err(|error| QueryError::provider(LexicalPhase::CandidateScores, error))?
            {
                let Some(selected) = self.corpus.selected.entity(entity) else {
                    return Err(QueryError::LexicalProvider {
                        phase: LexicalPhase::CandidateScores,
                        cause: LexicalFailureCause::MissingSelectedEntity(entity),
                    });
                };
                let row_id = selected.row.stable_key();
                if self.query.qualified_clauses().is_empty()
                    || qualified_row_matches(
                        row_id.as_str(),
                        self.query.qualified_clauses(),
                        &self.corpus.selected,
                        &self.corpus.semantic_evidence,
                    )
                {
                    relevance.push((entity, score));
                }
            }
        }
        Ok(CandidateLexicalScores::from_unsorted(relevance))
    }

    // Worker-only fallible path. Candidate batches retain their existing finite page bound.
    pub(super) fn lexical_relevance_for_candidates_controlled<
        E: From<QueryError> + From<lexical::TantivySourceError>,
    >(
        &self,
        entities: &[EntityId],
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<CandidateLexicalScores, E> {
        let mut remaining = BTreeSet::new();
        let mut relevance = Vec::new();
        relevance
            .try_reserve_exact(entities.len())
            .map_err(|error| QueryError::LexicalProvider {
                phase: LexicalPhase::CandidateScores,
                cause: LexicalFailureCause::Allocation {
                    requested: entities.len(),
                    error,
                },
            })?;
        for entity in entities {
            checkpoint()?;
            if let Some(score) = self.lexical_relevance(*entity) {
                relevance.push((*entity, score));
            } else {
                remaining.insert(*entity);
            }
        }
        if !remaining.is_empty() {
            let requested = remaining.into_iter().collect::<Vec<_>>();
            for (entity, score) in self.corpus.lexical.relevance_for_candidates_controlled(
                self.query.lexical(),
                &requested,
                checkpoint,
            )? {
                checkpoint()?;
                let Some(selected) = self.corpus.selected.entity(entity) else {
                    return Err(QueryError::LexicalProvider {
                        phase: LexicalPhase::CandidateScores,
                        cause: LexicalFailureCause::MissingSelectedEntity(entity),
                    }
                    .into());
                };
                let row_id = selected.row.stable_key();
                if self.query.qualified_clauses().is_empty()
                    || qualified_row_matches_controlled(
                        row_id.as_str(),
                        self.query.qualified_clauses(),
                        &self.corpus.selected,
                        &self.corpus.semantic_evidence,
                        checkpoint,
                    )?
                {
                    relevance.push((entity, score));
                }
            }
        }
        Ok(CandidateLexicalScores::from_unsorted(relevance))
    }

    pub(super) fn candidate_entity(
        &self,
        candidate: backend_extension_qdrant::CandidateId,
    ) -> Option<EntityId> {
        self.corpus
            .selected
            .candidate(candidate)
            .map(|selected| selected.entity)
    }

    pub(super) fn candidate_row(
        &self,
        candidate: backend_extension_qdrant::CandidateId,
    ) -> Option<(EntityId, Row)> {
        let selected = self.corpus.selected.candidate(candidate)?;
        let row = self.corpus.view.row(selected.row)?;
        Some((selected.entity, row))
    }

    pub(super) fn candidate_for_entity(
        &self,
        entity: EntityId,
    ) -> Option<backend_extension_qdrant::CandidateId> {
        self.corpus
            .selected
            .entity(entity)
            .map(|selected| selected.candidate)
    }

    pub(super) fn entity_for_row(&self, row: RowId) -> Option<EntityId> {
        self.corpus
            .selected
            .row(row, self.corpus.workspace)
            .map(|selected| selected.entity)
    }

    pub(super) fn search_origin(&self, row: RowId) -> Option<SearchOrigin> {
        let selected = &self.corpus.selected;
        selected
            .document_order
            .binary_search_by_key(&row, |index| selected.entities[*index].row)
            .ok()
            .map(|at| selected.entities[selected.document_order[at]].origin)
    }

    pub(super) fn selected_row_count(&self) -> usize {
        self.corpus.selected.entities.len()
    }

    /// Converts the local answer into the final result without attempting a
    /// semantic lane.
    #[must_use]
    pub fn finish(mut self) -> QueryResult {
        self.lanes.push(LaneReport {
            lane: Lane::Semantic,
            coverage: CoverageBasis::Unavailable,
            freshness: Freshness::Unknown,
            source: None,
        });
        QueryResult {
            rows: self.rows,
            total_matches: self.total_matches,
            lanes: self.lanes,
        }
    }
}

/// Final reconciled result.
#[derive(Clone, Debug)]
pub struct QueryResult {
    /// Bounded canonical rows in final order.
    pub rows: Vec<RankedRow>,
    /// Complete local lexical match cardinality.
    pub total_matches: usize,
    /// Explicit evidence for every lane.
    pub lanes: Vec<LaneReport>,
}

/// Immutable coordinator over one owner-selected view.
#[derive(Clone)]
pub struct QueryCoordinator {
    pub(super) corpus: Arc<Corpus>,
}

/// How the resident search snapshot absorbed the latest published view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SnapshotMaintenance {
    /// The published selection already matched the resident snapshot.
    Reused,
    /// An immutable selected root was restored from the durable cache.
    Restored,
    /// Lexical fields were unchanged, so only the binding stamp moved.
    Rebound,
    /// A bounded document edit was written into the resident Tantivy index.
    Revised {
        /// Documents added, removed, or rewritten. Untouched documents kept
        /// their postings.
        rewritten_documents: usize,
    },
    /// The resident projection was replaced by a complete build.
    Rebuilt,
}

/// Owns the immutable local search materialization selected by the current
/// published product view.
///
/// An unchanged selection reuses the coordinator. A changed selection keeps
/// the resident Tantivy index when the lexical document edit fits the
/// maintenance budget, and replaces it completely otherwise.
#[derive(Default)]
pub struct SearchSnapshotOwner {
    selected: Option<QueryCoordinator>,
    durable_root: Option<PathBuf>,
    durable_budget: lexical::DurableCacheBudget,
    builds: u64,
    opens: u64,
    maintenance: Option<SnapshotMaintenance>,
    corpus: Option<SemanticQueryCorpus>,
    corpus_builds: u64,
}

impl SearchSnapshotOwner {
    /// Uses a workspace-local durable projection cache for cold restarts.
    /// Every opened projection is still admitted against the exact selected
    /// view and complete lexical binding supplied by the owner.
    #[must_use]
    pub fn with_durable_root(root: impl Into<PathBuf>) -> Self {
        Self {
            durable_root: Some(root.into()),
            ..Self::default()
        }
    }

    /// Sets the typed byte budget used by this owner's durable index role.
    #[must_use]
    pub fn with_durable_cache_budget(mut self, budget: lexical::DurableCacheBudget) -> Self {
        self.durable_budget = budget;
        self
    }

    /// Selects the coordinator for one exact immutable product snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when the selected view cannot form a complete,
    /// bounded local search snapshot.
    pub fn select(
        &mut self,
        workspace: WorkspaceRoot,
        view: ViewRoot,
        expected_view_capability: CoverageCapability,
        coverage: CoverageWitness,
        semantic_evidence: SemanticQueryCorpus,
    ) -> Result<&QueryCoordinator, QueryError> {
        self.select_controlled(
            workspace,
            view,
            expected_view_capability,
            coverage,
            semantic_evidence,
            None,
        )
    }

    pub(crate) fn select_controlled(
        &mut self,
        workspace: WorkspaceRoot,
        view: ViewRoot,
        expected_view_capability: CoverageCapability,
        coverage: CoverageWitness,
        semantic_evidence: SemanticQueryCorpus,
        cancelled: Option<&AtomicBool>,
    ) -> Result<&QueryCoordinator, QueryError> {
        preparation_checkpoint(cancelled, "lexical-select")?;
        validate_view_binding(&view, &expected_view_capability)?;
        if self.selected.as_ref().is_some_and(|selected| {
            selected.matches_selection(workspace, &view, coverage, &semantic_evidence)
        }) {
            self.maintenance = Some(SnapshotMaintenance::Reused);
            return self.selected.as_ref().ok_or(QueryError::InvalidView);
        }
        // One admitted current corpus serves both the revision attempt and a
        // replacement build. A retained old answer can prohibit mutation, and
        // lexical maintenance can request a rebuild, without repeating this
        // complete row/evidence join and document-state construction.
        let prepared = prepare_corpus_controlled(
            workspace,
            view,
            &expected_view_capability,
            coverage,
            semantic_evidence,
            cancelled,
        )?;
        let revised = match self.selected.as_mut() {
            Some(selected) => selected.try_revise(
                &prepared,
                self.durable_root.as_deref(),
                self.durable_budget,
                cancelled,
            ),
            None => Ok(None),
        };
        match revised {
            Ok(Some((maintenance, corpus))) => {
                corpus.install_prepared(prepared);
                match maintenance {
                    SnapshotMaintenance::Rebuilt => {
                        self.builds = self.builds.saturating_add(1);
                    }
                    SnapshotMaintenance::Restored => {
                        self.opens = self.opens.saturating_add(1);
                    }
                    SnapshotMaintenance::Reused
                    | SnapshotMaintenance::Rebound
                    | SnapshotMaintenance::Revised { .. } => {}
                }
                self.maintenance = Some(maintenance);
                return self.selected.as_ref().ok_or(QueryError::InvalidView);
            }
            Ok(None) => {}
            Err(error) => {
                if error.permits_ephemeral_revision_rebuild() && self.durable_root.is_none() {
                    self.selected = None;
                } else {
                    return Err(error);
                }
            }
        }
        let (selected, action) = QueryCoordinator::from_prepared_corpus(
            prepared,
            self.durable_root.clone(),
            self.durable_budget,
            cancelled,
        )?;
        self.selected = Some(selected);
        match action {
            lexical::DurableProjectionAction::Opened => {
                self.opens = self.opens.saturating_add(1);
                self.maintenance = Some(SnapshotMaintenance::Restored);
            }
            lexical::DurableProjectionAction::Built => {
                self.builds = self.builds.saturating_add(1);
                self.maintenance = Some(SnapshotMaintenance::Rebuilt);
            }
            lexical::DurableProjectionAction::Revised => {
                self.maintenance = Some(SnapshotMaintenance::Revised {
                    rewritten_documents: 0,
                });
            }
        }
        self.selected.as_ref().ok_or(QueryError::InvalidView)
    }

    /// Transfers only projection ownership. The owner loop retains its admitted
    /// immutable semantic corpus and can continue other reads during preparation.
    pub(crate) fn take_projection(&mut self) -> Self {
        Self {
            selected: self.selected.take(),
            durable_root: self.durable_root.clone(),
            durable_budget: self.durable_budget,
            builds: self.builds,
            opens: self.opens,
            maintenance: self.maintenance,
            corpus: self.corpus.clone(),
            corpus_builds: 0,
        }
    }

    pub(crate) fn restore_projection(&mut self, mut projection: Self, admit: bool) {
        self.selected = if admit {
            projection.selected.take()
        } else {
            None
        };
        self.builds = projection.builds;
        self.opens = projection.opens;
        self.maintenance = if admit { projection.maintenance } else { None };
        self.corpus_builds = self.corpus_builds.saturating_add(projection.corpus_builds);
        if admit && projection.corpus.is_some() {
            self.corpus = projection.corpus.take();
        }
    }

    pub(crate) fn selected_for(
        &self,
        workspace: WorkspaceRoot,
        view: &ViewRoot,
    ) -> Option<&QueryCoordinator> {
        self.selected.as_ref().filter(|selected| {
            selected.corpus.workspace == workspace && selected.corpus.view.root() == view.root()
        })
    }

    /// Returns how the most recent [`Self::select`] treated the resident index.
    #[must_use]
    pub(crate) fn maintenance(&self) -> Option<SnapshotMaintenance> {
        self.maintenance
    }

    /// Returns how many complete Tantivy builds this owner has performed.
    #[must_use]
    pub(crate) fn projection_builds(&self) -> u64 {
        self.builds
    }

    /// Returns how many durable selected roots this owner restored from disk.
    #[must_use]
    pub(crate) fn projection_opens(&self) -> u64 {
        self.opens
    }

    /// Returns the corpus admitted for `workspace`, building it at most once.
    ///
    /// The corpus is a pure function of that workspace snapshot. A later call
    /// with the same root clones the admitted `Arc` and does not call `build`.
    /// A build error leaves the previous corpus in place.
    pub fn shared_corpus<E>(
        &mut self,
        workspace: WorkspaceRoot,
        build: impl FnOnce() -> Result<SemanticQueryCorpus, E>,
    ) -> Result<SemanticQueryCorpus, E> {
        if let Some(cached) = &self.corpus
            && cached.workspace() == workspace
        {
            return Ok(cached.clone());
        }
        let built = build()?;
        self.corpus_builds = self.corpus_builds.saturating_add(1);
        if built.workspace() == workspace {
            self.corpus = Some(built.clone());
        }
        Ok(built)
    }

    /// How many times [`Self::shared_corpus`] has admitted a new corpus.
    #[must_use]
    pub(crate) fn corpus_builds(&self) -> u64 {
        self.corpus_builds
    }

    /// Returns the corpus already admitted for `workspace`.
    #[must_use]
    pub fn resident_corpus(&self, workspace: WorkspaceRoot) -> Option<SemanticQueryCorpus> {
        self.corpus
            .as_ref()
            .filter(|cached| cached.workspace() == workspace)
            .cloned()
    }

    /// Admits the corpus for `workspace`, running `prepare` only on a miss.
    ///
    /// A resident corpus is cloned before `prepare` runs, so a warm search
    /// does not page the workspace source relation. A prepare or build error
    /// leaves any previous corpus in place and does not count as an admission.
    pub fn admit_corpus<S, E>(
        &mut self,
        workspace: WorkspaceRoot,
        prepare: impl FnOnce() -> Result<S, E>,
        build: impl FnOnce(S) -> Result<SemanticQueryCorpus, E>,
    ) -> Result<SemanticQueryCorpus, E> {
        if let Some(cached) = self.resident_corpus(workspace) {
            return Ok(cached);
        }
        let prepared = prepare()?;
        self.shared_corpus(workspace, || build(prepared))
    }
}

impl QueryCoordinator {
    /// Builds the exact local lexical materialization for a selected view.
    ///
    /// The supplied witness must come from the workspace owner. A digest or
    /// row list alone cannot manufacture complete local coverage.
    ///
    /// # Errors
    ///
    /// Returns a typed [`QueryError`] when ownership is incomplete, the view
    /// is incoherent, a document exceeds its lexical budget, or the lexical
    /// projection rejects input.
    pub fn new(
        workspace: WorkspaceRoot,
        view: ViewRoot,
        expected_view_capability: CoverageCapability,
        coverage: CoverageWitness,
        semantic_evidence: SemanticQueryCorpus,
    ) -> Result<Self, QueryError> {
        Self::new_with_durable_root(
            workspace,
            view,
            &expected_view_capability,
            coverage,
            semantic_evidence,
            None,
            lexical::DurableCacheBudget::default(),
            None,
        )
        .map(|(coordinator, _)| coordinator)
    }

    fn new_with_durable_root(
        workspace: WorkspaceRoot,
        view: ViewRoot,
        expected_view_capability: &CoverageCapability,
        coverage: CoverageWitness,
        semantic_evidence: SemanticQueryCorpus,
        durable_root: Option<PathBuf>,
        durable_budget: lexical::DurableCacheBudget,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(Self, lexical::DurableProjectionAction), QueryError> {
        let prepared = prepare_corpus_controlled(
            workspace,
            view,
            expected_view_capability,
            coverage,
            semantic_evidence,
            cancelled,
        )?;
        Self::from_prepared_corpus(prepared, durable_root, durable_budget, cancelled)
    }

    fn from_prepared_corpus(
        prepared: PreparedCorpus,
        durable_root: Option<PathBuf>,
        durable_budget: lexical::DurableCacheBudget,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(Self, lexical::DurableProjectionAction), QueryError> {
        let (source, action) = match (durable_root.as_deref(), cancelled) {
            (Some(root), Some(cancelled)) => lexical::TantivySource::open_or_build_cancelable(
                &prepared.state,
                lexical::Limits::default(),
                root,
                durable_budget,
                cancelled,
            ),
            (None, Some(cancelled)) => lexical::TantivySource::build_cancelable(
                &prepared.state,
                lexical::Limits::default(),
                cancelled,
            )
            .map(|source| (source, lexical::DurableProjectionAction::Built)),
            (Some(root), None) => {
                lexical::TantivySource::open_or_build_in_dir_with_budget_and_action(
                    &prepared.state,
                    lexical::Limits::default(),
                    root,
                    durable_budget,
                )
            }
            (None, None) => {
                lexical::TantivySource::build(&prepared.state, lexical::Limits::default())
                    .map(|source| (source, lexical::DurableProjectionAction::Built))
            }
        }
        .map_err(|error| QueryError::provider(LexicalPhase::OpenProjection, error))?;
        let lexical =
            lexical::TantivyAdapter::new(source, lexical::Limits::default()).map_err(|error| {
                QueryError::provider(
                    LexicalPhase::AdmitAdapter,
                    lexical::TantivySourceError::Contract(error),
                )
            })?;
        Ok((
            Self {
                corpus: Arc::new(Corpus {
                    workspace: prepared.workspace,
                    view: prepared.view,
                    coverage: prepared.coverage,
                    lexical,
                    lexical_binding: prepared.binding,
                    document_state: prepared.state,
                    selected: prepared.selected,
                    semantic_evidence: prepared.semantic_evidence,
                }),
            },
            action,
        ))
    }

    fn try_revise(
        &mut self,
        prepared: &PreparedCorpus,
        durable_root: Option<&Path>,
        durable_budget: lexical::DurableCacheBudget,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Option<(SnapshotMaintenance, &mut Corpus)>, QueryError> {
        // Existing readers own the old immutable corpus. Detect that before
        // replacing its document state; the replacement still consumes the
        // identical already-admitted current corpus above.
        let Some(corpus) = Arc::get_mut(&mut self.corpus) else {
            return Ok(None);
        };
        // Durable revision cannot reuse postings across workspace identities.
        // The caller already admitted the complete current corpus; return it
        // to the normal exact open/build path without reconstructing old rows
        // solely for a revision that must rebuild anyway.
        if durable_root.is_some() && corpus.workspace != prepared.workspace {
            return Ok(None);
        }
        let previous_state = if durable_root.is_some() {
            Some(corpus.document_state.clone())
        } else {
            None
        };
        let durable_publication = match (durable_root, previous_state.as_ref(), cancelled) {
            (Some(root), Some(previous), Some(cancelled)) => Some(
                lexical::TantivySource::open_or_advance_cancelable(
                    previous,
                    &prepared.state,
                    lexical::Limits::default(),
                    lexical::OverlayLimits::default(),
                    root,
                    durable_budget,
                    cancelled,
                )
                .map_err(|error| QueryError::provider(LexicalPhase::AdvanceProjection, error))?,
            ),
            (Some(root), Some(previous), None) => Some(
                lexical::TantivySource::open_or_advance_in_dir_with_budget_and_action(
                    previous,
                    &prepared.state,
                    lexical::Limits::default(),
                    lexical::OverlayLimits::default(),
                    root,
                    durable_budget,
                )
                .map_err(|error| QueryError::provider(LexicalPhase::AdvanceProjection, error))?,
            ),
            _ => None,
        };
        let maintenance = if let Some((source, revision, action)) = durable_publication {
            corpus.lexical = lexical::TantivyAdapter::new(source, lexical::Limits::default())
                .map_err(|error| {
                    QueryError::provider(
                        LexicalPhase::AdmitAdapter,
                        lexical::TantivySourceError::Contract(error),
                    )
                })?;
            match (revision, action) {
                (Some(revision), _) => match revision.kind {
                    lexical::ProjectionKind::Rebound => SnapshotMaintenance::Rebound,
                    lexical::ProjectionKind::Revised => SnapshotMaintenance::Revised {
                        rewritten_documents: revision.rewritten_documents,
                    },
                },
                (None, lexical::DurableProjectionAction::Opened) => SnapshotMaintenance::Restored,
                (None, _) => SnapshotMaintenance::Rebuilt,
            }
        } else {
            let maintained = match cancelled {
                Some(cancelled) => corpus.lexical.maintain_cancelable(
                    &prepared.state,
                    lexical::OverlayLimits::default(),
                    cancelled,
                ),
                None => corpus
                    .lexical
                    .maintain(&prepared.state, lexical::OverlayLimits::default()),
            };
            match maintained
                .map_err(|error| QueryError::provider(LexicalPhase::MaintainProjection, error))?
            {
                lexical::MaintainOutcome::RebuildRequired => return Ok(None),
                lexical::MaintainOutcome::Applied(revision) => match revision.kind {
                    lexical::ProjectionKind::Rebound => SnapshotMaintenance::Rebound,
                    lexical::ProjectionKind::Revised => SnapshotMaintenance::Revised {
                        rewritten_documents: revision.rewritten_documents,
                    },
                },
            }
        };
        corpus.document_state = prepared.state.clone();
        Ok(Some((maintenance, corpus)))
    }

    fn matches_selection(
        &self,
        workspace: WorkspaceRoot,
        view: &ViewRoot,
        coverage: CoverageWitness,
        semantic_evidence: &SemanticQueryCorpus,
    ) -> bool {
        self.corpus.workspace == workspace
            && self.corpus.view.root() == view.root()
            && self.corpus.coverage == coverage
            && self.corpus.semantic_evidence.evidence_digest()
                == semantic_evidence.evidence_digest()
    }

    /// Enumerates the requested rank prefix and one successor, under the
    /// selected relation and the provider's existing retained-rank budget.
    pub(crate) fn search_local_page(
        &self,
        request: &backend_engine::Query,
    ) -> Result<LocalAnswer, QueryError> {
        self.search_local(self.local_page_query(request)?)
    }

    /// Initial selected page traversal checks the real selected lifetime while
    /// preserving the existing cursor/rank-prefix admission and ordering.
    pub(crate) fn search_local_page_controlled<
        E: From<QueryError>
            + From<lexical::TantivySourceError>
            + From<std::collections::TryReserveError>,
    >(
        &self,
        request: &backend_engine::Query,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<LocalAnswer, E> {
        checkpoint()?;
        self.search_local_controlled(self.local_page_query(request)?, checkpoint)
    }

    /// Checks the exact page shape without allocating a second normalized query.
    /// The caller must also compare the original text; normalization is deterministic
    /// for that same text and these same rank-prefix/semantic bounds.
    pub(crate) fn matches_local_page_shape(
        &self,
        local: &LocalQuery,
        request: &backend_engine::Query,
    ) -> bool {
        if request.basis() != self.corpus.view.root() {
            return false;
        }
        let page_rows = usize::from(request.limit().get());
        let offset = request.cursor().map_or(Ok(0), |cursor| {
            usize::try_from(cursor.query_offset()).map_err(|_| QueryError::InvalidCursor)
        });
        let selected_rows = usize::try_from(self.corpus.view.row_count());
        let (Ok(offset), Ok(selected_rows)) = (offset, selected_rows) else {
            return false;
        };
        rank_prefix_limit(offset, page_rows, selected_rows)
            .is_ok_and(|limit| local.limit() == limit && local.semantic_limit() == page_rows)
    }

    fn local_page_query(&self, request: &backend_engine::Query) -> Result<LocalQuery, QueryError> {
        if request.basis() != self.corpus.view.root() {
            return Err(QueryError::StaleViewBinding);
        }
        let page_rows = usize::from(request.limit().get());
        let offset = request.cursor().map_or(Ok(0), |cursor| {
            usize::try_from(cursor.query_offset()).map_err(|_| QueryError::InvalidCursor)
        })?;
        let selected_rows =
            usize::try_from(self.corpus.view.row_count()).map_err(|_| QueryError::InvalidView)?;
        let mut query = LocalQuery::prefix(request.text(), page_rows)?;
        query.limit = rank_prefix_limit(offset, page_rows, selected_rows)?;
        // The semantic candidate window is independent of the requested page
        // position, so extending a lexical prefix cannot change prior ranks.
        query.semantic_limit = page_rows;
        Ok(query)
    }

    /// Executes a complete local search before optional provider work.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::LexicalProvider`] if the admitted local Tantivy
    /// projection cannot serve a page.
    pub fn search_local(&self, query: LocalQuery) -> Result<LocalAnswer, QueryError> {
        let mut retained = RetainedLocalRankPrefix::new(query.limit()).map_err(|error| {
            QueryError::LexicalProvider {
                phase: LexicalPhase::ComposeRows,
                cause: LexicalFailureCause::Allocation {
                    requested: query.limit(),
                    error,
                },
            }
        })?;
        let mut seen_hits = 0usize;
        let mut total_matches = 0usize;
        let mut composition_failure = None;
        let words = query.words();
        let compare = |left: &(EntityId, lexical::Relevance),
                       right: &(EntityId, lexical::Relevance)| {
            self.corpus
                .placement_key(left.0, words)
                .cmp(&self.corpus.placement_key(right.0, words))
                .reverse()
                .then_with(|| left.1.cmp(&right.1).reverse())
                // Provider IDs include the workspace namespace. Canonical
                // rows in an identical view must not change tie order when
                // that namespace changes after a fresh workspace/index run.
                .then_with(|| {
                    self.corpus
                        .selected
                        .entity(left.0)
                        .map(|selected| selected.row)
                        .cmp(
                            &self
                                .corpus
                                .selected
                                .entity(right.0)
                                .map(|selected| selected.row),
                        )
                })
        };
        let exact_lexical_total = self
            .corpus
            .lexical
            .for_each_ranked_hit(query.lexical(), |hit| {
                let Some(selected) = self.corpus.selected.entity(hit.document) else {
                    composition_failure
                        .get_or_insert(LexicalFailureCause::MissingSelectedEntity(hit.document));
                    return;
                };
                seen_hits = match seen_hits.checked_add(1) {
                    Some(total) => total,
                    None => {
                        composition_failure.get_or_insert(LexicalFailureCause::HitCountOverflow);
                        return;
                    }
                };
                if !query.qualified_clauses().is_empty()
                    && !qualified_row_matches(
                        selected.row.stable_key().as_str(),
                        query.qualified_clauses(),
                        &self.corpus.selected,
                        &self.corpus.semantic_evidence,
                    )
                {
                    return;
                }
                total_matches = match total_matches.checked_add(1) {
                    Some(total) => total,
                    None => {
                        composition_failure.get_or_insert(LexicalFailureCause::ResultCountOverflow);
                        return;
                    }
                };
                retained.offer((hit.document, hit.relevance), &compare);
            })
            .map_err(|error| QueryError::provider(LexicalPhase::ScanHits, error))?;
        if let Some(cause) = composition_failure {
            return Err(QueryError::LexicalProvider {
                phase: LexicalPhase::ComposeRows,
                cause,
            });
        }
        if exact_lexical_total != seen_hits {
            return Err(QueryError::LexicalProvider {
                phase: LexicalPhase::ComposeRows,
                cause: LexicalFailureCause::HitCountMismatch {
                    provider: exact_lexical_total,
                    selected: seen_hits,
                },
            });
        }
        let top_matches = retained.into_sorted(compare);
        let mut rows = Vec::with_capacity(top_matches.len());
        for (entity, relevance) in &top_matches {
            let selected =
                self.corpus
                    .selected
                    .entity(*entity)
                    .ok_or(QueryError::LexicalProvider {
                        phase: LexicalPhase::ComposeRows,
                        cause: LexicalFailureCause::MissingSelectedEntity(*entity),
                    })?;
            let row = self
                .corpus
                .view
                .row(selected.row)
                .ok_or(QueryError::LexicalProvider {
                    phase: LexicalPhase::ComposeRows,
                    cause: LexicalFailureCause::MissingViewRow(selected.row),
                })?;
            rows.push(RankedRow {
                row,
                lexical_relevance: Some(*relevance),
            });
        }
        let mut matches = top_matches;
        matches.sort_unstable_by_key(|(entity, _)| *entity);
        let selected_rows = self.corpus.selected.entities.len();
        let considered = if self.corpus.selected.left_out.is_empty() {
            CoverageBasis::CompleteView { selected_rows }
        } else {
            CoverageBasis::PartialView {
                selected_rows,
                left_out: self.corpus.selected.left_out,
            }
        };
        let canonical = SourceBasis::Canonical {
            workspace: self.corpus.workspace,
            view: self.corpus.view.version(),
            relation: self.corpus.view.root(),
        };
        let lexical = SourceBasis::Lexical {
            binding: self.corpus.lexical_binding,
            evidence: self.corpus.semantic_evidence.evidence_digest(),
            query: query.lexical().version,
            overlay: false,
        };
        Ok(LocalAnswer {
            corpus: Arc::clone(&self.corpus),
            query,
            matches,
            rows,
            total_matches,
            lanes: vec![
                LaneReport {
                    lane: Lane::Canonical,
                    coverage: considered,
                    freshness: Freshness::Current,
                    source: Some(canonical),
                },
                LaneReport {
                    lane: Lane::Lexical,
                    coverage: considered,
                    freshness: Freshness::Current,
                    source: Some(lexical),
                },
            ],
        })
    }

    /// Worker lexical execution: checkpoints inside scoring and before each retained row.
    pub(super) fn search_local_controlled<
        E: From<QueryError>
            + From<lexical::TantivySourceError>
            + From<std::collections::TryReserveError>,
    >(
        &self,
        query: LocalQuery,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<LocalAnswer, E> {
        checkpoint()?;
        let mut retained = RetainedLocalRankPrefix::new(query.limit()).map_err(|error| {
            QueryError::LexicalProvider {
                phase: LexicalPhase::ComposeRows,
                cause: LexicalFailureCause::Allocation {
                    requested: query.limit(),
                    error,
                },
            }
        })?;
        let mut seen_hits = 0usize;
        let mut total_matches = 0usize;
        let mut composition_failure = None;
        let words = query.words();
        let compare = |left: &(EntityId, lexical::Relevance),
                       right: &(EntityId, lexical::Relevance)| {
            self.corpus
                .placement_key(left.0, words)
                .cmp(&self.corpus.placement_key(right.0, words))
                .reverse()
                .then_with(|| left.1.cmp(&right.1).reverse())
                // Provider IDs include the workspace namespace. Canonical
                // rows in an identical view must not change tie order when
                // that namespace changes after a fresh workspace/index run.
                .then_with(|| {
                    self.corpus
                        .selected
                        .entity(left.0)
                        .map(|selected| selected.row)
                        .cmp(
                            &self
                                .corpus
                                .selected
                                .entity(right.0)
                                .map(|selected| selected.row),
                        )
                })
        };
        let exact_lexical_total = self.corpus.lexical.for_each_ranked_hit_controlled(
            query.lexical(),
            |hit, checkpoint| {
                let Some(selected) = self.corpus.selected.entity(hit.document) else {
                    composition_failure
                        .get_or_insert(LexicalFailureCause::MissingSelectedEntity(hit.document));
                    return Ok(());
                };
                seen_hits = match seen_hits.checked_add(1) {
                    Some(total) => total,
                    None => {
                        composition_failure.get_or_insert(LexicalFailureCause::HitCountOverflow);
                        return Ok(());
                    }
                };
                if !query.qualified_clauses().is_empty()
                    && !qualified_row_matches_controlled(
                        selected.row.stable_key().as_str(),
                        query.qualified_clauses(),
                        &self.corpus.selected,
                        &self.corpus.semantic_evidence,
                        checkpoint,
                    )?
                {
                    return Ok(());
                }
                total_matches = match total_matches.checked_add(1) {
                    Some(total) => total,
                    None => {
                        composition_failure.get_or_insert(LexicalFailureCause::ResultCountOverflow);
                        return Ok(());
                    }
                };
                checkpoint()?;
                retained.offer((hit.document, hit.relevance), &compare);
                Ok(())
            },
            checkpoint,
        )?;
        if let Some(cause) = composition_failure {
            return Err(QueryError::LexicalProvider {
                phase: LexicalPhase::ComposeRows,
                cause,
            }
            .into());
        }
        if exact_lexical_total != seen_hits {
            return Err(QueryError::LexicalProvider {
                phase: LexicalPhase::ComposeRows,
                cause: LexicalFailureCause::HitCountMismatch {
                    provider: exact_lexical_total,
                    selected: seen_hits,
                },
            }
            .into());
        }
        let mut top_matches = retained.into_sorted_controlled(compare, checkpoint)?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(top_matches.len())?;
        for (entity, relevance) in &top_matches {
            checkpoint()?;
            let selected =
                self.corpus
                    .selected
                    .entity(*entity)
                    .ok_or(QueryError::LexicalProvider {
                        phase: LexicalPhase::ComposeRows,
                        cause: LexicalFailureCause::MissingSelectedEntity(*entity),
                    })?;
            let row = self
                .corpus
                .view
                .row(selected.row)
                .ok_or(QueryError::LexicalProvider {
                    phase: LexicalPhase::ComposeRows,
                    cause: LexicalFailureCause::MissingViewRow(selected.row),
                })?;
            rows.push(RankedRow {
                row,
                lexical_relevance: Some(*relevance),
            });
        }
        super::controlled_sort::sort_unstable_in_place_checked(
            &mut top_matches,
            |entry| entry.0,
            checkpoint,
        )?;
        let matches = top_matches;
        let selected_rows = self.corpus.selected.entities.len();
        let considered = if self.corpus.selected.left_out.is_empty() {
            CoverageBasis::CompleteView { selected_rows }
        } else {
            CoverageBasis::PartialView {
                selected_rows,
                left_out: self.corpus.selected.left_out,
            }
        };
        let canonical = SourceBasis::Canonical {
            workspace: self.corpus.workspace,
            view: self.corpus.view.version(),
            relation: self.corpus.view.root(),
        };
        let lexical = SourceBasis::Lexical {
            binding: self.corpus.lexical_binding,
            evidence: self.corpus.semantic_evidence.evidence_digest(),
            query: query.lexical().version,
            overlay: false,
        };
        Ok(LocalAnswer {
            corpus: Arc::clone(&self.corpus),
            query,
            matches,
            rows,
            total_matches,
            lanes: vec![
                LaneReport {
                    lane: Lane::Canonical,
                    coverage: considered,
                    freshness: Freshness::Current,
                    source: Some(canonical),
                },
                LaneReport {
                    lane: Lane::Lexical,
                    coverage: considered,
                    freshness: Freshness::Current,
                    source: Some(lexical),
                },
            ],
        })
    }

    /// Iterates the selected document inputs in stable row order. Each page
    /// owns only its bounded text strings, and a caller can create another
    /// iterator over the same immutable selection.
    pub(crate) fn semantic_document_pages(
        &self,
        page_rows: NonZeroUsize,
    ) -> Result<SemanticDocumentPages<'_>, QueryError> {
        if page_rows.get() > MAX_SEMANTIC_DOCUMENT_PAGE_ROWS {
            return Err(QueryError::InvalidLimit);
        }
        Ok(SemanticDocumentPages {
            corpus: &self.corpus,
            offset: 0,
            page_rows,
        })
    }

    /// Checks every exact document extent without text allocation before local inference.
    pub(crate) fn admit_semantic_text_bounds_controlled<E: From<QueryError>>(
        &self,
        maximum: usize,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<usize, E> {
        let mut maximum_observed = 0usize;
        for index in &self.corpus.selected.document_order {
            checkpoint()?;
            let selected = self.corpus.selected.entities[*index];
            let row = self.corpus.semantic_evidence.facts()[selected.fact_index].presentation();
            maximum_observed =
                maximum_observed.max(semantic_text_extent(row, selected.row, maximum)?);
        }
        Ok(maximum_observed)
    }

    /// Borrowed maximum of the actual canonical row allocations copied by composition.
    /// Shared source/excerpt Arcs are retained without charging their bytes a second time.
    pub(crate) fn semantic_row_clone_extent_controlled<E: From<QueryError>>(
        &self,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<usize, E> {
        let mut maximum = 0usize;
        for index in &self.corpus.selected.document_order {
            checkpoint()?;
            let selected = self.corpus.selected.entities[*index];
            let row = self
                .corpus
                .view
                .row_ref(selected.row)
                .ok_or(QueryError::InvalidView)?;
            let mut bytes = std::mem::size_of::<Row>();
            for extent in [
                row.label.len(),
                row.signature.as_ref().map_or(0, String::len),
                row.identity_preimage()
                    .map_or(0, |preimage| preimage.as_str().len()),
                row.document
                    .len()
                    .checked_mul(std::mem::size_of::<backend_engine::Fragment>())
                    .ok_or(QueryError::CorpusLimit)?,
                row.facts
                    .deprecation
                    .present()
                    .map_or(0, backend_compile::Deprecation::text_bytes),
            ] {
                bytes = bytes.checked_add(extent).ok_or(QueryError::CorpusLimit)?;
            }
            for fragment in row.document.iter() {
                checkpoint()?;
                let extent = match fragment {
                    backend_engine::Fragment::Text(text) | backend_engine::Fragment::Code(text) => {
                        text.len()
                    }
                    backend_engine::Fragment::Link { label, .. } => label.len(),
                    backend_engine::Fragment::Break => 0,
                };
                bytes = bytes.checked_add(extent).ok_or(QueryError::CorpusLimit)?;
            }
            maximum = maximum.max(bytes);
        }
        Ok(maximum)
    }

    pub(crate) fn bounded_semantic_document_pages(
        &self,
        page_rows: NonZeroUsize,
        maximum_text_bytes: usize,
    ) -> Result<BoundedSemanticDocumentPages<'_>, QueryError> {
        if page_rows.get() > MAX_SEMANTIC_DOCUMENT_PAGE_ROWS || maximum_text_bytes == 0 {
            return Err(QueryError::InvalidLimit);
        }
        Ok(BoundedSemanticDocumentPages {
            corpus: &self.corpus,
            offset: 0,
            page_rows,
            maximum_text_bytes,
        })
    }

    /// Returns the exact number of rows requiring document embeddings for
    /// this immutable selection.
    #[must_use]
    pub fn semantic_document_count(&self) -> usize {
        self.corpus.selected.document_order.len()
    }

    /// Exact existing lexical binding for an opt-in worker observation.
    pub(crate) fn preparation_diagnostic_binding(&self) -> lexical::Binding {
        self.corpus.lexical_binding
    }

    /// Returns the typed workspace identity selected by this coordinator.
    #[must_use]
    pub(crate) fn workspace_root(&self) -> WorkspaceRoot {
        self.corpus.workspace
    }

    /// Checks the non-root portion of a semantic binding against this exact
    /// selected view.  The read manifest and frontier include the immutable
    /// view identity, while the candidate root is derived from the document
    /// relation itself.
    #[must_use]
    pub(crate) fn semantic_binding_matches(
        &self,
        binding: &backend_extension_qdrant::Binding,
    ) -> bool {
        self.corpus.semantic_binding_matches(binding)
    }

    /// Derives the only vector binding accepted for this selected view.
    /// Callers use it to build exact local vector facts and to configure a
    /// verified [`backend_extension_qdrant::QdrantHttpSource`].
    #[must_use]
    pub fn semantic_binding(
        &self,
        root: backend_extension_qdrant::Root,
        recipe: backend_extension_qdrant::Recipe,
    ) -> backend_extension_qdrant::Binding {
        backend_extension_qdrant::Binding::new(
            self.corpus.workspace,
            root,
            recipe,
            self.corpus.semantic_authority(),
            self.corpus.semantic_read_manifest(),
        )
        .with_frontier(self.corpus.semantic_frontier())
    }

    /// Returns the deterministic Qdrant identity for one canonical row.
    /// Rows outside this coordinator's immutable selection return `None`.
    #[must_use]
    pub fn semantic_candidate(&self, row: RowId) -> Option<backend_extension_qdrant::CandidateId> {
        let entity = entity_id(self.corpus.workspace, row).ok()?;
        let candidate =
            candidate_id(entity, self.corpus.semantic_evidence.evidence_digest()).ok()?;
        self.corpus
            .selected
            .row(row, self.corpus.workspace)
            .filter(|selected| selected.entity == entity && selected.candidate == candidate)
            .map(|_| candidate)
    }
}

fn qualified_row_matches(
    row_id: &str,
    clauses: &[QualifiedClause],
    selected: &SelectedCorpus,
    evidence: &SemanticQueryCorpus,
) -> bool {
    qualified_row_matches_controlled(row_id, clauses, selected, evidence, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or_else(|never| match never {})
}

fn qualified_row_matches_controlled<E>(
    row_id: &str,
    clauses: &[QualifiedClause],
    selected: &SelectedCorpus,
    evidence: &SemanticQueryCorpus,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    for clause in clauses {
        checkpoint()?;
        if !qualified_clause_matches_controlled(row_id, clause, selected, evidence, checkpoint)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
fn qualified_clause_matches(
    row_id: &str,
    clause: &QualifiedClause,
    selected: &SelectedCorpus,
    evidence: &SemanticQueryCorpus,
) -> bool {
    qualified_clause_matches_controlled(row_id, clause, selected, evidence, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or_else(|never| match never {})
}

fn qualified_clause_matches_controlled<E>(
    row_id: &str,
    clause: &QualifiedClause,
    selected: &SelectedCorpus,
    evidence: &SemanticQueryCorpus,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(candidate_index) = selected.fact_index(evidence, row_id) else {
        return Ok(false);
    };
    let Some(candidate) = evidence.facts().get(candidate_index) else {
        return Ok(false);
    };
    let mut current = candidate.presentation();
    if !current
        .name
        .to_ascii_lowercase()
        .starts_with(clause.leaf.as_str())
    {
        return Ok(false);
    }
    for (owner_index, owner) in clause.owners.iter().enumerate().rev() {
        checkpoint()?;
        let parent = current.parent.as_deref().and_then(|parent_id| {
            selected
                .fact_index(evidence, parent_id)
                .and_then(|index| evidence.facts().get(index))
        });
        if let Some(parent) = parent {
            let parent_presentation = parent.presentation();
            if parent_presentation.name.eq_ignore_ascii_case(owner) {
                current = parent_presentation;
                continue;
            }
            let parent_is_package_scope = matches!(
                parent.evidence(),
                SemanticQueryEvidence::Package(scope)
                    if matches!(candidate.evidence(), SemanticQueryEvidence::Compiler(compiler) if scope.package() == compiler.package())
            );
            if owner_index == 0
                && parent_is_package_scope
                && cargo_package_alias_matches_controlled(
                    candidate, owner, evidence, selected, checkpoint,
                )?
            {
                return Ok(true);
            }
            return Ok(false);
        }
        if owner_index == 0
            && cargo_package_alias_matches_controlled(
                candidate, owner, evidence, selected, checkpoint,
            )?
        {
            return Ok(true);
        }
        return Ok(false);
    }
    Ok(true)
}

fn cargo_package_alias_matches_controlled<E>(
    candidate: &SemanticQueryFact,
    owner: &str,
    evidence: &SemanticQueryCorpus,
    selected: &SelectedCorpus,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let SemanticQueryEvidence::Compiler(compiler) = candidate.evidence() else {
        return Ok(false);
    };
    let package = compiler.package();
    let profile = compiler.profile();
    let package_row = RowId::Package(package).stable_key();
    let Some(package_index) = selected.fact_index(evidence, &package_row) else {
        return Ok(false);
    };
    let Some(package_fact) = evidence.facts().get(package_index) else {
        return Ok(false);
    };
    let SemanticQueryEvidence::Package(scope) = package_fact.evidence() else {
        return Ok(false);
    };
    if scope.package() != package {
        return Ok(false);
    }
    let Some(aliases) = scope.cargo_aliases() else {
        return Ok(false);
    };
    let Some(observation) = aliases.observation(profile) else {
        return Ok(false);
    };
    for index in observation.alias_indices() {
        checkpoint()?;
        if aliases
            .aliases()
            .get(usize::from(*index))
            .is_some_and(|alias| alias.as_str().eq_ignore_ascii_case(owner))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

struct PreparedCorpus {
    workspace: WorkspaceRoot,
    view: ViewRoot,
    coverage: CoverageWitness,
    binding: lexical::Binding,
    state: lexical::DocumentState,
    selected: SelectedCorpus,
    semantic_evidence: SemanticQueryCorpus,
}

#[cfg(test)]
std::thread_local! {
    // Opt-in only: unrelated and parallel tests retain no observations or
    // allocations, and no second global allocator is introduced.
    static PREPARED_CORPORA: std::cell::RefCell<Option<Vec<backend_library::ViewStateRoot>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn prepare_corpus(
    workspace: WorkspaceRoot,
    view: ViewRoot,
    expected_view_capability: &CoverageCapability,
    coverage: CoverageWitness,
    semantic_evidence: SemanticQueryCorpus,
) -> Result<PreparedCorpus, QueryError> {
    prepare_corpus_controlled(
        workspace,
        view,
        expected_view_capability,
        coverage,
        semantic_evidence,
        None,
    )
}

fn preparation_checkpoint(
    cancelled: Option<&AtomicBool>,
    point: &'static str,
) -> Result<(), QueryError> {
    crate::builtin::check_search_preparation(cancelled, point)
        .map_err(|_| QueryError::SelectedViewRetired)
}

fn prepare_corpus_controlled(
    workspace: WorkspaceRoot,
    view: ViewRoot,
    expected_view_capability: &CoverageCapability,
    coverage: CoverageWitness,
    semantic_evidence: SemanticQueryCorpus,
    cancelled: Option<&AtomicBool>,
) -> Result<PreparedCorpus, QueryError> {
    preparation_checkpoint(cancelled, "lexical-prepare")?;
    validate_view_binding(&view, expected_view_capability)?;
    if !matches!(
        coverage,
        CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
    ) {
        return Err(QueryError::IncompleteCoverage);
    }
    if !view.is_coherent()
        || view.capability().is_none()
        || !view
            .coverage()
            .iter()
            .any(|coverage| coverage.is_complete())
    {
        return Err(QueryError::IncompleteCoverage);
    }
    if semantic_evidence.workspace() != workspace {
        return Err(QueryError::InvalidSemanticEvidence);
    }
    let (documents, selected) =
        collect_selected_documents(workspace, &view, &semantic_evidence, cancelled)?;
    let left_out = selected.left_out;
    if !left_out.is_empty() {
        eprintln!(
            "locald search: the view and its search evidence disagree ({} rows without evidence, {} evidence without a row); those are left out of search",
            left_out.rows_without_evidence, left_out.evidence_without_row
        );
    }
    preparation_checkpoint(cancelled, "lexical-relation")?;
    let state = RelationState::<lexical::IndexRelation>::from_entries(documents, coverage)
        .map_err(|_| QueryError::InvalidView)?;
    preparation_checkpoint(cancelled, "lexical-binding")?;
    let binding = lexical::Binding::new(
        workspace,
        state.root(),
        lexical::Recipe::from_value(view.recipe().as_bytes()),
        lexical::Authority::from_value(&semantic_evidence.evidence_digest()),
        lexical::ReadManifest::from_value(&semantic_read_identity(
            &view,
            semantic_evidence.evidence_digest(),
        )),
    )
    .with_frontier(lexical::Frontier::from_value(
        view.frontier().root.as_bytes(),
    ));
    preparation_checkpoint(cancelled, "lexical-document-state")?;
    let state = lexical::DocumentState::from_relation(binding, state, lexical::Limits::default())
        .map_err(QueryError::Lexical)?;
    preparation_checkpoint(cancelled, "lexical-complete")?;
    #[cfg(test)]
    PREPARED_CORPORA.with_borrow_mut(|prepared| {
        if let Some(prepared) = prepared {
            prepared.push(view.root());
        }
    });
    Ok(PreparedCorpus {
        workspace,
        view,
        coverage,
        binding,
        state,
        selected,
        semantic_evidence,
    })
}

#[cfg(test)]
mod prepared_corpus_tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    struct Measurement;
    impl Measurement {
        fn start() -> Self {
            PREPARED_CORPORA.with_borrow_mut(|work| *work = Some(Vec::new()));
            Self
        }
        fn take(&self) -> Vec<backend_library::ViewStateRoot> {
            PREPARED_CORPORA.with_borrow_mut(|work| work.replace(Vec::new()).unwrap_or_default())
        }
    }
    impl Drop for Measurement {
        fn drop(&mut self) {
            PREPARED_CORPORA.with_borrow_mut(|work| *work = None);
        }
    }

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> TestResult<Self> {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "nudox-search-prepare-{}-{unique}",
                std::process::id()
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            builder.create(&path)?;
            Ok(Self(path.canonicalize()?))
        }
        fn owner(&self, durable: bool, name: &str) -> SearchSnapshotOwner {
            if durable {
                SearchSnapshotOwner::with_durable_root(self.0.join(name))
            } else {
                SearchSnapshotOwner::default()
            }
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn view_capability(view: &ViewRoot) -> TestResult<CoverageCapability> {
        view.capability()
            .ok_or_else(|| std::io::Error::other("missing view capability").into())
    }

    #[derive(Debug)]
    struct ViewFixtureError(backend_library::ViewError);

    impl std::fmt::Display for ViewFixtureError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "renamed view fixture rejected: {:?}", self.0)
        }
    }

    impl std::error::Error for ViewFixtureError {}

    fn renamed_view(view: &ViewRoot) -> TestResult<ViewRoot> {
        relabeled_view(view, "alpha exact", "zephyr marker")
    }

    fn relabeled_view(view: &ViewRoot, from: &str, to: &str) -> TestResult<ViewRoot> {
        let mut rows = view.rows().to_vec();
        rows.iter_mut()
            .find(|row| row.label == from)
            .ok_or_else(|| std::io::Error::other("missing fixture symbol"))?
            .label = to.to_owned();
        let renamed = ViewRoot::new_checked(
            view.recipe(),
            view.basis(),
            view.frontier(),
            rows,
            vec![backend_engine::ViewCoverage::Complete],
            view_capability(view)?,
        )
        .map_err(|error| Box::new(ViewFixtureError(error)) as Box<dyn std::error::Error>)?;
        Ok(renamed)
    }

    fn select(
        owner: &mut SearchSnapshotOwner,
        workspace: WorkspaceRoot,
        view: &ViewRoot,
    ) -> TestResult {
        owner.select(
            workspace,
            view.clone(),
            view_capability(view)?,
            crate::builtin::admitted_coverage()?,
            super::super::tests::semantic_evidence(workspace, view),
        )?;
        Ok(())
    }

    // Preserve the original failed-revision path as a positive work control:
    // current preparation, optional previous preparation, the failed exclusive
    // borrow, then the real replacement constructor's repeated current work.
    fn legacy_shared_replacement(
        owner: &mut SearchSnapshotOwner,
        workspace: WorkspaceRoot,
        view: &ViewRoot,
    ) -> TestResult {
        let capability = view_capability(view)?;
        let coverage = crate::builtin::admitted_coverage()?;
        let evidence = super::super::tests::semantic_evidence(workspace, view);
        let old = owner
            .selected
            .as_mut()
            .ok_or_else(|| std::io::Error::other("missing previous selected snapshot"))?;
        let _current = prepare_corpus(
            workspace,
            view.clone(),
            &capability,
            coverage,
            evidence.clone(),
        )?;
        let _previous = owner
            .durable_root
            .as_ref()
            .map(|_| -> TestResult<PreparedCorpus> {
                prepare_corpus(
                    old.corpus.workspace,
                    old.corpus.view.clone(),
                    &view_capability(&old.corpus.view)?,
                    old.corpus.coverage,
                    old.corpus.semantic_evidence.clone(),
                )
                .map_err(Into::into)
            })
            .transpose()?;
        assert!(
            Arc::get_mut(&mut old.corpus).is_none(),
            "retained real answer prevents revision"
        );
        drop(_previous);
        drop(_current);
        let (replacement, _) = QueryCoordinator::new_with_durable_root(
            workspace,
            view.clone(),
            &capability,
            coverage,
            evidence,
            owner.durable_root.clone(),
            owner.durable_budget,
            None,
        )?;
        owner.selected = Some(replacement);
        Ok(())
    }

    fn search(owner: &SearchSnapshotOwner, query: &str) -> TestResult<LocalAnswer> {
        let selected = owner
            .selected
            .as_ref()
            .ok_or_else(|| std::io::Error::other("missing selected snapshot"))?;
        Ok(selected.search_local(LocalQuery::prefix(query, 4)?)?)
    }

    fn assert_equivalent_searches(
        baseline: &SearchSnapshotOwner,
        candidate: &SearchSnapshotOwner,
        view: &ViewRoot,
    ) -> TestResult {
        for query in ["zephyr", "alphabet", "exact"] {
            let baseline_answer = search(baseline, query)?;
            let candidate_answer = search(candidate, query)?;
            assert_eq!(
                candidate_answer.total_matches,
                baseline_answer.total_matches
            );
            assert_eq!(
                candidate_answer.ranked_row_ids(),
                baseline_answer.ranked_row_ids()
            );
            assert_eq!(candidate_answer.corpus.view.root(), view.root());
        }
        Ok(())
    }

    fn large_preparation_fixture() -> TestResult<(WorkspaceRoot, ViewRoot)> {
        let (workspace, base) = super::super::tests::selected_view();
        let package = backend_engine::package_key("pkg");
        let rows = std::iter::once(Row::new(RowId::Package(package), base.basis(), "pkg"))
            .chain((0..4095).map(|index| {
                let name = format!("corpus-item-{index:05}");
                Row::in_package(
                    RowId::Symbol(backend_engine::symbol_key(&name)),
                    base.basis(),
                    package,
                    name,
                )
                .with_document(vec![backend_engine::Fragment::Text(
                    "captured source text".to_owned(),
                )])
            }))
            .collect();
        let view = ViewRoot::new_checked(
            base.recipe(),
            base.basis(),
            base.frontier(),
            rows,
            vec![backend_engine::ViewCoverage::Complete],
            view_capability(&base)?,
        )
        .map_err(|error| Box::new(ViewFixtureError(error)) as Box<dyn std::error::Error>)?;
        Ok((workspace, view))
    }

    #[test]
    fn cancelled_corpus_preparation_stops_inside_rows_facts_and_sort_boundaries() -> TestResult {
        use std::sync::atomic::Ordering;
        let (workspace, view) = large_preparation_fixture()?;
        let evidence = super::super::tests::semantic_evidence(workspace, &view);
        let capability = view_capability(&view)?;
        let coverage = crate::builtin::admitted_coverage()?;
        let baseline = prepare_corpus(
            workspace,
            view.clone(),
            &capability,
            coverage,
            evidence.clone(),
        )?;
        let token = Arc::new(AtomicBool::new(true));
        assert!(matches!(
            prepare_corpus_controlled(
                workspace,
                view.clone(),
                &capability,
                coverage,
                evidence.clone(),
                Some(&token)
            ),
            Err(QueryError::SelectedViewRetired)
        ));
        for (point, visits) in [
            ("lexical-row", 2048),
            ("lexical-fact-index", 2048),
            ("lexical-fact", 2048),
            ("lexical-sort-entities", 1),
            ("lexical-sort-entities-step", 1000),
            ("lexical-sort-candidates-step", 1000),
            ("lexical-sort-documents-step", 1000),
            ("lexical-sort-facts-step", 1000),
            ("lexical-entity-uniqueness", 1000),
            ("lexical-candidate-uniqueness", 1000),
            ("lexical-sorted-facts", 1),
            ("lexical-document-state", 1),
        ] {
            token.store(false, Ordering::Release);
            let _cancel =
                crate::builtin::cancel_search_preparation_after(point, visits, Arc::clone(&token));
            assert!(
                matches!(
                    prepare_corpus_controlled(
                        workspace,
                        view.clone(),
                        &capability,
                        coverage,
                        evidence.clone(),
                        Some(&token)
                    ),
                    Err(QueryError::SelectedViewRetired)
                ),
                "{point}"
            );
            assert!(
                token.load(Ordering::Acquire),
                "real preparation reached {point}"
            );
        }
        token.store(false, Ordering::Release);
        let resumed = prepare_corpus_controlled(
            workspace,
            view.clone(),
            &capability,
            coverage,
            evidence,
            Some(&token),
        )?;
        assert_eq!(resumed.binding, baseline.binding);
        assert_eq!(resumed.state, baseline.state);
        assert_eq!(resumed.selected.entities.len(), 4096);
        assert_eq!(
            resumed
                .selected
                .entities
                .iter()
                .map(|entry| (entry.row, entry.candidate))
                .collect::<Vec<_>>(),
            baseline
                .selected
                .entities
                .iter()
                .map(|entry| (entry.row, entry.candidate))
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn cancelled_exact_selection_does_not_report_a_reused_coordinator() -> TestResult {
        let directory = Directory::new()?;
        let (workspace, view) = super::super::tests::selected_view();
        let mut owner = directory.owner(false, "cancelled-reuse");
        select(&mut owner, workspace, &view)?;
        let before = search(&owner, "alpha")?;
        let token = AtomicBool::new(true);
        assert!(matches!(
            owner.select_controlled(
                workspace,
                view.clone(),
                view_capability(&view)?,
                crate::builtin::admitted_coverage()?,
                super::super::tests::semantic_evidence(workspace, &view),
                Some(&token)
            ),
            Err(QueryError::SelectedViewRetired)
        ));
        assert_eq!(owner.projection_builds(), 1);
        assert_eq!(
            search(&owner, "alpha")?.ranked_row_ids(),
            before.ranked_row_ids()
        );
        Ok(())
    }

    #[test]
    fn retained_answers_prepare_each_new_corpus_once_instead_of_legacy_fallback_duplicates()
    -> TestResult {
        for durable in [false, true] {
            let directory = Directory::new()?;
            let (workspace, old_view) = super::super::tests::selected_view();
            let next_view = renamed_view(&old_view)?;
            assert_ne!(old_view.root(), next_view.root());
            let mut baseline = directory.owner(durable, "baseline");
            let mut candidate = directory.owner(durable, "candidate");
            select(&mut baseline, workspace, &old_view)?;
            select(&mut candidate, workspace, &old_view)?;
            let baseline_old = search(&baseline, "alpha")?;
            let candidate_old = search(&candidate, "alpha")?;
            let measure = Measurement::start();
            legacy_shared_replacement(&mut baseline, workspace, &next_view)?;
            let old_work = measure.take();
            select(&mut candidate, workspace, &next_view)?;
            let new_work = measure.take();
            assert_eq!(
                old_work
                    .iter()
                    .filter(|root| **root == next_view.root())
                    .count(),
                2
            );
            assert_eq!(
                old_work
                    .iter()
                    .filter(|root| **root == old_view.root())
                    .count(),
                usize::from(durable)
            );
            assert_eq!(new_work, [next_view.root()]);
            assert_equivalent_searches(&baseline, &candidate, &next_view)?;
            assert_eq!(baseline_old.corpus.view.root(), old_view.root());
            assert_eq!(candidate_old.corpus.view.root(), old_view.root());
            assert!(
                candidate_old
                    .rows
                    .iter()
                    .any(|row| row.row.label == "alpha exact")
            );
            select(&mut candidate, workspace, &next_view)?;
            assert!(
                measure.take().is_empty(),
                "the unchanged exact selection is still free"
            );
            assert_eq!(candidate.maintenance(), Some(SnapshotMaintenance::Reused));
        }
        Ok(())
    }

    #[test]
    fn a_durable_workspace_change_prepares_only_current_and_keeps_exact_cold_search()
    -> TestResult {
        let directory = Directory::new()?;
        let (workspace, old_view) = super::super::tests::selected_view();
        let next_view = renamed_view(&old_view)?;
        let head = crate::builtin::genesis()?;
        let authority = head
            .manifest()
            .authority_closure()
            .ok_or_else(|| std::io::Error::other("missing checked authority"))?;
        // Derive a different checked workspace root through the public
        // manifest API; no raw root constructor or fabricated digest.
        let next_workspace = backend_version::WorkspaceManifest::new_checked(
            u32::from(backend_version::CANONICAL_VERSION),
            Vec::new(),
            Vec::new(),
            authority,
            crate::builtin::admitted_coverage()?,
        )?
        .root();
        assert_ne!(workspace, next_workspace);
        let mut owner = directory.owner(true, "workspace-change");
        let mut fresh = directory.owner(true, "fresh-current");
        select(&mut owner, workspace, &old_view)?;
        let old_ids = search(&owner, "alpha")?.ranked_row_ids();
        select(&mut fresh, next_workspace, &next_view)?;
        let measure = Measurement::start();
        select(&mut owner, next_workspace, &next_view)?;
        assert_eq!(
            measure.take(),
            [next_view.root()],
            "a guaranteed rebuild must not prepare the old document relation"
        );
        assert_eq!(owner.maintenance(), Some(SnapshotMaintenance::Rebuilt));
        assert_eq!(owner.projection_builds(), 2);
        assert_equivalent_searches(&fresh, &owner, &next_view)?;
        select(&mut owner, next_workspace, &next_view)?;
        assert!(measure.take().is_empty(), "exact warm selection stays free");
        assert_eq!(owner.maintenance(), Some(SnapshotMaintenance::Reused));
        let mut reopened = directory.owner(true, "workspace-change");
        select(&mut reopened, next_workspace, &next_view)?;
        assert_eq!(reopened.maintenance(), Some(SnapshotMaintenance::Restored));
        assert_eq!(reopened.projection_builds(), 0);
        assert_eq!(reopened.projection_opens(), 1);
        assert_equivalent_searches(&fresh, &reopened, &next_view)?;
        let mut old_reopened = directory.owner(true, "workspace-change");
        select(&mut old_reopened, workspace, &old_view)?;
        assert_eq!(old_reopened.maintenance(), Some(SnapshotMaintenance::Restored));
        assert_eq!(search(&old_reopened, "alpha")?.ranked_row_ids(), old_ids);
        Ok(())
    }

    #[test]
    fn an_owned_revision_prepares_only_the_current_document_state() -> TestResult {
        for durable in [false, true] {
            let directory = Directory::new()?;
            let (workspace, old_view) = super::super::tests::selected_view();
            let next_view = renamed_view(&old_view)?;
            let mut owner = directory.owner(durable, "owned");
            select(&mut owner, workspace, &old_view)?;
            let measure = Measurement::start();
            select(&mut owner, workspace, &next_view)?;
            let work = measure.take();
            assert_eq!(
                work.iter()
                    .filter(|root| **root == next_view.root())
                    .count(),
                1
            );
            assert_eq!(
                work,
                [next_view.root()],
                "the prior admitted document state supplies the durable delta base"
            );
            assert_eq!(
                owner.maintenance(),
                Some(SnapshotMaintenance::Revised {
                    rewritten_documents: 1
                })
            );
            assert_eq!(owner.projection_builds(), 1);
        }
        Ok(())
    }

    #[test]
    fn durable_revisions_reuse_exact_document_states_and_preserve_retained_answers() -> TestResult {
        let directory = Directory::new()?;
        let (workspace, old_view) = super::super::tests::selected_view();
        let middle_view = renamed_view(&old_view)?;
        let current_view = relabeled_view(&middle_view, "zephyr marker", "nova marker")?;
        let mut owner = directory.owner(true, "successive-revisions");

        select(&mut owner, workspace, &old_view)?;
        let retained_old = search(&owner, "alpha")?;
        let old_binding = {
            let corpus = &owner
                .selected
                .as_ref()
                .ok_or_else(|| std::io::Error::other("missing old snapshot"))?
                .corpus;
            let binding = corpus.lexical_binding;
            assert_eq!(corpus.document_state.binding(), binding);
            binding
        };

        let measure = Measurement::start();
        select(&mut owner, workspace, &middle_view)?;
        assert_eq!(measure.take(), [middle_view.root()]);
        assert_eq!(retained_old.corpus.view.root(), old_view.root());
        assert_eq!(retained_old.corpus.document_state.binding(), old_binding);
        assert!(
            retained_old
                .rows
                .iter()
                .any(|row| row.row.label == "alpha exact")
        );
        assert_eq!(search(&owner, "alpha")?.total_matches, 0);
        let middle_answer = search(&owner, "zephyr")?;
        assert!(
            middle_answer
                .rows
                .iter()
                .any(|row| row.row.label == "zephyr marker")
        );
        let middle_binding = {
            let corpus = &owner
                .selected
                .as_ref()
                .ok_or_else(|| std::io::Error::other("missing middle snapshot"))?
                .corpus;
            let binding = corpus.lexical_binding;
            assert_eq!(corpus.document_state.binding(), binding);
            binding
        };
        drop(retained_old);
        drop(middle_answer);

        select(&mut owner, workspace, &current_view)?;
        assert_eq!(measure.take(), [current_view.root()]);
        assert_eq!(
            owner.maintenance(),
            Some(SnapshotMaintenance::Revised {
                rewritten_documents: 1,
            })
        );
        assert_eq!(search(&owner, "zephyr")?.total_matches, 0);
        let current_answer = search(&owner, "nova")?;
        assert!(
            current_answer
                .rows
                .iter()
                .any(|row| row.row.label == "nova marker")
        );
        let current_corpus = &owner
            .selected
            .as_ref()
            .ok_or_else(|| std::io::Error::other("missing current snapshot"))?
            .corpus;
        assert_eq!(current_corpus.view.root(), current_view.root());
        assert_eq!(
            current_corpus.document_state.binding(),
            current_corpus.lexical_binding
        );
        assert_ne!(old_binding.root, middle_binding.root);
        assert_ne!(middle_binding.root, current_corpus.lexical_binding.root);

        Ok(())
    }
}

fn validate_view_binding(
    view: &ViewRoot,
    expected_view_capability: &CoverageCapability,
) -> Result<(), QueryError> {
    if view.capability().as_ref() != Some(expected_view_capability) {
        return Err(QueryError::StaleViewBinding);
    }
    Ok(())
}

fn collect_selected_documents(
    workspace: WorkspaceRoot,
    view: &ViewRoot,
    semantic_evidence: &SemanticQueryCorpus,
    cancelled: Option<&AtomicBool>,
) -> Result<(Vec<(EntityId, Vec<(String, String)>)>, SelectedCorpus), QueryError> {
    preparation_checkpoint(cancelled, "lexical-collect")?;
    let mut documents = Vec::new();
    let mut selected = Vec::new();
    let mut candidate_ids = BTreeSet::new();
    let mut selected_rows = BTreeMap::new();
    for row in view.row_refs() {
        preparation_checkpoint(cancelled, "lexical-row")?;
        if selected_rows
            .insert(
                row.id.stable_key(),
                (row.id, SearchOrigin::from_source(&row.source)),
            )
            .is_some()
        {
            return Err(QueryError::IdentityCollision);
        }
    }
    // A producer can mint a synthetic child fact that carries its own
    // owning function's name for identity purposes only: a function's
    // return-type "result slot", admitted as a `Parameter`-kind fact
    // literally named like the function (see `driver/lower/python.rs` and
    // `driver/lower/rust.rs`). That name was never chosen for user-facing
    // lookup; it exists so the slot's content-addressed coordinate has one.
    // Indexing it under the lexical "name" field anyway turns every name
    // search for the function into two hits.
    //
    // A real, distinct declaration can coincidentally share its exact
    // spelling with its parent -- a constructor (`class Foo { Foo() {} }`),
    // a Rust `mod foo { pub fn foo() }`, a method named like its class -- so
    // name-sharing alone is not a safe signal; those must stay searchable.
    // What is unique to the synthetic slot is its *kind*: it is the only
    // "variable"-presented fact whose immediate parent is itself callable
    // (a function or method). A real field, constructor, or nested function
    // never presents as `variable`, so requiring `variable` kind and a
    // callable parent, on top of the name match, narrows this to exactly
    // the synthetic result slot without a new IR/wire bit.
    let mut facts_by_id: BTreeMap<&str, &SemanticQueryPresentation> = BTreeMap::new();
    for fact in semantic_evidence.facts() {
        preparation_checkpoint(cancelled, "lexical-fact-index")?;
        let presentation = fact.presentation();
        facts_by_id.insert(presentation.id.as_str(), presentation);
    }
    let mut left_out = LeftOut::default();
    for (fact_index, fact) in semantic_evidence.facts().iter().enumerate() {
        preparation_checkpoint(cancelled, "lexical-fact")?;
        let presentation = fact.presentation();
        let Some((row, source)) = selected_rows.remove(&presentation.id) else {
            left_out.evidence_without_row += 1;
            continue;
        };
        {
            let entity = entity_id(workspace, row)?;
            let candidate = candidate_id(entity, semantic_evidence.evidence_digest())?;
            if !candidate_ids.insert(candidate) {
                return Err(QueryError::IdentityCollision);
            }
            selected.push(SelectedEntity {
                entity,
                row,
                candidate,
                fact_index,
                origin: if matches!(
                    fact.evidence(),
                    SemanticQueryEvidence::CompilerExternalTarget(_)
                ) {
                    SearchOrigin::ExternalTarget
                } else {
                    source
                },
            });
            let is_synthetic_result_slot = presentation.kind == "variable"
                && presentation
                    .parent
                    .as_deref()
                    .and_then(|parent| facts_by_id.get(parent))
                    .is_some_and(|parent_fact| {
                        parent_fact.name == presentation.name
                            && matches!(parent_fact.kind.as_str(), "function" | "method")
                    });
            // The coordinate itself ends in `::<name>`, so merely dropping
            // the "name" field would still leave this fact lexically
            // matchable through its own "coordinate" field. Leaving the row
            // out of the lexical corpus entirely is the only way a name
            // search for the parent stops finding it too; it stays a normal
            // member of `entities`/`candidates`/`semantic_documents`; so
            // direct coordinate lookups (`show`) and the semantic/vector
            // lane are unaffected.
            if !is_synthetic_result_slot {
                let row_fields = fields_for_fact(fact);
                checked_document_bytes(0, &row_fields)?;
                documents.push((entity, row_fields));
            }
        }
    }
    left_out.rows_without_evidence = selected_rows.len();
    preparation_checkpoint(cancelled, "lexical-sort-entities")?;
    if cancelled.is_some() {
        super::controlled_sort::sort_unstable_in_place_checked(
            &mut selected,
            |entry: &SelectedEntity| entry.entity,
            &mut || preparation_checkpoint(cancelled, "lexical-sort-entities-step"),
        )?;
    } else {
        selected.sort_unstable_by_key(|entry| entry.entity);
    }
    preparation_checkpoint(cancelled, "lexical-sorted-entities")?;
    for pair in selected.windows(2) {
        preparation_checkpoint(cancelled, "lexical-entity-uniqueness")?;
        if pair[0].entity == pair[1].entity {
            return Err(QueryError::IdentityCollision);
        }
    }
    let mut candidate_order = (0..selected.len()).collect::<Vec<_>>();
    preparation_checkpoint(cancelled, "lexical-sort-candidates")?;
    if cancelled.is_some() {
        super::controlled_sort::sort_unstable_in_place_checked(
            &mut candidate_order,
            |index: &usize| selected[*index].candidate,
            &mut || preparation_checkpoint(cancelled, "lexical-sort-candidates-step"),
        )?;
    } else {
        candidate_order.sort_unstable_by_key(|index| selected[*index].candidate);
    }
    preparation_checkpoint(cancelled, "lexical-sorted-candidates")?;
    for pair in candidate_order.windows(2) {
        preparation_checkpoint(cancelled, "lexical-candidate-uniqueness")?;
        if selected[pair[0]].candidate == selected[pair[1]].candidate {
            return Err(QueryError::IdentityCollision);
        }
    }
    let mut document_order = (0..selected.len()).collect::<Vec<_>>();
    preparation_checkpoint(cancelled, "lexical-sort-documents")?;
    if cancelled.is_some() {
        super::controlled_sort::sort_unstable_in_place_checked(
            &mut document_order,
            |index: &usize| selected[*index].row,
            &mut || preparation_checkpoint(cancelled, "lexical-sort-documents-step"),
        )?;
    } else {
        document_order.sort_unstable_by_key(|index| selected[*index].row);
    }
    preparation_checkpoint(cancelled, "lexical-sorted-documents")?;
    let mut fact_order = (0..semantic_evidence.facts().len()).collect::<Vec<_>>();
    preparation_checkpoint(cancelled, "lexical-sort-facts")?;
    let compare_facts = |left: &usize, right: &usize| {
        semantic_evidence.facts()[*left]
            .presentation()
            .id
            .cmp(&semantic_evidence.facts()[*right].presentation().id)
    };
    if cancelled.is_some() {
        super::controlled_sort::sort_unstable_by_in_place_checked(
            &mut fact_order,
            compare_facts,
            &mut || preparation_checkpoint(cancelled, "lexical-sort-facts-step"),
        )?;
    } else {
        fact_order.sort_unstable_by(compare_facts);
    }
    preparation_checkpoint(cancelled, "lexical-sorted-facts")?;
    Ok((
        documents,
        SelectedCorpus {
            entities: selected.into_boxed_slice(),
            candidate_order: candidate_order.into_boxed_slice(),
            document_order: document_order.into_boxed_slice(),
            fact_order: fact_order.into_boxed_slice(),
            left_out,
        },
    ))
}

fn checked_document_bytes(
    initial: usize,
    fields: &[(String, String)],
) -> Result<usize, QueryError> {
    let bytes = fields.iter().try_fold(initial, |bytes, (field, text)| {
        bytes.checked_add(field.len())?.checked_add(text.len())
    });
    match bytes {
        Some(bytes) if bytes <= lexical::Limits::default().max_total_text_bytes => Ok(bytes),
        Some(_) | None => Err(QueryError::CorpusLimit),
    }
}

pub(super) fn entity_id(workspace: WorkspaceRoot, id: RowId) -> Result<EntityId, QueryError> {
    let mut namespace = [0_u8; 16];
    namespace.copy_from_slice(&workspace.as_bytes()[..16]);
    let source = Source::new(u128::from_be_bytes(namespace), "published-view")
        .map_err(|_| QueryError::InvalidView)?;
    let entity = Entity::new(source, id.stable_key(), None).map_err(|_| QueryError::InvalidView)?;
    Ok(EntityId::from_value(&entity))
}

pub(super) fn candidate_id(
    entity: EntityId,
    evidence: [u8; 32],
) -> Result<backend_extension_qdrant::CandidateId, QueryError> {
    let mut identity = blake3::Hasher::new();
    identity.update(b"backend.qdrant.semantic-evidence-candidate.v3\0");
    identity.update(&evidence);
    identity.update(entity.as_bytes());
    let digest = identity.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    let value = u64::from_be_bytes(bytes);
    backend_extension_qdrant::CandidateId::new(value).map_err(|_| QueryError::IdentityCollision)
}

fn fields(row: &SemanticQueryPresentation) -> Vec<(String, String)> {
    let mut fields = vec![
        ("coordinate".to_owned(), row.coordinate.clone()),
        ("kind".to_owned(), row.kind.clone()),
        ("name".to_owned(), row.name.clone()),
    ];
    if let Some(signature) = &row.signature {
        fields.push(("signature".to_owned(), signature.clone()));
    }
    if !row.documentation.is_empty() {
        fields.push(("documentation".to_owned(), row.documentation.clone()));
    }
    fields.sort();
    fields
}

fn fields_for_fact(fact: &SemanticQueryFact) -> Vec<(String, String)> {
    let mut fields = fields(fact.presentation());
    if let SemanticQueryEvidence::Package(scope) = fact.evidence()
        && let Some(aliases) = scope.cargo_aliases()
    {
        let mut seen = BTreeSet::new();
        for alias in aliases.aliases() {
            if seen.insert(alias.as_str()) {
                fields.push((
                    format!("cargo_alias_{:02}", seen.len() - 1),
                    alias.as_str().to_owned(),
                ));
            }
        }
        fields.sort();
        fields.dedup();
    }
    fields
}

fn semantic_text_fields(row: &SemanticQueryPresentation) -> [(&str, Option<&str>); 5] {
    // Exactly the existing sorted field grammar, without cloning the source strings.
    [
        ("coordinate", Some(row.coordinate.as_str())),
        (
            "documentation",
            (!row.documentation.is_empty()).then_some(row.documentation.as_str()),
        ),
        ("kind", Some(row.kind.as_str())),
        ("name", Some(row.name.as_str())),
        ("signature", row.signature.as_deref()),
    ]
}

fn semantic_text_extent(
    row: &SemanticQueryPresentation,
    id: RowId,
    maximum: usize,
) -> Result<usize, QueryError> {
    let bytes = semantic_text_fields(row)
        .into_iter()
        .try_fold(0usize, |bytes, (name, value)| match value {
            Some(value) => bytes
                .checked_add(name.len())?
                .checked_add(2)?
                .checked_add(value.len()),
            None => Some(bytes),
        })
        .ok_or(QueryError::CorpusLimit)?;
    if bytes > maximum {
        return Err(QueryError::SemanticDocumentTooLarge {
            row: id,
            observed: bytes,
            maximum,
        });
    }
    Ok(bytes)
}

fn bounded_semantic_text(
    row: &SemanticQueryPresentation,
    id: RowId,
    maximum: usize,
) -> Result<String, QueryError> {
    let bytes = semantic_text_extent(row, id, maximum)?;
    let mut text = String::new();
    text.try_reserve_exact(bytes)
        .map_err(|_| QueryError::CorpusLimit)?;
    for (name, value) in semantic_text_fields(row) {
        if let Some(value) = value {
            text.push_str(name);
            text.push('\n');
            text.push_str(value);
            text.push('\n');
        }
    }
    Ok(text)
}

fn semantic_text(row: &SemanticQueryPresentation) -> String {
    let fields = fields(row);
    let mut text = String::new();
    for (name, value) in fields {
        text.push_str(&name);
        text.push('\n');
        text.push_str(&value);
        text.push('\n');
    }
    text
}

pub(super) fn view_identity(view: &ViewRoot) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(192);
    bytes.extend_from_slice(view.version().as_bytes());
    bytes.extend_from_slice(view.root().as_bytes());
    bytes.extend_from_slice(view.basis().root.as_bytes());
    bytes.extend_from_slice(view.basis().object.as_bytes());
    bytes.extend_from_slice(view.frontier().root.as_bytes());
    bytes
}

fn semantic_read_identity(view: &ViewRoot, evidence: [u8; 32]) -> Vec<u8> {
    let mut bytes = view_identity(view);
    bytes.extend_from_slice(&evidence);
    bytes
}

/// Query construction or local materialization failure.
#[derive(Debug)]
pub enum QueryError {
    /// The exact selected owner lifetime was retired during this scan.
    SelectedViewRetired,
    /// A bounded controlled operation could not reserve its descriptors.
    Allocation(std::collections::TryReserveError),
    /// Empty text has no meaningful rank.
    EmptyQuery,
    /// Page size is outside the extension bound.
    InvalidLimit,
    /// A rank continuation cannot address the selected relation.
    InvalidCursor,
    /// A requested rank prefix exceeds the existing retained-rank allowance.
    RankPrefixBudgetExceeded {
        /// Entries needed for the requested position and lookahead.
        requested: usize,
        /// Entries admitted by the retained-rank byte budget.
        maximum: usize,
    },
    /// The selected source was not proven complete by its owner.
    IncompleteCoverage,
    /// The selected view was not published against the exact workspace
    /// snapshot selected by the caller.
    StaleViewBinding,
    /// Two logical rows collapsed onto one cross-index identity.
    IdentityCollision,
    /// The selected view could not form a canonical lexical relation.
    InvalidView,
    /// The typed search evidence belongs to another workspace. (Evidence and
    /// rows of the right workspace that do not pair are left out of search
    /// instead: [`LeftOut`].)
    InvalidSemanticEvidence,
    /// One document exceeds the lexical field or text budget.
    CorpusLimit,
    /// Exact selected text exceeded the activated local runtime before allocation.
    SemanticDocumentTooLarge {
        /// Exact canonical row refused; no prefix is embedded.
        row: RowId,
        /// Exact document UTF-8 extent.
        observed: usize,
        /// Activated runtime text bound.
        maximum: usize,
    },
    /// Typed lexical admission failed.
    Lexical(lexical::Error),
    /// The concrete Tantivy projection or provider boundary failed.
    LexicalProvider {
        /// Exact local boundary that refused the selected projection.
        phase: LexicalPhase,
        /// Original provider error or exact failed corpus join.
        cause: LexicalFailureCause,
    },
}

pub(super) fn rank_prefix_limit(
    offset: usize,
    page_rows: usize,
    selected_rows: usize,
) -> Result<usize, QueryError> {
    if offset > 0 && offset >= selected_rows {
        return Err(QueryError::InvalidCursor);
    }
    let requested = offset
        .checked_add(page_rows)
        .and_then(|end| end.checked_add(1))
        .ok_or(QueryError::InvalidCursor)?
        .min(selected_rows.max(1));
    let maximum = lexical::RankSnapshotBudget::default().max_retained_bytes()
        / size_of::<(EntityId, lexical::Relevance)>();
    if requested > maximum {
        return Err(QueryError::RankPrefixBudgetExceeded { requested, maximum });
    }
    Ok(requested)
}

/// Closed local lexical operation where a failure arose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexicalPhase {
    /// Open or construct the selected immutable projection.
    OpenProjection,
    /// Admit the concrete adapter against its lexical contract.
    AdmitAdapter,
    /// Publish the next durable projection root.
    AdvanceProjection,
    /// Revise a resident projection.
    MaintainProjection,
    /// Visit exact provider hits.
    ScanHits,
    /// Join provider identities to selected canonical rows.
    ComposeRows,
    /// Score a bounded semantic candidate collection.
    CandidateScores,
}

/// Concrete lexical refusal retained without parsing or replacing its cause.
#[derive(Debug)]
pub enum LexicalFailureCause {
    /// Original Tantivy source, I/O, contract, integrity, or resource failure.
    Source(lexical::TantivySourceError),
    /// Candidate score allocation failed.
    Allocation {
        /// Number of requested result entries.
        requested: usize,
        /// Original allocator refusal.
        error: std::collections::TryReserveError,
    },
    /// A provider hit has no member in the exact selected corpus.
    MissingSelectedEntity(EntityId),
    /// A selected corpus member has no canonical row in the selected view.
    MissingViewRow(RowId),
    /// Provider and selected traversal cardinalities disagree.
    HitCountMismatch {
        /// Exact total reported by the provider traversal.
        provider: usize,
        /// Number of hits joined to selected rows.
        selected: usize,
    },
    /// Provider-hit cardinality cannot fit in the counter.
    HitCountOverflow,
    /// Qualified-result cardinality cannot fit in the counter.
    ResultCountOverflow,
}

impl QueryError {
    // A resident ephemeral provider failure may require rebuilding the
    // projection. A rejected contract is terminal, as it was before provider
    // failures retained their typed causes; rebuilding must not bypass it.
    fn permits_ephemeral_revision_rebuild(&self) -> bool {
        matches!(
            self,
            Self::LexicalProvider {
                phase: LexicalPhase::MaintainProjection,
                cause: LexicalFailureCause::Source(error),
            } if !matches!(error, lexical::TantivySourceError::Contract(_))
        )
    }

    fn provider(phase: LexicalPhase, error: lexical::TantivySourceError) -> Self {
        Self::LexicalProvider {
            phase,
            cause: LexicalFailureCause::Source(error),
        }
    }
}

impl fmt::Display for LexicalFailureCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => error.fmt(formatter),
            Self::Allocation { requested, error } => write!(
                formatter,
                "candidate score allocation for {requested} entries failed: {error}"
            ),
            Self::MissingSelectedEntity(entity) => write!(
                formatter,
                "lexical hit {entity:?} has no selected corpus member"
            ),
            Self::MissingViewRow(row) => write!(
                formatter,
                "selected corpus row {row:?} is absent from the canonical view"
            ),
            Self::HitCountMismatch { provider, selected } => write!(
                formatter,
                "lexical hit count mismatch: provider {provider}, selected {selected}"
            ),
            Self::HitCountOverflow => formatter.write_str("lexical hit count overflow"),
            Self::ResultCountOverflow => formatter.write_str("lexical result count overflow"),
        }
    }
}

impl std::error::Error for LexicalFailureCause {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Allocation { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl From<std::collections::TryReserveError> for QueryError {
    fn from(error: std::collections::TryReserveError) -> Self {
        Self::Allocation(error)
    }
}
impl From<lexical::TantivySourceError> for QueryError {
    fn from(error: lexical::TantivySourceError) -> Self {
        Self::provider(LexicalPhase::ScanHits, error)
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelectedViewRetired => {
                formatter.write_str("selected query view was retired during execution")
            }
            Self::Allocation(error) => error.fmt(formatter),
            Self::EmptyQuery => formatter.write_str("query text is empty"),
            Self::InvalidLimit => formatter.write_str("query page limit is invalid"),
            Self::InvalidCursor => formatter.write_str("search cursor offset is invalid"),
            Self::RankPrefixBudgetExceeded { requested, maximum } => write!(
                formatter,
                "search rank prefix requires {requested} entries; retained-rank budget admits {maximum}"
            ),
            Self::IncompleteCoverage => formatter.write_str("selected view coverage is incomplete"),
            Self::StaleViewBinding => {
                formatter.write_str("selected view is bound to another workspace snapshot")
            }
            Self::IdentityCollision => formatter.write_str("cross-index identity collision"),
            Self::InvalidView => formatter.write_str("selected view is not a valid query corpus"),
            Self::InvalidSemanticEvidence => {
                formatter.write_str("typed semantic evidence belongs to another workspace")
            }
            Self::CorpusLimit => {
                formatter.write_str("selected view exceeds the query corpus bound")
            }
            Self::SemanticDocumentTooLarge {
                row,
                observed,
                maximum,
            } => write!(
                formatter,
                "semantic document {row:?} requires {observed} bytes; runtime admits {maximum}; complete corpus refused"
            ),
            Self::Lexical(error) => write!(formatter, "lexical query failed: {error}"),
            Self::LexicalProvider { phase, cause } => write!(
                formatter,
                "Tantivy query provider failed during {phase:?}: {cause}"
            ),
        }
    }
}

impl std::error::Error for QueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(error) => Some(error),
            Self::Lexical(error) => Some(error),
            Self::LexicalProvider { cause, .. } => Some(cause),
            _ => None,
        }
    }
}

#[cfg(test)]
mod lexical_failure_tests {
    use super::*;

    #[test]
    fn ephemeral_revision_recovery_keeps_contract_and_corpus_refusals_terminal() {
        let rejected = QueryError::provider(
            LexicalPhase::MaintainProjection,
            lexical::TantivySourceError::Contract(lexical::Error::StaleRoot),
        );
        assert!(!rejected.permits_ephemeral_revision_rebuild());
        let provider = QueryError::provider(
            LexicalPhase::MaintainProjection,
            lexical::TantivySourceError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "resident projection failed",
            )),
        );
        assert!(provider.permits_ephemeral_revision_rebuild());
        let durable = QueryError::provider(
            LexicalPhase::AdvanceProjection,
            lexical::TantivySourceError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "durable publication failed",
            )),
        );
        assert!(!durable.permits_ephemeral_revision_rebuild());
        let unjoined = QueryError::LexicalProvider {
            phase: LexicalPhase::ComposeRows,
            cause: LexicalFailureCause::HitCountMismatch {
                provider: 1,
                selected: 0,
            },
        };
        assert!(!unjoined.permits_ephemeral_revision_rebuild());
    }

    #[test]
    fn provider_failure_preserves_original_io_and_resource_causes() {
        let error = QueryError::provider(
            LexicalPhase::OpenProjection,
            lexical::TantivySourceError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "projection lease denied",
            )),
        );
        assert!(matches!(
            &error,
            QueryError::LexicalProvider {
                phase: LexicalPhase::OpenProjection,
                cause: LexicalFailureCause::Source(lexical::TantivySourceError::Io(cause)),
            } if cause.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert!(error.to_string().contains("projection lease denied"));
        assert!(std::error::Error::source(&error).is_some());
        let error = QueryError::provider(
            LexicalPhase::ScanHits,
            lexical::TantivySourceError::RankSnapshotBudgetExceeded {
                budget_bytes: 7,
                required_bytes: 19,
            },
        );
        assert!(matches!(
            error,
            QueryError::LexicalProvider {
                phase: LexicalPhase::ScanHits,
                cause: LexicalFailureCause::Source(
                    lexical::TantivySourceError::RankSnapshotBudgetExceeded {
                        budget_bytes: 7,
                        required_bytes: 19
                    }
                ),
            }
        ));
    }

    #[test]
    fn actual_provider_hit_missing_from_selected_corpus_is_not_empty_success() {
        let (workspace, view) = super::super::tests::selected_view();
        let capability = view.capability().expect("selected capability");
        let evidence = super::super::tests::semantic_evidence(workspace, &view);
        let mut coordinator = QueryCoordinator::new(
            workspace,
            view,
            capability,
            crate::builtin::admitted_coverage().expect("coverage"),
            evidence,
        )
        .expect("real selected Tantivy projection");
        // Keep the real provider postings and selected view unchanged, but
        // inject the exact forbidden join that the old generic error hid.
        Arc::get_mut(&mut coordinator.corpus)
            .expect("unshared corpus")
            .selected
            .entities = Box::new([]);
        let error = coordinator
            .search_local(LocalQuery::prefix("alpha", 5).expect("query"))
            .err()
            .expect("unjoined real hit must refuse");
        assert!(matches!(
            error,
            QueryError::LexicalProvider {
                phase: LexicalPhase::ComposeRows,
                cause: LexicalFailureCause::MissingSelectedEntity(_),
            }
        ));
    }
}

/// Times admitting a typed query corpus against reusing the resident one.
///
/// Fixture rows are built before the timer. `cold` admits them. `warm` returns
/// the resident corpus. Production search also reopens semantic images and
/// plans structure before admission; this measurement is the admission tail.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_search_corpus() {
    const FACTS: usize = 2_048;
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let workspace = super::super::genesis().expect("genesis").root();
    let facts = (0..FACTS).map(package_query_fact).collect::<Vec<_>>();
    let limits = backend_extension_trustfall::Limits {
        max_rows: FACTS,
        ..backend_extension_trustfall::Limits::default()
    };
    let cold = corpus_time(WARMUPS, SAMPLES, || {
        SemanticQueryCorpus::admit_with_limits(workspace, facts.clone(), limits).expect("admit")
    });
    let mut owner = SearchSnapshotOwner::default();
    owner
        .shared_corpus(workspace, || {
            SemanticQueryCorpus::admit_with_limits(workspace, facts.clone(), limits)
        })
        .expect("prime");
    let before = owner.corpus_builds();
    let warm = corpus_time(WARMUPS, SAMPLES, || {
        owner
            .shared_corpus(
                workspace,
                || -> Result<SemanticQueryCorpus, &'static str> {
                    Err("warm path rebuilt the corpus")
                },
            )
            .expect("reuse")
    });
    let (cold_median, cold_p95) = corpus_percentiles(&cold);
    let (warm_median, warm_p95) = corpus_percentiles(&warm);
    println!(
        "search_corpus facts={FACTS} cold_median_ns={cold_median} cold_p95_ns={cold_p95} warm_median_ns={warm_median} warm_p95_ns={warm_p95} warm_builds={}",
        owner.corpus_builds() - before
    );
}

fn package_query_fact(index: usize) -> SemanticQueryFact {
    let name = format!("pkg-{index}");
    let package = backend_engine::package_key(&name);
    let evidence = backend_extension_trustfall::PackageScopeEvidence::new(package);
    let id = evidence.row_id();
    SemanticQueryFact::new(
        SemanticQueryEvidence::Package(evidence),
        SemanticQueryPresentation {
            id,
            kind: "project".to_owned(),
            coordinate: format!("pkg-{index}"),
            name: format!("pkg-{index}"),
            signature: None,
            documentation: String::new(),
            score: None,
            project: None,
            parent: None,
            related: Box::new([]),
        },
    )
}

fn corpus_time<T>(warmups: usize, samples: usize, mut body: impl FnMut() -> T) -> Vec<u128> {
    for _ in 0..warmups {
        let _ = body();
    }
    let mut samples_ns = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = std::time::Instant::now();
        let _ = body();
        samples_ns.push(started.elapsed().as_nanos());
    }
    samples_ns
}

#[allow(clippy::indexing_slicing)]
fn corpus_percentiles(samples: &[u128]) -> (u128, u128) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let p95 = ordered[ordered.len() * 95 / 100];
    (median, p95)
}

#[cfg(test)]
mod cargo_alias_tests {
    use super::*;
    use backend_library::{
        CargoPackageAliasCargoFactsV1, CargoPackageAliasCoverageV1, CargoPackageAliasEvidenceV1,
        CargoPackageAliasObservationV1, CargoPackageAliasV1, CargoTargetNameV1,
    };
    use backend_semantic::ir::{
        DeclarationFamilyId, DeclarationIdentity, IrBuilder, SourceIdentity, VariantFingerprint,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

    fn alias_evidence(profile: LanguageProfile) -> CargoPackageAliasEvidenceV1 {
        let aliases = vec![CargoPackageAliasV1::CargoTargetName(
            CargoTargetNameV1::new("crate_alias").expect("target alias"),
        )];
        let observation = CargoPackageAliasObservationV1::from_wire_parts(
            profile,
            [21; 32],
            Some(CargoPackageAliasCargoFactsV1::from_wire_parts(
                [22; 32], [23; 32], [24; 32],
            )),
            vec![0],
            CargoPackageAliasCoverageV1::Complete,
        )
        .expect("profile observation");
        CargoPackageAliasEvidenceV1::from_wire_parts(aliases, vec![observation])
            .expect("bounded package aliases")
    }

    fn compiler_fact(
        package: backend_engine::PackageKey,
        profile: LanguageProfile,
        declaration_tag: u8,
        source_tag: u8,
        project: String,
    ) -> SemanticQueryFact {
        let coordinate =
            PackageUrl::parse("pkg:cargo/acme/demo@1.0.0".to_owned()).expect("package coordinate");
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[source_tag]),
            byte_len: 1,
        };
        let recipe = CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"cargo-alias-test-toolchain"),
        );
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, "src/lib.rs")
            .expect("captured compiler provenance");
        let image = builder.finish().expect("semantic image");
        let identity = DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([declaration_tag; 16]),
            variant: VariantFingerprint::from_raw([declaration_tag.wrapping_add(1); 16]),
        };
        let evidence = backend_extension_trustfall::CompilerSemanticEvidence::new(
            package,
            coordinate.clone(),
            profile,
            identity,
            [declaration_tag; 32],
            image.image_facts(),
        );
        let id = evidence.row_id();
        SemanticQueryFact::new(
            SemanticQueryEvidence::Compiler(evidence),
            SemanticQueryPresentation {
                id,
                kind: "struct".to_owned(),
                coordinate: format!("{}::Widget", coordinate.as_str()),
                name: "Widget".to_owned(),
                signature: None,
                documentation: String::new(),
                score: None,
                project: Some(project),
                parent: None,
                related: Box::new([]),
            },
        )
    }

    #[test]
    fn qualified_cargo_alias_is_bound_to_the_candidate_package_and_profile() {
        let workspace = crate::builtin::genesis().expect("workspace").root();
        let profile_2021 = LanguageProfile::Rust(RustEdition::Rust2021);
        let profile_2024 = LanguageProfile::Rust(RustEdition::Rust2024);
        let package_a = backend_engine::package_key("demo-a");
        let package_b = backend_engine::package_key("demo-b");
        let package_a_scope = backend_extension_trustfall::PackageScopeEvidence::with_cargo_aliases(
            package_a,
            alias_evidence(profile_2021),
        );
        let package_a_id = package_a_scope.row_id();
        let package_b_scope = backend_extension_trustfall::PackageScopeEvidence::new(package_b);
        let package_b_id = package_b_scope.row_id();
        let facts = vec![
            SemanticQueryFact::new(
                SemanticQueryEvidence::Package(package_a_scope.clone()),
                SemanticQueryPresentation {
                    id: package_a_id.clone(),
                    kind: "project".to_owned(),
                    coordinate: "demo-a".to_owned(),
                    name: "demo-a".to_owned(),
                    signature: None,
                    documentation: String::new(),
                    score: None,
                    project: None,
                    parent: None,
                    related: Box::new([]),
                },
            ),
            SemanticQueryFact::new(
                SemanticQueryEvidence::Package(package_b_scope),
                SemanticQueryPresentation {
                    id: package_b_id.clone(),
                    kind: "project".to_owned(),
                    coordinate: "demo-b".to_owned(),
                    name: "demo-b".to_owned(),
                    signature: None,
                    documentation: String::new(),
                    score: None,
                    project: None,
                    parent: None,
                    related: Box::new([]),
                },
            ),
            compiler_fact(package_a, profile_2021, 1, 11, package_a_id.clone()),
            compiler_fact(package_a, profile_2024, 3, 13, package_a_id),
            compiler_fact(package_b, profile_2021, 5, 15, package_b_id),
        ];
        let alias_fields = fields_for_fact(&facts[0]);
        assert!(
            alias_fields
                .iter()
                .any(|(field, value)| field.starts_with("cargo_alias_") && value == "crate_alias")
        );
        let corpus = SemanticQueryCorpus::admit(workspace, facts).expect("typed corpus");
        let mut fact_order = (0..corpus.facts().len()).collect::<Vec<_>>();
        fact_order.sort_unstable_by_key(|index| corpus.facts()[*index].presentation().id.as_str());
        let selected = SelectedCorpus {
            entities: Box::new([]),
            candidate_order: Box::new([]),
            document_order: Box::new([]),
            fact_order: fact_order.into_boxed_slice(),
            left_out: LeftOut::default(),
        };
        let clause = QualifiedClause {
            leaf: "wid".to_owned(),
            owners: vec!["crate_alias".to_owned()],
        };
        let candidate_id = |index: usize| corpus.facts()[index].presentation().id.clone();
        assert!(qualified_clause_matches(
            &candidate_id(2),
            &clause,
            &selected,
            &corpus,
        ));
        assert!(
            !qualified_clause_matches(&candidate_id(3), &clause, &selected, &corpus),
            "an alias observed for Rust 2021 cannot authorize a Rust 2024 candidate"
        );
        assert!(
            !qualified_clause_matches(&candidate_id(4), &clause, &selected, &corpus),
            "a different package cannot borrow the alias"
        );
    }
}

#[cfg(test)]
mod source_origin_tests {
    use super::SearchOrigin;
    use backend_library::{SourceAvailability, SourceLocation};

    #[test]
    fn source_declaration_ranking_retains_unhydrated_and_stale_declaration_evidence() {
        for source in [
            SourceAvailability::Captured(
                SourceLocation::new("core/config.py", 42).expect("source"),
            ),
            SourceAvailability::NotHydrated,
            SourceAvailability::stale_file("core/config.py").expect("stale captured path"),
        ] {
            assert_eq!(
                SearchOrigin::from_source(&source),
                SearchOrigin::SourceDeclaration
            );
        }
        for source in [
            SourceAvailability::NotCaptured,
            SourceAvailability::Unconfigured,
        ] {
            assert_eq!(SearchOrigin::from_source(&source), SearchOrigin::Unsourced);
        }
    }
}

#[cfg(test)]
mod bounded_semantic_text_tests {
    use super::*;

    #[test]
    fn bounded_text_preserves_exact_canonical_grammar_and_rejects_utf8_extent()
    -> Result<(), QueryError> {
        let fact = package_query_fact(0);
        let mut row = fact.presentation().clone();
        row.documentation = "café\ncontext".to_owned();
        row.signature = Some("fn sample()".to_owned());
        let id = RowId::Package(backend_engine::package_key("pkg-0"));
        let expected = "coordinate\npkg-0\ndocumentation\ncafé\ncontext\nkind\nproject\nname\npkg-0\nsignature\nfn sample()\n";
        assert_eq!(semantic_text(&row), expected);
        assert_eq!(
            semantic_text_extent(&row, id, expected.len())?,
            expected.len()
        );
        assert_eq!(bounded_semantic_text(&row, id, expected.len())?, expected);
        assert!(matches!(
            bounded_semantic_text(&row, id, expected.len() - 1),
            Err(QueryError::SemanticDocumentTooLarge { row, observed, maximum })
                if row == id && observed == expected.len() && maximum == expected.len() - 1
        ));
        row.signature = None;
        row.documentation.clear();
        assert_eq!(bounded_semantic_text(&row, id, 1024)?, semantic_text(&row));
        Ok(())
    }
}

#[cfg(test)]
mod retained_local_rank_prefix_tests {
    use super::*;
    use std::cell::Cell;

    fn hit(index: usize, weight: u16) -> (EntityId, lexical::Relevance) {
        let source = Source::new(1, "rank-prefix-law").expect("source");
        let entity = Entity::new(source, format!("declaration::{index}"), None).expect("entity");
        (
            EntityId::from_value(&entity),
            lexical::Relevance::exact(weight),
        )
    }

    fn compare(
        left: &(EntityId, lexical::Relevance),
        right: &(EntityId, lexical::Relevance),
    ) -> Ordering {
        right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0))
    }

    fn permutations(
        input: &mut [(EntityId, lexical::Relevance)],
        at: usize,
        visit: &mut impl FnMut(&[(EntityId, lexical::Relevance)]),
    ) {
        if at == input.len() {
            visit(input);
            return;
        }
        for next in at..input.len() {
            input.swap(at, next);
            permutations(input, at + 1, visit);
            input.swap(at, next);
        }
    }

    #[test]
    fn retained_prefix_matches_full_sort_for_every_traversal_tie_and_page_credit() {
        let mut input = [hit(0, 1), hit(1, 4), hit(2, 4), hit(3, 2), hit(4, 1)];
        let mut full = input.to_vec();
        full.sort_unstable_by(compare);
        let mut visits = 0;
        let count = input.len();
        permutations(&mut input, 0, &mut |traversal| {
            visits += 1;
            for limit in 0..=count + 1 {
                let mut retained = RetainedLocalRankPrefix::new(limit).expect("rank storage");
                for hit in traversal {
                    retained.offer(*hit, &compare);
                }
                assert_eq!(retained.into_sorted(compare), full[..limit.min(full.len())]);
            }
        });
        assert_eq!(visits, 120);
    }

    #[test]
    fn controlled_heap_output_preserves_current_rank_and_allocation_and_stops_inside_sort() {
        let mut expected = (0..2048)
            .map(|index| hit(index, (index % 7) as u16))
            .collect::<Vec<_>>();
        expected.sort_unstable_by(compare);
        let make = || {
            let mut retained = RetainedLocalRankPrefix::new(1024).expect("rank storage");
            for entry in expected.iter().rev() {
                retained.offer(*entry, &compare);
            }
            retained
        };
        let retained = make();
        let pointer = retained.hits.as_ptr();
        let capacity = retained.hits.capacity();
        let mut visits = 0;
        let actual = retained
            .into_sorted_controlled(compare, &mut || {
                visits += 1;
                Ok::<(), usize>(())
            })
            .expect("controlled output");
        assert_eq!(actual, expected[..1024]);
        assert_eq!(actual.as_ptr(), pointer);
        assert_eq!(actual.capacity(), capacity);
        assert!(visits > 1024);
        let mut visited = 0;
        let stopped = make().into_sorted_controlled(compare, &mut || {
            visited += 1;
            if visited == 513 { Err(visited) } else { Ok(()) }
        });
        assert_eq!(stopped, Err(513));
        assert_eq!(visited, 513);
    }
    #[test]
    fn deep_prefix_has_logarithmic_retention_work_and_no_growth_after_reservation() {
        const MATCHES: usize = 8_192;
        const PREFIX: usize = 4_096;
        let mut full = (0..MATCHES).map(|index| hit(index, 1)).collect::<Vec<_>>();
        full.sort_unstable_by(compare);
        let comparisons = Cell::new(0usize);
        let counted = |left: &(EntityId, lexical::Relevance),
                       right: &(EntityId, lexical::Relevance)| {
            comparisons.set(comparisons.get() + 1);
            compare(left, right)
        };
        let mut retained = RetainedLocalRankPrefix::new(PREFIX).expect("rank storage");
        let reserved = retained.hits.as_ptr();
        let capacity = retained.hits.capacity();
        // Worst-to-best traversal keeps replacing the root even after the
        // prefix is full; this is not an early-discard comparison shortcut.
        for hit in full.iter().rev() {
            retained.offer(*hit, &counted);
            assert!(retained.hits.len() <= PREFIX);
            assert_eq!(retained.hits.as_ptr(), reserved);
            assert_eq!(retained.hits.capacity(), capacity);
        }
        let logarithmic_bound = MATCHES * (2 * PREFIX.ilog2() as usize + 1);
        assert!(
            comparisons.get() <= logarithmic_bound,
            "{} comparisons exceed {logarithmic_bound}",
            comparisons.get()
        );
        assert_eq!(retained.into_sorted(compare), full[..PREFIX]);
    }
}

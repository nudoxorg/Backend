use backend_engine::{CoverageCapability, Row, RowId, ViewRoot, WorkspaceRoot};
use backend_extension_tantivy as lexical;
use backend_extension_trustfall::{SemanticQueryCorpus, SemanticQueryPresentation};
use backend_semantic::{Entity, EntityId, Source};
use backend_version::{CoverageWitness, RelationState};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    qualified: Vec<QualifiedClause>,
    /// The words as typed (case kept): what a match is placed by.
    words: Vec<String>,
}

impl LocalQuery {
    /// Creates a folded prefix query. Every whitespace-delimited clause must
    /// match, while exact tokens and canonical fields retain stronger rank.
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
        let mut terms = Vec::new();
        let mut qualified = Vec::new();
        let words = text.split_whitespace().map(str::to_owned).collect();
        for clause in text.split_whitespace() {
            if let Some(parsed) = parse_qualified_clause(clause) {
                terms.push(parsed.leaf.clone());
                qualified.push(parsed);
            } else {
                terms.push(clause.to_owned());
            }
        }
        let lexical = lexical::Query::prefix(terms, limits).map_err(QueryError::Lexical)?;
        Ok(Self {
            lexical,
            limit,
            qualified,
            words,
        })
    }

    /// Maximum displayed rows.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
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
    /// How much the row's kind is the thing its name names ([`kind_weight`]).
    kind: u8,
}

/// How much a row of `kind` is the thing its name names: a type (or the
/// package itself) before a callable, before a field or a module, before a
/// `use` that only brings the name in.
fn kind_weight(kind: &str) -> u8 {
    match kind {
        "project" | "struct" | "enum" | "trait" | "union" | "class" | "interface" => 4,
        "type" | "function" | "method" | "macro" | "constant" | "constructor" => 3,
        "field" | "property" | "variant" | "variable" | "module" => 2,
        "import" => 0,
        _ => 1,
    }
}

impl Placement {
    #[cfg(test)]
    pub(crate) fn for_test(name: &str, package: &str, kind: &str, external: bool) -> Self {
        Self {
            name: name.to_owned(),
            package: Some(package.to_owned()),
            external,
            kind: kind_weight(kind),
        }
    }

    /// The key a match is ordered by (higher first): a declaration before a
    /// link to one elsewhere; a row whose whole name is one of the words (as
    /// typed, then in any case); a row whose package another word names; a
    /// kind that is the named thing before one that only mentions it.
    pub(crate) fn key(&self, words: &[String]) -> (bool, u8, bool, u8) {
        placement_key(
            &self.name,
            self.package.as_deref(),
            self.external,
            self.kind,
            words,
        )
    }
}

fn placement_key(
    name: &str,
    package: Option<&str>,
    external: bool,
    kind: u8,
    words: &[String],
) -> (bool, u8, bool, u8) {
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
        kind,
    )
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

pub(crate) struct Corpus {
    pub(crate) workspace: WorkspaceRoot,
    pub(crate) view: ViewRoot,
    pub(crate) coverage: CoverageWitness,
    pub(crate) lexical: lexical::TantivyAdapter,
    pub(crate) lexical_binding: lexical::Binding,
    pub(crate) selected: SelectedCorpus,
    pub(crate) semantic_evidence: SemanticQueryCorpus,
}

#[derive(Clone, Copy)]
struct SelectedEntity {
    entity: EntityId,
    row: RowId,
    candidate: backend_extension_qdrant::CandidateId,
    fact_index: usize,
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
    fn placement_key(&self, entity: EntityId, words: &[String]) -> Option<(bool, u8, bool, u8)> {
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
            presentation.kind == "external",
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
            .map_err(|_| QueryError::LexicalProvider)?;
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
                .map_err(|_| QueryError::LexicalProvider)?
            {
                let Some(selected) = self.corpus.selected.entity(entity) else {
                    return Err(QueryError::LexicalProvider);
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
        validate_view_binding(&view, &expected_view_capability)?;
        if self.selected.as_ref().is_some_and(|selected| {
            selected.matches_selection(workspace, &view, coverage, &semantic_evidence)
        }) {
            self.maintenance = Some(SnapshotMaintenance::Reused);
            return self.selected.as_ref().ok_or(QueryError::InvalidView);
        }
        let revised = match self.selected.as_mut() {
            Some(selected) => selected.try_revise(
                workspace,
                &view,
                &expected_view_capability,
                coverage,
                &semantic_evidence,
                self.durable_root.as_deref(),
                self.durable_budget,
            ),
            None => Ok(None),
        };
        match revised {
            Ok(Some(maintenance)) => {
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
                if matches!(error, QueryError::LexicalProvider) && self.durable_root.is_none() {
                    self.selected = None;
                } else {
                    return Err(error);
                }
            }
        }
        let (selected, action) = QueryCoordinator::new_with_durable_root(
            workspace,
            view,
            &expected_view_capability,
            coverage,
            semantic_evidence,
            self.durable_root.clone(),
            self.durable_budget,
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
    ) -> Result<(Self, lexical::DurableProjectionAction), QueryError> {
        let prepared = prepare_corpus(
            workspace,
            view,
            expected_view_capability,
            coverage,
            semantic_evidence,
        )?;
        let (source, action) = match durable_root.as_deref() {
            Some(root) => lexical::TantivySource::open_or_build_in_dir_with_budget_and_action(
                &prepared.state,
                lexical::Limits::default(),
                root,
                durable_budget,
            ),
            None => lexical::TantivySource::build(&prepared.state, lexical::Limits::default())
                .map(|source| (source, lexical::DurableProjectionAction::Built)),
        }
        .map_err(|_| QueryError::LexicalProvider)?;
        let lexical = lexical::TantivyAdapter::new(source, lexical::Limits::default())
            .map_err(|_| QueryError::LexicalProvider)?;
        Ok((
            Self {
                corpus: Arc::new(Corpus {
                    workspace: prepared.workspace,
                    view: prepared.view,
                    coverage: prepared.coverage,
                    lexical,
                    lexical_binding: prepared.binding,
                    selected: prepared.selected,
                    semantic_evidence: prepared.semantic_evidence,
                }),
            },
            action,
        ))
    }

    fn try_revise(
        &mut self,
        workspace: WorkspaceRoot,
        view: &ViewRoot,
        expected_view_capability: &CoverageCapability,
        coverage: CoverageWitness,
        semantic_evidence: &SemanticQueryCorpus,
        durable_root: Option<&Path>,
        durable_budget: lexical::DurableCacheBudget,
    ) -> Result<Option<SnapshotMaintenance>, QueryError> {
        let prepared = prepare_corpus(
            workspace,
            view.clone(),
            expected_view_capability,
            coverage,
            semantic_evidence.clone(),
        )?;
        let previous_state = if durable_root.is_some() {
            let previous_view_capability = self
                .corpus
                .view
                .capability()
                .ok_or(QueryError::StaleViewBinding)?;
            Some(
                prepare_corpus(
                    self.corpus.workspace,
                    self.corpus.view.clone(),
                    &previous_view_capability,
                    self.corpus.coverage,
                    self.corpus.semantic_evidence.clone(),
                )?
                .state,
            )
        } else {
            None
        };
        let Some(corpus) = Arc::get_mut(&mut self.corpus) else {
            return Ok(None);
        };
        let durable_publication = match (durable_root, previous_state.as_ref()) {
            (Some(root), Some(previous)) => Some(
                lexical::TantivySource::open_or_advance_in_dir_with_budget_and_action(
                    previous,
                    &prepared.state,
                    lexical::Limits::default(),
                    lexical::OverlayLimits::default(),
                    root,
                    durable_budget,
                )
                .map_err(|_| QueryError::LexicalProvider)?,
            ),
            _ => None,
        };
        let maintenance = if let Some((source, revision, action)) = durable_publication {
            corpus.lexical = lexical::TantivyAdapter::new(source, lexical::Limits::default())
                .map_err(|_| QueryError::LexicalProvider)?;
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
            match corpus
                .lexical
                .maintain(&prepared.state, lexical::OverlayLimits::default())
                .map_err(|error| match error {
                    lexical::TantivySourceError::Contract(error) => QueryError::Lexical(error),
                    lexical::TantivySourceError::Backend(_)
                    | lexical::TantivySourceError::Io(_)
                    | lexical::TantivySourceError::Corrupt(_)
                    | lexical::TantivySourceError::BudgetExceeded { .. }
                    | lexical::TantivySourceError::OrdinalMapCapacityExceeded { .. }
                    | lexical::TantivySourceError::RankSnapshotBudgetExceeded { .. }
                    | lexical::TantivySourceError::PostingCoverBudgetExceeded { .. }
                    | lexical::TantivySourceError::PostingCoverWorkExceeded { .. }
                    | lexical::TantivySourceError::DurableProjectionImmutable => {
                        QueryError::LexicalProvider
                    }
                })? {
                lexical::MaintainOutcome::RebuildRequired => return Ok(None),
                lexical::MaintainOutcome::Applied(revision) => match revision.kind {
                    lexical::ProjectionKind::Rebound => SnapshotMaintenance::Rebound,
                    lexical::ProjectionKind::Revised => SnapshotMaintenance::Revised {
                        rewritten_documents: revision.rewritten_documents,
                    },
                },
            }
        };
        corpus.workspace = prepared.workspace;
        corpus.view = prepared.view;
        corpus.coverage = prepared.coverage;
        corpus.lexical_binding = prepared.binding;
        corpus.selected = prepared.selected;
        corpus.semantic_evidence = prepared.semantic_evidence;
        Ok(Some(maintenance))
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

    /// Executes a complete local search before optional provider work.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::LexicalProvider`] if the admitted local Tantivy
    /// projection cannot serve a page.
    pub fn search_local(&self, query: LocalQuery) -> Result<LocalAnswer, QueryError> {
        let mut top_matches: Vec<(EntityId, lexical::Relevance)> =
            Vec::with_capacity(query.limit());
        let mut seen_hits = 0usize;
        let mut total_matches = 0usize;
        let mut composition_failed = false;
        let words = query.words();
        let exact_lexical_total = self
            .corpus
            .lexical
            .for_each_ranked_hit(query.lexical(), |hit| {
                let Some(selected) = self.corpus.selected.entity(hit.document) else {
                    composition_failed = true;
                    return;
                };
                seen_hits = match seen_hits.checked_add(1) {
                    Some(total) => total,
                    None => {
                        composition_failed = true;
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
                        composition_failed = true;
                        return;
                    }
                };
                let ranked = (hit.document, hit.relevance);
                let insertion = top_matches.partition_point(|known| {
                    let known_key = self.corpus.placement_key(known.0, words);
                    let ranked_key = self.corpus.placement_key(ranked.0, words);
                    known_key
                        .cmp(&ranked_key)
                        .reverse()
                        .then_with(|| known.1.cmp(&ranked.1).reverse())
                        .then_with(|| known.0.cmp(&ranked.0))
                        .is_lt()
                });
                if insertion < query.limit() {
                    top_matches.insert(insertion, ranked);
                    if top_matches.len() > query.limit() {
                        top_matches.pop();
                    }
                }
            })
            .map_err(|_| QueryError::LexicalProvider)?;
        if composition_failed || exact_lexical_total != seen_hits {
            return Err(QueryError::LexicalProvider);
        }
        let mut rows = Vec::with_capacity(top_matches.len());
        for (entity, relevance) in &top_matches {
            let selected = self
                .corpus
                .selected
                .entity(*entity)
                .ok_or(QueryError::LexicalProvider)?;
            let row = self
                .corpus
                .view
                .row(selected.row)
                .ok_or(QueryError::LexicalProvider)?;
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

    /// Returns the exact number of rows requiring document embeddings for
    /// this immutable selection.
    #[must_use]
    pub fn semantic_document_count(&self) -> usize {
        self.corpus.selected.document_order.len()
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
    clauses
        .iter()
        .all(|clause| qualified_clause_matches(row_id, clause, selected, evidence))
}

fn qualified_clause_matches(
    row_id: &str,
    clause: &QualifiedClause,
    selected: &SelectedCorpus,
    evidence: &SemanticQueryCorpus,
) -> bool {
    let Some(mut current) = selected
        .fact_index(evidence, row_id)
        .map(|index| evidence.facts()[index].presentation())
    else {
        return false;
    };
    if !current
        .name
        .to_ascii_lowercase()
        .starts_with(clause.leaf.as_str())
    {
        return false;
    }
    for owner in clause.owners.iter().rev() {
        let Some(parent_id) = current.parent.as_deref() else {
            return false;
        };
        let Some(parent) = selected
            .fact_index(evidence, parent_id)
            .map(|index| evidence.facts()[index].presentation())
        else {
            return false;
        };
        if parent.name.to_ascii_lowercase() != *owner {
            return false;
        }
        current = parent;
    }
    true
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

fn prepare_corpus(
    workspace: WorkspaceRoot,
    view: ViewRoot,
    expected_view_capability: &CoverageCapability,
    coverage: CoverageWitness,
    semantic_evidence: SemanticQueryCorpus,
) -> Result<PreparedCorpus, QueryError> {
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
    let (documents, selected) = collect_selected_documents(workspace, &view, &semantic_evidence)?;
    let left_out = selected.left_out;
    if !left_out.is_empty() {
        eprintln!(
            "locald search: the view and its search evidence disagree ({} rows without evidence, {} evidence without a row); those are left out of search",
            left_out.rows_without_evidence, left_out.evidence_without_row
        );
    }
    let state = RelationState::<lexical::IndexRelation>::from_entries(documents, coverage)
        .map_err(|_| QueryError::InvalidView)?;
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
    let state = lexical::DocumentState::from_relation(binding, state, lexical::Limits::default())
        .map_err(QueryError::Lexical)?;
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
) -> Result<(Vec<(EntityId, Vec<(String, String)>)>, SelectedCorpus), QueryError> {
    let mut documents = Vec::new();
    let mut selected = Vec::new();
    let mut candidate_ids = BTreeSet::new();
    let mut selected_rows = BTreeMap::new();
    let mut cursor = backend_engine::ViewPageCursor::first(view);
    loop {
        let page = view
            .page(cursor, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|_| QueryError::InvalidView)?;
        for row in page.rows() {
            if selected_rows.insert(row.id.stable_key(), row.id).is_some() {
                return Err(QueryError::IdentityCollision);
            }
        }
        let Some(next) = page.next() else { break };
        cursor = next;
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
    let facts_by_id: BTreeMap<&str, &SemanticQueryPresentation> = semantic_evidence
        .facts()
        .iter()
        .map(|fact| {
            let presentation = fact.presentation();
            (presentation.id.as_str(), presentation)
        })
        .collect();
    let mut left_out = LeftOut::default();
    for (fact_index, fact) in semantic_evidence.facts().iter().enumerate() {
        let presentation = fact.presentation();
        let Some(row) = selected_rows.remove(&presentation.id) else {
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
                let row_fields = fields(presentation);
                checked_document_bytes(0, &row_fields)?;
                documents.push((entity, row_fields));
            }
        }
    }
    left_out.rows_without_evidence = selected_rows.len();
    selected.sort_unstable_by_key(|entry| entry.entity);
    if selected
        .windows(2)
        .any(|pair| pair[0].entity == pair[1].entity)
    {
        return Err(QueryError::IdentityCollision);
    }
    let mut candidate_order = (0..selected.len()).collect::<Vec<_>>();
    candidate_order.sort_unstable_by_key(|index| selected[*index].candidate);
    if candidate_order
        .windows(2)
        .any(|pair| selected[pair[0]].candidate == selected[pair[1]].candidate)
    {
        return Err(QueryError::IdentityCollision);
    }
    let mut document_order = (0..selected.len()).collect::<Vec<_>>();
    document_order.sort_unstable_by_key(|index| selected[*index].row);
    let mut fact_order = (0..semantic_evidence.facts().len()).collect::<Vec<_>>();
    fact_order.sort_unstable_by(|left, right| {
        semantic_evidence.facts()[*left]
            .presentation()
            .id
            .cmp(&semantic_evidence.facts()[*right].presentation().id)
    });
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryError {
    /// Empty text has no meaningful rank.
    EmptyQuery,
    /// Page size is outside the extension bound.
    InvalidLimit,
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
    /// Typed lexical admission failed.
    Lexical(lexical::Error),
    /// The concrete Tantivy projection or provider boundary failed.
    LexicalProvider,
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyQuery => formatter.write_str("query text is empty"),
            Self::InvalidLimit => formatter.write_str("query page limit is invalid"),
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
            Self::Lexical(error) => write!(formatter, "lexical query failed: {error}"),
            Self::LexicalProvider => formatter.write_str("Tantivy query provider failed"),
        }
    }
}

impl std::error::Error for QueryError {}

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

fn package_query_fact(index: usize) -> backend_extension_trustfall::SemanticQueryFact {
    let name = format!("pkg-{index}");
    let package = backend_engine::package_key(&name);
    let evidence = backend_extension_trustfall::PackageScopeEvidence::new(package);
    let id = evidence.row_id();
    backend_extension_trustfall::SemanticQueryFact::new(
        backend_extension_trustfall::SemanticQueryEvidence::Package(evidence),
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

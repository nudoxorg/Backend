use backend_engine::{Row, RowId, ViewRoot, WorkspaceRoot};
use backend_extension_tantivy as lexical;
use backend_extension_trustfall::{SemanticQueryCorpus, SemanticQueryPresentation};
use backend_semantic::{Entity, EntityId, Source};
use backend_version::{CoverageWitness, RelationState};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

/// A validated bounded local text query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalQuery {
    lexical: lexical::Query,
    limit: usize,
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
        let terms = text
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let lexical = lexical::Query::prefix(terms, limits).map_err(QueryError::Lexical)?;
        Ok(Self { lexical, limit })
    }

    /// Maximum displayed rows.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    pub(crate) const fn lexical(&self) -> &lexical::Query {
        &self.lexical
    }
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

pub(crate) struct Corpus {
    pub(crate) workspace: WorkspaceRoot,
    pub(crate) view: ViewRoot,
    pub(crate) coverage: CoverageWitness,
    pub(crate) lexical: lexical::TantivyAdapter,
    pub(crate) lexical_binding: lexical::Binding,
    pub(crate) entities: BTreeMap<EntityId, RowId>,
    pub(crate) candidates: BTreeMap<backend_extension_qdrant::CandidateId, EntityId>,
    pub(crate) semantic_documents: Box<[SemanticDocument]>,
    pub(crate) semantic_evidence: SemanticQueryCorpus,
}

impl Corpus {
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

type SelectedDocuments = (
    Vec<(EntityId, Vec<(String, String)>)>,
    BTreeMap<EntityId, RowId>,
    BTreeMap<backend_extension_qdrant::CandidateId, EntityId>,
    Box<[SemanticDocument]>,
);

/// A complete local answer. It is immediately renderable and is the only
/// input accepted by optional semantic acceleration.
#[derive(Clone)]
pub struct LocalAnswer {
    pub(crate) corpus: Arc<Corpus>,
    pub(crate) query: LocalQuery,
    pub(crate) matches: Vec<(EntityId, lexical::Relevance)>,
    /// First bounded local page.
    pub rows: Vec<RankedRow>,
    /// Total complete local lexical matches before paging. Semantic
    /// augmentation does not rewrite this coverage fact.
    pub total_matches: usize,
    /// Canonical and lexical lane evidence.
    pub lanes: Vec<LaneReport>,
}

impl LocalAnswer {
    /// Returns the complete relevance-ordered canonical identity set.
    ///
    /// The coordinator resolves provider identities through its selected
    /// immutable view, so missing projection entries cannot cross into a
    /// command reply.
    #[must_use]
    pub fn ranked_row_ids(&self) -> Vec<RowId> {
        self.matches
            .iter()
            .filter_map(|(entity, _)| self.corpus.entities.get(entity).copied())
            .collect()
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

/// Owns the immutable local search materialization selected by the current
/// published product view.
///
/// Search commands reuse the Tantivy adapter and canonical identity maps
/// while the workspace, view root, and coverage witness remain unchanged.
/// A replacement is built completely before it becomes current.
#[derive(Default)]
pub struct SearchSnapshotOwner {
    selected: Option<QueryCoordinator>,
}

impl SearchSnapshotOwner {
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
        coverage: CoverageWitness,
        semantic_evidence: SemanticQueryCorpus,
    ) -> Result<&QueryCoordinator, QueryError> {
        let current = self.selected.as_ref().is_some_and(|selected| {
            selected.matches_selection(workspace, &view, coverage, &semantic_evidence)
        });
        if !current {
            let replacement = QueryCoordinator::new(workspace, view, coverage, semantic_evidence)?;
            self.selected = Some(replacement);
        }
        self.selected.as_ref().ok_or(QueryError::InvalidView)
    }
}

impl QueryCoordinator {
    const MAX_QUERY_ROWS: u64 = 65_536;

    /// Builds the exact local lexical materialization for a selected view.
    ///
    /// The supplied witness must come from the workspace owner. A digest or
    /// row list alone cannot manufacture complete local coverage.
    ///
    /// # Errors
    ///
    /// Returns a typed [`QueryError`] when ownership is incomplete, the view
    /// is incoherent or too large, or the lexical projection rejects input.
    pub fn new(
        workspace: WorkspaceRoot,
        view: ViewRoot,
        coverage: CoverageWitness,
        semantic_evidence: SemanticQueryCorpus,
    ) -> Result<Self, QueryError> {
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
        if view.row_count() > Self::MAX_QUERY_ROWS {
            return Err(QueryError::CorpusLimit);
        }
        if semantic_evidence.workspace() != workspace {
            return Err(QueryError::InvalidSemanticEvidence);
        }
        let (documents, entities, candidates, semantic_documents) =
            collect_selected_documents(workspace, &view, &semantic_evidence)?;
        let state = RelationState::<lexical::IndexRelation>::from_entries(
            documents.iter().cloned(),
            coverage,
        )
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
        let state =
            lexical::DocumentState::new(binding, coverage, documents, lexical::Limits::default())
                .map_err(QueryError::Lexical)?;
        let lexical = lexical::TantivySource::local_adapter(&state, lexical::Limits::default())
            .map_err(|_| QueryError::LexicalProvider)?;
        Ok(Self {
            corpus: Arc::new(Corpus {
                workspace,
                view,
                coverage,
                lexical,
                lexical_binding: binding,
                entities,
                candidates,
                semantic_documents,
                semantic_evidence,
            }),
        })
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
        let mut hits = Vec::new();
        let mut cursor = None;
        loop {
            let request = lexical::QueryRequest {
                binding: self.corpus.lexical_binding,
                query: query.lexical().clone(),
                cursor,
                limit: lexical::Limits::default().max_page,
            };
            let page = self
                .corpus
                .lexical
                .query(&request)
                .map_err(|_| QueryError::LexicalProvider)?;
            hits.extend(page.hits);
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        let total_matches = hits.len();
        let matches = hits
            .into_iter()
            .map(|hit| (hit.document, hit.relevance))
            .collect::<Vec<_>>();
        let rows = matches
            .iter()
            .take(query.limit())
            .filter_map(|(entity, relevance)| {
                self.corpus
                    .entities
                    .get(entity)
                    .and_then(|id| self.corpus.view.row(*id))
                    .map(|row| RankedRow {
                        row,
                        lexical_relevance: Some(*relevance),
                    })
            })
            .collect();
        let selected_rows = self.corpus.entities.len();
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
                    coverage: CoverageBasis::CompleteView { selected_rows },
                    freshness: Freshness::Current,
                    source: Some(canonical),
                },
                LaneReport {
                    lane: Lane::Lexical,
                    coverage: CoverageBasis::CompleteView { selected_rows },
                    freshness: Freshness::Current,
                    source: Some(lexical),
                },
            ],
        })
    }

    /// Returns the exact document inputs selected by this coordinator.
    ///
    /// The coordinator has already validated the view and bounded its rows,
    /// so callers can safely pass these inputs to an optional semantic
    /// producer.  Rebuilding a coordinator is required when the selected view
    /// changes.
    #[must_use]
    pub fn semantic_documents(&self) -> &[SemanticDocument] {
        &self.corpus.semantic_documents
    }

    /// Returns the exact number of rows requiring document embeddings for
    /// this immutable selection.
    #[must_use]
    pub fn semantic_document_count(&self) -> usize {
        self.corpus.semantic_documents.len()
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
            .candidates
            .get(&candidate)
            .is_some_and(|selected| *selected == entity)
            .then_some(candidate)
    }
}

fn collect_selected_documents(
    workspace: WorkspaceRoot,
    view: &ViewRoot,
    semantic_evidence: &SemanticQueryCorpus,
) -> Result<SelectedDocuments, QueryError> {
    let capacity = usize::try_from(view.row_count()).map_err(|_| QueryError::CorpusLimit)?;
    let mut documents = Vec::with_capacity(capacity);
    let mut entities = BTreeMap::new();
    let mut candidates = BTreeMap::new();
    let mut document_bytes = 0usize;
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
    let mut semantic_documents = Vec::with_capacity(semantic_evidence.facts().len());
    for fact in semantic_evidence.facts() {
        let presentation = fact.presentation();
        let row = selected_rows
            .remove(&presentation.id)
            .ok_or(QueryError::InvalidSemanticEvidence)?;
        {
            let entity = entity_id(workspace, row)?;
            let candidate = candidate_id(entity, semantic_evidence.evidence_digest())?;
            if entities.insert(entity, row).is_some()
                || candidates.insert(candidate, entity).is_some()
            {
                return Err(QueryError::IdentityCollision);
            }
            let row_fields = fields(presentation);
            document_bytes = checked_document_bytes(document_bytes, &row_fields)?;
            documents.push((entity, row_fields));
            semantic_documents.push(SemanticDocument {
                row,
                text: semantic_text(presentation),
            });
        }
    }
    if !selected_rows.is_empty() {
        return Err(QueryError::InvalidSemanticEvidence);
    }
    Ok((
        documents,
        entities,
        candidates,
        semantic_documents.into_boxed_slice(),
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
    /// Two logical rows collapsed onto one cross-index identity.
    IdentityCollision,
    /// The selected view could not form a canonical lexical relation.
    InvalidView,
    /// Typed semantic facts do not exactly cover the selected presentation rows.
    InvalidSemanticEvidence,
    /// The view exceeds the coordinator's explicit materialization bound.
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
            Self::IdentityCollision => formatter.write_str("cross-index identity collision"),
            Self::InvalidView => formatter.write_str("selected view is not a valid query corpus"),
            Self::InvalidSemanticEvidence => formatter.write_str(
                "typed semantic evidence does not exactly cover the selected query corpus",
            ),
            Self::CorpusLimit => {
                formatter.write_str("selected view exceeds the query corpus bound")
            }
            Self::Lexical(error) => write!(formatter, "lexical query failed: {error}"),
            Self::LexicalProvider => formatter.write_str("Tantivy query provider failed"),
        }
    }
}

impl std::error::Error for QueryError {}

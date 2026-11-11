//! Result lists: search, names, relations, and any other bounded row page.
//!
//! One record is two lines, never one. The first line is the readable trail;
//! the second is the exact coordinate an agent passes back, on its own so it
//! can be copied without picking it out of prose, and never truncated by the
//! width logic. Everything else — kind glyph, language tag, lane provenance,
//! score — rides on the trail line where it costs nothing.

use crate::coverage::CoverageLine;
use crate::identity::Identity;
use crate::language::Language;
use crate::signature::Signature;
use backend_library::{DeclarationKind, PageContinuation, Row, RowState, SourceLocation};

/// The lifecycle of one result row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RecordState {
    /// Row content is available.
    Ready,
    /// Row is waiting for a demanded lane.
    Loading,
    /// Row failed with a typed lane result.
    Failed,
}

impl RecordState {
    /// Lowers one engine row state.
    #[must_use]
    pub const fn from_row(state: RowState) -> Self {
        match state {
            RowState::Ready => Self::Ready,
            RowState::Loading => Self::Loading,
            RowState::Failed => Self::Failed,
        }
    }

    /// Returns the stable lowercase name shared by every surface.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Loading => "loading",
            Self::Failed => "failed",
        }
    }

    /// Returns the glyph that precedes a record.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Ready => "●",
            Self::Loading => "◐",
            Self::Failed => "✗",
        }
    }
}

/// The owner's descending order key, not a calibrated relevance measurement.
/// Published names/search encode zero-based absolute rank as `u32::MAX - rank`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Score(u32);

impl Score {
    /// Retains one reported score.
    #[must_use]
    pub const fn new(score: u32) -> Self {
        Self(score)
    }

    /// Returns the reported score.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One result row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    identity: Identity,
    kind: Option<DeclarationKind>,
    language: Language,
    state: RecordState,
    signature: Option<Signature>,
    summary: Option<String>,
    score: Option<Score>,
}

impl Record {
    /// Lowers one engine row into a result record.
    #[must_use]
    pub fn from_row(row: &Row) -> Self {
        let captured = row.source.captured();
        let identity = Identity::parse_with_key(&row.label, row.id.into()).with_captured_source(
            captured.map(SourceLocation::path),
            captured.map(SourceLocation::start_line),
        );
        let language = identity.language();
        let signature = row
            .signature
            .as_ref()
            .and_then(|text| {
                Signature::preview_declaration(
                    text,
                    language,
                    row.kind,
                    identity.name(),
                    &row.excerpt,
                )
            });
        Self {
            identity,
            kind: row.kind,
            language,
            state: RecordState::from_row(row.state),
            signature: signature.filter(|signature| !signature.is_empty()),
            summary: None,
            score: row.score.map(Score::new),
        }
    }

    /// Attaches a one-line summary drawn from the row's documentation.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        let summary = summary.into();
        let trimmed = summary.trim();
        self.summary = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        self
    }

    /// Returns the record identity.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the declaration kind.
    #[must_use]
    pub const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the source language.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    /// Whether the selected coordinate names an external endpoint. This is
    /// presentation classification, never a claim of local source ownership.
    #[must_use]
    pub(crate) fn is_external_reference(&self) -> bool {
        self.identity.shape() == crate::IdentityShape::External
            && !matches!(self.identity.key(), crate::IdentityKey::Package(_))
    }

    /// Returns the row lifecycle.
    #[must_use]
    pub const fn state(&self) -> RecordState {
        self.state
    }

    /// Returns the tokenized signature.
    #[must_use]
    pub const fn signature(&self) -> Option<&Signature> {
        self.signature.as_ref()
    }

    /// Returns the one-line summary.
    #[must_use]
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// Returns the reported score.
    #[must_use]
    pub const fn score(&self) -> Option<Score> {
        self.score
    }
}

/// A raw source-policy claim attached to its exact graph result identity.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphSourcePolicy {
    /// Claim carried by a node.
    Node { id: backend_library::GraphNodeId, source_policy: backend_library::SourcePolicyQualification },
}

impl GraphSourcePolicy {
    pub(crate) fn note(&self) -> Option<String> {
        let (kind, id, policy) = match self {
            Self::Node { id, source_policy } => ("node", backend_library::encode_id(id.as_bytes()), source_policy),
        };
        crate::product::source_policy_note(policy).map(|note| format!("graph {kind} {id}: {note}"))
    }
}

/// One bounded page of result records with its honest lane coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordList {
    query: String,
    coverage: CoverageLine,
    records: Box<[Record]>,
    more: bool,
    continuation: Option<PageContinuation>,
    page_basis: Option<(backend_library::Basis, u64)>,
    /// Why this page has no records, when the caller proved a reason other
    /// than a missed declaration. Search leaves this empty.
    empty_reason: Option<String>,
    graph_gap: Option<String>,
    graph_source_policies: Vec<GraphSourcePolicy>,
    graph_center: Option<backend_library::GraphNodeId>,
    graph_edges: Vec<backend_library::RichGraphEdge>,
    graph_labels: Vec<(backend_library::GraphNodeId, String)>,
}

impl RecordList {
    /// Records one bounded result page.
    #[must_use]
    pub fn new(
        query: impl Into<String>,
        coverage: CoverageLine,
        records: impl Into<Box<[Record]>>,
    ) -> Self {
        Self {
            query: query.into(),
            coverage,
            records: records.into(),
            more: false,
            continuation: None,
            page_basis: None,
            empty_reason: None,
            graph_gap: None,
            graph_source_policies: Vec::new(),
            graph_center: None,
            graph_edges: Vec::new(),
            graph_labels: Vec::new(),
        }
    }

    /// Retains qualified graph claims with their exact result identities.
    #[must_use]
    pub fn with_graph_source_policies(mut self, graph: Option<&backend_library::RichGraphSnapshot>) -> Self {
        if let Some(graph) = graph {
            self.graph_source_policies.extend(graph.nodes.iter().filter(|node| !node.source_policy.is_unknown()).map(|node| GraphSourcePolicy::Node { id: node.id, source_policy: node.source_policy.clone() }));
            self.graph_labels.extend(graph.nodes.iter().map(|node| (node.id, node.label.as_str().to_owned())));
            self.graph_center = Some(graph.center);
            self.graph_edges.extend(graph.edges.iter().cloned());
        }
        self
    }

    /// Returns a readable graph node label when supplied by the owner.
    #[must_use]
    pub fn graph_label(&self, id: backend_library::GraphNodeId) -> String {
        self.graph_labels.iter().find(|(key, _)| *key == id).map(|(_, label)| label.clone())
            .unwrap_or_else(|| format!("node {}", &backend_library::encode_id(id.as_bytes())[..12]))
    }

    /// Returns unverified result-specific graph claims.
    #[must_use]
    pub fn graph_source_policies(&self) -> &[GraphSourcePolicy] {
        &self.graph_source_policies
    }

    /// Returns the query center as metadata, not as a page row.
    #[must_use]
    pub const fn graph_center(&self) -> Option<backend_library::GraphNodeId> {
        self.graph_center
    }

    /// Returns the exact page-touching graph edges and their own evidence.
    #[must_use]
    pub fn graph_edges(&self) -> &[backend_library::RichGraphEdge] {
        &self.graph_edges
    }

    /// Names why an empty page is empty. A graph or related probe with no
    /// edges must not reuse the search sentence.
    #[must_use]
    pub fn with_empty_reason(mut self, reason: impl Into<String>) -> Self {
        self.empty_reason = Some(reason.into());
        self
    }

    /// Retains graph-specific uncertainty independently of pagination.
    #[must_use]
    pub fn with_graph_gap(mut self, reason: impl Into<String>) -> Self {
        self.coverage = self.coverage.without_graph_completion();
        self.graph_gap = Some(reason.into());
        self
    }

    /// Explains why the returned graph relations may be incomplete.
    #[must_use]
    pub fn graph_gap(&self) -> Option<&str> {
        self.graph_gap.as_deref()
    }

    /// Attaches the per-query semantic lane state without changing the
    /// complete lexical rows in this page.
    #[must_use]
    pub fn with_semantic_search_status(
        mut self,
        status: backend_library::SemanticSearchStatus,
    ) -> Self {
        self.coverage = self.coverage.with_semantic_search_status(status);
        self
    }

    /// Keeps the response's name-only scope separate from full search readiness.
    #[must_use]
    pub fn with_published_names(
        mut self,
        readiness: backend_library::PublishedSearchReadiness,
    ) -> Self {
        self.coverage = self.coverage.with_published_names(readiness);
        self
    }

    /// The sentence a renderer prints when [`Self::is_empty`] is true.
    #[must_use]
    pub fn empty_explanation(&self) -> String {
        if let Some(reason) = &self.empty_reason {
            return reason.clone();
        }
        if self.coverage.has_unavailable() {
            return "no rows, and at least one lane answered nothing — read the marks above"
                .to_owned();
        }
        if self.coverage.readiness() == "indexing" {
            return "no rows yet; indexing has not finished for this revision".to_owned();
        }
        format!("no declaration matches {:?} at this revision", self.query)
    }

    /// Narrows a name resolution to the declarations that could be meant.
    ///
    /// An import of a name and an external endpoint that shares it are not candidates for
    /// "which declaration is this"; `search` still lists them. They are dropped only while a
    /// real declaration remains, so a name that exists only as an import or an external is
    /// still answered.
    #[must_use]
    pub fn resolution_view(mut self) -> Self {
        let candidates = self
            .records
            .iter()
            .filter(|record| {
                record.kind() != Some(DeclarationKind::Import) && !record.is_external_reference()
            })
            .cloned()
            .collect::<Vec<_>>();
        if !candidates.is_empty() && candidates.len() < self.records.len() {
            self.records = candidates.into_boxed_slice();
        }
        self
    }

    /// Marks that the owner holds another page after this one.
    #[must_use]
    pub const fn with_more(mut self, more: bool) -> Self {
        self.more = more;
        self
    }

    /// Retains the owner-issued continuation for a following page.
    #[must_use]
    pub const fn with_continuation(mut self, continuation: Option<PageContinuation>) -> Self {
        self.continuation = continuation;
        self.more = self.continuation.is_some();
        self
    }

    /// Retains immutable producer scope for bounded first-page re-reads.
    #[must_use]
    pub const fn with_page_basis(mut self, basis: backend_library::Basis, sequence: u64) -> Self {
        self.page_basis = Some((basis, sequence));
        self
    }

    /// Returns native scope; row count and display text are never a basis.
    #[must_use]
    pub const fn page_basis(&self) -> Option<(backend_library::Basis, u64)> {
        self.page_basis
    }

    /// Returns the query text this page answers.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns the honest lane coverage behind this page.
    #[must_use]
    pub const fn coverage(&self) -> CoverageLine {
        self.coverage
    }

    /// Returns the records in rank order.
    #[must_use]
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Returns whether another page follows.
    #[must_use]
    pub const fn has_more(&self) -> bool {
        self.more
    }

    /// Returns the owner-issued continuation, when another page follows.
    #[must_use]
    pub const fn continuation(&self) -> Option<PageContinuation> {
        self.continuation
    }

    /// Returns whether this page carried no record.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The proven empty-page reason, when the caller set one.
    #[must_use]
    pub fn empty_reason(&self) -> Option<&str> {
        self.empty_reason.as_deref()
    }
}

#[cfg(test)]
mod signature_preview_tests {
    use super::*;
    use backend_library::{
        Basis, Coverage, DeclarationKind, SourceExcerpt, SourceExcerptExtent, SourceLocation,
        package_key, symbol_key,
    };

    #[test]
    fn graph_gap_survives_json_and_text_without_claiming_empty_complete() {
        let list = RecordList::new(
            "caller",
            CoverageLine::new(&[Coverage::Complete], None),
            Vec::<Record>::new(),
        )
        .with_graph_gap("TSX profile partial")
        .with_empty_reason("no graph edges were returned; relation coverage is unknown");
        let dto = crate::dto::RecordListDto::new(&list);
        assert_eq!(
            list.coverage()
                .lanes()
                .iter()
                .find(|lane| lane.lane() == backend_library::Lane::Graph)
                .unwrap()
                .state(),
            crate::coverage::LaneState::Unobserved
        );
        assert_eq!(
            list.coverage()
                .lanes()
                .iter()
                .find(|lane| lane.lane() == backend_library::Lane::Semantic)
                .unwrap()
                .state(),
            crate::coverage::LaneState::Complete
        );
        assert_eq!(dto.graph_gap.as_deref(), Some("TSX profile partial"));
        assert!(crate::render::markdown::records(&list, None).contains("TSX profile partial"));
        assert!(crate::render::markdown::records(&list, None).contains("coverage is unknown"));
        let answer = crate::Answer::Records(Box::new(list));
        for detail in [
            crate::budget::Detail::Summary,
            crate::budget::Detail::Standard,
            crate::budget::Detail::Full,
        ] {
            let encoded = crate::budget::encode_answer(
                &answer,
                detail,
                None,
                crate::budget::DEFAULT_RESPONSE_BUDGET_BYTES,
            )
            .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).unwrap();
            assert_eq!(value["graph_gap"], "TSX profile partial");
        }
    }

    #[test]
    fn result_lists_share_bounded_previews_and_keep_the_captured_declaration() {
        for (name, signature) in [
            (
                "Workout",
                format!(
                    "@Component({{\r\n  template: `{}`\r\n}})\r\nexport class Workout {{}}",
                    "<div>workout</div>".repeat(80)
                ),
            ),
            (
                "routes",
                format!(
                    "const routes = [\r\n{}\r\n];",
                    "  { path: '/workouts', component: Workout },\r\n".repeat(30)
                ),
            ),
        ] {
            let coordinate = format!("/abs/app::src/app.ts:4::{name}");
            let row = Row::in_package(
                backend_library::RowId::Symbol(symbol_key(&coordinate)),
                Basis::new(
                    backend_library::view_state_root(&[]),
                    backend_library::object_version(&[]),
                ),
                package_key("/abs/app"),
                &coordinate,
            )
            .with_kind(if name == "Workout" {
                DeclarationKind::Class
            } else {
                DeclarationKind::Constant
            })
            .with_signature(&signature)
            .with_source(SourceLocation::new("src/app.ts", 4).expect("source site"))
            .with_excerpt(
                SourceExcerpt::captured(&signature, SourceExcerptExtent::Complete)
                    .expect("captured source"),
            );
            let list = crate::assemble::record_list_from_rows(
                name,
                std::slice::from_ref(&row),
                &[Coverage::Complete],
                false,
            );
            let dto = crate::dto::RecordListDto::new(&list);
            let preview = dto.records[0]
                .signature
                .as_ref()
                .expect("result signature preview");
            assert!(preview.chars().count() <= 240);
            assert!(preview.ends_with('…'));
            assert!(!preview.contains(['\r', '\n']));
            assert_eq!(dto.records[0].identity.coordinate, coordinate);
            assert_eq!(row.signature.as_deref(), Some(signature.as_str()));
            assert_eq!(row.excerpt.text(), Some(signature.as_str()));
            let unix = signature.replace("\r\n", "\n");
            let unix_row = row.clone().with_signature(&unix);
            let unix_list = crate::assemble::record_list_from_rows(
                name,
                std::slice::from_ref(&unix_row),
                &[Coverage::Complete],
                false,
            );
            assert_eq!(
                dto,
                crate::dto::RecordListDto::new(&unix_list),
                "CLI/MCP result DTOs must give equivalent source line endings identical previews"
            );
            let text = crate::render::text::records(&list, None, crate::Theme::plain());
            let markdown = crate::render::markdown::records(&list, None);
            assert!(text.contains(preview));
            assert!(markdown.contains(preview));
            assert!(text.contains(&coordinate));
            assert!(markdown.contains(&coordinate));
            assert!(!text.contains("\r\n"));
            assert!(!markdown.contains("\r\n"));
        }
    }
}

#[cfg(test)]
mod search_provenance_tests {
    use super::*;
    use backend_library::{Basis, Coverage, RowId, SourceLocation, object_version, symbol_key, view_state_root};

    #[test]
    fn external_search_rows_keep_distinct_operands_without_claiming_local_source() {
        let basis = Basis::new(view_state_root(&[]), object_version(&[]));
        let external = [
            format!("/abs/app::external::symbol:{}::React", "11".repeat(32)),
            format!("/abs/app::external::symbol:{}::React", "22".repeat(32)),
        ];
        let coordinates = [
            "/abs/app::src/react.tsx:3::React".to_owned(),
            "/abs/app::src/external.ts:7::React".to_owned(),
            format!("/abs/app::semantic::{}::React", "33".repeat(32)),
            external[0].clone(), external[1].clone(),
        ];
        let mut rows: Vec<_> = coordinates.iter().enumerate().map(|(rank, coordinate)| {
            let mut row = Row::new(RowId::Symbol(symbol_key(coordinate)), basis, coordinate);
            row.score = Some(u32::MAX - u32::try_from(rank).expect("bounded fixture"));
            row
        }).collect();
        rows[0] = rows[0].clone().with_source(SourceLocation::new("src/react.tsx", 3).expect("captured import"));
        let original = rows.clone();
        let list = crate::assemble::record_list_from_rows("React", &rows, &[Coverage::Complete], false);
        let dto = crate::dto::RecordListDto::new(&list);
        assert_eq!(dto.records.len(), coordinates.len(), "no spelling-based deduplication");
        for (rank, record) in dto.records.iter().enumerate() {
            assert_eq!(record.identity.coordinate, coordinates[rank]);
            assert_eq!(record.score, Some(u32::MAX - u32::try_from(rank).expect("rank")));
            assert_eq!(list.records()[rank].is_external_reference(), rank >= 3);
        }
        assert_eq!(dto.records[0].identity.path.as_deref(), Some("src/react.tsx"));
        assert!(dto.records[2].identity.path.is_none(), "unsourced owned semantic row stays owned");
        assert!(dto.records[3..].iter().all(|record|
            record.identity.shape == "external" && record.identity.path.is_none()
                && record.identity.line.is_none()));
        let answer = crate::Answer::Records(Box::new(list.clone()));
        for rendered in [
            crate::text::records(&list, None, crate::Theme::plain()),
            crate::markdown::records(&list, None),
            crate::markdown::summary_with_cursor(&answer, None,
                crate::ContinuationTarget::McpTool("backend.search")),
        ] {
            assert_eq!(rendered.matches("external reference").count(), 2);
            for coordinate in &coordinates { assert!(rendered.contains(coordinate)); }
        }
        assert_eq!(rows, original, "presentation never mutates selected source/rank evidence");
    }

    #[test]
    fn package_row_keys_cannot_be_relabelled_external_by_root_spelling() {
        let basis = Basis::new(view_state_root(&[]), object_version(&[]));
        for root in ["/abs/app".to_owned(), format!(
            "/abs/app::external::symbol:{}::React", "11".repeat(32))] {
            let row = Row::new(RowId::Package(backend_library::package_key(&root)), basis, &root);
            let record = Record::from_row(&row);
            assert!(matches!(record.identity().key(), crate::IdentityKey::Package(_)));
            assert!(!record.is_external_reference(), "a package key outranks display spelling");
            let list = crate::assemble::record_list_from_rows("app", &[row], &[Coverage::Complete], false);
            for rendered in [crate::text::records(&list, None, crate::Theme::plain()),
                crate::markdown::records(&list, None)] {
                assert!(!rendered.contains("external reference"));
                assert!(rendered.contains(&root), "the exact root operand stays available");
            }
        }
    }

    #[test]
    fn search_and_resolve_help_explain_reported_order_key_without_rewriting_it() {
        for command in ["search", "resolve"] {
            let grammar = crate::grammar_for(command).expect("registered command");
            assert!(grammar.when().contains("u32::MAX - rank"));
            assert!(grammar.when().contains("zero-based"));
            assert!(grammar.when().contains("not a relevance probability"));
        }
        assert_eq!(Score::new(u32::MAX).get(), u32::MAX);
        assert_eq!(Score::new(0).get(), 0, "no invented probability or rank conversion");
    }
}

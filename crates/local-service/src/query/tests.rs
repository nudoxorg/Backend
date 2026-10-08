#![allow(clippy::expect_used, clippy::too_many_lines)]

use super::*;
use backend_engine::{Fragment, Row, RowId, ViewCoverage};
use backend_extension_qdrant as semantic;
use backend_version::RelationState;
use std::num::NonZeroU32;

struct OfflineSource;

impl semantic::AnnSource for OfflineSource {
    type Error = &'static str;

    fn fetch(&self, _: &semantic::VectorSearchRequest) -> Result<semantic::AnnPage, Self::Error> {
        Err("offline")
    }
}

pub(super) fn selected_view() -> (backend_engine::WorkspaceRoot, backend_engine::ViewRoot) {
    selected_view_with_first_label("alpha exact")
}

fn selected_view_with_first_label(
    first_label: &str,
) -> (backend_engine::WorkspaceRoot, backend_engine::ViewRoot) {
    let head = crate::builtin::genesis().expect("genesis");
    let (base, _) = crate::builtin::initial_view().expect("initial view");
    let capability = crate::builtin::test_builtin_view_capability().expect("capability");
    let package = backend_engine::package_key("pkg");
    let project = Row::new(RowId::Package(package), base.basis(), "pkg");
    let first = Row::in_package(
        RowId::Symbol(backend_engine::symbol_key("alpha::exact")),
        base.basis(),
        package,
        first_label,
    )
    .with_signature("fn alpha()")
    .with_document(vec![Fragment::Text("first candidate".to_owned())]);
    let second = Row::in_package(
        RowId::Symbol(backend_engine::symbol_key("alpha::prefix")),
        base.basis(),
        package,
        "alphabet prefix",
    )
    .with_signature("fn alphabet()")
    .with_document(vec![Fragment::Text("second candidate".to_owned())]);
    let semantic_only = Row::in_package(
        RowId::Symbol(backend_engine::symbol_key("meaning::related")),
        base.basis(),
        package,
        "meaning related",
    )
    .with_document(vec![Fragment::Text("semantic-only candidate".to_owned())]);
    let view = backend_engine::ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        vec![project, first, second, semantic_only],
        vec![ViewCoverage::Complete],
        capability,
    )
    .expect("selected view");
    (head.root(), view)
}

pub(super) fn semantic_evidence(
    workspace: backend_engine::WorkspaceRoot,
    view: &backend_engine::ViewRoot,
) -> backend_extension_trustfall::SemanticQueryCorpus {
    semantic_evidence_marked(workspace, view, [7; 32])
}

fn semantic_evidence_marked(
    workspace: backend_engine::WorkspaceRoot,
    view: &backend_engine::ViewRoot,
    marker: [u8; 32],
) -> backend_extension_trustfall::SemanticQueryCorpus {
    semantic_evidence_marked_with_limits(
        workspace,
        view,
        marker,
        backend_extension_trustfall::Limits::default(),
    )
}

fn semantic_evidence_marked_with_limits(
    workspace: backend_engine::WorkspaceRoot,
    view: &backend_engine::ViewRoot,
    marker: [u8; 32],
    limits: backend_extension_trustfall::Limits,
) -> backend_extension_trustfall::SemanticQueryCorpus {
    let package = backend_engine::package_key("pkg");
    let project = RowId::Package(package).stable_key();
    let profile = backend_semantic::vocabulary::LanguageProfile::Rust(
        backend_semantic::vocabulary::RustEdition::Rust2021,
    );
    let facts = view
        .rows()
        .iter()
        .map(|row| {
            let evidence = if row.id == RowId::Package(package) {
                backend_extension_trustfall::SemanticQueryEvidence::Package(
                    backend_extension_trustfall::PackageScopeEvidence::new(package),
                )
            } else {
                backend_extension_trustfall::SemanticQueryEvidence::StructuralFallback(
                    backend_extension_trustfall::StructuralFallbackEvidence::new(
                        package, profile, marker, [8; 32],
                    ),
                )
            };
            backend_extension_trustfall::SemanticQueryFact::new(
                evidence,
                backend_extension_trustfall::SemanticQueryPresentation {
                    id: row.id.stable_key(),
                    kind: row
                        .kind
                        .map_or("project", backend_engine::DeclarationKind::name)
                        .to_owned(),
                    coordinate: row.label.clone(),
                    name: row.label.clone(),
                    signature: row.signature.clone(),
                    documentation: row
                        .document
                        .iter()
                        .filter_map(|fragment| match fragment {
                            Fragment::Text(value) | Fragment::Code(value) => Some(value.as_str()),
                            Fragment::Link { label, .. } => Some(label.as_str()),
                            Fragment::Break => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    score: None,
                    project: (row.id != RowId::Package(package)).then(|| project.clone()),
                    parent: None,
                    related: Box::new([]),
                },
            )
        })
        .collect();
    backend_extension_trustfall::SemanticQueryCorpus::admit_with_limits(workspace, facts, limits)
        .expect("typed semantic evidence")
}

#[test]
fn local_answer_is_complete_and_ranked_before_optional_work() {
    let (workspace, view) = selected_view();
    let evidence = semantic_evidence(workspace, &view);
    let capability = view.capability().expect("view capability");
    let coordinator = QueryCoordinator::new(
        workspace,
        view,
        capability,
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("coordinator");
    let local = coordinator
        .search_local(LocalQuery::prefix("alpha", 2).expect("query"))
        .expect("local search");
    assert_eq!(local.total_matches, 2);
    assert_eq!(local.rows[0].row.label, "alpha exact");
    assert!(
        local.rows[0]
            .lexical_relevance
            .is_some_and(backend_extension_tantivy::Relevance::is_exact)
    );
    assert_eq!(
        local.lanes[1].coverage,
        CoverageBasis::CompleteView { selected_rows: 4 }
    );
    let finished = local.finish();
    assert_eq!(finished.lanes[2].coverage, CoverageBasis::Unavailable);
}

/// One row the evidence does not describe, and one fact for a row the view
/// does not hold: the query still answers over every row that pairs, and
/// says what it left out, instead of failing every search.
#[test]
fn rows_and_evidence_that_do_not_pair_are_left_out_and_said_not_fatal() {
    let (workspace, view) = selected_view();
    let full = semantic_evidence(workspace, &view);
    let capability = view.capability().expect("view capability");
    let missing =
        backend_engine::RowId::Symbol(backend_engine::symbol_key("alpha::prefix")).stable_key();
    let mut facts = full
        .facts()
        .iter()
        .filter(|fact| fact.presentation().id != missing)
        .cloned()
        .collect::<Vec<_>>();
    let moved = full
        .facts()
        .iter()
        .find(|fact| fact.presentation().id == missing)
        .expect("the fact to move");
    let mut presentation = moved.presentation().clone();
    presentation.id =
        backend_engine::RowId::Symbol(backend_engine::symbol_key("nowhere::stray")).stable_key();
    facts.push(backend_extension_trustfall::SemanticQueryFact::new(
        moved.evidence().clone(),
        presentation,
    ));
    let evidence = backend_extension_trustfall::SemanticQueryCorpus::admit(workspace, facts)
        .expect("typed semantic evidence");
    let coordinator = QueryCoordinator::new(
        workspace,
        view,
        capability,
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("a disagreement is one package's, not every query's");
    let answer = coordinator
        .search_local(LocalQuery::prefix("alpha", 5).expect("query"))
        .expect("local search");
    assert_eq!(
        answer
            .rows
            .iter()
            .map(|ranked| ranked.row.label.as_str())
            .collect::<Vec<_>>(),
        ["alpha exact"],
        "every row that pairs is searched; the one without evidence is not"
    );
    assert_eq!(
        answer.lanes[1].coverage,
        CoverageBasis::PartialView {
            selected_rows: 3,
            left_out: LeftOut {
                rows_without_evidence: 1,
                evidence_without_row: 1
            },
        },
        "and the lane says what it left out"
    );
}

#[test]
fn borrowed_selection_preserves_large_documents_and_source_origins_across_pages() {
    const SYMBOLS: usize = 300;
    let (workspace, base) = selected_view();
    let package = backend_engine::package_key("pkg");
    let mut rows = vec![Row::new(RowId::Package(package), base.basis(), "pkg")];
    for at in 0..SYMBOLS {
        let label = format!("borrowed_{at:03}");
        let mut row = Row::in_package(
            RowId::Symbol(backend_engine::symbol_key(&label)),
            base.basis(),
            package,
            &label,
        )
        .with_kind(backend_engine::DeclarationKind::Function)
        .with_signature(format!("fn {label}()"))
        .with_document(vec![Fragment::Text(format!(
            "documentation for {label}: {}",
            "large naïve payload ".repeat(400)
        ))]);
        row.source = match at % 5 {
            0 => backend_library::SourceAvailability::Captured(
                backend_library::SourceLocation::new(
                    "src/app.rs",
                    u32::try_from(at + 1).expect("bounded declaration line"),
                )
                .expect("captured declaration site"),
            ),
            1 => backend_library::SourceAvailability::NotHydrated,
            2 => backend_library::SourceAvailability::stale_file("src/app.rs")
                .expect("stale declaration site"),
            3 => backend_library::SourceAvailability::NotCaptured,
            _ => backend_library::SourceAvailability::Unconfigured,
        };
        rows.push(row);
    }
    let view = backend_engine::ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        rows,
        vec![ViewCoverage::Complete],
        base.capability().expect("capability"),
    )
    .expect("large-document selected view");
    // The existing public snapshot reader is the independent identity/payload
    // oracle. Its first-page lookahead must not lose or duplicate a boundary row.
    let first = view
        .page(backend_engine::ViewPageCursor::first(&view), 256)
        .expect("first snapshot page");
    let second = view
        .page(first.next().expect("more than one page"), 256)
        .expect("second snapshot page");
    assert_eq!((first.rows().len(), second.rows().len()), (256, 45));
    assert!(second.next().is_none());
    let snapshot = first
        .rows()
        .iter()
        .chain(second.rows())
        .map(|row| (row.id, row))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(snapshot.len(), SYMBOLS + 1);
    assert!(
        snapshot
            .values()
            .filter(|row| row.signature.is_some())
            .all(|row| { matches!(&row.document[0], Fragment::Text(text) if text.len() > 8_000) })
    );

    let missing = RowId::Symbol(backend_engine::symbol_key("borrowed_017"));
    let stray = RowId::Symbol(backend_engine::symbol_key("borrowed_stray"));
    let profile = backend_semantic::vocabulary::LanguageProfile::Rust(
        backend_semantic::vocabulary::RustEdition::Rust2021,
    );
    let facts = snapshot
        .values()
        .map(|row| {
            let id = if row.id == missing { stray } else { row.id };
            let evidence = if row.id == RowId::Package(package) {
                backend_extension_trustfall::SemanticQueryEvidence::Package(
                    backend_extension_trustfall::PackageScopeEvidence::new(package),
                )
            } else {
                backend_extension_trustfall::SemanticQueryEvidence::StructuralFallback(
                    backend_extension_trustfall::StructuralFallbackEvidence::new(
                        package, profile, [7; 32], [8; 32],
                    ),
                )
            };
            backend_extension_trustfall::SemanticQueryFact::new(
                evidence,
                backend_extension_trustfall::SemanticQueryPresentation {
                    id: id.stable_key(),
                    kind: if row.signature.is_some() {
                        "function"
                    } else {
                        "project"
                    }
                    .to_owned(),
                    coordinate: row.label.clone(),
                    name: row.label.clone(),
                    signature: row.signature.clone(),
                    documentation: row.document.first().map_or_else(String::new, |fragment| {
                        match fragment {
                            Fragment::Text(text) => text.clone(),
                            _ => panic!("fixture has text documentation"),
                        }
                    }),
                    score: None,
                    project: (row.id != RowId::Package(package))
                        .then(|| RowId::Package(package).stable_key()),
                    parent: None,
                    related: Box::new([]),
                },
            )
        })
        .collect();
    let evidence = backend_extension_trustfall::SemanticQueryCorpus::admit(workspace, facts)
        .expect("admitted paired and unpaired facts");
    let digest = evidence.evidence_digest();
    let coordinator = QueryCoordinator::new(
        workspace,
        view.clone(),
        view.capability().expect("capability"),
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("large-document corpus");
    assert_eq!(coordinator.semantic_document_count(), SYMBOLS);
    let pages = coordinator
        .semantic_document_pages(std::num::NonZeroUsize::new(256).expect("page size"))
        .expect("document pages")
        .collect::<Vec<_>>();
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [256, 44]);
    let mut document_ids = std::collections::BTreeSet::new();
    let mut candidates = std::collections::BTreeSet::new();
    for document in pages.iter().flatten() {
        assert!(document_ids.insert(document.row));
        let row = snapshot[&document.row];
        let expected = match (row.id, row.document.first()) {
            (RowId::Package(selected), Some(Fragment::Text(text))) => {
                assert_eq!(selected, package);
                assert_eq!(row.label, "pkg");
                assert_eq!(text, "pkg");
                assert!(row.signature.is_none());
                "coordinate\npkg\ndocumentation\npkg\nkind\nproject\nname\npkg\n".to_owned()
            }
            (RowId::Symbol(_), Some(Fragment::Text(text))) => format!(
                "coordinate\n{}\ndocumentation\n{text}\nkind\nfunction\nname\n{}\nsignature\n{}\n",
                row.label,
                row.label,
                row.signature.as_deref().expect("signature")
            ),
            _ => panic!("fixture has documented package and function rows"),
        };
        assert_eq!(document.text, expected);
        let entity = local::entity_id(workspace, document.row).expect("entity");
        let candidate = local::candidate_id(entity, digest).expect("candidate");
        assert_eq!(
            coordinator.semantic_candidate(document.row),
            Some(candidate)
        );
        assert!(candidates.insert(candidate));
    }
    let paired_ids = snapshot
        .keys()
        .copied()
        .filter(|id| *id != missing)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(document_ids, paired_ids);
    assert_eq!(coordinator.semantic_candidate(missing), None);
    assert_eq!(coordinator.semantic_candidate(stray), None);
    let answer = coordinator
        .search_local(LocalQuery::prefix("borrowed", 200).expect("query"))
        .expect("large-document local search");
    assert_eq!(answer.total_matches, SYMBOLS - 1);
    // Canonical source-origin placement is retained by the borrowed selection,
    // including declaration sites whose source bytes are not currently hydrated.
    for at in [0, 1, 2] {
        let id = RowId::Symbol(backend_engine::symbol_key(&format!("borrowed_{at:03}")));
        assert_eq!(
            answer.search_origin(id),
            Some(local::SearchOrigin::SourceDeclaration)
        );
    }
    for at in [3, 4] {
        let id = RowId::Symbol(backend_engine::symbol_key(&format!("borrowed_{at:03}")));
        assert_eq!(
            answer.search_origin(id),
            Some(local::SearchOrigin::Unsourced)
        );
    }

    assert_eq!(
        answer.lanes[1].coverage,
        CoverageBasis::PartialView {
            selected_rows: SYMBOLS,
            left_out: LeftOut {
                rows_without_evidence: 1,
                evidence_without_row: 1
            },
        }
    );
    for ranked in &answer.rows {
        assert_eq!(&ranked.row, snapshot[&ranked.row.id]);
    }
    let library = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )
    .expect("large-document library");
    let query = backend_engine::Query::new(
        "borrowed",
        view.root(),
        backend_engine::QueryLimit::new(200).expect("search page size"),
    );
    let first = route_search_page(&coordinator, &library, &query).expect("first search page");
    let second = route_search_page(
        &coordinator,
        &library,
        &query.with_cursor(first.next.expect("search continuation")),
    )
    .expect("second search page");
    assert!(second.next.is_none());
    assert_eq!(
        ranked_page_ids(&first),
        answer
            .rows
            .iter()
            .map(|ranked| ranked.row.id)
            .collect::<Vec<_>>()
    );
    let ranked_ids = [ranked_page_ids(&first), ranked_page_ids(&second)].concat();
    assert_eq!(ranked_ids.len(), SYMBOLS - 1);
    let expected_matches = paired_ids
        .into_iter()
        .filter(|id| *id != RowId::Package(package))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ranked_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        expected_matches
    );
    for (rank, id) in ranked_ids.iter().enumerate() {
        let row = first
            .root
            .row_ref(*id)
            .or_else(|| second.root.row_ref(*id))
            .expect("ranked row");
        assert_eq!(row.document, snapshot[id].document);
        assert_eq!(
            row.score,
            Some(u32::MAX - u32::try_from(rank).expect("bounded rank"))
        );
    }
    assert!(!view.compatibility_rows_are_materialized());
}

#[test]
fn query_tokenization_matches_document_unicode_whitespace() {
    let query = LocalQuery::prefix("alpha\u{00a0}exact", 2).expect("query");
    assert_eq!(query.lexical().terms, ["alpha", "exact"]);

    let (workspace, view) = selected_view();
    let evidence = semantic_evidence(workspace, &view);
    let capability = view.capability().expect("view capability");
    let coordinator = QueryCoordinator::new(
        workspace,
        view,
        capability,
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("coordinator");
    let answer = coordinator.search_local(query).expect("local search");
    assert_eq!(answer.total_matches, 1);
    assert_eq!(answer.rows[0].row.label, "alpha exact");
}

#[test]
fn punctuation_query_matches_components_from_an_indexed_source_path() {
    let path = "/Users/example/projects/real-rust-canary";
    let (workspace, view) = selected_view_with_first_label(path);
    let all_symbol_ids = view
        .rows()
        .iter()
        .filter(|row| matches!(row.id, RowId::Symbol(_)))
        .map(|row| row.id)
        .collect::<std::collections::BTreeSet<_>>();
    let evidence = semantic_evidence(workspace, &view);
    let capability = view.capability().expect("view capability");
    let coordinator = QueryCoordinator::new(
        workspace,
        view,
        capability,
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("coordinator");
    for (text, terms, expected_matches) in [
        ("real-rust-canary", vec!["canary", "real", "rust"], 1),
        ("rea-rus-can", vec!["can", "rea", "rus"], 1),
        // Every symbol's admitted documentation also contains "candidate".
        ("can", vec!["can"], 3),
    ] {
        let query = LocalQuery::prefix(text, 4).expect("query");
        assert_eq!(
            query
                .lexical()
                .terms
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            terms,
            "normalized query {text:?}"
        );
        let answer = coordinator.search_local(query).expect("local search");
        assert_eq!(answer.total_matches, expected_matches, "query {text:?}");
        if text == "can" {
            assert_eq!(
                answer
                    .rows
                    .iter()
                    .map(|ranked| ranked.row.id)
                    .collect::<std::collections::BTreeSet<_>>(),
                all_symbol_ids
            );
        }
        if text == "can" {
            assert!(
                answer.rows.iter().any(|ranked| ranked.row.label == path),
                "the source-path match remains present among documentation matches"
            );
        } else {
            assert_eq!(answer.rows[0].row.label, path, "query {text:?}");
        }
    }
}

#[test]
fn punctuation_only_query_does_not_degrade_to_match_all() {
    assert!(matches!(
        LocalQuery::prefix("...", 4),
        Err(QueryError::EmptyQuery)
    ));
}

#[test]
fn search_snapshot_owner_reuses_the_exact_published_selection() {
    let (workspace, view) = selected_view();
    let coverage = crate::builtin::admitted_coverage().expect("coverage");
    let mut owner = SearchSnapshotOwner::default();
    let evidence = semantic_evidence(workspace, &view);
    let capability = view.capability().expect("view capability");
    let first = std::sync::Arc::as_ptr(
        &owner
            .select(
                workspace,
                view.clone(),
                capability.clone(),
                coverage,
                evidence.clone(),
            )
            .expect("first snapshot")
            .corpus,
    );
    let second = std::sync::Arc::as_ptr(
        &owner
            .select(workspace, view, capability, coverage, evidence)
            .expect("reused snapshot")
            .corpus,
    );
    assert_eq!(first, second);
}

#[test]
fn one_result_search_page_keeps_a_continuation_when_more_local_matches_exist() {
    let (workspace, view) = selected_view();
    let capability = view.capability().expect("view capability");
    let coordinator = QueryCoordinator::new(
        workspace,
        view.clone(),
        capability,
        crate::builtin::admitted_coverage().expect("coverage"),
        semantic_evidence(workspace, &view),
    )
    .expect("coordinator");
    let library = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )
    .expect("selected search library");
    let request = backend_engine::Query::new(
        "alpha",
        view.root(),
        backend_engine::QueryLimit::new(1).expect("one-row page"),
    );

    let (page, status) = search_page(
        &coordinator,
        &library,
        &mut RemoteSemantic::Unconfigured,
        crate::builtin::admitted_coverage().expect("coverage"),
        &request,
    )
    .expect("product search page");
    assert!(matches!(
        status,
        backend_library::SemanticSearchStatus::Unavailable { .. }
    ));

    assert!(
        page.next.is_some(),
        "a one-row page over two real matches must advertise its continuation"
    );
}

fn selected_search_route(
    workspace: backend_engine::WorkspaceRoot,
    view: backend_engine::ViewRoot,
) -> (QueryCoordinator, backend_library::Library) {
    let coordinator = QueryCoordinator::new(
        workspace,
        view.clone(),
        view.capability().expect("capability"),
        crate::builtin::admitted_coverage().expect("coverage"),
        semantic_evidence(workspace, &view),
    )
    .expect("coordinator");
    let library = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )
    .expect("library");
    (coordinator, library)
}

fn route_search_page(
    coordinator: &QueryCoordinator,
    library: &backend_library::Library,
    request: &backend_engine::Query,
) -> Result<backend_engine::ViewSnapshot, SearchPageError> {
    search_page(
        coordinator,
        library,
        &mut RemoteSemantic::Unconfigured,
        crate::builtin::admitted_coverage().expect("coverage"),
        request,
    )
    .map(|(page, _)| page)
}

fn ranked_page_ids(page: &backend_engine::ViewSnapshot) -> Vec<RowId> {
    let mut rows = page.root.rows().to_vec();
    rows.sort_by_key(|row| std::cmp::Reverse(row.score));
    rows.into_iter().map(|row| row.id).collect()
}

#[test]
fn search_page_continuation_uses_stable_scores_and_rejects_changed_inputs() {
    let (workspace, view) = selected_view();
    let (coordinator, library) = selected_search_route(workspace, view.clone());
    let query = backend_engine::Query::new(
        "alpha",
        view.root(),
        backend_engine::QueryLimit::new(1).expect("limit"),
    );
    let first = route_search_page(&coordinator, &library, &query).expect("first");
    let cursor = first.next.expect("continuation");
    let continuation = query.clone().with_cursor(cursor);
    // Fresh coordinator/library reconstruction must reproduce the scored
    // predecessor exactly, without relying on a resident page result.
    let (cold, reopened) = selected_search_route(workspace, view.clone());
    let second = route_search_page(&cold, &reopened, &continuation).expect("cold continuation");
    assert!(second.next.is_none());
    assert_ne!(ranked_page_ids(&first), ranked_page_ids(&second));
    let full = route_search_page(
        &coordinator,
        &library,
        &backend_engine::Query::new(
            "alpha",
            view.root(),
            backend_engine::QueryLimit::new(2).expect("limit"),
        ),
    )
    .expect("full");
    assert_eq!(
        first.root.rows()[0].score,
        full.root
            .row(first.root.rows()[0].id)
            .expect("first ranked row")
            .score
    );
    assert_eq!(
        [ranked_page_ids(&first), ranked_page_ids(&second)].concat(),
        ranked_page_ids(&full)
    );
    let changed_text =
        backend_engine::Query::new("alph", view.root(), query.limit()).with_cursor(cursor);
    assert!(matches!(
        route_search_page(&coordinator, &library, &changed_text),
        Err(SearchPageError::Projection(
            backend_library::LibraryError::CursorMismatch
        ))
    ));
    let changed_limit = backend_engine::Query::new(
        "alpha",
        view.root(),
        backend_engine::QueryLimit::new(2).expect("limit"),
    )
    .with_cursor(cursor);
    assert!(matches!(
        route_search_page(&coordinator, &library, &changed_limit),
        Err(SearchPageError::Projection(
            backend_library::LibraryError::CursorMismatch
        ))
    ));
    let mut reversed = ranked_page_ids(&full);
    reversed.reverse();
    assert!(matches!(
        library.search_from_ranked_ids(&continuation, &reversed),
        Err(backend_library::LibraryError::CursorMismatch)
    ));
    let (_, changed_view) = selected_view_with_first_label("alpha edited");
    let (changed, changed_library) = selected_search_route(workspace, changed_view);
    assert!(matches!(
        route_search_page(&changed, &changed_library, &continuation),
        Err(SearchPageError::Local(QueryError::StaleViewBinding))
    ));
    let mut envelope = cursor.encode_query().to_vec();
    envelope[backend_library::CURSOR_CONTROL_BYTES..].copy_from_slice(&u64::MAX.to_be_bytes());
    let oversized = backend_library::Cursor::decode_query_against(&envelope, cursor)
        .expect("typed oversized cursor");
    assert!(matches!(
        route_search_page(&coordinator, &library, &query.with_cursor(oversized)),
        Err(SearchPageError::Local(QueryError::InvalidCursor))
    ));
}

#[test]
fn search_page_enumerates_multiple_200_row_pages_beyond_4096_with_tied_names() {
    const MATCHES: usize = 4_205;
    let (workspace, base) = selected_view();
    let package = backend_engine::package_key("pkg");
    let mut rows = vec![Row::new(RowId::Package(package), base.basis(), "pkg")];
    for at in 0..MATCHES {
        rows.push(
            Row::in_package(
                RowId::Symbol(backend_engine::symbol_key(&format!("alpha::{at}"))),
                base.basis(),
                package,
                "alpha",
            )
            .with_signature("fn alpha()"),
        );
    }
    let view = backend_engine::ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        rows,
        vec![ViewCoverage::Complete],
        base.capability().expect("capability"),
    )
    .expect("tied-name selected view");
    // Admit this fixture under its exact bounded cardinality, as the product
    // does for corpora larger than Trustfall's standalone default 4096 rows.
    let evidence = semantic_evidence_marked_with_limits(
        workspace,
        &view,
        [7; 32],
        backend_extension_trustfall::Limits {
            max_rows: MATCHES + 1,
            ..backend_extension_trustfall::Limits::default()
        },
    );
    let coordinator = QueryCoordinator::new(
        workspace,
        view.clone(),
        view.capability().expect("capability"),
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("large bounded coordinator");
    let library = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )
    .expect("large library");
    let original = backend_engine::Query::new(
        "alpha",
        view.root(),
        backend_engine::QueryLimit::new(200).expect("limit"),
    );
    let mut query = original.clone();
    let mut ids = Vec::new();
    let mut pages = 0;
    loop {
        if matches!(ids.len(), 0 | 200 | 4_000) {
            let prefix = coordinator
                .search_local_page(&query)
                .expect("bounded retained prefix");
            assert_eq!(prefix.rows.len(), (ids.len() + 201).min(MATCHES));
            assert_eq!(prefix.total_matches, MATCHES);
        }
        let page = route_search_page(&coordinator, &library, &query).expect("bounded page");
        let page_ids = ranked_page_ids(&page);
        assert_eq!(page_ids.len(), (MATCHES - ids.len()).min(200));
        for row in page.root.rows() {
            let score = row.score.expect("stable ordinal score");
            assert!((u32::MAX - score) as usize >= ids.len());
        }
        ids.extend(page_ids);
        pages += 1;
        match page.next {
            Some(next) => {
                assert_eq!(next.query_offset(), ids.len() as u64);
                query = original.clone().with_cursor(next);
            }
            None => break,
        }
    }
    assert_eq!(pages, 22);
    assert_eq!(ids.len(), MATCHES);
    assert_eq!(
        ids.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        MATCHES
    );
    // Repeated names and equal relevance have a deterministic identity tie
    // breaker, which must survive both the 256 and 4096 positions.
    // The set equality above proves no drops/duplicates; a repeated full walk
    // additionally proves the stable ordering, independent of resident pages.
    let mut replay = Vec::new();
    let mut query = original.clone();
    loop {
        let page = route_search_page(&coordinator, &library, &query).expect("replay page");
        replay.extend(ranked_page_ids(&page));
        let Some(next) = page.next else { break };
        query = original.clone().with_cursor(next);
    }
    assert_eq!(replay, ids);
}

#[test]
fn search_page_prefix_budget_and_overflow_are_explicit_refusals() {
    use super::local::rank_prefix_limit;
    assert_eq!(
        rank_prefix_limit(200, 200, 4_206).expect("second page"),
        401
    );
    assert_eq!(
        rank_prefix_limit(4_000, 200, 4_206).expect("beyond 4096"),
        4_201
    );
    assert!(matches!(
        rank_prefix_limit(usize::MAX - 1, 200, usize::MAX),
        Err(QueryError::InvalidCursor)
    ));
    let maximum = backend_extension_tantivy::RankSnapshotBudget::default().max_retained_bytes()
        / size_of::<(
            backend_semantic::EntityId,
            backend_extension_tantivy::Relevance,
        )>();
    assert!(matches!(rank_prefix_limit(maximum, 1, maximum + 10),
        Err(QueryError::RankPrefixBudgetExceeded { requested, maximum: admitted })
        if requested == maximum + 2 && admitted == maximum));
}

#[test]
fn search_selection_refuses_a_view_from_the_previous_workspace_frontier() {
    let (old_workspace, old_view) = selected_view();
    let old_capability = old_view.capability().expect("old view capability");
    let coverage = crate::builtin::admitted_coverage().expect("coverage");
    let mut owner = SearchSnapshotOwner::default();
    owner
        .select(
            old_workspace,
            old_view.clone(),
            old_capability,
            coverage,
            semantic_evidence(old_workspace, &old_view),
        )
        .expect("admit old publication");

    let intent = crate::builtin::BuiltinIntent::add(
        backend_engine::package_key("new-project"),
        "new-project",
    )
    .expect("workspace edit");
    let current_head = crate::builtin::test_head_for_intent(&intent).expect("current head");
    let current_workspace = current_head.root();
    let (_, _, current_capability) =
        crate::builtin::test_view_generation(&current_head).expect("current binding");

    // Model the publication-failure window: durable source advanced, while
    // `library().view()` still exposes the complete previous view. The
    // evidence belongs to the current source snapshot, and row IDs still line
    // up, so a raw workspace/evidence/id join alone would admit mixed data.
    assert!(matches!(
        owner.select(
            current_workspace,
            old_view.clone(),
            current_capability,
            coverage,
            semantic_evidence(current_workspace, &old_view),
        ),
        Err(QueryError::StaleViewBinding)
    ));
}

#[test]
fn shared_corpus_builds_once_and_survives_a_failed_rebuild() {
    let (workspace, view) = selected_view();
    let evidence = semantic_evidence(workspace, &view);
    let mut owner = SearchSnapshotOwner::default();
    let Err("missing") = owner.shared_corpus(workspace, || Err("missing")) else {
        panic!("a failed build must surface");
    };
    assert_eq!(owner.corpus_builds(), 0);
    let mut builds = 0u64;
    let first = owner
        .shared_corpus(workspace, || {
            builds += 1;
            Ok::<_, &str>(evidence.clone())
        })
        .expect("admit");
    let second = owner
        .shared_corpus(workspace, || {
            builds += 1;
            Err("must not rebuild a resident workspace")
        })
        .expect("reuse");
    assert_eq!(builds, 1);
    assert_eq!(owner.corpus_builds(), 1);
    assert_eq!(first.evidence_digest(), evidence.evidence_digest());
    assert_eq!(second.evidence_digest(), first.evidence_digest());
    assert_eq!(second.facts().len(), first.facts().len());
}

#[test]
fn admit_corpus_retries_after_a_failed_build_and_skips_prepare_once_resident() {
    let (workspace, view) = selected_view();
    let evidence = semantic_evidence(workspace, &view);
    let mut owner = SearchSnapshotOwner::default();
    let mut prepares = 0u32;
    let Err("prepare failed") = owner.admit_corpus(
        workspace,
        || Err::<(), _>("prepare failed"),
        |_| Ok(evidence.clone()),
    ) else {
        panic!("a failed prepare must surface");
    };
    assert_eq!(prepares, 0);
    assert_eq!(owner.corpus_builds(), 0);

    let Err("build failed") = owner.admit_corpus(
        workspace,
        || {
            prepares += 1;
            Ok(())
        },
        |_| Err("build failed"),
    ) else {
        panic!("a failed build must surface");
    };
    assert_eq!(prepares, 1);
    assert_eq!(owner.corpus_builds(), 0);

    let admitted = owner
        .admit_corpus(
            workspace,
            || {
                prepares += 1;
                Ok::<(), &str>(())
            },
            |_| Ok::<_, &str>(evidence.clone()),
        )
        .expect("admit");
    assert_eq!(prepares, 2);
    assert_eq!(owner.corpus_builds(), 1);

    let reused = owner
        .admit_corpus(
            workspace,
            || Err::<(), &str>("must not page sources for a resident corpus"),
            |_| Err("must not rebuild a resident corpus"),
        )
        .expect("reuse");
    assert_eq!(prepares, 2);
    assert_eq!(owner.corpus_builds(), 1);
    assert_eq!(reused.evidence_digest(), admitted.evidence_digest());
    assert_eq!(reused.facts().len(), evidence.facts().len());
}

#[test]
fn semantic_candidate_identity_is_scoped_to_the_immutable_view() {
    let (workspace, _) = selected_view();
    let row = RowId::Symbol(backend_engine::symbol_key("same-declaration"));
    let entity = local::entity_id(workspace, row).expect("entity");
    let first = local::candidate_id(entity, [1; 32]).expect("first candidate");
    let second = local::candidate_id(entity, [2; 32]).expect("second candidate");
    assert_ne!(first, second);
}

/// A producer's synthetic function return-type "result slot" is a
/// `Variable`-kind row named exactly like its parent function, minted only
/// so the slot has a content-addressed identity (see
/// `crates/engine/src/driver/lower/{python,rust}.rs`). It must not surface
/// from `backend.search`. A class's own constructor, named exactly like the
/// class, must still surface: excluding every row that merely shares its
/// parent's name (rather than requiring `Variable` kind and a callable
/// parent) would wrongly hide it too. This asserts on the actual returned
/// rows, not counts.
#[test]
fn local_search_excludes_only_the_synthetic_result_slot() {
    let head = crate::builtin::genesis().expect("genesis");
    let (base, _) = crate::builtin::initial_view().expect("initial view");
    let capability = crate::builtin::test_builtin_view_capability().expect("capability");
    let workspace = head.root();
    let package = backend_engine::package_key("pkg");

    let ferris_id = backend_engine::symbol_key("pkg::ferris");
    let slot_id = backend_engine::symbol_key("pkg::ferris::ferris");
    let class_id = backend_engine::symbol_key("pkg::Foo");
    let ctor_id = backend_engine::symbol_key("pkg::Foo::Foo");

    let project = Row::new(RowId::Package(package), base.basis(), "pkg");
    let ferris = Row::in_package(
        RowId::Symbol(ferris_id),
        base.basis(),
        package,
        "pkg::ferris",
    )
    .with_kind(backend_engine::DeclarationKind::Function);
    // The result slot's own label ends in "::ferris" too: it is named
    // exactly like the function it belongs to.
    let slot = Row::in_package(
        RowId::Symbol(slot_id),
        base.basis(),
        package,
        "pkg::ferris::ferris",
    )
    .with_kind(backend_engine::DeclarationKind::Variable)
    .with_parent(ferris_id);
    let class = Row::in_package(RowId::Symbol(class_id), base.basis(), package, "pkg::Foo")
        .with_kind(backend_engine::DeclarationKind::Class);
    let ctor = Row::in_package(
        RowId::Symbol(ctor_id),
        base.basis(),
        package,
        "pkg::Foo::Foo",
    )
    .with_kind(backend_engine::DeclarationKind::Constructor)
    .with_parent(class_id);

    let view = backend_engine::ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        vec![
            project.clone(),
            ferris.clone(),
            slot.clone(),
            class.clone(),
            ctor.clone(),
        ],
        vec![ViewCoverage::Complete],
        capability,
    )
    .expect("selected view");

    let profile = backend_semantic::vocabulary::LanguageProfile::Rust(
        backend_semantic::vocabulary::RustEdition::Rust2021,
    );
    let project_id = RowId::Package(package).stable_key();
    let fact = |row: &Row, parent: Option<RowId>| {
        let evidence = if row.id == RowId::Package(package) {
            backend_extension_trustfall::SemanticQueryEvidence::Package(
                backend_extension_trustfall::PackageScopeEvidence::new(package),
            )
        } else {
            backend_extension_trustfall::SemanticQueryEvidence::StructuralFallback(
                backend_extension_trustfall::StructuralFallbackEvidence::new(
                    package, profile, [7; 32], [8; 32],
                ),
            )
        };
        backend_extension_trustfall::SemanticQueryFact::new(
            evidence,
            backend_extension_trustfall::SemanticQueryPresentation {
                id: row.id.stable_key(),
                kind: row
                    .kind
                    .map_or("project", backend_engine::DeclarationKind::name)
                    .to_owned(),
                coordinate: row.label.clone(),
                name: row
                    .label
                    .rsplit("::")
                    .next()
                    .unwrap_or(&row.label)
                    .to_owned(),
                signature: None,
                documentation: String::new(),
                score: None,
                project: (row.id != RowId::Package(package)).then(|| project_id.clone()),
                parent: parent.map(|id| id.stable_key()),
                related: Box::new([]),
            },
        )
    };
    let facts = vec![
        fact(&project, None),
        fact(&ferris, None),
        fact(&slot, Some(RowId::Symbol(ferris_id))),
        fact(&class, None),
        fact(&ctor, Some(RowId::Symbol(class_id))),
    ];
    let evidence = backend_extension_trustfall::SemanticQueryCorpus::admit(workspace, facts)
        .expect("typed evidence");

    let capability = view.capability().expect("view capability");
    let coordinator = QueryCoordinator::new(
        workspace,
        view,
        capability,
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("coordinator");

    let ferris_hits = coordinator
        .search_local(LocalQuery::prefix("ferris", 10).expect("query"))
        .expect("local search")
        .rows
        .into_iter()
        .map(|ranked| ranked.row.id)
        .collect::<Vec<_>>();
    assert_eq!(
        ferris_hits,
        vec![RowId::Symbol(ferris_id)],
        "the synthetic result slot must not surface from `backend.search`"
    );

    let foo_hits = coordinator
        .search_local(LocalQuery::prefix("foo", 10).expect("query"))
        .expect("local search")
        .rows
        .into_iter()
        .map(|ranked| ranked.row.id)
        .collect::<Vec<_>>();
    assert!(
        foo_hits.contains(&RowId::Symbol(ctor_id)),
        "a constructor named like its class must stay searchable: {foo_hits:?}"
    );
}

#[test]
fn qualified_owner_search_finds_the_named_method() {
    let head = crate::builtin::genesis().expect("genesis");
    let (base, _) = crate::builtin::initial_view().expect("initial view");
    let capability = crate::builtin::test_builtin_view_capability().expect("capability");
    let workspace = head.root();
    let package = backend_engine::package_key("pkg");

    let service_id = backend_engine::symbol_key("pkg::Service");
    let workout_service_id = backend_engine::symbol_key("pkg::WorkoutService");
    let update_ws_id = backend_engine::symbol_key("pkg::WorkoutService::update");
    let set_note_id = backend_engine::symbol_key("pkg::WorkoutService::setNote");
    let other_service_id = backend_engine::symbol_key("pkg::OtherService");
    let update_os_id = backend_engine::symbol_key("pkg::OtherService::update");

    let project = Row::new(RowId::Package(package), base.basis(), "pkg");
    let service = Row::in_package(
        RowId::Symbol(service_id),
        base.basis(),
        package,
        "pkg::Service",
    )
    .with_kind(backend_engine::DeclarationKind::Module);
    let workout_service = Row::in_package(
        RowId::Symbol(workout_service_id),
        base.basis(),
        package,
        "pkg::WorkoutService",
    )
    .with_kind(backend_engine::DeclarationKind::Class)
    .with_parent(service_id);
    let update_ws = Row::in_package(
        RowId::Symbol(update_ws_id),
        base.basis(),
        package,
        "pkg::WorkoutService::update",
    )
    .with_kind(backend_engine::DeclarationKind::Function)
    .with_parent(workout_service_id);
    let set_note = Row::in_package(
        RowId::Symbol(set_note_id),
        base.basis(),
        package,
        "pkg::WorkoutService::setNote",
    )
    .with_kind(backend_engine::DeclarationKind::Function)
    .with_parent(workout_service_id);
    let other_service = Row::in_package(
        RowId::Symbol(other_service_id),
        base.basis(),
        package,
        "pkg::OtherService",
    )
    .with_kind(backend_engine::DeclarationKind::Class);
    let update_os = Row::in_package(
        RowId::Symbol(update_os_id),
        base.basis(),
        package,
        "pkg::OtherService::update",
    )
    .with_kind(backend_engine::DeclarationKind::Function)
    .with_parent(other_service_id);

    let view = backend_engine::ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        vec![
            project.clone(),
            service.clone(),
            workout_service.clone(),
            update_ws.clone(),
            set_note.clone(),
            other_service.clone(),
            update_os.clone(),
        ],
        vec![ViewCoverage::Complete],
        capability,
    )
    .expect("selected view");

    let profile = backend_semantic::vocabulary::LanguageProfile::Rust(
        backend_semantic::vocabulary::RustEdition::Rust2021,
    );
    let project_id = RowId::Package(package).stable_key();
    let fact = |row: &Row, parent: Option<RowId>| {
        let evidence = if row.id == RowId::Package(package) {
            backend_extension_trustfall::SemanticQueryEvidence::Package(
                backend_extension_trustfall::PackageScopeEvidence::new(package),
            )
        } else {
            backend_extension_trustfall::SemanticQueryEvidence::StructuralFallback(
                backend_extension_trustfall::StructuralFallbackEvidence::new(
                    package, profile, [7; 32], [8; 32],
                ),
            )
        };
        backend_extension_trustfall::SemanticQueryFact::new(
            evidence,
            backend_extension_trustfall::SemanticQueryPresentation {
                id: row.id.stable_key(),
                kind: row
                    .kind
                    .map_or("project", backend_engine::DeclarationKind::name)
                    .to_owned(),
                coordinate: row.label.clone(),
                name: row
                    .label
                    .rsplit("::")
                    .next()
                    .unwrap_or(&row.label)
                    .to_owned(),
                signature: None,
                documentation: String::new(),
                score: None,
                project: (row.id != RowId::Package(package)).then(|| project_id.clone()),
                parent: parent.map(|id| id.stable_key()),
                related: Box::new([]),
            },
        )
    };
    let facts = vec![
        fact(&project, None),
        fact(&service, None),
        fact(&workout_service, Some(RowId::Symbol(service_id))),
        fact(&update_ws, Some(RowId::Symbol(workout_service_id))),
        fact(&set_note, Some(RowId::Symbol(workout_service_id))),
        fact(&other_service, None),
        fact(&update_os, Some(RowId::Symbol(other_service_id))),
    ];
    let evidence = backend_extension_trustfall::SemanticQueryCorpus::admit(workspace, facts)
        .expect("typed evidence");

    let capability = view.capability().expect("view capability");
    let coordinator = QueryCoordinator::new(
        workspace,
        view,
        capability,
        crate::builtin::admitted_coverage().expect("coverage"),
        evidence,
    )
    .expect("coordinator");

    let search = |text: &str| -> (Vec<RowId>, usize) {
        let answer = coordinator
            .search_local(LocalQuery::prefix(text, 10).expect("query"))
            .expect("local search");
        (
            answer
                .rows
                .into_iter()
                .map(|ranked| ranked.row.id)
                .collect(),
            answer.total_matches,
        )
    };

    let workout_update = RowId::Symbol(update_ws_id);
    let other_update = RowId::Symbol(update_os_id);
    let set_note_row = RowId::Symbol(set_note_id);

    let query = LocalQuery::prefix("WorkoutService.update", 10).expect("query");
    assert_eq!(query.lexical().terms, vec!["update"]);
    let (ids, total) = search("WorkoutService.update");
    assert_eq!(ids, vec![workout_update]);
    assert_eq!(total, 1);

    let (ids, total) = search("WorkoutService::update");
    assert_eq!(ids, vec![workout_update]);
    assert_eq!(total, 1);

    let (ids, total) = search("OtherService.update");
    assert_eq!(ids, vec![other_update]);
    assert_eq!(total, 1);

    let (ids, total) = search("Missing.update");
    assert_eq!(ids, Vec::<RowId>::new());
    assert_eq!(total, 0);

    let (ids, total) = search("WorkoutService.upd");
    assert_eq!(ids, vec![workout_update]);
    assert_eq!(total, 1);

    let (ids, total) = search("WorkoutService.setNote");
    assert_eq!(ids, vec![set_note_row]);
    assert_eq!(total, 1);

    let (ids, total) = search("update");
    let mut update_ids = ids;
    update_ids.sort();
    let mut expected_updates = vec![other_update, workout_update];
    expected_updates.sort();
    assert_eq!(update_ids, expected_updates);
    assert_eq!(total, 2);

    let (ids, total) = search("Service.WorkoutService.update");
    assert_eq!(ids, vec![workout_update]);
    assert_eq!(total, 1);

    let workout_type = RowId::Symbol(workout_service_id);
    let (ids, total) = search("Service.WorkoutService");
    assert_eq!(ids, vec![workout_type]);
    assert_eq!(total, 1);

    let (ids, total) = search("Service::WorkoutService");
    assert_eq!(ids, vec![workout_type]);
    assert_eq!(total, 1);

    let (ids, total) = search("Other.WorkoutService.update");
    assert_eq!(ids, Vec::<RowId>::new());
    assert_eq!(total, 0);
}

fn recipe() -> semantic::EmbeddingRecipe {
    semantic::EmbeddingRecipe {
        model: semantic::ModelVersion::from_value(&[8; 32]),
        tokenizer: semantic::TokenizerVersion::from_value(&[9; 32]),
        dimensions: NonZeroU32::new(2).expect("dimension"),
        metric: semantic::Metric::EuclideanSquared,
        pooling: semantic::EmbeddingPooling::Mean,
        normalization: semantic::EmbeddingNormalization::None,
        encoding: semantic::EmbeddingEncoding::Float32,
        query_treatment: semantic::TreatmentVersion::from_value(b"query".as_slice()),
        document_treatment: semantic::TreatmentVersion::from_value(b"document".as_slice()),
    }
}

#[test]
fn semantic_lane_only_reorders_local_matches_and_suppresses_unknown_ids() {
    let (workspace, view) = selected_view();
    let coverage = crate::builtin::admitted_coverage().expect("coverage");
    let evidence = semantic_evidence(workspace, &view);
    let capability = view.capability().expect("view capability");
    let coordinator = QueryCoordinator::new(workspace, view, capability, coverage, evidence)
        .expect("coordinator");
    let local = coordinator
        .search_local(LocalQuery::prefix("alpha", 2).expect("query"))
        .expect("local search");
    let mut known_candidates = local
        .matches
        .iter()
        .map(|(entity, _)| *entity)
        .collect::<Vec<_>>();
    assert!(
        known_candidates.len() > 1,
        "the oracle must exercise ordering"
    );
    known_candidates.reverse();
    known_candidates.push(known_candidates[0]);
    assert_eq!(
        local
            .lexical_relevance_for_candidates(&known_candidates)
            .expect("resolve already-known candidates")
            .as_slice(),
        local.matches.as_slice(),
        "all-cached candidate scores must be sorted and deduplicated like exact lexical results"
    );
    let lexical_ids = local
        .matches
        .iter()
        .map(|(entity, _)| {
            local
                .candidate_for_entity(*entity)
                .expect("lexical candidate")
        })
        .collect::<Vec<_>>();
    let lexical_entities = local
        .matches
        .iter()
        .map(|(entity, _)| *entity)
        .collect::<std::collections::BTreeSet<_>>();
    let semantic_only_row = RowId::Symbol(backend_engine::symbol_key("meaning::related"));
    let semantic_only_entity = local::entity_id(workspace, semantic_only_row).expect("entity");
    assert!(!lexical_entities.contains(&semantic_only_entity));
    let mut mixed_candidates = known_candidates.clone();
    mixed_candidates.push(semantic_only_entity);
    mixed_candidates.reverse();
    mixed_candidates.push(semantic_only_entity);
    assert_eq!(
        local
            .lexical_relevance_for_candidates(&mixed_candidates)
            .expect("resolve cached and provider candidates")
            .as_slice(),
        local.matches.as_slice(),
        "cached and fallback candidate scores share the same sorted unique result contract"
    );
    let semantic_only = coordinator
        .semantic_candidate(semantic_only_row)
        .expect("semantic-only candidate");
    let unknown = semantic::CandidateId::new(u64::MAX).expect("unknown id");
    assert!(local.candidate_row(unknown).is_none());
    let embedding = recipe();
    let payloads = vec![
        (
            lexical_ids[0].0,
            semantic::VectorPoint::new(lexical_ids[0], vec![1.0, 1.0])
                .expect("point")
                .to_payload(),
        ),
        (
            lexical_ids[1].0,
            semantic::VectorPoint::new(lexical_ids[1], vec![2.0, 2.0])
                .expect("point")
                .to_payload(),
        ),
        (
            semantic_only.0,
            semantic::VectorPoint::new(semantic_only, vec![0.1, 0.1])
                .expect("point")
                .to_payload(),
        ),
        (
            unknown.0,
            semantic::VectorPoint::new(unknown, vec![0.0, 0.0])
                .expect("point")
                .to_payload(),
        ),
    ];
    let relation =
        RelationState::<semantic::CandidateRelation>::from_entries(payloads.clone(), coverage)
            .expect("candidate relation");
    let binding = coordinator.semantic_binding(relation.root(), embedding.version());
    let state = semantic::CandidateState::new(
        binding,
        coverage,
        payloads
            .into_iter()
            .map(|(id, payload)| (semantic::CandidateId(id), payload))
            .collect(),
        semantic::Limits::default(),
    )
    .expect("candidate state");
    let facts = semantic::VectorFacts::from_recipe(state, embedding).expect("facts");
    let base = semantic::AnnBase::from_facts(
        &facts,
        semantic::SearchQuality::Exact,
        semantic::Limits::default(),
    )
    .expect("base");
    let source = semantic::MemorySource::new(
        binding,
        coverage,
        vec![unknown, semantic_only, lexical_ids[0], lexical_ids[1]],
        semantic::Limits::default(),
    )
    .expect("source");
    let index = semantic::VectorIndex::new(base, facts, source).expect("index");
    let query = semantic::QueryVector::new(embedding, vec![0.0, 0.0]).expect("query vector");
    let expected_first = local
        .candidate_row(lexical_ids[0])
        .map(|(_, row)| row.id)
        .expect("expected first row");
    let result = local.accelerate_with(
        CompositionPolicy::RerankLexical,
        SemanticAcceleration {
            index: &index,
            query: &query,
        },
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].row.id, expected_first);
    assert!(matches!(
        result.lanes[2].coverage,
        CoverageBasis::CandidateSubset { suppressed: 2, .. }
    ));

    let augmented_local = coordinator
        .search_local(LocalQuery::prefix("alpha", 3).expect("query"))
        .expect("local search");
    let augmented = augmented_local.accelerate(SemanticAcceleration {
        index: &index,
        query: &query,
    });
    assert_eq!(augmented.rows.len(), 3);
    assert_eq!(augmented.rows[0].row.label, "meaning related");
    assert!(augmented.rows[0].lexical_relevance.is_none());
    assert_eq!(augmented.lanes[2].freshness, Freshness::Current);
    assert!(matches!(
        augmented.lanes[2].source.as_ref(),
        Some(SourceBasis::Semantic { .. })
    ));
    assert!(matches!(
        augmented.lanes[2].coverage,
        CoverageBasis::CandidateSubset {
            augmented: 1,
            suppressed: 1,
            ..
        }
    ));

    // The same admitted semantic candidates must keep their ordering when
    // the product's retained lexical prefix grows on each continuation.
    let (_, page_view) = selected_view();
    let library = backend_library::Library::from_view(
        page_view.clone(),
        backend_library::Cursor::for_view_root(&page_view),
    )
    .expect("semantic paging library");
    let first_request = backend_engine::Query::new(
        "alpha",
        page_view.root(),
        backend_engine::QueryLimit::new(1).expect("semantic page limit"),
    );
    let mut request = first_request.clone();
    let mut paged_ids = Vec::new();
    for at in 0..3 {
        let local_page = coordinator
            .search_local_page(&request)
            .expect("semantic local prefix");
        let result = local_page.accelerate(SemanticAcceleration {
            index: &index,
            query: &query,
        });
        let ranked_ids = result
            .rows
            .iter()
            .map(|ranked| ranked.row.id)
            .collect::<Vec<_>>();
        let page = library
            .search_from_ranked_ids(&request, &ranked_ids)
            .expect("semantic continuation");
        paged_ids.extend(ranked_page_ids(&page));
        if at < 2 {
            request = first_request
                .clone()
                .with_cursor(page.next.expect("semantic successor"));
        } else {
            assert!(page.next.is_none());
        }
    }
    assert_eq!(
        paged_ids,
        augmented
            .rows
            .iter()
            .map(|ranked| ranked.row.id)
            .collect::<Vec<_>>()
    );

    let offline =
        semantic::VectorIndex::new(index.base().clone(), index.facts().clone(), OfflineSource)
            .expect("offline index");
    let outage_local = coordinator
        .search_local(LocalQuery::prefix("alpha", 2).expect("query"))
        .expect("outage local");
    let expected = outage_local
        .rows
        .iter()
        .map(|row| row.row.id)
        .collect::<Vec<_>>();
    let outage = outage_local.accelerate(SemanticAcceleration {
        index: &offline,
        query: &query,
    });
    assert_eq!(
        outage.rows.iter().map(|row| row.row.id).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(outage.lanes[2].coverage, CoverageBasis::Unavailable);
}

#[test]
fn evidence_only_refresh_rebinds_the_resident_lexical_index() {
    let (workspace, view) = selected_view();
    let coverage = crate::builtin::admitted_coverage().expect("coverage");
    let capability = view.capability().expect("view capability");
    let mut owner = SearchSnapshotOwner::default();
    owner
        .select(
            workspace,
            view.clone(),
            capability.clone(),
            coverage,
            semantic_evidence(workspace, &view),
        )
        .expect("cold snapshot");
    assert_eq!(owner.projection_builds(), 1);
    owner
        .select(
            workspace,
            view.clone(),
            capability.clone(),
            coverage,
            semantic_evidence_marked(workspace, &view, [9; 32]),
        )
        .expect("rebound snapshot");
    assert_eq!(
        owner.maintenance(),
        Some(local::SnapshotMaintenance::Rebound)
    );
    assert_eq!(owner.projection_builds(), 1);
    let marked = semantic_evidence_marked(workspace, &view, [9; 32]);
    let answer = owner
        .select(workspace, view, capability, coverage, marked)
        .expect("reused rebound")
        .search_local(LocalQuery::prefix("alphabet", 4).expect("query"))
        .expect("search");
    assert_eq!(answer.total_matches, 1);
    assert_eq!(answer.rows[0].row.label, "alphabet prefix");
    assert_eq!(
        owner.maintenance(),
        Some(local::SnapshotMaintenance::Reused)
    );
    assert_eq!(owner.projection_builds(), 1);
}

#[test]
fn one_renamed_symbol_rewrites_only_that_lexical_document() {
    let (workspace, view) = selected_view();
    let coverage = crate::builtin::admitted_coverage().expect("coverage");
    let mut owner = SearchSnapshotOwner::default();
    let evidence = semantic_evidence(workspace, &view);
    let capability = view.capability().expect("view capability");
    owner
        .select(workspace, view, capability, coverage, evidence)
        .expect("cold snapshot");
    let (workspace, renamed) = selected_view_with_first_label("zephyr marker");
    let evidence = semantic_evidence(workspace, &renamed);
    let capability = renamed.capability().expect("view capability");
    owner
        .select(
            workspace,
            renamed.clone(),
            capability.clone(),
            coverage,
            evidence.clone(),
        )
        .expect("revised snapshot");
    assert_eq!(
        owner.maintenance(),
        Some(local::SnapshotMaintenance::Revised {
            rewritten_documents: 1
        })
    );
    assert_eq!(owner.projection_builds(), 1);
    owner
        .select(
            workspace,
            renamed.clone(),
            capability.clone(),
            coverage,
            evidence.clone(),
        )
        .expect("reused revision");
    assert_eq!(
        owner.maintenance(),
        Some(local::SnapshotMaintenance::Reused)
    );
    assert_eq!(owner.projection_builds(), 1);
    let coordinator = owner
        .select(workspace, renamed, capability, coverage, evidence)
        .expect("search snapshot");
    let renamed_hit = coordinator
        .search_local(LocalQuery::prefix("zephyr", 4).expect("query"))
        .expect("renamed search");
    assert_eq!(renamed_hit.total_matches, 1);
    assert_eq!(renamed_hit.rows[0].row.label, "zephyr marker");
    let sibling = coordinator
        .search_local(LocalQuery::prefix("alphabet", 4).expect("query"))
        .expect("sibling search");
    assert_eq!(sibling.total_matches, 1);
    assert_eq!(sibling.rows[0].row.label, "alphabet prefix");
    let retired = coordinator
        .search_local(LocalQuery::prefix("exact", 4).expect("query"))
        .expect("retired token");
    assert_eq!(retired.total_matches, 0);
}

#[test]
fn a_package_is_named_by_its_folder_less_the_version_a_registry_tree_carries() {
    use super::local::package_name;
    assert_eq!(
        package_name("/cache/index.crates.io-1949cf8c6b5b557f/toml-0.8.23"),
        "toml"
    );
    assert_eq!(
        package_name("/cache/index.crates.io-1949cf8c6b5b557f/proc-macro2-1.0.107"),
        "proc-macro2"
    );
    assert_eq!(
        package_name("/cache/index.crates.io-1949cf8c6b5b557f/toml_edit-0.22.27"),
        "toml_edit"
    );
    assert_eq!(package_name("pkg:cargo/toml@0.8.23"), "toml");
    assert_eq!(package_name("/Users/someone/code/toml_pin"), "toml_pin");
}

/// `toml Value`: the declaration named `Value` in the package named `toml`
/// comes first, before `toml_edit`'s `Value` (its path also has the word
/// `toml`), a macro named `toml_internal` whose docs say "value", and a link
/// to a declaration elsewhere.
#[test]
fn the_words_place_the_declaration_they_name_in_the_package_they_name_first() {
    use super::local::Placement;
    let words = ["toml".to_owned(), "Value".to_owned()];
    let place = |name: &str, package: &str, kind: &str, external: bool| {
        Placement::for_test(name, package, kind, external).key(&words)
    };
    let toml_value = place("Value", "toml", "enum", false);
    let toml_use = place("Value", "toml", "import", false);
    let edit_value = place("Value", "toml_edit", "enum", false);
    let macro_row = place("toml_internal", "toml", "macro", false);
    let method = place("value", "toml", "method", false);
    let external = place("Value", "toml", "external", true);
    assert!(
        toml_value > toml_use,
        "the enum before a `use` that brings the name in"
    );
    assert!(
        toml_use > edit_value,
        "the package the words name: {toml_use:?} vs {edit_value:?}"
    );
    assert!(
        edit_value > method,
        "the name as typed before the name in another case"
    );
    assert!(
        method > macro_row,
        "a whole name before words that only start one"
    );
    assert!(
        macro_row > external,
        "a declaration before a link to one elsewhere"
    );
    let one = ["Datetime".to_owned()];
    assert!(
        Placement::for_test("Datetime", "toml_datetime", "struct", false).key(&one)
            > Placement::for_test("Datetime", "toml", "import", false).key(&one),
        "one word: the struct it names before a re-export of it"
    );
    let package = ["toml".to_owned()];
    assert!(
        Placement::for_test("toml", "toml", "project", false).key(&package)
            > Placement::for_test("toml_internal", "toml", "macro", false).key(&package),
        "one word: the row it names (the package itself) first"
    );
}

#[path = "source_ranking_tests.rs"]
mod source_ranking_tests;

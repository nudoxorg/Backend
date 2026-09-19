//! Library-level contract tests.

use super::*;

fn source() -> (ViewStateRoot, SemanticObject) {
    (view_state_root(&[]), object_version(b"source"))
}

fn capability(object: SemanticObject) -> CoverageCapability {
    let declared = AuthorityScopeClaim::from_object_version(object);
    let scope = ScopeRoot::from_bytes(object.to_bytes());
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new(
            *scope.as_bytes(),
            scope,
            *scope.as_bytes(),
            scope.as_bytes().to_vec(),
        ),
        &TestCoverageVerifier,
    )
    .expect("producer observation");
    CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(declared, observation).expect("producer coverage"),
        scope.as_bytes().to_vec(),
    )
    .expect("coverage evidence")
}

struct TestCoverageVerifier;

impl ProducerObservationVerifier for TestCoverageVerifier {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        if observation.producer_identity() == *observation.scope_root().as_bytes()
            && observation.context() == *observation.scope_root().as_bytes()
            && observation.evidence() == observation.scope_root().as_bytes()
        {
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        } else {
            Err("invalid test producer observation")
        }
    }
}

fn projection(rows: Vec<Row>) -> Library {
    let (source, object) = source();
    let basis = Basis::new(source, object);
    let view = ViewRoot::new_checked(
        view_key(b"test-projection"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
        rows,
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("projection root");
    let cursor = Cursor::for_view(
        view.recipe(),
        view.version(),
        Frontier::new(
            view.frontier().branch,
            view.frontier().log,
            view.frontier().schema,
            view.root(),
            0,
        ),
    );
    Library::from_view(view, cursor).expect("projection")
}

fn advance(
    library: Library,
    delta: ViewDelta,
    capability: CoverageCapability,
) -> (Library, CommittedViewDelta) {
    let prepared = library.view().prepare(delta, capability).expect("prepare");
    library.commit(prepared).expect("commit")
}

#[test]
fn duplicate_intents_are_idempotent_and_emit_one_event() {
    let mut intents = IntentLog::new();
    let intent = Intent::RequestPackage {
        id: package_key("package"),
        request: intent_id("request_package", b"package"),
    };
    let first = intents.append(intent.clone()).expect("first");
    let second = intents.append(intent).expect("replay");
    assert!(first.inserted);
    assert!(!second.inserted);
    assert_eq!(intents.len(), 1);
}

#[test]
fn view_transition_rejects_unrelated_producer_scope() {
    let source = view_state_root(&[]);
    let basis = Basis::new(source, object_version(b"source"));
    let root = ViewRoot::empty_checked(
        view_key(b"scope-check"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
        capability(basis.object),
    )
    .expect("checked view root");
    let row = Row::new(RowId::Symbol(symbol_key("pkg::Thing")), basis, "Thing");
    assert_eq!(
        root.prepare(
            ViewDelta::Upsert { row },
            capability(object_version(b"another-source")),
        ),
        Err(ViewError::InvalidCoverage)
    );
}

#[test]
fn complete_coverage_requires_a_producer_capability_at_root_construction() {
    let source = view_state_root(&[]);
    let basis = Basis::new(source, object_version(b"source"));
    let frontier = Frontier::new(basis.branch, basis.log, basis.schema, source, 0);
    assert_eq!(
        ViewRoot::new_incomplete(
            view_key(b"unchecked-complete"),
            basis,
            frontier,
            Vec::new(),
            vec![Coverage::Complete],
        ),
        Err(ViewError::InvalidCoverage)
    );
    assert_eq!(
        ViewRoot::new_checked(
            view_key(b"wrong-scope"),
            basis,
            frontier,
            Vec::new(),
            vec![Coverage::Complete],
            capability(object_version(b"different-source")),
        ),
        Err(ViewError::InvalidCoverage)
    );
}

#[test]
fn default_library_stays_unavailable_until_a_producer_admits_coverage() {
    let library = Library::new();
    assert!(matches!(
        library.view().coverage(),
        [Coverage::Unavailable {
            lane: Lane::Exact,
            reason: Reason::NoIndex
        }]
    ));
    let source = library.view().basis.object;
    let checked = Library::with_coverage(capability(source)).expect("checked library");
    assert_eq!(checked.view().coverage(), &[Coverage::Complete]);
}

#[test]
fn view_frontier_is_bound_to_the_flow_relation_root() {
    let library = Library::new();
    let bound = library.view().flow_frontier();
    assert_eq!(bound.root(), library.view().root());
    assert_eq!(bound.frontier().upper().epoch.0, 0);
    assert_eq!(bound.frontier().upper().iteration, 0);
}

#[test]
fn subscriptions_bind_the_recipe_and_source_stream() {
    let library = Library::new();
    let subscription = library.subscribe();
    assert_eq!(subscription.cursor(), library.cursor());
    let future = Cursor::for_view(
        library.view().recipe(),
        library.view().version(),
        Frontier::new(
            library.cursor().branch(),
            library.cursor().log(),
            library.cursor().schema(),
            library.view().root(),
            library.cursor().sequence() + 1,
        ),
    );
    assert!(matches!(
        library.subscribe_from(future),
        Err(LibraryError::CursorMismatch)
    ));
}

#[test]
fn names_documents_and_outlines_are_pinned_to_one_basis() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let symbol = symbol_key("pkg::Thing");
    let package = package_key("pkg");
    let library = projection(vec![
        Row::new(RowId::Package(package), basis, "pkg"),
        Row::new(RowId::Symbol(symbol), basis, "Thing"),
    ]);
    let revision = library.revision_root();
    let names = library
        .names(&NameQuery::new("thing", revision, QueryLimit::default()))
        .expect("name query");
    assert_eq!(names.root.rows()[0].id, RowId::Symbol(symbol));
    assert_eq!(
        library
            .document(DocumentQuery {
                symbol: SymbolAddress::canonical(symbol),
                basis: revision.into(),
                source: None,
            })
            .expect("document")
            .text(),
        "Thing"
    );
    assert_eq!(
        library
            .outline(OutlineQuery {
                package,
                basis: revision.into(),
                source: None,
            })
            .expect("outline")
            .package,
        package
    );
    assert!(
        library
            .document(DocumentQuery {
                symbol: SymbolAddress::canonical(symbol),
                basis: view_state_root(&[("other".to_owned(), "root".to_owned())]).into(),
                source: None,
            })
            .is_err()
    );
    assert_eq!(basis.root, root);
}

#[test]
fn document_projection_uses_the_complete_row_document() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let symbol = symbol_key("pkg::Documented");
    let library = projection(vec![
        Row::new(RowId::Symbol(symbol), basis, "Documented")
            .with_signature("fn documented() -> bool")
            .with_document(
                vec![Fragment::Text(
                    "Returns whether the value is documented.".to_owned(),
                )]
                .into_boxed_slice(),
            ),
    ]);
    let revision = library.revision_root();
    let document = library
        .document(DocumentQuery {
            symbol: SymbolAddress::canonical(symbol),
            basis: revision.into(),
            source: None,
        })
        .expect("document projection");
    assert_eq!(
        document.signature.as_deref(),
        Some("fn documented() -> bool")
    );
    assert_eq!(
        document.text(),
        "fn documented() -> bool\nReturns whether the value is documented."
    );
}

#[test]
fn graph_query_uses_the_retained_parent_child_arrangement() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package = package_key("pkg");
    let parent = symbol_key("pkg::Parent");
    let child = symbol_key("pkg::Child");
    let library = projection(vec![
        Row::new(RowId::Package(package), basis, "pkg"),
        Row::in_package(RowId::Symbol(parent), basis, package, "Parent"),
        Row::in_package(RowId::Symbol(child), basis, package, "Child").with_parent(parent),
    ]);
    let revision = library.revision_root();
    let snapshot = library
        .graph(GraphNeighborhoodQuery::new(parent, revision))
        .expect("graph neighborhood");
    let selected_snapshot = library
        .graph(GraphNeighborhoodQuery::selected(parent, revision))
        .expect("selected graph neighborhood");
    assert_eq!(selected_snapshot.root, snapshot.root);
    assert!(
        library
            .graph(GraphNeighborhoodQuery::selected(
                symbol_key("pkg::Absent"),
                revision,
            ))
            .is_err(),
        "an opaque selector must resolve through membership in the pinned view"
    );
    let ids = snapshot
        .root
        .rows()
        .iter()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    assert!(ids.contains(&RowId::Symbol(parent)));
    assert!(ids.contains(&RowId::Symbol(child)));
    assert_eq!(snapshot.root.basis().root, revision);
}

#[test]
fn document_only_row_changes_are_visible_in_root_and_delta_identity() {
    let (source, object) = source();
    let basis = Basis::new(source, object);
    let symbol = symbol_key("pkg::Documented");
    let base = ViewRoot::new_checked(
        view_key(b"document-identity"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
        vec![Row::new(RowId::Symbol(symbol), basis, "Documented")],
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("base");
    let changed = Row::new(RowId::Symbol(symbol), basis, "Documented")
        .with_document(vec![Fragment::Text("changed docs".to_owned())].into_boxed_slice());
    let prepared = base
        .prepare(ViewDelta::Upsert { row: changed }, capability(object))
        .expect("document change");
    assert_ne!(prepared.target_root(), base.root());
    let (_, committed) = base.commit(prepared).expect("commit");
    assert_ne!(committed.base_root(), committed.target_root());
}

#[test]
fn outlines_are_package_scoped_and_follow_parent_links() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let first_package = package_key("pkg-one");
    let second_package = package_key("pkg-two");
    let first_root = symbol_key("pkg-one::Root");
    let first_child = symbol_key("pkg-one::Child");
    let first_peer = symbol_key("pkg-one::Peer");
    let second_root = symbol_key("pkg-two::Root");
    let library = projection(vec![
        Row::new(RowId::Package(first_package), basis, "pkg-one"),
        Row::new(RowId::Package(second_package), basis, "pkg-two"),
        Row::in_package(RowId::Symbol(first_child), basis, first_package, "Child")
            .with_parent(first_root),
        Row::in_package(RowId::Symbol(first_root), basis, first_package, "Root"),
        Row::in_package(RowId::Symbol(first_peer), basis, first_package, "Peer"),
        Row::in_package(RowId::Symbol(second_root), basis, second_package, "Root"),
    ]);
    let revision = library.revision_root();

    let first = library
        .outline(OutlineQuery::new(first_package, revision))
        .expect("first package outline");
    let root_node = first
        .roots()
        .find(|node| node.symbol == first_root)
        .expect("first package root");
    assert_eq!(root_node.children[0].symbol, first_child);
    assert_eq!(first.roots().count(), 2);
    assert!(first.roots().any(|node| node.symbol == first_peer));
    assert_eq!(first.extent, OutlineExtent::Complete);
    assert!(
        library
            .outline(OutlineQuery::new(second_package, revision))
            .is_ok()
    );
    assert_eq!(first.root.children.len(), 1);
}

#[test]
fn package_outline_and_graph_pages_share_root_and_query_bound_continuations() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package_a = package_key("page-a");
    let package_b = package_key("page-b");
    let graph_root = symbol_key("page-a::Root");
    let graph_child = symbol_key("page-a::Child");
    let library = projection(vec![
        Row::new(RowId::Package(package_a), basis, "page-a"),
        Row::new(RowId::Package(package_b), basis, "page-b"),
        Row::in_package(RowId::Symbol(graph_root), basis, package_a, "Root"),
        Row::in_package(RowId::Symbol(graph_child), basis, package_a, "Child")
            .with_parent(graph_root),
    ]);
    let request = PageRequest::new(
        library.revision_root(),
        QueryLimit::new(1).expect("page limit"),
    );
    let first = library.packages_page(request).expect("package page");
    let next = match first.terminal {
        PageTerminal::More(next) => Some(next),
        PageTerminal::Complete | PageTerminal::Cancelled => None,
    };
    assert!(next.is_some(), "first package page must continue");
    let Some(next) = next else {
        return;
    };
    let wrong_query = CommandDto::new(
        41,
        Command::OutlinePage {
            package: package_a,
            page: request,
        },
    );
    let package_reply = ReplyDto::new(41, CommandReply::ProjectionPage(first.clone()));
    assert!(admit_reply(&wrong_query, &package_reply).is_err());

    let inconsistent_terminal = ReplyDto::new(
        42,
        CommandReply::ProjectionPage(ProjectionPage {
            snapshot: first.snapshot.clone(),
            terminal: PageTerminal::Complete,
        }),
    );
    let package_command = CommandDto::new(42, Command::PackagePage(request));
    assert!(admit_reply(&package_command, &inconsistent_terminal).is_err());

    let second = library
        .packages_page(request.with_continuation(next))
        .expect("second package page");
    assert_eq!(second.terminal, PageTerminal::Complete);
    let resumed_command =
        CommandDto::new(43, Command::PackagePage(request.with_continuation(next)));
    let resumed_reply = ReplyDto::new(43, CommandReply::ProjectionPage(second.clone()));
    assert_eq!(admit_reply(&resumed_command, &resumed_reply), Ok(()));
    assert_ne!(
        first.snapshot.root.rows()[0].id,
        second.snapshot.root.rows()[0].id
    );

    assert!(matches!(
        library.outline_page(package_a, request.with_continuation(next)),
        Err(LibraryError::CursorMismatch)
    ));
    let outline = library
        .outline_page(package_a, request)
        .expect("outline page");
    assert!(matches!(outline.terminal, PageTerminal::More(_)));
    let PageTerminal::More(outline_next) = outline.terminal else {
        return;
    };
    assert!(
        library
            .outline_page(package_a, request.with_continuation(outline_next))
            .is_ok()
    );
    let graph = library.graph_page(graph_root, request).expect("graph page");
    assert!(matches!(graph.terminal, PageTerminal::More(_)));
}

#[test]
fn view_transition_consumes_preparation_and_binds_root() {
    let library = Library::with_coverage(capability(Library::new().view().basis.object))
        .expect("checked library");
    let basis = library.view().basis;
    let row = Row::new(RowId::Symbol(symbol_key("pkg::Thing")), basis, "Thing");
    let (library, committed) =
        advance(library, ViewDelta::Upsert { row }, capability(basis.object));
    assert_ne!(committed.base_root, committed.target_root);
    assert_eq!(library.view().root, committed.target_root);
    assert_eq!(library.cursor().sequence(), 1);
    assert_eq!(library.cursor().version(), library.view().version);
    assert_eq!(library.cursor().root(), library.view().root);
}

#[test]
fn atomic_patch_updates_multiple_rows_in_one_committed_frontier_step() {
    let (source, object) = source();
    let basis = Basis::new(source, object);
    let removed = RowId::Symbol(symbol_key("pkg::Removed"));
    let inserted = RowId::Symbol(symbol_key("pkg::Inserted"));
    let changed = RowId::Symbol(symbol_key("pkg::Changed"));
    let base = projection(vec![
        Row::new(removed, basis, "Removed"),
        Row::new(changed, basis, "Before"),
    ]);
    let mut patch = vec![
        RowChange::Remove(removed),
        RowChange::Upsert(Box::new(Row::new(inserted, basis, "Inserted"))),
        RowChange::Upsert(Box::new(Row::new(changed, basis, "After"))),
    ];
    patch.sort_by_key(RowChange::id);
    let before_sequence = base.cursor().sequence();
    let (updated, committed) = advance(
        base,
        ViewDelta::Patch {
            changes: patch.into(),
        },
        capability(object),
    );
    assert_eq!(updated.cursor().sequence(), before_sequence + 1);
    assert!(updated.view().row(removed).is_none());
    assert_eq!(
        updated
            .view()
            .row(inserted)
            .as_ref()
            .map(|row| row.label.as_str()),
        Some("Inserted")
    );
    assert_eq!(
        updated
            .view()
            .row(changed)
            .as_ref()
            .map(|row| row.label.as_str()),
        Some("After")
    );
    assert_eq!(committed.changed_row_count(), 3);
}

#[test]
#[allow(clippy::too_many_lines)]
fn view_root_commitment_includes_basis_frontier_and_coverage() {
    let source = view_state_root(&[]);
    let object = object_version(b"source");
    let recipe = view_key(b"recipe");
    let main = Basis::with_context(
        source,
        object,
        branch_key("main"),
        log_key("library"),
        PROTOCOL_SCHEMA,
    );
    let main_frontier = Frontier::new(main.branch, main.log, main.schema, main.root, 0);
    let main_row = Row::new(RowId::Symbol(symbol_key("pkg::Thing")), main, "Thing");
    let base = ViewRoot::new_checked(
        recipe,
        main,
        main_frontier,
        vec![main_row.clone()],
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("checked view root");

    let feature = Basis::with_context(
        source,
        object,
        branch_key("feature"),
        log_key("library"),
        PROTOCOL_SCHEMA,
    );
    let feature_root = ViewRoot::new_checked(
        recipe,
        feature,
        Frontier::new(feature.branch, feature.log, feature.schema, feature.root, 0),
        vec![Row::new(
            RowId::Symbol(symbol_key("pkg::Thing")),
            feature,
            "Thing",
        )],
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("checked feature root");
    assert_ne!(base.root, feature_root.root);
    assert_ne!(base.version, feature_root.version);

    let sequence_root = ViewRoot::new_checked(
        recipe,
        main,
        Frontier::new(main.branch, main.log, main.schema, main.root, 1),
        vec![main_row.clone()],
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("checked sequence root");
    assert_ne!(base.root, sequence_root.root);
    assert_ne!(base.version, sequence_root.version);

    let coverage_root = ViewRoot::new_incomplete(
        recipe,
        main,
        main_frontier,
        vec![main_row],
        vec![Coverage::Partial {
            lane: Lane::Semantic,
            completed: 1,
            total: 2,
        }],
    )
    .expect("incomplete coverage root");
    assert_ne!(base.root, coverage_root.root);
    assert_ne!(base.version, coverage_root.version);

    let schema_basis =
        Basis::with_context(source, object, main.branch, main.log, PROTOCOL_SCHEMA + 1);
    let schema_root = ViewRoot::new_checked(
        recipe,
        schema_basis,
        Frontier::new(
            schema_basis.branch,
            schema_basis.log,
            schema_basis.schema,
            schema_basis.root,
            0,
        ),
        vec![Row::new(
            RowId::Symbol(symbol_key("pkg::Thing")),
            schema_basis,
            "Thing",
        )],
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("schema root");
    assert_ne!(base.root, schema_root.root);
    assert_ne!(base.version, schema_root.version);

    let log_basis = Basis::with_context(
        source,
        object,
        main.branch,
        log_key("other-log"),
        main.schema,
    );
    let log_root = ViewRoot::new_checked(
        recipe,
        log_basis,
        Frontier::new(
            log_basis.branch,
            log_basis.log,
            log_basis.schema,
            log_basis.root,
            0,
        ),
        vec![Row::new(
            RowId::Symbol(symbol_key("pkg::Thing")),
            log_basis,
            "Thing",
        )],
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("log root");
    assert_ne!(base.root, log_root.root);
    assert_ne!(base.version, log_root.version);
}

#[test]
fn forged_relation_delta_id_is_rejected_on_apply() {
    let source = view_state_root(&[]);
    let basis = Basis::new(source, object_version(b"source"));
    let base = ViewRoot::empty_checked(
        view_key(b"recipe"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        capability(basis.object),
    )
    .expect("checked view root");
    let first_row = Row::new(RowId::Symbol(symbol_key("pkg::first")), basis, "first");
    let first_prepared = base
        .prepare(
            ViewDelta::Upsert { row: first_row },
            capability(basis.object),
        )
        .expect("first transition");
    let (_, first) = base.clone().commit(first_prepared).expect("first commit");

    let second_row = Row::new(RowId::Symbol(symbol_key("pkg::second")), basis, "second");
    let second_prepared = base
        .prepare(
            ViewDelta::Upsert { row: second_row },
            capability(basis.object),
        )
        .expect("second transition");
    let (_, second) = base.clone().commit(second_prepared).expect("second commit");
    assert_ne!(first.id, second.id);

    let mut forged = first;
    forged.id = second.id;
    assert_eq!(forged.apply_to(&base), Err(ViewError::InvalidRelationDelta));
}

#[test]
fn forged_committed_coverage_and_frontier_are_rejected() {
    let source = view_state_root(&[]);
    let basis = Basis::new(source, object_version(b"source"));
    let base = ViewRoot::empty_checked(
        view_key(b"recipe"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        capability(basis.object),
    )
    .expect("checked view root");
    let row = Row::new(RowId::Symbol(symbol_key("pkg::Thing")), basis, "Thing");
    let prepared = base
        .prepare(ViewDelta::Upsert { row }, capability(basis.object))
        .expect("prepare");
    let (_, committed) = base.clone().commit(prepared).expect("commit");

    let mut coverage = committed.clone();
    coverage.coverage = vec![Coverage::Partial {
        lane: Lane::Semantic,
        completed: 1,
        total: 2,
    }]
    .into_boxed_slice();
    assert_eq!(
        coverage.apply_to(&base.clone()),
        Err(ViewError::InvalidRelationDelta)
    );

    let mut frontier = committed;
    frontier.frontier.sequence += 1;
    assert_eq!(
        frontier.apply_to(&base),
        Err(ViewError::InvalidRelationDelta)
    );
}

#[test]
fn remove_reset_and_coverage_transitions_are_checked_and_consumed() {
    let source = view_state_root(&[]);
    let basis = Basis::new(source, object_version(b"source"));
    let base = ViewRoot::empty_checked(
        view_key(b"recipe"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        capability(basis.object),
    )
    .expect("checked view root");
    let row_id = RowId::Symbol(symbol_key("pkg::Thing"));
    let row = Row::new(row_id, basis, "Thing");
    let prepared = base
        .prepare(
            ViewDelta::Upsert { row: row.clone() },
            capability(basis.object),
        )
        .expect("upsert");
    let (with_row, _) = base.clone().commit(prepared).expect("upsert commit");

    let remove = with_row
        .prepare(ViewDelta::Remove { id: row_id }, capability(basis.object))
        .expect("remove");
    let (empty, removed) = with_row.clone().commit(remove).expect("remove commit");
    assert!(empty.rows().is_empty());
    assert_eq!(removed.base_root, with_row.root);
    assert_eq!(removed.target_root, empty.root);

    let replacement = ViewRoot::new_checked(
        base.recipe,
        basis,
        base.frontier,
        vec![row.clone()],
        vec![Coverage::Complete],
        capability(basis.object),
    )
    .expect("checked replacement root");
    let reset = base
        .prepare(
            ViewDelta::Reset {
                root: Box::new(replacement),
            },
            capability(basis.object),
        )
        .expect("reset");
    let (reset_root, reset_delta) = base.clone().commit(reset).expect("reset commit");
    assert_eq!(reset_root.rows(), &[row]);
    assert_eq!(reset_delta.base_root, base.root);

    let coverage = empty
        .prepare(
            ViewDelta::Coverage {
                coverage: Coverage::Partial {
                    lane: Lane::Semantic,
                    completed: 1,
                    total: 2,
                },
            },
            capability(basis.object),
        )
        .expect("coverage");
    let (covered, covered_delta) = empty.clone().commit(coverage).expect("coverage commit");
    assert_eq!(
        covered.coverage.last(),
        Some(&Coverage::Partial {
            lane: Lane::Semantic,
            completed: 1,
            total: 2,
        })
    );
    assert_ne!(covered_delta.base_root, covered_delta.target_root);
    assert!(
        empty
            .prepare(
                ViewDelta::Coverage {
                    coverage: Coverage::Partial {
                        lane: Lane::Semantic,
                        completed: 2,
                        total: 1,
                    },
                },
                capability(basis.object),
            )
            .is_err()
    );
}

#[test]
fn query_results_keep_stable_row_identity_when_rank_changes() {
    let (root, object) = source();
    let a = symbol_key("pkg::a");
    let b = symbol_key("pkg::b");
    let library = projection(vec![
        Row::new(RowId::Symbol(a), Basis::new(root, object), "a"),
        Row::new(RowId::Symbol(b), Basis::new(root, object), "b"),
    ]);
    let revision = library.revision_root();
    let result = library
        .search(&Query::new("", revision, QueryLimit::default()))
        .expect_err("empty search is rejected");
    assert!(matches!(result, LibraryError::InvalidQuery(_)));
    let names = library
        .names(&NameQuery::new("", revision, QueryLimit::default()))
        .expect("all names");
    assert_eq!(names.root.rows().len(), 2);
    assert_ne!(names.root.rows()[0].id, names.root.rows()[1].id);
}

#[test]
fn changed_documents_are_retested_and_local_only_matches_enter_search() {
    let (source, object) = source();
    let basis = Basis::new(source, object);
    let replaced = RowId::Symbol(symbol_key("pkg::replaced"));
    let retained = RowId::Symbol(symbol_key("pkg::retained"));
    let local_only = RowId::Symbol(symbol_key("pkg::local-only"));
    let mut old = Row::new(replaced, basis, "replaced");
    old.document = vec![Fragment::Text("needle before edit".to_owned())].into_boxed_slice();
    old.score = Some(100);
    let mut still_matching = Row::new(retained, basis, "retained");
    still_matching.document = vec![Fragment::Text("needle retained".to_owned())].into_boxed_slice();
    still_matching.score = Some(50);
    let library = projection(vec![old, still_matching]);
    let first = library
        .search_ranked(&Query::new(
            "needle",
            library.revision_root(),
            QueryLimit::new(1).expect("bounded limit"),
        ))
        .expect("base search");
    assert_eq!(first.order()[0], replaced);
    let stale_cursor = first.snapshot().next.expect("base continuation");

    let mut replaced_after_edit = Row::new(replaced, basis, "replaced");
    replaced_after_edit.document =
        vec![Fragment::Text("different documentation".to_owned())].into_boxed_slice();
    replaced_after_edit.score = Some(1_000);
    let mut inserted = Row::new(local_only, basis, "local-only");
    inserted.document = vec![Fragment::Code("needle()".to_owned())].into_boxed_slice();
    inserted.score = Some(500);
    let mut overlay_changes = vec![
        RowChange::Upsert(Box::new(replaced_after_edit)),
        RowChange::Upsert(Box::new(inserted)),
    ];
    overlay_changes.sort_by_key(RowChange::id);
    let (updated, _) = advance(
        library,
        ViewDelta::Patch {
            changes: overlay_changes.into(),
        },
        capability(object),
    );
    let current = updated
        .search_ranked(&Query::new(
            "needle",
            updated.revision_root(),
            QueryLimit::default(),
        ))
        .expect("overlay-current search");
    assert_eq!(current.order(), [local_only, retained]);
    assert!(matches!(
        updated.search(
            &Query::new("needle", updated.revision_root(), QueryLimit::default())
                .with_cursor(stale_cursor)
        ),
        Err(LibraryError::CursorMismatch)
    ));
}

#[test]
fn bounded_query_continuation_is_bound_to_the_snapshot_view() {
    let (basis, object) = source();
    let library = projection(vec![
        Row::new(
            RowId::Symbol(symbol_key("pkg::a")),
            Basis::new(basis, object),
            "a",
        ),
        Row::new(
            RowId::Symbol(symbol_key("pkg::b")),
            Basis::new(basis, object),
            "b",
        ),
    ]);
    let revision = library.revision_root();
    let snapshot = library
        .names(&NameQuery::new(
            "",
            revision,
            QueryLimit::new(1).expect("bounded limit"),
        ))
        .expect("name page");
    let cursor = snapshot.next.expect("continuation");
    assert_eq!(cursor.recipe(), snapshot.root.recipe);
    assert_eq!(cursor.version(), snapshot.root.version);
    assert_eq!(cursor.root(), snapshot.root.root);
    let continued = library
        .names(
            &NameQuery::new("", revision, QueryLimit::new(1).expect("bounded limit"))
                .with_cursor(cursor),
        )
        .expect("accepted continuation");
    assert_eq!(continued.root.recipe(), snapshot.root.recipe());
    assert_eq!(continued.root.rows().len(), 1);
    assert_ne!(continued.root.rows()[0].id, snapshot.root.rows()[0].id);
    assert!(matches!(
        library.names(
            &NameQuery::new("", revision, QueryLimit::new(1).expect("bounded limit"),)
                .with_cursor(Cursor::new())
        ),
        Err(LibraryError::CursorMismatch)
    ));
    let expected = ReplyDto::new(1, CommandReply::Names(snapshot));
    let encoded = serde_json::to_vec(&expected).expect("encode page");
    let decoded = ReplyDto::decode_against(&encoded, &expected).expect("decode page");
    assert_eq!(decoded.request_id, 1);
}

#[test]
fn repeated_queries_seek_prebuilt_arrangements_and_sort_only_the_bounded_page() {
    let (basis, object) = source();
    let rows = (0..16)
        .map(|index| {
            Row::new(
                RowId::Symbol(symbol_key(&format!("pkg::{index:02}"))),
                Basis::new(basis, object),
                format!("Name {index:02}"),
            )
        })
        .collect();
    let library = projection(rows);
    let revision = library.revision_root();
    let built = library.work_counters();
    assert_eq!(built.indexed_rows, 16);
    assert_eq!(built.scan_rows, 0);
    assert_eq!(built.sort_rows, 0);
    let query = NameQuery::new("name 1", revision, QueryLimit::new(4).expect("limit"));
    let _ = library.names(&query).expect("first seek");
    let first = library.work_counters();
    let _ = library.names(&query).expect("second seek");
    let second = library.work_counters();
    assert_eq!(first.scan_rows, second.scan_rows);
    assert!(first.sort_rows <= built.sort_rows + 5);
    assert!(second.sort_rows <= first.sort_rows + 5);
    assert!(second.seek_probes > first.seek_probes);
    assert_eq!(second.output_rows, first.output_rows * 2);
}

#[test]
fn one_scalar_query_seeks_its_posting_instead_of_scanning_the_corpus() {
    let (basis, object) = source();
    let mut rows = (0..10_000)
        .map(|index| {
            Row::new(
                RowId::Symbol(symbol_key(&format!("pkg::z{index:05}"))),
                Basis::new(basis, object),
                format!("zeta {index:05}"),
            )
        })
        .collect::<Vec<_>>();
    rows.push(Row::new(
        RowId::Symbol(symbol_key("pkg::quill")),
        Basis::new(basis, object),
        "quill",
    ));
    let library = projection(rows);
    assert_eq!(library.arrangement.name_posting_candidates("q"), 1);

    let query = NameQuery::new(
        "q",
        library.revision_root(),
        QueryLimit::new(1).expect("limit"),
    );
    let result = library.names(&query).expect("short posting seek");
    assert_eq!(result.root.rows().len(), 1);
    assert_eq!(result.root.rows()[0].label, "quill");
    let work = library.work_counters();
    assert_eq!(work.scan_rows, 0);
    assert_eq!(work.sort_rows, 1);
}

#[test]
fn cold_index_build_does_not_retain_a_second_copy_of_all_rows() {
    let (basis, object) = source();
    let rows = (0..10_000)
        .map(|index| {
            Row::new(
                RowId::Symbol(symbol_key(&format!("pkg::{index:05}"))),
                Basis::new(basis, object),
                format!("Name {index:05}"),
            )
        })
        .collect();
    let library = projection(rows);
    assert!(!library.view.compatibility_rows_are_materialized());
    assert_eq!(
        library
            .view
            .row_ref(RowId::Symbol(symbol_key("pkg::00000")))
            .map(|row| row.label.as_str()),
        Some("Name 00000")
    );
    assert!(!library.view.compatibility_rows_are_materialized());

    assert_eq!(library.view.rows().len(), 10_000);
    assert!(library.view.compatibility_rows_are_materialized());
}

#[test]
fn sequential_one_row_edits_do_not_retain_a_view_history_chain() {
    let source = view_state_root(&[]);
    let object = object_version(b"source");
    let basis = Basis::new(source, object);
    let mut view = ViewRoot::empty_checked(
        view_key(b"history-free-view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
        capability(object),
    )
    .expect("checked view root");
    let id = RowId::Symbol(symbol_key("pkg::stable"));
    for sequence in 0..100_000_u32 {
        let row = Row::new(id, basis, format!("stable-{sequence}"));
        let prepared = view
            .prepare(ViewDelta::Upsert { row }, capability(object))
            .expect("prepare sequential edit");
        let (next, _) = view.commit(prepared).expect("commit sequential edit");
        view = next;
    }
    assert_eq!(
        view.row(id).map(|row| row.label),
        Some("stable-99999".to_owned())
    );
    let first_rows = view.rows().as_ptr();
    assert_eq!(view.rows().as_ptr(), first_rows);
}

#[test]
fn semantic_diff_wire_preserves_typed_evidence_and_enforces_the_nested_bound() {
    let identity = SemanticDeclarationIdentity {
        family: [1; 16],
        variant: [2; 16],
    };
    let evidence = SemanticLinkEvidence {
        confidence: SemanticConfidence::Compiler,
        source: Some(SemanticSourceSpan {
            file: ProductText::new("src/lib.rs").expect("source path"),
            start: 8,
            end: 14,
        }),
    };
    let delta = SemanticLinkDelta::Added {
        from: identity,
        target: SemanticLinkTarget::Foreign {
            declaration: [3; 16],
            variant: None,
        },
        relation: SemanticLinkKind::Calls,
        evidence,
    };
    let row = DiffRecord {
        label: ProductText::new("sample::call").expect("label"),
        change: DeclarationChange::Added,
        before: None,
        after: Some(identity),
        links: vec![delta.clone()].into_boxed_slice(),
    };
    let reply = SurfaceReply::Diff(vec![row].into_boxed_slice());
    assert_eq!(reply.admit(CommandId::Diff), Ok(()));

    let encoded = serde_json::to_vec(&ReplyDto::new(7, CommandReply::Surface(reply)))
        .expect("encode typed diff");
    let decoded: ReplyDto = serde_json::from_slice(&encoded).expect("decode typed diff");
    let CommandReply::Surface(SurfaceReply::Diff(rows)) = decoded.reply else {
        panic!("typed diff reply");
    };
    assert!(matches!(
        rows[0].links.as_ref(),
        [SemanticLinkDelta::Added {
            from,
            relation: SemanticLinkKind::Calls,
            evidence: SemanticLinkEvidence {
                confidence: SemanticConfidence::Compiler,
                ..
            },
            ..
        }] if *from == identity
    ));

    let oversized = DiffRecord {
        label: ProductText::new("sample::call").expect("label"),
        change: DeclarationChange::Added,
        before: None,
        after: Some(identity),
        links: vec![delta; MAX_PRODUCT_ROWS].into_boxed_slice(),
    };
    assert_eq!(
        SurfaceReply::Diff(vec![oversized].into_boxed_slice()).admit(CommandId::Diff),
        Err(ProductAdmissionError::RowBound)
    );
}

#[test]
fn semantic_diff_rejects_relation_evidence_attached_to_the_wrong_side() {
    let declared = SemanticDeclarationIdentity {
        family: [1; 16],
        variant: [2; 16],
    };
    let other = SemanticDeclarationIdentity {
        family: [3; 16],
        variant: [4; 16],
    };
    let row = DiffRecord {
        label: ProductText::new("sample::call").expect("label"),
        change: DeclarationChange::Added,
        before: None,
        after: Some(declared),
        links: vec![SemanticLinkDelta::Added {
            from: other,
            target: SemanticLinkTarget::Local {
                declaration: declared,
            },
            relation: SemanticLinkKind::Calls,
            evidence: SemanticLinkEvidence {
                confidence: SemanticConfidence::Compiler,
                source: None,
            },
        }]
        .into_boxed_slice(),
    };
    assert_eq!(
        SurfaceReply::Diff(vec![row].into_boxed_slice()).admit(CommandId::Diff),
        Err(ProductAdmissionError::DiffShape)
    );
}

#[test]
fn references_name_sites_from_compiler_facts_against_the_selected_view() {
    let basis = Basis::new(view_state_root(&[]), object_version(b"source"));
    let site_symbol = symbol_key("pkg::semantic::aa::caller");
    let target_label = "pkg::semantic::bb::callee";
    let rows = vec![
        Row::new(RowId::Symbol(site_symbol), basis, "pkg::caller"),
        Row::new(
            RowId::Symbol(symbol_key("pkg::semantic::bb::callee")),
            basis,
            target_label,
        ),
    ];
    let library = projection(rows);
    let fact = ReferenceFact {
        site: site_symbol,
        target: SemanticLinkTarget::Local {
            declaration: SemanticDeclarationIdentity {
                family: [2; 16],
                variant: [3; 16],
            },
        },
        relation: SemanticLinkKind::Calls,
        evidence: SemanticLinkEvidence {
            confidence: SemanticConfidence::Compiler,
            source: Some(SemanticSourceSpan {
                file: ProductText::new("src/main.rs").expect("path"),
                start: 40,
                end: 46,
            }),
        },
    };
    let target = ProductText::new(target_label).expect("target");
    let records = library
        .references(&target, &[fact.clone()])
        .expect("references");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].site.as_str(), "pkg::caller");
    assert_eq!(records[0].relation, SemanticLinkKind::Calls);
    let span = records[0].evidence.source.as_ref().expect("span");
    assert_eq!((span.start, span.end), (40, 46));

    // A fact citing a declaration absent from the view is refused, not
    // silently dropped: a reference answer may never name an invisible row.
    let orphan = ReferenceFact {
        site: symbol_key("pkg::semantic::cc::absent"),
        ..fact.clone()
    };
    assert!(matches!(
        library.references(&target, &[orphan]),
        Err(LibraryError::NotFound)
    ));
    // An unknown target is equally refused.
    let missing = ProductText::new("pkg::semantic::dd::missing").expect("missing");
    assert!(matches!(
        library.references(&missing, &[fact]),
        Err(LibraryError::NotFound)
    ));
}

//! Producer-admitted query fixtures, independent of native compiler coverage.

use super::*;

fn ordered_ids(page: &ViewSnapshot) -> Vec<RowId> {
    let mut rows = page.root.row_refs().collect::<Vec<_>>();
    rows.sort_by_key(|row| std::cmp::Reverse(row.score));
    rows.into_iter().map(|row| row.id).collect()
}

#[test]
fn exact_names_precede_partial_names_through_bounded_pages_and_cold_reopen() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package = package_key("/fixture");
    let mut rows = vec![Row::new(RowId::Package(package), basis, "/fixture")];
    let mut partial_ids = Vec::new();
    // The identity precedes the name in a compiler coordinate. Every partial
    // sorts before both exact matches under the former coordinate-only order,
    // and there are enough partials to exercise heap eviction at small limits.
    for index in 0..18 {
        let id = RowId::Symbol(symbol_key(&format!("partial-{index}")));
        let coordinate = format!("/fixture::semantic::{index:064x}::MockEnvironment");
        rows.push(Row::in_package(id, basis, package, coordinate));
        partial_ids.push(id);
    }
    let exact_ids = [
        RowId::Symbol(symbol_key("exact-python")),
        RowId::Symbol(symbol_key("exact-typescript")),
    ];
    for (index, id) in exact_ids.into_iter().enumerate() {
        rows.push(Row::in_package(
            id,
            basis,
            package,
            format!("/fixture::semantic::{:064x}::Environment", index + 100),
        ));
    }
    let original = projection(rows);
    let mut expected = exact_ids.to_vec();
    expected.extend(partial_ids);
    for credit in [1, 2, 5, 16, 32] {
        let query = NameQuery::new(
            "eNVIRONMENT",
            original.revision_root(),
            QueryLimit::new(credit).expect("bounded credit"),
        );
        let mut owner = original.clone();
        let mut continued = query.clone();
        let mut seen = Vec::new();
        loop {
            let page = owner.names(&continued).expect("selected name page");
            for row in page.root.row_refs() {
                assert_eq!(
                    row.score,
                    Some(
                        u32::MAX
                            - u32::try_from(
                                expected
                                    .iter()
                                    .position(|id| *id == row.id)
                                    .expect("fixture identity")
                            )
                            .expect("bounded ordinal")
                    )
                );
            }
            seen.extend(ordered_ids(&page));
            let Some(next) = page.next else { break };
            // Rebuild all arrangements, so continuation admission cannot rely
            // on the query heap, page rows, or a retained process-local map.
            owner = Library::from_view(owner.view().clone(), owner.cursor())
                .expect("cold owner projection");
            continued = query.clone().with_cursor(next);
        }
        assert_eq!(seen, expected, "page credit {credit}");
        assert_eq!(owner.work_counters().scan_rows, 0);
        assert!(!owner.view().compatibility_rows_are_materialized());
    }
    for row in original.view().row_refs() {
        assert!(
            row.score.is_none(),
            "ranking must leave published values intact"
        );
    }
}

#[test]
fn exact_relative_qualified_unicode_names_require_a_segment_boundary() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let names = [
        ("a::MockÖwner::Étude", "partial-owner"),
        ("b::Öwner::ÉtudePlus", "partial-method"),
        ("z::Öwner::Étude", "exact-qualified"),
    ];
    let ids = names.map(|(_, preimage)| RowId::Symbol(symbol_key(preimage)));
    let library = projection(
        names
            .into_iter()
            .zip(ids)
            .map(|((label, _), id)| Row::new(id, basis, label))
            .collect(),
    );
    let query = NameQuery::new(
        "öWNER::éTUDE",
        library.revision_root(),
        QueryLimit::default(),
    );
    let page = library.names(&query).expect("unicode qualified names");
    assert_eq!(ordered_ids(&page), [ids[2], ids[0], ids[1]]);
    assert_eq!(page.root.row_count(), 3, "partials remain available");
    assert!(page.next.is_none());
}

#[test]
fn exact_name_ranking_keeps_query_credit_and_predecessor_snapshot_binding() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let exact = RowId::Symbol(symbol_key("exact"));
    let partial = RowId::Symbol(symbol_key("partial"));
    let library = projection(vec![
        Row::new(exact, basis, "z::Environment"),
        Row::new(partial, basis, "a::MockEnvironment"),
    ]);
    let query = NameQuery::new(
        "Environment",
        library.revision_root(),
        QueryLimit::new(1).expect("credit"),
    );
    let first = library.names(&query).expect("first page");
    assert_eq!(ordered_ids(&first), [exact]);
    let next = first.next.expect("partial successor");
    for altered in [
        NameQuery::new(
            "environment",
            library.revision_root(),
            QueryLimit::new(1).expect("credit"),
        ),
        NameQuery::new(
            "Environment",
            library.revision_root(),
            QueryLimit::new(2).expect("credit"),
        ),
    ] {
        assert!(matches!(
            library.names(&altered.with_cursor(next)),
            Err(LibraryError::CursorMismatch)
        ));
    }
    let (updated, _) = advance(
        library,
        ViewDelta::Upsert {
            row: Row::new(exact, basis, "z::EnvironmentChanged"),
        },
        capability(object),
    );
    let current = NameQuery::new(
        "Environment",
        updated.revision_root(),
        QueryLimit::new(1).expect("credit"),
    );
    assert!(matches!(
        updated.names(&current.with_cursor(next)),
        Err(LibraryError::CursorMismatch)
    ));
}

#[test]
fn exact_source_declarations_precede_inferred_aliases_fields_and_imports() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let cases = [
        ("a::Environment", DeclarationKind::Type, false),
        ("b::Environment", DeclarationKind::Import, true),
        ("c::Environment", DeclarationKind::Field, true),
        ("z::Environment", DeclarationKind::Class, true),
        ("zz::MockEnvironment", DeclarationKind::Class, true),
    ];
    let ids = cases.map(|(label, _, _)| RowId::Symbol(symbol_key(label)));
    let library = projection(
        cases
            .into_iter()
            .zip(ids)
            .map(|((label, kind, sourced), id)| {
                let row = Row::new(id, basis, label).with_kind(kind);
                if sourced {
                    row.with_source(
                        SourceLocation::new("captured.py", 1).expect("fixture captured site"),
                    )
                } else {
                    row
                }
            })
            .collect(),
    );
    let expected = [ids[3], ids[2], ids[1], ids[0], ids[4]];
    for credit in [1, 2, 5] {
        let query = NameQuery::new(
            "Environment",
            library.revision_root(),
            QueryLimit::new(credit).expect("credit"),
        );
        let mut continued = query.clone();
        let mut seen = Vec::new();
        loop {
            let page = library.names(&continued).expect("typed ranked page");
            seen.extend(ordered_ids(&page));
            let Some(next) = page.next else { break };
            continued = query.clone().with_cursor(next);
        }
        assert_eq!(seen, expected, "credit {credit}");
    }
    let alias = library
        .view()
        .row_ref(ids[0])
        .expect("alias remains selected");
    assert_eq!(alias.kind, Some(DeclarationKind::Type));
    assert!(alias.source.captured().is_none());
    assert_eq!(library.work_counters().scan_rows, 0);
}

#[test]
fn relative_member_lookup_uses_retained_parents_and_keeps_cold_cursor_proofs() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package = package_key("/gymbro");
    let foreign = package_key("/foreign");
    let auth = symbol_key("native-auth-owner");
    let other = symbol_key("native-other-owner");
    let wrong_package = symbol_key("native-foreign-owner");
    let member = symbol_key("native-current-user");
    let partial = symbol_key("native-current-user-detail");
    let unrelated = symbol_key("native-unrelated-current-user");
    let cross_package = symbol_key("native-cross-package-parent");
    let cycle = symbol_key("native-cycle-member");
    let coordinate = |tag: &str, name: &str| format!("/gymbro::semantic::{tag}::{name}");
    let original = projection(vec![
        Row::new(RowId::Package(package), basis, "/gymbro"),
        Row::new(RowId::Package(foreign), basis, "/foreign"),
        Row::in_package(
            RowId::Symbol(auth),
            basis,
            package,
            coordinate("owner", "AuthService"),
        )
        .with_kind(DeclarationKind::Class),
        Row::in_package(
            RowId::Symbol(other),
            basis,
            package,
            coordinate("other", "OtherService"),
        )
        .with_kind(DeclarationKind::Class),
        Row::in_package(
            RowId::Symbol(wrong_package),
            basis,
            foreign,
            "/foreign::semantic::owner::AuthService",
        )
        .with_kind(DeclarationKind::Class),
        Row::in_package(
            RowId::Symbol(member),
            basis,
            package,
            coordinate("member", "currentUser"),
        )
        .with_kind(DeclarationKind::Property)
        .with_parent(auth),
        Row::in_package(
            RowId::Symbol(partial),
            basis,
            package,
            coordinate("partial", "currentUserDetails"),
        )
        .with_kind(DeclarationKind::Property)
        .with_parent(auth),
        Row::in_package(
            RowId::Symbol(unrelated),
            basis,
            package,
            coordinate("unrelated", "currentUser"),
        )
        .with_parent(other),
        Row::in_package(
            RowId::Symbol(cross_package),
            basis,
            package,
            coordinate("foreign", "currentUser"),
        )
        .with_parent(wrong_package),
        Row::in_package(
            RowId::Symbol(cycle),
            basis,
            package,
            coordinate("cycle", "AuthService"),
        )
        .with_parent(cycle),
    ]);
    for spelling in [
        "AuthService.currentUser",
        "AuthService::currentUser",
        "aUTHsERVICE.cURRENTuSER",
    ] {
        let query = NameQuery::new(
            spelling,
            original.revision_root(),
            QueryLimit::new(1).expect("limit"),
        );
        let first = original.names(&query).expect("typed owner/member page");
        assert_eq!(ordered_ids(&first), [RowId::Symbol(member)]);
        assert_eq!(
            first.root.rows()[0].label,
            coordinate("member", "currentUser")
        );
        let next = first.next.expect("partial member remains discoverable");
        let cold =
            Library::from_view(original.view().clone(), original.cursor()).expect("cold owner");
        let second = cold
            .names(&query.with_cursor(next))
            .expect("same query resumes");
        assert_eq!(ordered_ids(&second), [RowId::Symbol(partial)]);
        assert!(second.next.is_none());
        let wrong = NameQuery::new(
            "OtherService.currentUser",
            original.revision_root(),
            QueryLimit::new(1).expect("limit"),
        );
        assert!(matches!(
            cold.names(&wrong.with_cursor(next)),
            Err(LibraryError::CursorMismatch)
        ));
    }
    for spelling in [
        "Absent.currentUser",
        "AuthService..currentUser",
        ".AuthService.currentUser",
        "AuthService.AuthService",
        "AuthService::currentUser.",
    ] {
        let page = original
            .names(&NameQuery::new(
                spelling,
                original.revision_root(),
                QueryLimit::default(),
            ))
            .expect("bounded query");
        assert!(page.root.rows().is_empty(), "{spelling}");
    }
    let unqualified = original
        .names(&NameQuery::new(
            "currentUser",
            original.revision_root(),
            QueryLimit::default(),
        ))
        .expect("all named facts remain discoverable");
    assert_eq!(unqualified.root.row_count(), 4);
}

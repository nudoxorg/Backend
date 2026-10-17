use super::*;

fn catalog(unrelated: usize, members: usize) -> (Library, SymbolKey, SymbolKey) {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package = package_key("pkg");
    // Equal coordinates are display claims, not selected identity.
    let owner = symbol_key("native owner A");
    let other = symbol_key("native owner B");
    let mut rows = vec![
        Row::in_package(RowId::Package(package), basis, package, "pkg"),
        Row::in_package(
            RowId::Symbol(owner),
            basis,
            package,
            "pkg::models.py:994::Document",
        ),
        Row::in_package(
            RowId::Symbol(other),
            basis,
            package,
            "pkg::models.py:994::Document",
        ),
    ];
    for index in 0..members {
        rows.push(
            Row::in_package(
                RowId::Symbol(symbol_key(&format!("member {index}"))),
                basis,
                package,
                format!("pkg::models.py:{}::ancestors_link_definition", 1000 + index),
            )
            .with_parent(owner),
        );
    }
    rows.push(
        Row::in_package(
            RowId::Symbol(symbol_key("other child")),
            basis,
            package,
            "pkg::models.py:1000::ancestors_link_definition",
        )
        .with_parent(other),
    );
    rows.push(
        Row::in_package(
            RowId::Symbol(symbol_key("grandchild")),
            basis,
            package,
            "pkg::models.py:1001::nested",
        )
        .with_parent(symbol_key("member 0")),
    );
    rows.extend((0..unrelated).map(|index| {
        Row::in_package(
            RowId::Symbol(symbol_key(&format!("unrelated {index}"))),
            basis,
            package,
            format!("pkg::unrelated.py:{index}::unrelated"),
        )
    }));
    (projection(rows), owner, other)
}

#[test]
fn direct_members_seek_only_the_selected_owner_range_independent_of_package_size() {
    for unrelated in [0, 35_300] {
        let (library, owner, other) = catalog(unrelated, 48);
        let before = library.work_counters();
        let query =
            DocumentQuery::selected(owner, library.revision_root()).with_source_basis(Basis {
                root: library.revision_root(),
                ..library.view().basis()
            });
        let page = library
            .members_page(
                query,
                PageRequest::new(
                    library.revision_root(),
                    QueryLimit::new(200).expect("credit"),
                ),
            )
            .expect("direct members");
        assert_eq!(page.terminal, PageTerminal::Complete);
        assert_eq!(page.snapshot.root.row_count(), 49);
        assert!(
            page.snapshot
                .root
                .rows()
                .iter()
                .all(|row| row.id == RowId::Symbol(owner) || row.parent == Some(owner))
        );
        assert!(
            !page
                .snapshot
                .root
                .rows()
                .iter()
                .any(|row| row.parent == Some(other))
        );
        let after = library.work_counters();
        assert_eq!(after.scan_rows, before.scan_rows);
        assert_eq!(after.sort_rows, before.sort_rows);
        assert_eq!(after.seek_probes - before.seek_probes, 1);
        assert_eq!(after.output_rows - before.output_rows, 49);
        assert!(!library.view().compatibility_rows_are_materialized());
    }
}

#[test]
fn direct_members_preserve_all_sites_and_refuse_retargeted_or_stale_continuations() {
    let (library, owner, other) = catalog(35_300, 405);
    let basis = library.revision_root();
    let query = DocumentQuery::selected(owner, basis).with_source_basis(Basis {
        root: library.revision_root(),
        ..library.view().basis()
    });
    let request = PageRequest::new(basis, QueryLimit::new(200).expect("credit"));
    let first = library.members_page(query, request).expect("first");
    let next = first.snapshot.next.expect("multiple pages");
    let continued = request.with_continuation(PageContinuation::from_cursor(next));
    assert!(
        library
            .members_page(DocumentQuery::selected(other, basis), continued)
            .is_err()
    );
    assert!(
        library
            .members_page(
                query,
                PageRequest::new(basis, QueryLimit::new(199).expect("changed credit"))
                    .with_continuation(PageContinuation::from_cursor(next))
            )
            .is_err()
    );
    let stale = Basis {
        object: object_version(b"different captured object"),
        ..Basis {
            root: library.revision_root(),
            ..library.view().basis()
        }
    };
    assert!(
        library
            .members_page(query.with_source_basis(stale), request)
            .is_err()
    );
    assert!(
        library
            .members_page(
                DocumentQuery::selected(symbol_key("missing owner"), basis),
                request
            )
            .is_err()
    );
    let mut seen = std::collections::BTreeSet::new();
    let mut current = first;
    loop {
        for row in current.snapshot.root.rows() {
            assert!(seen.insert(row.id));
        }
        let Some(next) = current.snapshot.next else {
            break;
        };
        current = library
            .members_page(
                query,
                request.with_continuation(PageContinuation::from_cursor(next)),
            )
            .expect("exact predecessor");
    }
    assert_eq!(seen.len(), 406);
    assert_eq!(library.work_counters().scan_rows, 0);
    assert_eq!(
        library
            .members_page(DocumentQuery::selected(other, basis), request)
            .expect("distinct same-coordinate owner")
            .snapshot
            .root
            .row_count(),
        2
    );
    let changed = view_state_root(&[("changed".to_owned(), "source".to_owned())]);
    assert!(
        library
            .members_page(DocumentQuery::selected(owner, changed), request)
            .is_err()
    );
}

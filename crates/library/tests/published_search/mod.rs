use super::*;

fn owner(unrelated: usize) -> (Library, SymbolKey, SymbolKey) {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package = package_key("pkg");
    let parent = symbol_key("native outer owner");
    let other = symbol_key("native other owner");
    let mut rows = vec![
        Row::in_package(RowId::Package(package), basis, package, "pkg"),
        Row::in_package(
            RowId::Symbol(parent),
            basis,
            package,
            "pkg::models.py:1::Outer",
        ),
        Row::in_package(
            RowId::Symbol(other),
            basis,
            package,
            "pkg::models.py:2::Other",
        ),
    ];
    for index in 0..7 {
        rows.push(
            Row::in_package(
                RowId::Symbol(symbol_key(&format!("native Document {index}"))),
                basis,
                package,
                format!("pkg::models.py:{}::Document", 100 + index),
            )
            .with_parent(if index == 6 { other } else { parent })
            .with_source(SourceLocation::new("models.py", 100 + index).expect("authored site")),
        );
    }
    rows.extend((0..unrelated).map(|index| {
        Row::in_package(
            RowId::Symbol(symbol_key(&format!("unrelated {index}"))),
            basis,
            package,
            format!("pkg::other.py:{index}::Unrelated_{index}"),
        )
    }));
    (projection(rows), parent, other)
}

#[test]
fn published_name_search_uses_current_hit_and_parent_witnesses_without_package_scan() {
    for unrelated in [0, 35_300] {
        let (library, parent, other) = owner(unrelated);
        let query = PublishedNameSearchQuery::admit(Query::new(
            "Outer.Document",
            library.revision_root(),
            QueryLimit::new(10).expect("credit"),
        ))
        .expect("qualified name");
        let before = library.work_counters();
        let page = library
            .search_published_names(&query)
            .expect("current owner names");
        assert_eq!(page.root.row_count(), 6);
        assert!(
            page.root
                .rows()
                .iter()
                .all(|row| row.parent == Some(parent))
        );
        assert!(!page.root.rows().iter().any(|row| row.parent == Some(other)));
        assert!(
            page.root
                .rows()
                .iter()
                .all(|row| matches!(row.source, SourceAvailability::Captured(_)))
        );
        let after = library.work_counters();
        assert!(
            after.scan_rows - before.scan_rows <= 7,
            "only relevant name postings and their actual parent chain are inspected"
        );
        assert_eq!(after.output_rows - before.output_rows, 6);
        assert!(!library.view().compatibility_rows_are_materialized());
    }
}

#[test]
fn published_name_cursors_never_cross_full_search_or_changed_source() {
    let (library, _, _) = owner(35_300);
    let query = PublishedNameSearchQuery::admit(Query::new(
        "Document",
        library.revision_root(),
        QueryLimit::new(3).expect("credit"),
    ))
    .expect("name");
    let first = library.search_published_names(&query).expect("first");
    let cursor = first.next.expect("bounded names");
    let continued = query.clone().with_cursor(cursor);
    assert!(
        library.search(continued.query()).is_err(),
        "no full-search promotion"
    );
    assert!(
        library
            .names(
                &NameQuery::new(
                    "Document",
                    library.revision_root(),
                    QueryLimit::new(3).expect("credit")
                )
                .with_cursor(cursor)
            )
            .is_err(),
        "ordinary resolve cursor is a different contract"
    );
    let mut seen = first
        .root
        .rows()
        .iter()
        .map(|row| row.id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut next = Some(cursor);
    while let Some(cursor) = next {
        let page = library
            .search_published_names(&query.clone().with_cursor(cursor))
            .expect("same stage");
        for row in page.root.rows() {
            assert!(seen.insert(row.id));
        }
        next = page.next;
    }
    assert_eq!(seen.len(), 7, "distinct same-name bindings remain distinct");
    let changed = PublishedNameSearchQuery::admit(Query::new(
        "Document",
        view_state_root(&[("changed".to_owned(), "source".to_owned())]),
        QueryLimit::new(3).expect("credit"),
    ))
    .expect("same grammar")
    .with_cursor(cursor);
    assert!(library.search_published_names(&changed).is_err());
    let changed = PublishedNameSearchQuery::admit(Query::new(
        "Outer.Document",
        library.revision_root(),
        QueryLimit::new(3).expect("credit"),
    ))
    .expect("changed owner")
    .with_cursor(cursor);
    assert!(library.search_published_names(&changed).is_err());
}

#[test]
fn general_search_queries_do_not_silently_degrade_to_a_name_stage() {
    let basis = view_state_root(&[]);
    for text in [
        "",
        " ",
        "Document source",
        "Document*",
        "name:Document",
        "Outer..Document",
        "Outer::",
        "Document OR User",
        "Document—User",
    ] {
        let original = Query::new(text, basis, QueryLimit::new(10).expect("credit"));
        assert_eq!(
            PublishedNameSearchQuery::admit(original.clone()),
            Err(original),
            "{text}"
        );
    }
    for text in [
        "Document",
        "_Document2",
        "Outer.Document",
        "Outer::Document",
        "Δοκιμή.μέλος",
    ] {
        assert!(
            PublishedNameSearchQuery::admit(Query::new(
                text,
                basis,
                QueryLimit::new(10).expect("credit")
            ))
            .is_ok(),
            "{text}"
        );
    }
}

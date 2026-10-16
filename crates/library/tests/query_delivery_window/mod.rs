//! Native catalog selection with independent issued credit and delivery spans.
use super::*;
use std::collections::BTreeSet;

fn window(issued: u16, delivery: u16) -> QueryWindow {
    QueryWindow::new(
        QueryLimit::new(issued).expect("issued"),
        QueryLimit::new(delivery).expect("delivery"),
    )
    .expect("window")
}

fn catalog() -> Library {
    let basis = Basis::new(source().0, source().1);
    let package = package_key("window");
    let center = symbol_key("window::Thing");
    let mut rows = vec![
        Row::new(RowId::Package(package), basis, "window"),
        Row::in_package(RowId::Symbol(center), basis, package, "window::Thing"),
    ];
    rows.extend((0..10).map(|i| {
        Row::in_package(
            RowId::Symbol(symbol_key(&format!("window::Thing{i}"))),
            basis,
            package,
            format!("window::Thing{i}"),
        )
        .with_parent(center)
    }));
    projection(rows)
}

#[test]
fn catalog_delivery_windows_replay_and_emit_each_row_once_across_density_frontiers() {
    let library = catalog();
    for family in 0..3 {
        let read = |delivery, cursor: Option<Cursor>| -> Result<ViewSnapshot, LibraryError> {
            let selected = window(4, delivery);
            match family {
                0 => {
                    let query = NameQuery::new("Thing", library.revision_root(), selected.issued())
                        .with_window(selected)
                        .expect("names window");
                    library.names(&if let Some(cursor) = cursor {
                        query.with_cursor(cursor)
                    } else {
                        query
                    })
                }
                1 => {
                    let query = Query::new("Thing", library.revision_root(), selected.issued())
                        .with_window(selected)
                        .expect("search window");
                    library.search(&if let Some(cursor) = cursor {
                        query.with_cursor(cursor)
                    } else {
                        query
                    })
                }
                _ => {
                    let page = PageRequest::new(library.revision_root(), selected.issued())
                        .with_window(selected)
                        .expect("graph window");
                    library
                        .graph_page(
                            symbol_key("window::Thing"),
                            if let Some(cursor) = cursor {
                                page.with_continuation(PageContinuation::from_cursor(cursor))
                            } else {
                                page
                            },
                        )
                        .map(|page| page.snapshot)
                }
            }
        };
        let mut input = None;
        let mut seen = BTreeSet::new();
        let mut offset = 0;
        // Successful widths at offsets 0/4/5/7/9; every attempted page is replayable.
        for delivery in [4, 1, 2, 2, 4] {
            let page = read(delivery, input).expect("native window");
            assert_eq!(page, read(delivery, input).expect("deterministic replay"));
            for row in page.root.rows() {
                assert!(seen.insert(row.id), "no duplicate across windows");
            }
            offset += page.root.row_count();
            if let Some(next) = page.next {
                assert_eq!(next.query_offset(), offset);
                let span = next
                    .query_position()
                    .predecessor()
                    .expect("window position");
                assert_eq!(span.issued().get(), 4);
                assert_eq!(u64::from(span.delivery().get()), page.root.row_count());
                assert_eq!(
                    span.delivery().get(),
                    delivery,
                    "More is a complete delivery"
                );
                input = Some(next);
            } else {
                let terminal = Cursor::for_view(
                    page.root.recipe(),
                    page.root.version(),
                    Frontier::new(
                        library.cursor().branch(),
                        library.cursor().log(),
                        library.cursor().schema(),
                        page.root.root(),
                        library.cursor().sequence(),
                    ),
                )
                .with_query_position(
                    QueryPosition::windowed(
                        offset,
                        window(4, u16::try_from(page.root.row_count()).expect("tail count")),
                    )
                    .expect("synthetic end position"),
                );
                assert!(
                    read(1, Some(terminal)).is_err(),
                    "a Complete page cannot be repurposed as a continued empty page"
                );
                break;
            }
        }
        assert_eq!(seen.len(), 11, "complete query union");
        assert_eq!(offset, 11);
    }
}

#[test]
fn catalog_window_predecessor_span_credit_family_and_fixed_routes_are_checked_natively() {
    let library = catalog();
    let selected = window(4, 2);
    let query = NameQuery::new("Thing", library.revision_root(), selected.issued())
        .with_window(selected)
        .expect("window");
    let first = library.names(&query).expect("first");
    let next = first.next.expect("next");
    let mut mutants = Vec::new();
    for (offset, issued, span) in [(2, 4, 1), (3, 4, 2), (2, 3, 2)] {
        mutants.push(next.with_query_position(
            QueryPosition::windowed(offset, window(issued, span)).expect("well-typed adversary"),
        ));
    }
    for cursor in mutants {
        assert!(library.names(&query.clone().with_cursor(cursor)).is_err());
    }
    assert!(
        library
            .search(
                &Query::new("Thing", library.revision_root(), selected.issued())
                    .with_window(selected)
                    .expect("search window")
                    .with_cursor(next)
            )
            .is_err(),
        "cross-family recipe"
    );
    let changed = NameQuery::new(
        "Thing",
        library.revision_root(),
        QueryLimit::new(3).expect("changed credit"),
    )
    .with_window(window(3, 1))
    .expect("changed window")
    .with_cursor(next);
    assert!(
        library.names(&changed).is_err(),
        "issued credit cannot change"
    );
    let structured = GraphQueryRequest::new(
        "{ Declaration { name @output } }",
        std::collections::BTreeMap::new(),
        library.revision_root(),
        selected.issued(),
    )
    .expect("structured query")
    .with_continuation(PageContinuation::from_cursor(next));
    assert!(
        admit_request(&CommandDto::new(1, Command::GraphQuery(structured))).is_err(),
        "fixed structured query rejects catalog positions at typed admission"
    );
    let fixed = PageRequest::new(library.revision_root(), selected.issued())
        .with_continuation(PageContinuation::from_cursor(next));
    for page in [
        fixed,
        PageRequest::new(library.revision_root(), selected.issued())
            .with_window(selected)
            .expect("window"),
    ] {
        assert!(library.packages_page(page).is_err());
        assert!(library.outline_page(package_key("window"), page).is_err());
        assert!(admit_request(&CommandDto::new(1, Command::PackagePage(page))).is_err());
        assert!(
            admit_request(&CommandDto::new(
                1,
                Command::OutlinePage {
                    package: package_key("window"),
                    page
                }
            ))
            .is_err()
        );
    }
}

#[test]
fn legacy_predecessor_is_authenticated_before_windowed_handover() {
    let library = catalog();
    let fixed = NameQuery::new(
        "Thing",
        library.revision_root(),
        QueryLimit::new(4).expect("credit"),
    );
    let first = library.names(&fixed).expect("legacy page");
    let cursor = first.next.expect("legacy cursor");
    assert!(cursor.query_position().predecessor().is_none());
    let resumed = fixed
        .with_window(window(4, 1))
        .expect("handover")
        .with_cursor(cursor);
    let page = library
        .names(&resumed)
        .expect("old recipe predecessor, new recipe successor");
    assert_eq!(
        page.root.recipe(),
        QueryPageRecipe::names(library.revision_root(), &resumed).identity()
    );
    assert_ne!(page.root.recipe(), cursor.recipe());
    assert_eq!(page.next.expect("windowed next").query_offset(), 5);
    assert_eq!(
        page.next
            .expect("windowed next")
            .query_position()
            .predecessor(),
        Some(window(4, 1))
    );
}

//! Bounded outline membership and retained source navigation laws.
use super::*;

fn ids(node: &OutlineNode, result: &mut Vec<RowId>) {
    result.push(RowId::Symbol(node.symbol));
    for child in &node.children {
        ids(child, result);
    }
}

#[test]
fn tree_names_share_the_exact_bounded_page_and_cold_pages_retain_every_row() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package = package_key("/gymbro");
    let mut symbols = (0..300)
        .map(|i| symbol_key(&format!("/gymbro::node::{i}")))
        .collect::<Vec<_>>();
    symbols.sort_unstable();
    let owner = symbols[0];
    let mut rows = vec![Row::new(RowId::Package(package), basis, "/gymbro")];
    for (i, symbol) in symbols.iter().enumerate() {
        let mut row = Row::in_package(
            RowId::Symbol(*symbol),
            basis,
            package,
            format!("/gymbro::semantic::{i}::Name{i}"),
        );
        // Many independent low-ID roots precede the high-ID children: the
        // former full-tree DFS escaped the independently selected flat page.
        if i >= 100 {
            row = row.with_parent(owner);
        }
        rows.push(row);
    }
    let library = projection(rows);
    let request = PageRequest::new(
        library.revision_root(),
        QueryLimit::new(200).expect("limit"),
    );
    let before = library.work_counters();
    let first = library.outline_page(package, request).expect("flat page");
    assert_eq!(
        library.work_counters().scan_rows - before.scan_rows,
        300,
        "rank selection accounts for every actual visited package row"
    );
    let tree = library
        .outline(OutlineQuery::new(package, library.revision_root()))
        .expect("tree");
    let mut tree_ids = Vec::new();
    for node in tree.roots() {
        ids(node, &mut tree_ids);
    }
    tree_ids.sort_unstable();
    let mut first_ids = first
        .snapshot
        .root
        .rows()
        .iter()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    first_ids.sort_unstable();
    assert_eq!(
        tree_ids, first_ids,
        "every outline node has its exact name row"
    );
    assert_eq!(tree_ids.len(), 200);
    assert_eq!(tree.extent, OutlineExtent::Truncated);
    let PageTerminal::More(next) = first.terminal else {
        panic!("remaining rows");
    };
    let cold = Library::from_view(
        library.view().clone(),
        Cursor::for_view_root(library.view()),
    )
    .expect("cold owner");
    let second = cold
        .outline_page(package, request.with_continuation(next))
        .expect("cold second page");
    assert_eq!(second.terminal, PageTerminal::Complete);
    first_ids.extend(second.snapshot.root.rows().iter().map(|row| row.id));
    first_ids.sort_unstable();
    assert_eq!(
        first_ids,
        symbols
            .iter()
            .copied()
            .map(RowId::Symbol)
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        cold.packages_page(request.with_continuation(next)),
        Err(LibraryError::CursorMismatch)
    ));
}

#[test]
fn digest_only_outline_witness_rehydrates_only_after_exact_cold_predecessor_rebuild() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package = package_key("/gymbro");
    let mut rows = vec![Row::new(RowId::Package(package), basis, "/gymbro")];
    rows.extend((0..9).map(|index| {
        Row::in_package(
            RowId::Symbol(symbol_key(&format!("/gymbro::Item{index}"))),
            basis,
            package,
            format!("/gymbro::Item{index}"),
        )
    }));
    let library = projection(rows);
    let credit = QueryLimit::new(3).expect("credit");
    let request = PageRequest::new(library.revision_root(), credit);
    let first = library.outline_page(package, request).expect("first page");
    let PageTerminal::More(next) = first.terminal else {
        panic!("expected another page");
    };
    let cursor = next.cursor();
    let order = first.outline_order.as_ref().expect("canonical page order");
    let recipe = QueryPageRecipe::outline_window(
        library.revision_root(),
        package,
        QueryWindow::fixed(credit),
        order,
    );
    let mut root_commitment = [0_u8; 32];
    root_commitment.copy_from_slice(cursor.root().as_bytes());
    let witness = OutlineCursorWitness::new_untrusted(
        cursor.recipe(),
        cursor.version(),
        cursor.branch(),
        cursor.log(),
        cursor.schema(),
        root_commitment,
        cursor.sequence(),
        cursor.query_offset(),
        cursor.query_position().predecessor(),
    )
    .expect("valid witness position");
    let cold = Library::from_view(
        library.view().clone(),
        Cursor::for_view_root(library.view()),
    )
    .expect("equivalent durable owner after restart");
    let rebuilt = cold
        .rehydrate_outline_cursor(
            package,
            request.basis(),
            request.window(),
            witness,
            recipe.canonical_preimage(),
        )
        .expect("rebuild predecessor from durable rows");
    assert_eq!(rebuilt, cursor);
    let resumed = cold
        .outline_page(
            package,
            request.with_continuation(PageContinuation::from_cursor(rebuilt)),
        )
        .expect("standard typed path after rehydration");
    assert_eq!(resumed.snapshot.root.rows().len(), 3);

    let mut forged_root = root_commitment;
    forged_root[0] ^= 1;
    let tampered = OutlineCursorWitness::new_untrusted(
        cursor.recipe(),
        cursor.version(),
        cursor.branch(),
        cursor.log(),
        cursor.schema(),
        forged_root,
        cursor.sequence(),
        cursor.query_offset(),
        cursor.query_position().predecessor(),
    )
    .expect("same well-formed position");
    assert!(
        cold.rehydrate_outline_cursor(
            package,
            request.basis(),
            request.window(),
            tampered,
            recipe.canonical_preimage(),
        )
        .is_err()
    );
    let mut wrong_recipe = recipe.canonical_preimage().to_vec();
    wrong_recipe[0] ^= 1;
    assert!(
        cold.rehydrate_outline_cursor(
            package,
            request.basis(),
            request.window(),
            witness,
            &wrong_recipe,
        )
        .is_err()
    );
}

#[test]
fn retained_application_declarations_precede_spec_mentions_without_dropping_them() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package = package_key("/gymbro");
    let class = symbol_key("/gymbro::WeekService");
    let field = symbol_key("/gymbro::weekService");
    let spec = symbol_key("/gymbro::WeekServiceSpec");
    let rows = vec![
        Row::new(RowId::Package(package), basis, "/gymbro"),
        Row::in_package(
            RowId::Symbol(field),
            basis,
            package,
            "/gymbro::semantic::field::weekService",
        )
        .with_kind(DeclarationKind::Field)
        .with_source(SourceLocation::new("src/app.ts", 3).expect("site")),
        Row::in_package(
            RowId::Symbol(spec),
            basis,
            package,
            "/gymbro::semantic::spec::WeekServiceSpec",
        )
        .with_kind(DeclarationKind::Class)
        .with_source(SourceLocation::new("src/week.service.spec.ts", 1).expect("site")),
        Row::in_package(
            RowId::Symbol(class),
            basis,
            package,
            "/gymbro::semantic::class::WeekService",
        )
        .with_kind(DeclarationKind::Class)
        .with_source(SourceLocation::new("src/week.service.ts", 1).expect("site")),
    ];
    let library = projection(rows);
    let request = PageRequest::new(library.revision_root(), QueryLimit::new(1).expect("limit"));
    let mut continued = request;
    for expected in [class, field, spec] {
        let page = library
            .outline_page(package, continued)
            .expect("ranked membership page");
        assert_eq!(
            page.snapshot
                .root
                .rows()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![RowId::Symbol(expected)]
        );
        if let PageTerminal::More(next) = page.terminal {
            continued = request.with_continuation(next);
        } else {
            assert_eq!(expected, spec);
        }
    }
    let tree = library
        .outline(OutlineQuery::new(package, library.revision_root()))
        .expect("tree");
    assert_eq!(tree.root.symbol, class);
    assert_eq!(tree.roots().count(), 3);
    assert_eq!(tree.extent, OutlineExtent::Complete);
}

#[test]
fn cyclic_and_deep_retained_parents_never_drop_the_bounded_name_set() {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let package = package_key("/gymbro");
    let symbols = (0..100)
        .map(|i| symbol_key(&format!("deep::{i}")))
        .collect::<Vec<_>>();
    let mut rows = vec![Row::new(RowId::Package(package), basis, "/gymbro")];
    for (i, symbol) in symbols.iter().enumerate() {
        rows.push(
            Row::in_package(RowId::Symbol(*symbol), basis, package, format!("N{i}"))
                .with_parent(symbols[(i + 99) % 100]),
        );
    }
    let library = projection(rows);
    let tree = library
        .outline(OutlineQuery::new(package, library.revision_root()))
        .expect("cycle bounded");
    let mut observed = Vec::new();
    for node in tree.roots() {
        ids(node, &mut observed);
    }
    observed.sort_unstable();
    let mut expected = symbols.into_iter().map(RowId::Symbol).collect::<Vec<_>>();
    expected.sort_unstable();
    assert_eq!(observed, expected);
    assert_eq!(tree.extent, OutlineExtent::Truncated);
}

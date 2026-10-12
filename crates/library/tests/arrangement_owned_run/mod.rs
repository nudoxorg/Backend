use super::*;

/// Synthetic admitted view with the same row-count shape as the profiled
/// 260-function fixture: 36 parameters and one synthetic result per function.
/// This is an arrangement-phase fixture, not compiler or product evidence.
fn admitted_fixture(functions: usize) -> (ViewProjection, usize) {
    let (source, object) = source();
    let basis = Basis::new(source, object);
    let package = package_key("/projection/owned-run");
    let mut rows = vec![Row::new(
        RowId::Package(package),
        basis,
        "/projection/owned-run",
    )];
    for index in 0..functions {
        let label = format!("pkg::function_{index:04}");
        let parent = symbol_key(&label);
        rows.push(
            Row::in_package(RowId::Symbol(parent), basis, package, label.clone())
                .with_kind(DeclarationKind::Function)
                .with_signature(format!("def function_{index:04}(value: café) -> Δ"))
                .with_document(vec![Fragment::Text("Δ café needle repeated ".repeat(128))]),
        );
        rows.push(
            Row::in_package(
                RowId::Symbol(symbol_key(&format!("{label}::result"))),
                basis,
                package,
                label.clone(),
            )
            .with_parent(parent)
            .with_kind(DeclarationKind::Variable)
            .with_signature("Δ"),
        );
        for parameter in 0..36 {
            let label = format!("{label}::parameter_{parameter:02}");
            rows.push(
                Row::in_package(RowId::Symbol(symbol_key(&label)), basis, package, label)
                    .with_parent(parent)
                    .with_kind(DeclarationKind::Variable)
                    .with_signature("café"),
            );
        }
    }
    let input_text_bytes = rows
        .iter()
        .map(|row| {
            row.label.len()
                + row.signature.as_ref().map_or(0, String::len)
                + row
                    .document
                    .iter()
                    .map(|fragment| match fragment {
                        Fragment::Text(text) | Fragment::Code(text) => text.len(),
                        Fragment::Link { label, .. } => label.len(),
                        Fragment::Break => 0,
                    })
                    .sum::<usize>()
        })
        .sum();
    // Primary relation admission accepts arbitrary input order. The derived
    // arrangement must independently produce canonical secondary key order.
    rows.reverse();
    let view = ViewRoot::new_checked(
        view_key(b"owned-run-arrangement"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
        rows,
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("admitted primary relation");
    let cursor = Cursor::for_view_root(&view);
    (
        ViewProjection::admit(view, cursor).expect("admitted root and cursor"),
        input_text_bytes,
    )
}

#[test]
fn owned_arrangement_keeps_large_view_membership_paging_and_delta_results() {
    let (projection, _) = admitted_fixture(8);
    let library = Library::from_projection(projection).expect("initial arrangement");
    assert_eq!(library.view().row_count(), 305);
    assert_eq!(library.work_counters().indexed_rows, 305);
    assert!(!library.view().compatibility_rows_are_materialized());
    let functions = library
        .names(&NameQuery::new(
            "function_",
            library.revision_root(),
            QueryLimit::new(200).expect("bounded page"),
        ))
        .expect("function membership");
    assert_eq!(functions.root.row_count(), 8, "result slots stay excluded");

    let parameters = NameQuery::new(
        "parameter_",
        library.revision_root(),
        QueryLimit::new(200).expect("bounded page"),
    );
    let first = library.names(&parameters).expect("first parameter page");
    assert_eq!(first.root.row_count(), 200);
    let second = library
        .names(
            &parameters
                .clone()
                .with_cursor(first.next.expect("continuation")),
        )
        .expect("second parameter page");
    assert_eq!(second.root.row_count(), 88);
    assert!(second.next.is_none());
    let ids = first
        .root
        .row_refs()
        .chain(second.root.row_refs())
        .map(|row| row.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ids.len(),
        288,
        "no omitted or duplicate parameter membership"
    );

    let id = RowId::Symbol(symbol_key("pkg::function_0000::parameter_00"));
    let mut replacement = library.view().row(id).expect("captured parameter");
    replacement.label = "pkg::function_0000::renamed_parameter".to_owned();
    replacement.document = vec![Fragment::Text("replacement needle".to_owned())].into();
    let object = library.view().basis().object;
    let (updated, _) = advance(
        library,
        ViewDelta::Upsert { row: replacement },
        capability(object),
    );
    let rebuilt = Library::from_projection(
        ViewProjection::admit(updated.view().clone(), updated.cursor())
            .expect("current root and cursor"),
    )
    .expect("rebuilt arrangement");
    for text in [
        "function_",
        "parameter_00",
        "renamed_parameter",
        "Δ",
        "absent",
    ] {
        let query = NameQuery::new(text, updated.revision_root(), QueryLimit::default());
        assert_eq!(updated.names(&query), rebuilt.names(&query), "{text}");
    }
    for text in ["needle", "replacement", "café", "absent"] {
        let query = Query::new(text, updated.revision_root(), QueryLimit::default());
        assert_eq!(updated.search(&query), rebuilt.search(&query), "{text}");
    }
    assert!(matches!(
        updated.names(&parameters),
        Err(LibraryError::WrongBasis { .. })
    ));
    assert_eq!(rebuilt.view().root(), updated.view().root());
    assert_eq!(
        rebuilt.view().relation_coverage(),
        updated.view().relation_coverage()
    );
    assert!(!updated.view().compatibility_rows_are_materialized());
    assert!(!rebuilt.view().compatibility_rows_are_materialized());
}

#[test]
#[ignore = "paired arrangement allocation experiment on frozen baseline and candidate"]
fn profile_admitted_projection_arrangement_allocations() {
    for functions in [8, 260] {
        let (projection, input_text_bytes) = admitted_fixture(functions);
        let rows = projection.root().row_count();
        assert_eq!(rows, 1 + functions as u64 * 38);
        for observation in 0..3 {
            // Source construction, source admission, and projection cloning
            // are outside this phase. Every call builds a fresh arrangement.
            let admitted = projection.clone();
            let mut library = None;
            let started = std::time::Instant::now();
            let allocations = allocation_counter::measure(|| {
                library = Some(Library::from_projection(admitted).expect("arrangement"));
            });
            let elapsed = started.elapsed();
            let library = library.expect("measured library");
            assert_eq!(library.view().row_count(), rows);
            assert_eq!(library.work_counters().indexed_rows, rows);
            assert!(!library.view().compatibility_rows_are_materialized());
            eprintln!(
                "arrangement_allocation {}",
                serde_json::json!({
                    "functions": functions,
                    "rows": rows,
                    "input_text_bytes": input_text_bytes,
                    "observation": observation,
                    "elapsed_ns": elapsed.as_nanos(),
                    "count_total": allocations.count_total,
                    "count_current": allocations.count_current,
                    "count_max": allocations.count_max,
                    "bytes_total": allocations.bytes_total,
                    "bytes_current": allocations.bytes_current,
                    "bytes_max": allocations.bytes_max,
                })
            );
            std::hint::black_box(library);
        }
    }
}

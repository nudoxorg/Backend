//! Public locators never replace compiler-owned declaration identities.

use super::*;

fn rows() -> (Basis, PackageKey, SymbolKey, Row, Row) {
    let (root, object) = source();
    let basis = Basis::new(root, object);
    let label = "pkg:workspace::nested";
    let package = package_key(label);
    let native = symbol_key("actual-compiler-row-identity");
    let owner = Row::new(RowId::Package(package), basis, label);
    let declaration = Row::in_package(
        RowId::Symbol(native),
        basis,
        package,
        format!("{label}::semantic::actual-compiler-origin::draw"),
    )
    .with_source(SourceLocation::new("src/lib.rs", 9).expect("captured site"));
    (basis, package, native, owner, declaration)
}

const ALIAS: &str = "pkg:workspace::nested::src/lib.rs:9::draw";

#[test]
fn captured_coordinate_stream_uses_existing_identity_schema_and_borrows_native_row() {
    let (_, _, native, owner, row) = rows();
    let proof = CapturedSourceCoordinate::from_row_and_package(&row, &owner).expect("proof");
    assert_eq!(proof.coordinate(), ALIAS);
    assert_eq!(proof.address_key(), Some(symbol_key(ALIAS)));
    assert_ne!(proof.address_key(), Some(native));
    assert!(std::ptr::eq(proof.row(), &row));
    assert!(proof.matches(ALIAS));
    assert!(!proof.matches_address(SymbolAddress::selected(symbol_key(ALIAS))));
    for alias in [
        "pkg:foreign::src/lib.rs:9::draw",
        "pkg:workspace::nested::src/other.rs:9::draw",
        "pkg:workspace::nested::src/lib.rs:10::draw",
        "pkg:workspace::nested::src/lib.rs:09::draw",
        "pkg:workspace::nested::src/lib.rs:+9::draw",
        "pkg:workspace::nested::src/lib.rs:0::draw",
        "pkg:workspace::nested::src/lib.rs:9::other",
    ] {
        assert!(!proof.matches(alias), "{alias}");
    }
    let mut foreign_label = row.clone();
    foreign_label.label = "pkg:foreign::semantic::same-origin::draw".to_owned();
    assert!(CapturedSourceCoordinate::from_row(&foreign_label).is_none());
    assert!(CapturedSourceCoordinate::from_row_and_package(&foreign_label, &owner).is_none());
    let mut wrong_owner = owner.clone();
    wrong_owner.label = "pkg:foreign".to_owned();
    assert!(CapturedSourceCoordinate::from_row_and_package(&row, &wrong_owner).is_none());
    wrong_owner = owner.clone();
    wrong_owner.basis = Basis::new(view_state_root(&[]), object_version(b"foreign-source"));
    assert!(CapturedSourceCoordinate::from_row_and_package(&row, &wrong_owner).is_none());
    for source in [
        SourceAvailability::NotCaptured,
        SourceAvailability::NotHydrated,
        SourceAvailability::Unconfigured,
        SourceAvailability::stale_file("src/lib.rs").expect("stale path"),
    ] {
        let mut unavailable = row.clone();
        unavailable.source = source;
        assert!(CapturedSourceCoordinate::from_row(&unavailable).is_none());
    }
}

#[test]
fn qualified_declaration_binds_package_prefix_independently_of_captured_source() {
    let (_, _, native, owner, mut row) = rows();
    row.source = SourceAvailability::NotCaptured;
    let proof = PackageQualifiedDeclaration::from_row(&row).expect("qualified declaration");
    assert_eq!(proof.package_label(), owner.label);
    assert_eq!(proof.row().id, RowId::Symbol(native));
    assert!(std::ptr::eq(proof.row(), &row));
    assert!(PackageQualifiedDeclaration::from_row_and_package(&row, &owner).is_some());
    assert!(CapturedSourceCoordinate::from_row(&row).is_none());

    let mut foreign_owner = owner.clone();
    foreign_owner.basis = Basis::new(view_state_root(&[]), object_version(b"other-package-basis"));
    assert!(PackageQualifiedDeclaration::from_row_and_package(&row, &foreign_owner).is_none());
    foreign_owner = owner.clone();
    foreign_owner.label = "pkg:foreign".to_owned();
    assert!(PackageQualifiedDeclaration::from_row_and_package(&row, &foreign_owner).is_none());
    for label in [
        "pkg:foreign::semantic::identity::draw".to_owned(),
        format!("{}::", owner.label),
        format!("{}-foreign::semantic::identity::draw", owner.label),
    ] {
        row.label = label;
        assert!(PackageQualifiedDeclaration::from_row(&row).is_none());
        assert!(PackageQualifiedDeclaration::from_row_and_package(&row, &owner).is_none());
    }
    row.label = format!("{}::semantic::identity::draw", owner.label);
    row.package = None;
    assert!(PackageQualifiedDeclaration::from_row(&row).is_none());
    assert!(PackageQualifiedDeclaration::from_row_and_package(&row, &owner).is_none());
}

#[test]
fn immutable_alias_index_revalidates_tuple_rejects_ambiguity_and_resets_after_transition() {
    let (basis, _, native, owner, row) = rows();
    let library = projection(vec![owner.clone(), row.clone()]);
    let old = library.view().clone();
    for _ in 0..3 {
        let selected = resolve_selected_view_address(library.view(), ALIAS)
            .declaration()
            .expect("well-typed address")
            .expect("selected row");
        assert_eq!(selected.id, RowId::Symbol(native));
        assert!(std::ptr::eq(
            selected,
            library.view().row_ref(row.id).expect("native row")
        ));
    }
    assert!(!library.view().compatibility_rows_are_materialized());
    let source = Basis {
        root: old.root(),
        ..old.basis()
    };
    let selection =
        DocumentSelection::from_selected_view(&old, source, ALIAS).expect("unique selection");
    assert_eq!(selection.coordinate(), ALIAS);
    assert_eq!(selection.symbol(), native);
    assert!(
        DocumentSelection::from_selected_view(&old, basis, ALIAS).is_none(),
        "old root is not this selection basis"
    );
    let moved = row
        .clone()
        .with_source(SourceLocation::new("src/lib.rs", 10).expect("new site"));
    let (next, _) = advance(
        library,
        ViewDelta::Upsert { row: moved },
        capability(basis.object),
    );
    assert!(matches!(
        resolve_selected_view_address(next.view(), ALIAS),
        SelectedViewAddress::Absent
    ));
    assert_eq!(
        resolve_selected_view_address(next.view(), "pkg:workspace::nested::src/lib.rs:10::draw")
            .declaration()
            .expect("new address")
            .expect("current row")
            .id,
        row.id
    );
    assert_eq!(
        resolve_selected_view_address(&old, ALIAS)
            .declaration()
            .expect("retained root")
            .expect("retained row")
            .id,
        row.id
    );
    assert!(!old.compatibility_rows_are_materialized());
    assert!(!next.view().compatibility_rows_are_materialized());

    let mut duplicate = row.clone();
    duplicate.id = RowId::Symbol(symbol_key("other-compiler-identity"));
    duplicate.label = "pkg:workspace::nested::semantic::other-origin::draw".to_owned();
    let ambiguous = projection(vec![owner.clone(), row.clone(), duplicate]);
    assert!(matches!(
        resolve_selected_view_address(ambiguous.view(), ALIAS),
        SelectedViewAddress::Ambiguous
    ));
    assert!(
        DocumentSelection::from_selected_view(
            ambiguous.view(),
            Basis {
                root: ambiguous.revision_root(),
                ..basis
            },
            ALIAS
        )
        .is_none()
    );
    // Existing exact-label priority remains intact even when an alias is ambiguous.
    assert_eq!(
        resolve_selected_view_address(ambiguous.view(), &row.label)
            .declaration()
            .expect("exact label")
            .expect("actual row")
            .id,
        row.id
    );
    let no_owner = projection(vec![row.clone()]);
    assert!(matches!(
        resolve_selected_view_address(no_owner.view(), ALIAS),
        SelectedViewAddress::Absent
    ));
    let mut foreign = row.clone();
    foreign.label = "pkg:foreign::semantic::origin::draw".to_owned();
    let wrong_namespace = projection(vec![owner, foreign]);
    assert!(matches!(
        resolve_selected_view_address(wrong_namespace.view(), ALIAS),
        SelectedViewAddress::Absent
    ));
}

#[test]
fn graph_pages_admit_actual_center_address_before_paging_and_keep_original_alias_recipe() {
    let (basis, package, native, owner, row) = rows();
    let neighbor = symbol_key("neighbor-compiler-row");
    let library = projection(vec![
        owner,
        row,
        Row::in_package(
            RowId::Symbol(neighbor),
            basis,
            package,
            "pkg:workspace::nested::semantic::neighbor::other",
        )
        .with_parent(native),
    ]);
    let alias = SymbolAddress::canonical(symbol_key(ALIAS));
    let request = PageRequest::new(library.revision_root(), QueryLimit::new(1).expect("credit"));
    let mut ids = vec![RowId::Symbol(native), RowId::Symbol(neighbor)];
    ids.sort();
    let first = library
        .graph_page_from_ids(native, alias, request, &ids)
        .expect("valid first page");
    let PageTerminal::More(next) = first.terminal else {
        panic!("next page");
    };
    let second = library
        .graph_page_from_ids(native, alias, request.with_continuation(next), &ids)
        .expect("original alias resumes");
    assert_eq!(second.terminal, PageTerminal::Complete);
    assert_ne!(
        first.snapshot.root.row_refs().next().expect("first row").id,
        second
            .snapshot
            .root
            .row_refs()
            .next()
            .expect("second row")
            .id
    );
    // The selected declaration need not be in every page: its full-view proof
    // is checked before the page subset and original-address recipe are built.
    for page in [request, request.with_continuation(next)] {
        assert!(
            library
                .graph_page_for_address(
                    native,
                    SymbolAddress::canonical(symbol_key("foreign")),
                    page
                )
                .is_err()
        );
        assert!(
            library
                .graph_page_from_ids(neighbor, alias, page, &ids)
                .is_err()
        );
        assert!(
            library
                .graph_page_for_address(native, SymbolAddress::selected(symbol_key(ALIAS)), page)
                .is_err()
        );
    }
    assert!(
        library
            .graph_page_from_ids(
                native,
                SymbolAddress::selected(native),
                request.with_continuation(next),
                &ids
            )
            .is_err(),
        "alias recipe cannot become a native-selector recipe"
    );
}

#[test]
fn captured_coordinate_preserves_qualified_names_paths_and_maximum_line_without_reparsing_identity()
{
    let (basis, package, native, owner, _) = rows();
    let path = "src/name::part.rs";
    let location = SourceLocation::new(path, u32::MAX).expect("bounded path, nonzero line");
    let expected = "pkg:workspace::nested::src/name::part.rs:4294967295::namespace::draw";
    for label in [
        "pkg:workspace::nested::semantic::origin::namespace::draw",
        expected,
    ] {
        let row = Row::in_package(RowId::Symbol(native), basis, package, label)
            .with_source(location.clone());
        let proof = CapturedSourceCoordinate::from_row_and_package(&row, &owner)
            .expect("borrowed exact fields");
        assert_eq!(proof.name(), "namespace::draw");
        assert_eq!(proof.coordinate(), expected);
        assert_eq!(proof.address_key(), Some(symbol_key(expected)));
        let library = projection(vec![owner.clone(), row]);
        assert_eq!(
            resolve_selected_view_address(library.view(), expected)
                .declaration()
                .expect("qualified name address")
                .expect("actual row")
                .id,
            RowId::Symbol(native)
        );
        assert!(
            matches!(
                resolve_selected_view_address(
                    library.view(),
                    "pkg:workspace::nested::src/name::part.rs:4294967295::draw"
                ),
                SelectedViewAddress::Absent
            ),
            "a final name component is not the complete declaration name"
        );
    }
}

#[test]
fn retained_symbol_document_address_requires_exact_unique_membership_and_full_basis() {
    let (basis, _, native, owner, declaration) = rows();
    let external = symbol_key("actual-scoped-external-target");
    let label = format!(
        "pkg:workspace::nested::external::{}::draw",
        RowId::Symbol(external)
    );
    let row = Row::new(RowId::Symbol(external), basis, &label);
    let library = projection(vec![owner.clone(), declaration.clone(), row.clone()]);
    let view = library.view();
    let source = Basis {
        root: view.root(),
        ..view.basis()
    };
    let selected = resolve_selected_document_row(view, &label)
        .expect("document address")
        .expect("retained actual row");
    assert!(std::ptr::eq(selected, view.row_ref(row.id).expect("row")));
    assert_eq!(selected.id, row.id);
    assert_eq!(selected.package, None);
    assert!(matches!(
        resolve_selected_view_address(view, &label),
        SelectedViewAddress::Absent
    ));
    let selection = DocumentSelection::from_selected_view(view, source, &label)
        .expect("actual retained symbol selection");
    let document = Document::new(external, view.root(), row.document.clone())
        .with_source_basis(source)
        .with_location(row.source.clone())
        .with_excerpt(row.excerpt.clone())
        .with_selection(selection)
        .expect("exact native identity");
    assert_ne!(external, symbol_key(&label));
    assert!(document.admits_query(
        &DocumentQuery::new(symbol_key(&label), view.root()).with_source_basis(source)
    ));
    assert!(
        document.admits_query(
            &DocumentQuery::selected(external, view.root()).with_source_basis(source)
        )
    );
    assert!(document.fragments.is_empty());
    assert_eq!(document.location, SourceAvailability::NotCaptured);
    assert_eq!(document.excerpt, SourceExcerpt::NotCaptured);
    for coordinate in [
        String::new(),
        "x".repeat(MAX_ROW_IDENTITY_PREIMAGE_BYTES + 1),
        "draw".to_owned(),
        label.replace("draw", "other"),
        label.replace("pkg:workspace", "pkg:foreign"),
        format!("{label}::suffix"),
    ] {
        assert!(
            resolve_selected_document_row(view, &coordinate)
                .expect("absent address")
                .is_none()
        );
        assert!(DocumentSelection::from_selected_view(view, source, &coordinate).is_none());
    }
    for foreign in [
        Basis {
            root: basis.root,
            ..source
        },
        Basis {
            object: object_version(b"foreign-object"),
            ..source
        },
        Basis {
            branch: branch_key("foreign-branch"),
            ..source
        },
        Basis {
            log: log_key("foreign-log"),
            ..source
        },
        Basis {
            schema: source.schema.wrapping_add(1),
            ..source
        },
    ] {
        assert!(DocumentSelection::from_selected_view(view, foreign, &label).is_none());
        assert!(!document.admits_query(
            &DocumentQuery::new(symbol_key(&label), view.root()).with_source_basis(foreign)
        ));
    }
    let duplicate = Row::new(
        RowId::Symbol(symbol_key("other-retained-target")),
        basis,
        &label,
    );
    let ambiguous = projection(vec![row.clone(), duplicate]);
    assert!(matches!(
        resolve_selected_document_row(ambiguous.view(), &label),
        Err(CommandFailure::InvalidQuery(_))
    ));
    let collision = projection(vec![
        row.clone(),
        Row::new(RowId::Package(package_key(&label)), basis, &label),
    ]);
    assert!(matches!(
        resolve_selected_document_row(collision.view(), &label),
        Err(CommandFailure::InvalidQuery(_))
    ));
    let orphan = projection(vec![declaration.clone()]);
    assert!(
        resolve_selected_document_row(orphan.view(), &declaration.label)
            .expect("missing owner")
            .is_none()
    );
    let mut malformed = declaration;
    malformed.package = Some(package_key("pkg:foreign"));
    let malformed = projection(vec![owner, malformed.clone()]);
    assert!(
        resolve_selected_document_row(
            malformed.view(),
            &malformed
                .view()
                .row_ref(RowId::Symbol(native))
                .expect("native")
                .label
        )
        .expect("foreign owner")
        .is_none()
    );
    let old = view.clone();
    let (next, _) = advance(
        library,
        ViewDelta::Remove { id: row.id },
        capability(basis.object),
    );
    assert!(
        resolve_selected_document_row(next.view(), &label)
            .expect("removed row")
            .is_none()
    );
    assert!(
        resolve_selected_document_row(&old, &label)
            .expect("retained old view")
            .is_some()
    );
}

fn structural_twin_of(semantic: &Row, basis: Basis, package: PackageKey) -> Row {
    Row::in_package(RowId::Symbol(symbol_key(ALIAS)), basis, package, ALIAS).with_source(
        semantic
            .source
            .captured()
            .expect("semantic row captured its site")
            .clone(),
    )
}

#[test]
fn lane_twins_are_one_declaration_addressed_by_its_structural_row() {
    let (basis, package, _, owner, semantic) = rows();
    let structural = structural_twin_of(&semantic, basis, package);
    let library = projection(vec![owner, semantic.clone(), structural.clone()]);
    let view = library.view();

    // Exact labels keep selecting exactly their own row ...
    assert_eq!(
        resolve_selected_view_address(view, ALIAS)
            .declaration()
            .expect("structural label")
            .expect("row")
            .id,
        structural.id
    );
    assert_eq!(
        resolve_selected_view_address(view, &semantic.label)
            .declaration()
            .expect("semantic label")
            .expect("row")
            .id,
        semantic.id
    );
    // ... while the canonical resolvers send both spellings to one row.
    for spelling in [ALIAS, semantic.label.as_str()] {
        let address = resolve_canonical_declaration(view, spelling);
        let SelectedViewAddress::Declaration { row, .. } = address else {
            panic!("{spelling} names a declaration");
        };
        assert_eq!(row.id, structural.id, "{spelling}");
        assert_eq!(
            resolve_canonical_document_row(view, spelling)
                .expect("document address")
                .expect("document row")
                .id,
            structural.id,
            "{spelling}"
        );
    }
    assert!(matches!(
        resolve_canonical_declaration(view, &semantic.label),
        SelectedViewAddress::Declaration {
            matched_as: AddressMatchKind::LaneTwin,
            ..
        }
    ));
    assert_eq!(
        view.structural_twin(&semantic).map(|row| row.id),
        Some(structural.id)
    );
    assert_eq!(
        view.semantic_twin(&structural).map(|row| row.id),
        Some(semantic.id)
    );
    assert!(view.structural_twin(&structural).is_none());
    assert!(view.semantic_twin(&semantic).is_none());
    assert_eq!(view.canonical_row_id(semantic.id), structural.id);
    assert_eq!(view.canonical_row_id(structural.id), structural.id);
    // The selection a document certificate is built from names the same row
    // for either spelling.
    let source = Basis {
        root: view.root(),
        ..view.basis()
    };
    for spelling in [ALIAS, semantic.label.as_str()] {
        let selection = DocumentSelection::from_selected_view(view, source, spelling)
            .expect("either spelling selects a document");
        assert_eq!(RowId::Symbol(selection.symbol()), structural.id);
        assert_eq!(selection.coordinate(), spelling);
    }

    // A names page lists the declaration once, under its structural row.
    let names = library
        .names(&NameQuery::new(
            "draw",
            library.revision_root(),
            QueryLimit::default(),
        ))
        .expect("names page");
    assert_eq!(
        names.root.row_refs().map(|row| row.id).collect::<Vec<_>>(),
        vec![structural.id]
    );
    // Naming only the compiler-backed spelling still resolves, to the twin.
    let by_address = library
        .names(&NameQuery::new(
            semantic.label.clone(),
            library.revision_root(),
            QueryLimit::default(),
        ))
        .expect("address page");
    assert_eq!(
        by_address
            .root
            .row_refs()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![structural.id]
    );
}

#[test]
fn lane_twins_stay_separate_unless_each_lane_has_exactly_one_row_of_the_same_sort() {
    let (basis, package, _, owner, semantic) = rows();
    let structural = structural_twin_of(&semantic, basis, package);

    // A second compiler-backed row at the site makes the pairing ambiguous.
    let mut other = semantic.clone();
    other.id = RowId::Symbol(symbol_key("other-compiler-identity"));
    other.label = "pkg:workspace::nested::semantic::other-origin::draw".to_owned();
    let ambiguous = projection(vec![
        owner.clone(),
        semantic.clone(),
        other,
        structural.clone(),
    ]);
    assert!(ambiguous.view().structural_twin(&semantic).is_none());
    assert_eq!(
        ambiguous.view().canonical_row_id(semantic.id),
        semantic.id,
        "an ambiguous site never rewrites an identity"
    );

    // An import pairs only with an import.
    let imported = semantic.clone().with_kind(DeclarationKind::Import);
    let mixed = projection(vec![owner.clone(), imported.clone(), structural.clone()]);
    assert!(mixed.view().structural_twin(&imported).is_none());
    let both = projection(vec![
        owner.clone(),
        imported.clone(),
        structural.clone().with_kind(DeclarationKind::Import),
    ]);
    assert!(both.view().structural_twin(&imported).is_some());

    // A different name at the same line is another declaration.
    let mut renamed = structural.clone();
    renamed.id = RowId::Symbol(symbol_key("pkg:workspace::nested::src/lib.rs:9::paint"));
    renamed.label = "pkg:workspace::nested::src/lib.rs:9::paint".to_owned();
    let apart = projection(vec![owner, semantic.clone(), renamed]);
    assert!(apart.view().structural_twin(&semantic).is_none());
}

#[test]
fn external_placeholders_are_inert_only_without_any_declaration_fact() {
    let (basis, _, _, _, _) = rows();
    let hex = "ab".repeat(32);
    let label = format!("pkg:workspace::nested::external::symbol:{hex}::get");
    assert_eq!(external_placeholder_display(&label), Some("get"));
    for not_external in [
        "pkg:workspace::nested::semantic::origin::get",
        "pkg:workspace::nested::src/lib.rs:9::get",
        "pkg:workspace::nested::external::symbol:short::get",
        &format!("pkg:workspace::nested::external::symbol:{hex}::"),
    ] {
        assert_eq!(
            external_placeholder_display(not_external),
            None,
            "{not_external}"
        );
    }
    let placeholder = Row::new(RowId::Symbol(symbol_key(&label)), basis, label)
        .with_document(Box::<[Fragment]>::default());
    assert!(is_inert_external_placeholder(&placeholder));
    assert!(!is_inert_external_placeholder(
        &placeholder.clone().with_kind(DeclarationKind::Function)
    ));
    assert!(!is_inert_external_placeholder(
        &placeholder.clone().with_signature("fn get()")
    ));
    assert!(!is_inert_external_placeholder(
        &placeholder
            .clone()
            .with_source(SourceLocation::new("src/lib.rs", 1).expect("site"))
    ));
    assert!(!is_inert_external_placeholder(
        &placeholder.with_document(vec![Fragment::Text("documented".to_owned())])
    ));
}

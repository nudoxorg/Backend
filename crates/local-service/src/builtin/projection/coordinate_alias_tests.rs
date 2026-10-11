//! Authenticate real service certificates before testing source-coordinate rules.

use super::*;
use backend_library::{
    Basis, CapturedSourceCoordinate, CommandDto, Document, DocumentQuery, DocumentSelection,
    GraphNeighborhoodQuery, Library, ReplyDto, RichGraphRevision, RichGraphSnapshot, Row,
    SemanticGenerationId, SourceAvailability, SourceLocation, package_key, symbol_key,
};

fn build_owner(rows: Vec<Row>) -> ViewRoot {
    let (template, _) = super::super::initial_view().expect("initial view");
    ViewRoot::new_checked(
        template.recipe(),
        template.basis(),
        template.frontier(),
        rows,
        vec![backend_engine::ViewCoverage::Complete],
        super::super::test_builtin_view_capability().expect("source capability"),
    )
    .expect("immutable owner")
}

fn fixture() -> (Row, Row, Row) {
    let (template, _) = super::super::initial_view().expect("basis");
    let package = package_key("pkg:alias-owner");
    let native = symbol_key("selected-native-compiler-identity");
    let child = symbol_key("selected-native-neighbor-identity");
    (
        Row::new(RowId::Package(package), template.basis(), "pkg:alias-owner"),
        Row::in_package(
            RowId::Symbol(native),
            template.basis(),
            package,
            "pkg:alias-owner::semantic::identity::draw",
        )
        .with_source(SourceLocation::new("src/lib.rs", 9).expect("captured site")),
        Row::in_package(
            RowId::Symbol(child),
            template.basis(),
            package,
            "pkg:alias-owner::semantic::neighbor::other",
        )
        .with_parent(native)
        .with_source(SourceLocation::new("src/other.rs", 4).expect("neighbor")),
    )
}

const ALIAS: &str = "pkg:alias-owner::src/lib.rs:9::draw";

fn certify_decode(command: &Command, reply: CommandReply, owner: &ViewRoot) -> ReplyDto {
    let certificate = reply_certificate(
        command,
        &reply,
        owner,
        backend_engine::Cursor::for_view_root(owner),
        None,
    )
    .expect("service certificate")
    .expect("certified reply");
    let encoded = serde_json::to_vec(&ReplyDto::new(0x3200, reply).with_certificate(certificate))
        .expect("wire envelope");
    ReplyDto::decode_with_certificate(&encoded, owner.capability())
        .expect("independent authenticated decoder")
}

#[test]
fn admitted_source_alias_document_tuple_refuses_foreign_source_basis_and_wire_mutations() {
    let (package, row, child) = fixture();
    let native = match row.id {
        RowId::Symbol(native) => native,
        _ => panic!("symbol"),
    };
    let owner = build_owner(vec![package.clone(), row.clone(), child]);
    let basis = Basis {
        root: owner.root(),
        ..owner.basis()
    };
    let selection = DocumentSelection::from_selected_view(&owner, basis, ALIAS)
        .expect("unique captured source selection");
    let document = Document::new(native, owner.root(), row.document.clone())
        .with_source_basis(basis)
        .with_location(row.source.clone())
        .with_selection(selection)
        .expect("native document");
    let command = Command::Document(
        DocumentQuery::new(symbol_key(ALIAS), owner.root()).with_source_basis(basis),
    );
    let decoded = certify_decode(&command, CommandReply::Document(document), &owner);
    backend_library::admit_reply(&CommandDto::new(0x3200, command), &decoded)
        .expect("same source alias");
    let mut foreign = basis;
    foreign.object = backend_library::object_version(b"foreign-source-object");
    assert!(DocumentSelection::from_selected_view(&owner, foreign, ALIAS).is_none());
    assert!(
        backend_library::admit_reply(
            &CommandDto::new(
                0x3200,
                Command::Source(
                    DocumentQuery::new(symbol_key(ALIAS), owner.root()).with_source_basis(foreign)
                )
            ),
            &decoded
        )
        .is_err()
    );
    let json = serde_json::to_value(&decoded).expect("actual certified document");
    for (field, value) in [
        (
            "coordinate",
            serde_json::json!("pkg:foreign::src/lib.rs:9::draw"),
        ),
        (
            "symbol",
            serde_json::json!(backend_library::encode_id(
                symbol_key("foreign-native").as_bytes()
            )),
        ),
        (
            "version",
            serde_json::json!(backend_library::encode_id(
                symbol_key("foreign-tuple").as_bytes()
            )),
        ),
    ] {
        let mut mutated = json.clone();
        mutated["reply"]["data"]["selection"][field] = value;
        assert!(
            ReplyDto::decode_with_certificate(
                &serde_json::to_vec(&mutated).expect("mutated tuple"),
                owner.capability()
            )
            .is_err(),
            "{field}"
        );
    }
    let mut lost_source = row.clone();
    lost_source.source = SourceAvailability::NotCaptured;
    let changed = build_owner(vec![package.clone(), lost_source]);
    assert!(
        DocumentSelection::from_selected_view(
            &changed,
            Basis {
                root: changed.root(),
                ..changed.basis()
            },
            ALIAS
        )
        .is_none()
    );
    let mut foreign_label = row;
    foreign_label.label = "pkg:foreign::semantic::identity::draw".to_owned();
    let changed = build_owner(vec![package, foreign_label]);
    assert!(
        DocumentSelection::from_selected_view(
            &changed,
            Basis {
                root: changed.root(),
                ..changed.basis()
            },
            ALIAS
        )
        .is_none()
    );
}

#[test]
fn certified_alias_graph_rejects_ambiguous_foreign_and_unavailable_source_rows() {
    let (package, row, child) = fixture();
    let RowId::Symbol(native) = row.id else {
        panic!("native declaration");
    };
    for mutation in 0..6 {
        let mut row = row.clone();
        let mut child = child.clone();
        let mut rows = vec![package.clone()];
        match mutation {
            0 => {}
            1 => row.label = "pkg:foreign::semantic::identity::draw".to_owned(),
            2 => row.source = SourceAvailability::NotCaptured,
            3 => row.source = SourceAvailability::stale_file("src/lib.rs").expect("stale source"),
            4 => {
                row.source = SourceAvailability::Captured(
                    SourceLocation::new("src/lib.rs", 10).expect("moved source"),
                )
            }
            5 => {
                child.label = "pkg:alias-owner::semantic::duplicate::draw".to_owned();
                child.source = row.source.clone();
            }
            _ => unreachable!(),
        }
        rows.extend([row, child]);
        let owner = build_owner(rows);
        let library =
            Library::from_view(owner.clone(), backend_engine::Cursor::for_view_root(&owner))
                .expect("library");
        let snapshot = library
            .graph(GraphNeighborhoodQuery::selected(native, owner.root()))
            .expect("actual native graph");
        for related in [false, true] {
            let query = GraphNeighborhoodQuery::new(symbol_key(ALIAS), owner.root());
            let command = if related {
                Command::Related(query)
            } else {
                Command::Graph(query)
            };
            let decoded = certify_decode(&command, CommandReply::Graph(snapshot.clone()), &owner);
            let result = backend_library::admit_reply(&CommandDto::new(0x3200, command), &decoded);
            assert_eq!(
                result.is_ok(),
                mutation == 0,
                "mutation {mutation}, related {related}"
            );
            // A native selector still reads its actual row despite an unavailable
            // source address; source aliases cannot fabricate that selector.
            backend_library::admit_reply(
                &CommandDto::new(
                    0x3200,
                    Command::Graph(GraphNeighborhoodQuery::selected(native, owner.root())),
                ),
                &decoded,
            )
            .expect("native membership remains exact");
        }
    }
    assert!(CapturedSourceCoordinate::from_row(&row).is_some());
}

#[test]
fn certified_alias_graph_center_must_be_the_selected_native_declaration() {
    let (package, row, child) = fixture();
    let RowId::Symbol(native) = row.id else {
        panic!("symbol");
    };
    let child_id = child.id;
    let owner = build_owner(vec![package, row, child]);
    let library = Library::from_view(owner.clone(), backend_engine::Cursor::for_view_root(&owner))
        .expect("library");
    let mut snapshot = library
        .graph(GraphNeighborhoodQuery::selected(native, owner.root()))
        .expect("native graph");
    let revision = RichGraphRevision::new(
        snapshot.root.root().to_bytes(),
        SemanticGenerationId::new([41; 32]),
        [42; 32],
    );
    snapshot.rich_graph = Some(
        RichGraphSnapshot::from_view(&snapshot.root, RowId::Symbol(native), &[], revision)
            .expect("typed graph"),
    );
    let command = Command::Graph(GraphNeighborhoodQuery::new(symbol_key(ALIAS), owner.root()));
    let decoded = certify_decode(&command, CommandReply::Graph(snapshot.clone()), &owner);
    backend_library::admit_reply(&CommandDto::new(0x3200, command.clone()), &decoded)
        .expect("actual native center");
    snapshot.rich_graph = Some(
        RichGraphSnapshot::from_view(&snapshot.root, child_id, &[], revision)
            .expect("valid graph for the wrong center"),
    );
    let decoded = certify_decode(&command, CommandReply::Graph(snapshot), &owner);
    assert!(
        backend_library::admit_reply(&CommandDto::new(0x3200, command), &decoded).is_err(),
        "a matching neighbor cannot certify another center"
    );
}

#[test]
fn certified_graph_exact_label_wins_over_shadow_alias_without_borrowing_its_rich_center() {
    let (package, semantic, _) = fixture();
    let RowId::Symbol(semantic_native) = semantic.id else {
        panic!("semantic row");
    };
    let exact_native = symbol_key("exact-label-selected-compiler-native");
    assert_ne!(exact_native, symbol_key(ALIAS));
    let exact = Row::in_package(
        RowId::Symbol(exact_native),
        semantic.basis,
        package_key(&package.label),
        ALIAS,
    )
    .with_source(semantic.source.captured().expect("site").clone());
    let shadow = semantic.clone().with_parent(exact_native);
    for duplicate_exact in [false, true] {
        let mut rows = vec![package.clone(), exact.clone(), shadow.clone()];
        if duplicate_exact {
            let mut duplicate = exact.clone();
            duplicate.id = RowId::Symbol(symbol_key("duplicate-exact-label-native-row"));
            duplicate.parent = Some(exact_native);
            rows.push(duplicate);
        }
        let owner = build_owner(rows);
        let library =
            Library::from_view(owner.clone(), backend_engine::Cursor::for_view_root(&owner))
                .expect("library");
        let snapshot = library
            .graph(GraphNeighborhoodQuery::selected(exact_native, owner.root()))
            .expect("actual native whole graph");
        assert!(
            snapshot
                .root
                .row_ref(RowId::Symbol(symbol_key(ALIAS)))
                .is_none(),
            "canonical request must exercise label fallback without a native-key match"
        );
        assert!(
            snapshot
                .root
                .row_ref(RowId::Symbol(semantic_native))
                .is_some(),
            "shadow is a selected graph neighbor"
        );
        let resolution = backend_library::resolve_selected_view_address(&owner, ALIAS);
        if duplicate_exact {
            assert!(matches!(
                resolution,
                backend_library::SelectedViewAddress::Ambiguous
            ));
        } else {
            assert!(
                matches!(resolution, backend_library::SelectedViewAddress::Declaration {
                row, matched_as: backend_library::AddressMatchKind::ExactLabel,
            } if row.id == exact.id)
            );
        }
        for rich in [false, true] {
            let mut snapshot = snapshot.clone();
            let revision = RichGraphRevision::new(
                snapshot.root.root().to_bytes(),
                SemanticGenerationId::new([51; 32]),
                [52; 32],
            );
            if rich {
                snapshot.rich_graph = Some(
                    RichGraphSnapshot::from_view(&snapshot.root, exact.id, &[], revision)
                        .expect("exact center"),
                );
            }
            for related in [false, true] {
                let query = GraphNeighborhoodQuery::new(symbol_key(ALIAS), owner.root());
                let command = if related {
                    Command::Related(query)
                } else {
                    Command::Graph(query)
                };
                let decoded =
                    certify_decode(&command, CommandReply::Graph(snapshot.clone()), &owner);
                let admitted = backend_library::admit_reply(
                    &CommandDto::new(0x3200, command.clone()),
                    &decoded,
                );
                assert_eq!(
                    admitted.is_ok(),
                    !duplicate_exact,
                    "exact-label precedence and ambiguity: duplicate {duplicate_exact}, rich {rich}, related {related}"
                );
                let selected = GraphNeighborhoodQuery::selected(exact_native, owner.root());
                let selected_command = if related {
                    Command::Related(selected)
                } else {
                    Command::Graph(selected)
                };
                backend_library::admit_reply(&CommandDto::new(0x3200, selected_command), &decoded)
                    .expect("selected address uses only unique actual native membership even with duplicate labels");
                if rich {
                    let mut wrong_center = snapshot.clone();
                    wrong_center.rich_graph = Some(
                        RichGraphSnapshot::from_view(
                            &wrong_center.root,
                            RowId::Symbol(semantic_native),
                            &[],
                            revision,
                        )
                        .expect("valid graph with the shadow center"),
                    );
                    let decoded =
                        certify_decode(&command, CommandReply::Graph(wrong_center), &owner);
                    assert!(
                        backend_library::admit_reply(&CommandDto::new(0x3200, command), &decoded)
                            .is_err(),
                        "an exact label cannot borrow its shadow neighbor's rich center"
                    );
                }
            }
        }
    }
    let owner = build_owner(vec![package, semantic]);
    let library = Library::from_view(owner.clone(), backend_engine::Cursor::for_view_root(&owner))
        .expect("alias-only view");
    let snapshot = library
        .graph(GraphNeighborhoodQuery::selected(
            semantic_native,
            owner.root(),
        ))
        .expect("semantic graph");
    for related in [false, true] {
        let selected = GraphNeighborhoodQuery::selected(symbol_key(ALIAS), owner.root());
        let command = if related {
            Command::Related(selected)
        } else {
            Command::Graph(selected)
        };
        let decoded = certify_decode(&command, CommandReply::Graph(snapshot.clone()), &owner);
        assert!(
            backend_library::admit_reply(&CommandDto::new(0x3200, command), &decoded).is_err(),
            "a Selected address cannot use the canonical source-alias fallback"
        );
    }
}

#[test]
fn certified_graph_native_id_wins_over_qualified_shadow_labels() {
    let (package, mut row, mut shadow) = fixture();
    let requested = symbol_key(&shadow.label);
    row.id = RowId::Symbol(requested);
    shadow.parent = Some(requested);
    assert_ne!(row.label, shadow.label);
    assert_ne!(row.id, shadow.id);
    assert!(backend_library::PackageQualifiedDeclaration::from_row(&shadow).is_some());
    for duplicate_label in [false, true] {
        let mut rows = vec![package.clone(), row.clone(), shadow.clone()];
        if duplicate_label {
            let mut duplicate = shadow.clone();
            duplicate.id = RowId::Symbol(symbol_key("second-qualified-shadow-native"));
            rows.push(duplicate);
        }
        let owner = build_owner(rows);
        let library =
            Library::from_view(owner.clone(), backend_engine::Cursor::for_view_root(&owner))
                .expect("library");
        let snapshot = library
            .graph(GraphNeighborhoodQuery::selected(requested, owner.root()))
            .expect("native whole graph");
        assert!(snapshot.root.row_ref(shadow.id).is_some());
        assert!(snapshot.root.row_ref(package.id).is_none());
        for rich in [false, true] {
            let mut snapshot = snapshot.clone();
            let revision = RichGraphRevision::new(
                snapshot.root.root().to_bytes(),
                SemanticGenerationId::new([61; 32]),
                [62; 32],
            );
            if rich {
                snapshot.rich_graph = Some(
                    RichGraphSnapshot::from_view(&snapshot.root, row.id, &[], revision)
                        .expect("actual native center"),
                );
            }
            for related in [false, true] {
                let query = GraphNeighborhoodQuery::new(requested, owner.root());
                let command = if related {
                    Command::Related(query)
                } else {
                    Command::Graph(query)
                };
                let decoded =
                    certify_decode(&command, CommandReply::Graph(snapshot.clone()), &owner);
                backend_library::admit_reply(&CommandDto::new(0x3200, command.clone()), &decoded)
                    .expect("native ID wins even over duplicate qualified shadow labels");
                if rich {
                    let mut wrong_center = snapshot.clone();
                    wrong_center.rich_graph = Some(
                        RichGraphSnapshot::from_view(&wrong_center.root, shadow.id, &[], revision)
                            .expect("independently valid shadow-centered graph"),
                    );
                    let decoded =
                        certify_decode(&command, CommandReply::Graph(wrong_center), &owner);
                    assert!(
                        backend_library::admit_reply(&CommandDto::new(0x3200, command), &decoded)
                            .is_err(),
                        "a qualified shadow label cannot lend its rich center to the native ID"
                    );
                }
            }
        }
    }
}

#[test]
fn certified_graph_exact_label_requires_package_ownership_without_captured_source() {
    let (package, row, _) = fixture();
    for foreign_label in [false, true] {
        let mut row = row.clone();
        row.source = SourceAvailability::NotCaptured;
        if foreign_label {
            row.label = "pkg:foreign::semantic::identity::draw".to_owned();
        }
        let RowId::Symbol(native) = row.id else {
            panic!("native declaration");
        };
        let requested = symbol_key(&row.label);
        assert_ne!(native, requested, "must exercise exact-label fallback");
        assert!(CapturedSourceCoordinate::from_row(&row).is_none());
        assert_eq!(
            backend_library::PackageQualifiedDeclaration::from_row(&row).is_some(),
            !foreign_label
        );
        let owner = build_owner(vec![package.clone(), row.clone()]);
        assert_eq!(
            backend_library::resolve_selected_view_address(&owner, &row.label)
                .declaration()
                .expect("unambiguous label")
                .is_some(),
            !foreign_label,
            "full owner checks the actual package row"
        );
        let library =
            Library::from_view(owner.clone(), backend_engine::Cursor::for_view_root(&owner))
                .expect("library");
        let snapshot = library
            .graph(GraphNeighborhoodQuery::selected(native, owner.root()))
            .expect("actual native graph");
        assert!(snapshot.root.row_ref(package.id).is_none());
        for rich in [false, true] {
            let mut snapshot = snapshot.clone();
            if rich {
                let revision = RichGraphRevision::new(
                    snapshot.root.root().to_bytes(),
                    SemanticGenerationId::new([71; 32]),
                    [72; 32],
                );
                snapshot.rich_graph = Some(
                    RichGraphSnapshot::from_view(&snapshot.root, row.id, &[], revision)
                        .expect("actual center"),
                );
            }
            for related in [false, true] {
                let query = GraphNeighborhoodQuery::new(requested, owner.root());
                let command = if related {
                    Command::Related(query)
                } else {
                    Command::Graph(query)
                };
                let decoded =
                    certify_decode(&command, CommandReply::Graph(snapshot.clone()), &owner);
                assert_eq!(
                    backend_library::admit_reply(&CommandDto::new(0x3200, command), &decoded)
                        .is_ok(),
                    !foreign_label,
                    "exact label needs package ownership, not captured source: foreign {foreign_label}, rich {rich}, related {related}"
                );
            }
        }
    }
}

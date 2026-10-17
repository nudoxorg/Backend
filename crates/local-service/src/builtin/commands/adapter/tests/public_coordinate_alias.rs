//! Actual owner certificates and strict clients must retain the requested alias.

use super::*;
use backend_library::{
    CommandDto, CommandReply, DocumentQuery, GraphNeighborhoodQuery, PageRequest, PageTerminal,
    ProductText, QueryLimit, ReplyDto, Row, RowId, SourceLocation, SurfaceCommand, SurfaceReply,
    SymbolAddress, SymbolKey, symbol_key,
};

fn fixture() -> (AdapterFixture, SymbolKey, SymbolKey, String) {
    let mut fixture = AdapterFixture::new();
    let label = fixture.label.clone();
    let package = fixture.package;
    let basis = fixture.parts().1.engine().daemon().library().view().basis();
    let native = symbol_key("alias-actual-compiler-identity");
    let child = symbol_key("alias-neighbor-compiler-identity");
    fixture.publish_rows(vec![
        Row::in_package(
            RowId::Symbol(native),
            basis,
            package,
            format!("{label}::semantic::compiler-digest::draw"),
        )
        .with_source(SourceLocation::new("src/lib.rs", 9).expect("captured site")),
        Row::in_package(
            RowId::Symbol(child),
            basis,
            package,
            format!("{label}::semantic::neighbor-digest::neighbor"),
        )
        .with_source(SourceLocation::new("src/lib.rs", 17).expect("neighbor site"))
        .with_parent(native),
    ]);
    (
        fixture,
        native,
        child,
        format!("{label}::src/lib.rs:9::draw"),
    )
}

fn request_certificate(alias: &str, root: backend_library::ViewStateRoot) -> WireCertificate {
    symbol_coordinate_certificate(alias).with_claim(WireClaim::RootCommitment {
        schema: backend_library::WireSchema::ViewRelation,
        id: backend_library::encode_id(root.as_bytes()),
    })
}

fn serve(adapter: &mut CommandAdapter, daemon: &ProductDaemon, request: &CommandDto) -> ReplyDto {
    let certificate = request.certificate().cloned();
    let admitted = match request.command {
        Command::Graph(query) => adapter.graph(daemon, query, certificate, false),
        Command::Related(query) => adapter.graph(daemon, query, certificate, true),
        Command::GraphPage { symbol, page } => {
            adapter.graph_page(daemon, symbol, page, certificate)
        }
        _ => adapter.standard(daemon, &request.command, certificate),
    }
    .expect("actual command dispatch");
    let encoded = CommandAdapter::encode(daemon, request.request_id, admitted, None)
        .expect("real certified reply envelope");
    let decoded = ReplyDto::decode_with_certificate(
        &encoded,
        daemon.engine().daemon().library().view().capability(),
    )
    .expect("strict independent reply decoder");
    backend_library::admit_reply(request, &decoded)
        .expect("strict client admits original alias command");
    decoded
}

#[test]
fn public_alias_document_source_graph_related_and_every_page_survive_real_wire_admission() {
    let (mut fixture, native, neighbor, alias) = fixture();
    assert_ne!(symbol_key(&alias), native);
    let (adapter, daemon) = fixture.parts();
    let library = daemon.engine().daemon().library();
    let root = library.revision_root();
    let before = owner_cursor(daemon);
    let doc = DocumentQuery::new(symbol_key(&alias), root);
    let graph = GraphNeighborhoodQuery::new(symbol_key(&alias), root);
    for (index, command) in [
        Command::Document(doc),
        Command::Source(doc),
        Command::Graph(graph),
        Command::Related(graph),
    ]
    .into_iter()
    .enumerate()
    {
        let request = CommandDto::new(0x3100 + index as u64, command)
            .with_certificate(request_certificate(&alias, root));
        let request_wire = serde_json::to_vec(&request).expect("encode original copied address");
        let request = CommandDto::decode_for_owner(&request_wire, library.cursor())
            .expect("owner admits copied source address request");
        let decoded = serve(adapter, daemon, &request);
        match &decoded.reply {
            CommandReply::Document(document) => {
                assert_eq!(document.symbol, native);
                let selection = document
                    .selection
                    .as_ref()
                    .expect("exact requested selection");
                assert_eq!(selection.coordinate(), alias);
                assert_eq!(selection.symbol(), native);
                assert_eq!(selection.source().root, root);
                for foreign in [symbol_key("foreign"), neighbor] {
                    assert!(
                        backend_library::admit_reply(
                            &CommandDto::new(
                                request.request_id,
                                Command::Document(DocumentQuery::selected(foreign, root))
                            ),
                            &decoded
                        )
                        .is_err()
                    );
                }
                let mut wire = serde_json::to_value(&decoded).expect("actual document JSON");
                wire["reply"]["data"]["selection"]["coordinate"] =
                    serde_json::json!(format!("{alias}-foreign"));
                assert!(
                    ReplyDto::decode_with_certificate(
                        &serde_json::to_vec(&wire).expect("mutated tuple"),
                        library.view().capability()
                    )
                    .is_err(),
                    "unchanged owner commitment rejects alias substitution"
                );
            }
            CommandReply::Graph(snapshot) => {
                assert!(snapshot.root.row_ref(RowId::Symbol(native)).is_some());
                assert!(
                    snapshot
                        .root
                        .row_ref(RowId::Symbol(symbol_key(&alias)))
                        .is_none()
                );
                let foreign = GraphNeighborhoodQuery::new(symbol_key("foreign"), root);
                assert!(
                    backend_library::admit_reply(
                        &CommandDto::new(request.request_id, Command::Graph(foreign)),
                        &decoded
                    )
                    .is_err()
                );
            }
            other => panic!("alias selected an actual declaration: {other:?}"),
        }
    }

    let mut page = PageRequest::new(root, QueryLimit::new(1).expect("one row"));
    let mut collected = Vec::new();
    let mut page_certificate = request_certificate(&alias, root);
    for id in 0..2 {
        let request = CommandDto::new(
            0x3120 + id,
            Command::GraphPage {
                symbol: SymbolAddress::canonical(symbol_key(&alias)),
                page,
            },
        )
        .with_certificate(page_certificate.clone());
        let request = CommandDto::decode_for_owner(
            &serde_json::to_vec(&request).expect("page command wire"),
            library.cursor(),
        )
        .expect("owner admits original alias and continuation");
        let decoded = serve(adapter, daemon, &request);
        let CommandReply::ProjectionPage(result) = &decoded.reply else {
            panic!("alias graph page");
        };
        collected.extend(result.snapshot.root.row_refs().map(|row| row.id));
        assert_eq!(result.snapshot.root.row_count(), 1);
        let foreign = CommandDto::new(
            request.request_id,
            Command::GraphPage {
                symbol: SymbolAddress::canonical(symbol_key("foreign")),
                page,
            },
        );
        assert!(
            backend_library::admit_reply(&foreign, &decoded).is_err(),
            "recipe binds original alias on every page"
        );
        match result.terminal {
            PageTerminal::More(next) => {
                page = page.with_continuation(next);
                page_certificate = decoded
                    .certificate()
                    .expect("continuation certificate")
                    .clone();
            }
            PageTerminal::Complete => assert_eq!(id, 1),
            PageTerminal::Cancelled => panic!("uncancelled read"),
        }
    }
    collected.sort();
    let mut expected = vec![RowId::Symbol(native), RowId::Symbol(neighbor)];
    expected.sort();
    assert_eq!(collected, expected, "one page necessarily omits the center");
    assert_eq!(owner_cursor(daemon), before, "queries never admit work");
}

#[test]
fn public_alias_references_keep_original_target_through_selected_structural_catalog_boundary() {
    let (mut fixture, _, _, alias) = fixture();
    let (adapter, daemon) = fixture.parts();
    let command = SurfaceCommand::References {
        target: ProductText::new(alias.clone()).expect("alias"),
    };
    let admitted = adapter
        .surface(daemon, command.clone(), 0x3140)
        .expect("actual references alias");
    let bytes = CommandAdapter::encode(daemon, 0x3140, admitted, None).expect("references wire");
    let decoded = backend_library::decode_reply_body(&bytes).expect("references decoder");
    backend_library::admit_reply(
        &CommandDto::new(0x3140, Command::Surface(command)),
        &decoded,
    )
    .expect("references caller admission");
    let CommandReply::Surface(SurfaceReply::References { target, references }) = decoded.reply
    else {
        panic!("references alias remains useful");
    };
    assert_eq!(target.as_str(), alias);
    assert!(
        references.is_empty(),
        "fixture contains no call facts; completeness is not invented"
    );
}

#[test]
fn public_alias_requests_refuse_foreign_malformed_stale_and_duplicate_claims() {
    let (mut fixture, _, _, alias) = fixture();
    let (adapter, daemon) = fixture.parts();
    let library = daemon.engine().daemon().library();
    let root = library.revision_root();
    let before = owner_cursor(daemon);
    for coordinate in [
        alias.replace("src/lib.rs", "src/foreign.rs"),
        alias.replace(":9::", ":10::"),
        alias.replace(":9::", ":09::"),
        alias.replace("::draw", "::other"),
        "pkg:foreign::src/lib.rs:9::draw".to_owned(),
    ] {
        let doc = DocumentQuery::new(symbol_key(&coordinate), root);
        let graph = GraphNeighborhoodQuery::new(symbol_key(&coordinate), root);
        let page = PageRequest::new(root, QueryLimit::new(1).expect("credit"));
        for command in [
            Command::Document(doc),
            Command::Source(doc),
            Command::Graph(graph),
            Command::Related(graph),
            Command::GraphPage {
                symbol: SymbolAddress::canonical(symbol_key(&coordinate)),
                page,
            },
        ] {
            let request = CommandDto::new(0x3150, command)
                .with_certificate(request_certificate(&coordinate, root));
            let request = CommandDto::decode_for_owner(
                &serde_json::to_vec(&request).expect("foreign request"),
                library.cursor(),
            )
            .expect("well-formed request cannot mint view membership");
            let decoded = serve(adapter, daemon, &request);
            assert!(
                matches!(decoded.reply, CommandReply::Failed(_)),
                "{coordinate}"
            );
        }
    }
    let stale = backend_library::view_state_root(&[("foreign".to_owned(), "revision".to_owned())]);
    for command in [
        Command::Document(DocumentQuery::new(symbol_key(&alias), stale)),
        Command::Graph(GraphNeighborhoodQuery::new(symbol_key(&alias), stale)),
    ] {
        let request =
            CommandDto::new(0x3151, command).with_certificate(request_certificate(&alias, stale));
        let decoded = serve(adapter, daemon, &request);
        assert!(matches!(
            decoded.reply,
            CommandReply::Failed(backend_library::CommandFailure::WrongBasis { .. })
        ));
    }
    let request = CommandDto::new(
        0x3152,
        Command::Document(DocumentQuery::new(symbol_key(&alias), root)),
    )
    .with_certificate(request_certificate(&alias, root));
    let mut wire = serde_json::to_value(&request).expect("actual alias command");
    let claims = wire["certificate"]["claims"]
        .as_array_mut()
        .expect("claims");
    let duplicate = claims
        .iter()
        .find(|claim| claim["kind"] == "key")
        .expect("alias key claim")
        .clone();
    claims.push(duplicate);
    assert!(
        CommandDto::decode_for_owner(
            &serde_json::to_vec(&wire).expect("duplicate claim"),
            library.cursor()
        )
        .is_err()
    );
    assert_eq!(owner_cursor(daemon), before);
}

#[test]
fn selected_same_coordinate_roles_round_trip_documents_source_graph_and_pages() {
    use backend_library::{
        DeclarationKind, DeclarationRead, DeclarationSelector, NameQuery, SelectedSymbolId,
        SourceExcerpt,
    };
    let mut fixture = AdapterFixture::new();
    let label = format!("{}::box.ts:1::Box::constructor::item", fixture.label);
    let package = fixture.package;
    let basis = fixture.parts().1.engine().daemon().library().view().basis();
    let field = symbol_key("compiler parameter-property field");
    let variable = symbol_key("compiler parameter variable");
    fixture.publish_rows(
        [
            (field, DeclarationKind::Field),
            (variable, DeclarationKind::Variable),
        ]
        .into_iter()
        .map(|(key, kind)| {
            Row::in_package(RowId::Symbol(key), basis, package, label.clone())
                .with_kind(kind)
                .with_source(SourceLocation::new("box.ts", 1).expect("site"))
                .with_excerpt(SourceExcerpt::capture_bounded("item: number"))
        })
        .collect(),
    );
    let (adapter, daemon) = fixture.parts();
    let library = daemon.engine().daemon().library();
    let root = library.revision_root();
    let found = library
        .names(&NameQuery::new(
            "item",
            root,
            QueryLimit::new(200).expect("credit"),
        ))
        .expect("real name query");
    let selected = found
        .root
        .row_refs()
        .filter(|row| row.label == label)
        .map(|row| (row.id, row.kind))
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 2);
    assert!(
        backend_library::resolve_selected_document_row(library.view(), &label).is_err(),
        "coordinate alone stays ambiguous"
    );
    let before = owner_cursor(daemon);
    for (row_id, kind) in selected {
        let RowId::Symbol(key) = row_id else {
            panic!("symbol row")
        };
        let id = SelectedSymbolId::new(*key.as_bytes()).expect("actual row ID");
        for selector in [
            DeclarationSelector::Selected(id),
            DeclarationSelector::SelectedAt {
                symbol: id,
                coordinate: label.clone(),
            },
        ] {
            for read in [
                DeclarationRead::Document,
                DeclarationRead::Source,
                DeclarationRead::Graph,
                DeclarationRead::Related,
            ] {
                let query = DocumentQuery::addressed(selector.address(), root);
                let graph = GraphNeighborhoodQuery::addressed(selector.address(), root);
                let command = match read {
                    DeclarationRead::Document => Command::Document(query),
                    DeclarationRead::Source => Command::Source(query),
                    DeclarationRead::Graph => Command::Graph(graph),
                    DeclarationRead::Related => Command::Related(graph),
                };
                let mut certificate = WireCertificate::new()
                    .with_claim(WireClaim::RootCommitment {
                        schema: backend_library::WireSchema::ViewRelation,
                        id: backend_library::encode_id(root.as_bytes()),
                    })
                    .with_claim(WireClaim::KeyCommitment {
                        schema: backend_library::WireSchema::Symbol,
                        id: backend_library::encode_id(id.bytes()),
                    });
                if let Some(label) = selector.coordinate() {
                    certificate = certificate.with_claim(WireClaim::Key {
                        schema: backend_library::WireSchema::Symbol,
                        id: backend_library::encode_id(symbol_key(label).as_bytes()),
                        value: label.to_owned(),
                    });
                }
                let request =
                    CommandDto::new(0x3190, command).with_certificate(certificate.clone());
                let request = CommandDto::decode_for_owner(
                    &serde_json::to_vec(&request).expect("public operand request"),
                    library.cursor(),
                )
                .expect("strict selected request decoder");
                let response = serve(adapter, daemon, &request);
                match response.reply {
                    CommandReply::Document(document) => {
                        assert_eq!(document.symbol, key);
                        assert_eq!(
                            document
                                .selection
                                .as_ref()
                                .expect("authenticated selected label")
                                .coordinate(),
                            label
                        );
                        assert_eq!(document.excerpt.text(), Some("item: number"));
                    }
                    CommandReply::Graph(snapshot) => {
                        assert_eq!(
                            snapshot
                                .root
                                .row_ref(RowId::Symbol(key))
                                .expect("selected center")
                                .kind,
                            kind
                        );
                    }
                    other => panic!("selected {read:?}: {other:?}"),
                }
                // Optional coordinate is checked by the real owner, never used as fallback.
                let wrong = certificate.with_claim(WireClaim::Key {
                    schema: backend_library::WireSchema::Symbol,
                    id: backend_library::encode_id(symbol_key("wrong-coordinate").as_bytes()),
                    value: "wrong-coordinate".to_owned(),
                });
                let request =
                    CommandDto::new(0x3191, request.command.clone()).with_certificate(wrong);
                assert!(matches!(
                    serve(adapter, daemon, &request).reply,
                    CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(_))
                ));
            }
            let certificate = WireCertificate::new()
                .with_claim(WireClaim::RootCommitment {
                    schema: backend_library::WireSchema::ViewRelation,
                    id: backend_library::encode_id(root.as_bytes()),
                })
                .with_claim(WireClaim::KeyCommitment {
                    schema: backend_library::WireSchema::Symbol,
                    id: backend_library::encode_id(id.bytes()),
                });
            let request = CommandDto::new(
                0x3192,
                Command::GraphPage {
                    symbol: selector.address(),
                    page: PageRequest::new(root, QueryLimit::new(1).expect("credit")),
                },
            )
            .with_certificate(certificate);
            let result = serve(adapter, daemon, &request);
            assert!(
                matches!(result.reply, CommandReply::ProjectionPage(_)),
                "selected graph page"
            );
        }
    }
    assert_eq!(
        owner_cursor(daemon),
        before,
        "all reads leave the owner unchanged"
    );
}

#[test]
fn selected_public_requests_refuse_missing_rows_and_stale_roots_without_coordinate_fallback() {
    let (mut fixture, native, _, alias) = fixture();
    let (adapter, daemon) = fixture.parts();
    let library = daemon.engine().daemon().library();
    let current = library.revision_root();
    let stale = backend_library::view_state_root(&[("stale".to_owned(), "view".to_owned())]);
    for (id, root) in [
        (symbol_key("no-current-membership"), current),
        (native, stale),
    ] {
        let address = SymbolAddress::Selected(*id.as_bytes());
        let certificate = request_certificate(&alias, root).with_claim(WireClaim::KeyCommitment {
            schema: backend_library::WireSchema::Symbol,
            id: backend_library::encode_id(id.as_bytes()),
        });
        for command in [
            Command::Document(DocumentQuery::addressed(address, root)),
            Command::Source(DocumentQuery::addressed(address, root)),
            Command::Graph(GraphNeighborhoodQuery::addressed(address, root)),
            Command::Related(GraphNeighborhoodQuery::addressed(address, root)),
            Command::GraphPage {
                symbol: address,
                page: PageRequest::new(root, QueryLimit::new(1).expect("credit")),
            },
        ] {
            let request = CommandDto::new(0x3193, command).with_certificate(certificate.clone());
            assert!(
                matches!(
                    serve(adapter, daemon, &request).reply,
                    CommandReply::Failed(_)
                ),
                "no fallback to valid alias {alias}"
            );
        }
    }
}

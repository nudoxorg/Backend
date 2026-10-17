//! Combined route controls over real immutable Library projections and the
//! production Python grammar. This fixture grants no native compiler coverage
//! and launches no owner; actual installed CLI/MCP journeys remain separate.

use super::*;
use backend_library::{
    Command, DocumentQuery, DocumentSelection, GraphNeighborhoodQuery, Library, OutlineQuery,
    PageRequest, Query, QueryLimit, SymbolKey,
};

const SHELF: &str = "/abs/httpie-fidelity";
const NAME: &str = "HTTPieArgumentParser";
const HEADER: &str = "class HTTPieArgumentParser(BaseHTTPieArgumentParser):";
const SOURCE: &str = include_str!("../fixtures/httpie-signature/argparser.py");

struct RetainedProduct {
    library: Library,
    selected_documents: Vec<(String, SymbolKey)>,
    continuations: std::collections::BTreeMap<String, (PageContinuation, ViewStateRoot)>,
}

impl RetainedProduct {
    fn symbol(&self, coordinate: &str) -> Result<SymbolKey, ClientError> {
        let row = backend_library::resolve_selected_document_row(self.library.view(), coordinate)
            .map_err(ClientError::CommandFailed)?
            .ok_or(ClientError::CommandFailed(
                backend_library::CommandFailure::NotFound,
            ))?;
        let RowId::Symbol(symbol) = row.id else {
            panic!("the admitted address resolver returns only symbols");
        };
        Ok(symbol)
    }

    fn selected_row(
        &self,
        selector: &backend_library::DeclarationSelector,
    ) -> Result<&Row, ClientError> {
        let symbol =
            selector
                .address()
                .resolve(self.library.view())
                .ok_or(ClientError::CommandFailed(
                    backend_library::CommandFailure::NotFound,
                ))?;
        let row = self.library.view().row_ref(RowId::Symbol(symbol)).ok_or(
            ClientError::CommandFailed(backend_library::CommandFailure::NotFound),
        )?;
        if selector
            .coordinate()
            .is_some_and(|coordinate| coordinate != row.label)
        {
            return Err(ClientError::CommandFailed(
                backend_library::CommandFailure::InvalidQuery(
                    "selected coordinate mismatch".to_owned(),
                ),
            ));
        }
        Ok(row)
    }

    fn page(&self, limit: u16, continuation: Option<PageContinuation>) -> PageRequest {
        let page = PageRequest::new(
            self.library.revision_root(),
            QueryLimit::new(limit).expect("bounded route credit"),
        );
        continuation.map_or(page, |next| page.with_continuation(next))
    }
}

impl Engine for RetainedProduct {
    fn members_page(
        &mut self,
        document: &backend_library::Document,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let mut owner = DocumentQuery::selected(document.symbol, document.basis);
        if let Some(source) = document.source_basis() {
            owner = owner.with_source_basis(source);
        }
        let page = self
            .library
            .members_page(owner, self.page(limit, continuation))
            .map_err(|error| ClientError::CommandFailed(error.into()))?;
        Ok(ReplyDto::new(17, CommandReply::ProjectionPage(page)))
    }
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        Ok(self.library.revision_root())
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        Ok(HealthReport::from_root(
            self.library.view(),
            self.library.cursor(),
        ))
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        let revision = self.library.revision_root();
        let reply = match probe {
            Probe::Declaration { selector, read } => {
                use backend_library::DeclarationRead;
                let row = self.selected_row(selector)?;
                if matches!(read, DeclarationRead::Document | DeclarationRead::Source) {
                    let source = Basis {
                        root: revision,
                        ..self.library.view().basis()
                    };
                    let selection =
                        DocumentSelection::from_row(row, source).expect("actual selected row");
                    let document = self
                        .library
                        .document(
                            DocumentQuery::addressed(selector.address(), revision)
                                .with_source_basis(source),
                        )
                        .map_err(|error| ClientError::CommandFailed(error.into()))?
                        .with_selection(selection)
                        .map_err(|error| ClientError::Protocol(error.to_owned()))?;
                    let coordinate = row.label.clone();
                    self.selected_documents.push((coordinate, document.symbol));
                    return Ok(ReplyDto::new(17, CommandReply::Document(document)));
                }
                self.library
                    .graph(GraphNeighborhoodQuery::addressed(
                        selector.address(),
                        revision,
                    ))
                    .map(CommandReply::Graph)
            }
            Probe::Packages => self.library.execute(Command::Packages),
            Probe::Search { text, limit } => selected_search_page(
                &self.library,
                &Query::new(
                    text,
                    revision,
                    QueryLimit::new(limit).expect("bounded search"),
                ),
            )
            .map(CommandReply::Search),
            Probe::Names { text, limit } => self
                .library
                .names(&backend_library::NameQuery::new(
                    text,
                    revision,
                    QueryLimit::new(limit).expect("bounded names"),
                ))
                .map(CommandReply::Names),
            Probe::Document(coordinate) | Probe::Source(coordinate) => {
                let symbol = self.symbol(coordinate)?;
                let source = Basis {
                    root: revision,
                    ..self.library.view().basis()
                };
                let selection =
                    DocumentSelection::from_selected_view(self.library.view(), source, coordinate)
                        .expect("exact immutable row membership");
                let document = self
                    .library
                    .document(DocumentQuery::selected(symbol, revision).with_source_basis(source))
                    .map_err(|error| ClientError::CommandFailed(error.into()))?
                    .with_selection(selection)
                    .map_err(|error| ClientError::Protocol(error.to_owned()))?;
                self.selected_documents
                    .push((coordinate.to_owned(), document.symbol));
                return Ok(ReplyDto::new(17, CommandReply::Document(document)));
            }
            Probe::Graph(coordinate) | Probe::Related(coordinate) => self
                .library
                .graph(GraphNeighborhoodQuery::selected(
                    self.symbol(coordinate)?,
                    revision,
                ))
                .map(CommandReply::Graph),
            Probe::Outline(path) => self
                .library
                .outline(OutlineQuery::new(package_key(path), revision))
                .map(CommandReply::Outline),
            Probe::OutlinePage { path, limit } => self
                .library
                .outline_page(package_key(path), self.page(limit, None))
                .map(CommandReply::ProjectionPage),
            _ => {
                return Err(ClientError::CommandFailed(
                    backend_library::CommandFailure::MutationRequiresOwner,
                ));
            }
        }
        .map_err(|error| ClientError::CommandFailed(error.into()))?;
        Ok(ReplyDto::new(17, reply))
    }

    fn probe_page_window(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
        window: backend_library::QueryWindow,
    ) -> Result<ReplyDto, ClientError> {
        let cursor = continuation.map(PageContinuation::cursor);
        let reply = match probe {
            Probe::Search { text, limit } => {
                assert_eq!(limit, window.issued().get());
                let query = Query::new(text, self.library.revision_root(), window.issued())
                    .with_window(window)
                    .expect("search window");
                let query = if let Some(cursor) = cursor {
                    query.with_cursor(cursor)
                } else {
                    query
                };
                selected_search_page(&self.library, &query).map(CommandReply::Search)
            }
            Probe::Names { text, limit } => {
                assert_eq!(limit, window.issued().get());
                let query = backend_library::NameQuery::new(
                    text,
                    self.library.revision_root(),
                    window.issued(),
                )
                .with_window(window)
                .expect("names window");
                let query = if let Some(cursor) = cursor {
                    query.with_cursor(cursor)
                } else {
                    query
                };
                self.library.names(&query).map(CommandReply::Names)
            }
            _ => return self.probe(probe),
        }
        .map_err(|error| ClientError::CommandFailed(error.into()))?;
        Ok(ReplyDto::new(17, reply))
    }

    fn surface(&mut self, _command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        Err(ClientError::CommandFailed(
            backend_library::CommandFailure::MutationRequiresOwner,
        ))
    }
}

impl Product for RetainedProduct {
    fn encode_continuation(
        &mut self,
        continuation: PageContinuation,
    ) -> Result<String, ClientError> {
        // Retain only actual Library-issued cursors, using the same typed
        // cursor bytes and issuing-owner check as the shared client fixture.
        let token = fixture_page_cursor(continuation);
        self.continuations
            .insert(token.clone(), (continuation, self.library.revision_root()));
        Ok(token)
    }

    fn decode_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
        let (continuation, owner) = self
            .continuations
            .get(token)
            .ok_or_else(|| ClientError::Protocol("unissued retained cursor".to_owned()))?;
        if *owner != self.library.revision_root() || continuation.cursor().query_offset() == 0 {
            return Err(ClientError::StaleCursor);
        }
        Ok(*continuation)
    }

    fn graph_page(
        &mut self,
        coordinate: String,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let page = self
            .library
            .graph_page_for_address(
                self.symbol(&coordinate)?,
                backend_library::SymbolAddress::canonical(symbol_key(&coordinate)),
                self.page(limit, continuation),
            )
            .map_err(|error| ClientError::CommandFailed(error.into()))?;
        Ok(ReplyDto::new(17, CommandReply::ProjectionPage(page)))
    }

    fn graph_page_window(
        &mut self,
        coordinate: String,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let page = self
            .page(window.issued().get(), continuation)
            .with_window(window)
            .expect("graph window");
        let page = self
            .library
            .graph_page_for_address(
                self.symbol(&coordinate)?,
                backend_library::SymbolAddress::canonical(symbol_key(&coordinate)),
                page,
            )
            .map_err(|error| ClientError::CommandFailed(error.into()))?;
        Ok(ReplyDto::new(17, CommandReply::ProjectionPage(page)))
    }

    fn graph_page_declaration(
        &mut self,
        selector: backend_library::DeclarationSelector,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let RowId::Symbol(symbol) = self.selected_row(&selector)?.id else {
            panic!("selected symbol")
        };
        let page = self
            .page(window.issued().get(), continuation)
            .with_window(window)
            .expect("same credit");
        let page = self
            .library
            .graph_page_for_address(symbol, selector.address(), page)
            .map_err(|error| ClientError::CommandFailed(error.into()))?;
        Ok(ReplyDto::new(17, CommandReply::ProjectionPage(page)))
    }

    fn graph_query(
        &mut self,
        _input: AdmittedGraphQueryInput,
        _limit: u16,
        _continuation: Option<PageContinuation>,
    ) -> Result<GraphQueryPage, ClientError> {
        Err(ClientError::Protocol(
            "structured execution requires an actual owner".to_owned(),
        ))
    }
}

fn retained_library() -> Library {
    let frontend = backend_frontend_python::syntax_frontend().expect("production grammar");
    let analysis = frontend
        .analyze(
            std::path::Path::new("httpie/cli/argparser.py"),
            SOURCE.as_bytes(),
        )
        .expect("exact pinned HTTPie source");
    let declaration = analysis
        .declarations()
        .iter()
        .find(|row| row.name() == NAME)
        .expect("actual class definition");
    assert_eq!(declaration.line(), 138);
    assert_eq!(declaration.signature(), HEADER);
    assert!(
        declaration
            .documentation()
            .contains("Adds additional logic")
    );
    assert!(
        declaration
            .source_excerpt()
            .text()
            .expect("captured class body")
            .contains("def parse_args(")
    );
    assert_eq!(
        declaration.source_excerpt().extent(),
        Some(SourceExcerptExtent::Truncated)
    );

    let native = symbol_key("independent retained class fixture identity");
    let coordinate = format!("{SHELF}::semantic::{}::{NAME}", "ab".repeat(32));
    assert_ne!(native, symbol_key(&coordinate));
    let mut rows = vec![
        Row::new(RowId::Package(package_key(SHELF)), basis(), SHELF),
        Row::in_package(
            RowId::Symbol(native),
            basis(),
            package_key(SHELF),
            coordinate,
        )
        .with_kind(declaration.kind())
        .with_signature(declaration.signature())
        .with_source(declaration.location().clone())
        .with_excerpt(declaration.source_excerpt().clone())
        .with_document([Fragment::Text(declaration.documentation().to_owned())]),
        Row::in_package(
            RowId::Symbol(symbol_key("unknown retained type fixture")),
            basis(),
            package_key(SHELF),
            format!("{SHELF}::semantic::{}::{NAME}", "cd".repeat(32)),
        )
        .with_kind(DeclarationKind::Type)
        .with_signature("unknown(unannotated)"),
    ];
    for display in [NAME, "external semantic target"] {
        for index in 0..2 {
            let symbol = symbol_key(&format!("independent retained external {display}/{index}"));
            rows.push(Row::new(
                RowId::Symbol(symbol),
                basis(),
                format!(
                    "{SHELF}::external::{}::{display}",
                    RowId::Symbol(symbol).stable_key(),
                ),
            ));
        }
    }
    let view = root(rows);
    let cursor = backend_library::Cursor::for_view_root(&view);
    Library::from_view(view, cursor).expect("real immutable incomplete catalog")
}

fn server(library: Library) -> Server<RetainedProduct> {
    let mut server = Server::with_authority(
        RetainedProduct {
            library,
            selected_documents: Vec::new(),
            continuations: std::collections::BTreeMap::new(),
        },
        SHELF.to_owned(),
        [9; 32],
    );
    assert!(server.handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fidelity-control","version":"1"}}}"#).is_some());
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    server
}

fn invoke(server: &mut Server<RetainedProduct>, tool: &str, arguments: Value) -> Value {
    let request = json!({"jsonrpc":"2.0","id":"exact-fidelity-call","method":"tools/call",
        "params":{"name":tool,"arguments":arguments}});
    let reply = server
        .handle(&serde_json::to_vec(&request).expect("request bytes"))
        .expect("correlated response");
    let mut bytes = Vec::new();
    write_message(&mut bytes, &reply).expect("production whole-line serializer");
    let wire = assert_stdio_line_accounting(std::str::from_utf8(&bytes).expect("UTF-8 wire"));
    assert_eq!(wire["id"], request["id"]);
    assert!(wire.get("error").is_none(), "{wire}");
    wire["result"].clone()
}

#[test]
fn immutable_library_routes_keep_distinct_reusable_operands_headers_and_summary_intent() {
    let library = retained_library();
    let original = library.view().clone();
    for detail in ["summary", "standard", "full"] {
        let mut server = server(library.clone());
        for (query, count) in [(NAME, 4), ("external semantic target", 2)] {
            for tool in ["backend.search", "backend.resolve"] {
                let result = invoke(
                    &mut server,
                    tool,
                    json!({"query":query,"limit":200,"detail":detail}),
                );
                assert_eq!(result["isError"], false, "{result}");
                let records = result["structuredContent"]["records"]
                    .as_array()
                    .expect("selected records");
                assert_eq!(records.len(), count);
                let mut operands = std::collections::BTreeSet::new();
                let mut symbols = std::collections::BTreeSet::new();
                for record in records {
                    let coordinate = record["identity"]["coordinate"]
                        .as_str()
                        .expect("exact emitted address");
                    let symbol = server
                        .product
                        .symbol(coordinate)
                        .expect("address reuses actual membership");
                    if record["identity"]["shape"] == "external" {
                        let row = server
                            .product
                            .library
                            .view()
                            .row_ref(RowId::Symbol(symbol))
                            .expect("retained external target");
                        assert_eq!(row.package, None, "navigation is not local ownership");
                        assert_eq!(row.source, SourceAvailability::NotCaptured);
                    }
                    assert!(operands.insert(coordinate.to_owned()));
                    assert!(symbols.insert(symbol));
                    assert_eq!(
                        record["identity"]["semantic_data"]["value"],
                        json!(symbol.as_bytes())
                    );
                    let document = invoke(
                        &mut server,
                        "backend.document",
                        json!({"coordinate":coordinate,"detail":detail}),
                    );
                    assert_eq!(document["isError"], false, "{document}");
                    assert!(
                        document["structuredContent"].get("notes").is_none(),
                        "a valid external target is not a failed local member proof: {document}"
                    );
                    assert_eq!(
                        document["structuredContent"]["identity"]["coordinate"],
                        coordinate
                    );
                    assert_eq!(
                        document["structuredContent"]["identity"]["semantic_data"]["value"],
                        json!(symbol.as_bytes())
                    );
                    assert_eq!(
                        server.product.selected_documents.last(),
                        Some(&(coordinate.to_owned(), symbol))
                    );
                    if record["identity"].get("path").is_some() {
                        assert_eq!(document["structuredContent"]["signature"], HEADER);
                        assert!(text_of(&document).contains(HEADER));
                        if detail == "summary" {
                            assert!(document["structuredContent"].get("source").is_none());
                            assert!(!text_of(&document).contains("def parse_args("));
                        } else {
                            assert_eq!(
                                document["structuredContent"]["source"]["extent"],
                                "truncated"
                            );
                            assert!(text_of(&document).contains("def parse_args("));
                        }
                    } else {
                        assert_eq!(
                            document["structuredContent"]["source_fault"]["cause"],
                            "not-captured"
                        );
                        assert!(!text_of(&document).contains("not-found"));
                        if record["identity"]["shape"] == "external" {
                            assert!(document["structuredContent"].get("signature").is_none());
                        }
                    }
                    let graph = invoke(
                        &mut server,
                        "backend.graph",
                        json!({"coordinate":coordinate,"limit":200,"detail":detail}),
                    );
                    assert_eq!(graph["isError"], false, "{graph}");
                    assert_eq!(
                        graph["structuredContent"]["records"][0]["identity"]["coordinate"],
                        coordinate
                    );
                    assert_eq!(
                        graph["structuredContent"]["records"][0]["identity"]["semantic_data"]["value"],
                        json!(symbol.as_bytes())
                    );
                    assert_eq!(
                        graph["structuredContent"]["records"]
                            .as_array()
                            .expect("singleton graph")
                            .len(),
                        1
                    );
                    assert!(!text_of(&graph).contains("def parse_args("));
                    if detail == "summary" {
                        assert!(text_of(&graph).contains("structuredContent.records"));
                        assert!(
                            graph["structuredContent"]["records"][0]
                                .get("signature")
                                .is_none()
                        );
                    }
                }
                assert!(!text_of(&result).contains("def parse_args("));
                if detail == "summary" {
                    assert!(text_of(&result).contains("structuredContent.records"));
                    assert!(!text_of(&result).contains(HEADER));
                    assert!(records.iter().all(|row| row.get("signature").is_none()));
                }
            }
        }
        let refused = invoke(
            &mut server,
            "backend.document",
            json!({"coordinate":"external semantic target","detail":detail}),
        );
        assert_eq!(refused["isError"], true);
        assert_eq!(refused["structuredContent"]["slug"], "not-found");
        assert_eq!(
            refused["structuredContent"]["operand"],
            "external semantic target"
        );
        assert!(refused["structuredContent"].get("identity").is_none());
        assert_eq!(server.product.library.view(), &original);
    }
}

#[test]
fn actual_package_only_route_keeps_unknown_counts_after_successful_selected_reads() {
    let library = retained_library();
    let shelf = backend_present::shelf_from_root(
        library.view(),
        backend_present::KeyTag::from_key(library.revision_root().as_bytes()),
    );
    assert_eq!(
        shelf.entries()[0]
            .declarations()
            .expect("actual captured source evidence")
            .get(),
        1
    );
    for detail in ["summary", "standard", "full"] {
        let mut server = server(library.clone());
        let search = invoke(
            &mut server,
            "backend.search",
            json!({"query":NAME,"limit":200,"detail":detail}),
        );
        assert_eq!(search["isError"], false);
        assert_eq!(
            search["structuredContent"]["records"]
                .as_array()
                .expect("published fixture rows")
                .len(),
            4
        );
        let packages = invoke(&mut server, "backend.packages", json!({"detail":detail}));
        assert_eq!(packages["isError"], false);
        let projects = packages["structuredContent"]["projects"]
            .as_array()
            .expect("real package-only projection");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0]["identity"]["coordinate"], SHELF);
        assert_eq!(projects[0]["readiness"], "ready");
        assert!(
            projects[0]
                .get("declarations")
                .expect("explicit count availability")
                .is_null()
        );
        assert!(text_of(&packages).contains("declaration count unavailable"));
        assert!(!text_of(&packages).contains("0 declaration"));
    }
}

#[test]
fn selected_external_documents_do_not_claim_local_outline_membership() {
    let library = retained_library();
    let external = library
        .view()
        .rows()
        .iter()
        .filter(|row| matches!(row.id, RowId::Symbol(_)) && row.package.is_none())
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(external.len(), 4);
    let mut server = server(library);
    for row in external {
        let RowId::Symbol(symbol) = row.id else {
            panic!("external symbol")
        };
        for registered in [false, true] {
            let arguments = json!({"coordinate":row.label,
                "selected_symbol":{"kind":"selected-symbol-id","value":symbol.as_bytes()},"detail":"full"});
            let document = if registered {
                invoke(
                    &mut server,
                    "backend.call",
                    json!({"name":"backend.document","arguments":arguments}),
                )
            } else {
                invoke(&mut server, "backend.document", arguments)
            };
            assert_eq!(document["isError"], false, "{document}");
            let body = &document["structuredContent"];
            assert_eq!(body["identity"]["coordinate"], row.label);
            assert_eq!(
                body["identity"]["semantic_data"]["value"],
                json!(symbol.as_bytes())
            );
            assert_eq!(body["source_fault"]["cause"], "not-captured");
            for absent in ["notes", "members", "source", "signature"] {
                assert!(
                    body.get(absent).is_none(),
                    "no invented foreign fact: {body}"
                );
            }
            let source_arguments = json!({"coordinate":row.label,
                "selected_symbol":{"kind":"selected-symbol-id","value":symbol.as_bytes()},"detail":"full"});
            let source = if registered {
                invoke(
                    &mut server,
                    "backend.call",
                    json!({"name":"backend.source","arguments":source_arguments}),
                )
            } else {
                invoke(&mut server, "backend.source", source_arguments)
            };
            assert_eq!(
                source["isError"], true,
                "source remains unavailable: {source}"
            );
            assert_eq!(source["structuredContent"]["slug"], "source-unavailable");
            assert_eq!(source["structuredContent"]["cause"], "not-captured");
        }
    }
}

#[test]
fn returned_selected_ids_keep_same_coordinate_roles_distinct_on_named_and_registered_reads() {
    let coordinate = format!("{SHELF}::box.ts:1::Box::constructor::item");
    let mut rows = vec![Row::new(RowId::Package(package_key(SHELF)), basis(), SHELF)];
    for (key, kind) in [
        ("field identity", DeclarationKind::Field),
        ("variable identity", DeclarationKind::Variable),
    ] {
        rows.push(
            Row::in_package(
                RowId::Symbol(symbol_key(key)),
                basis(),
                package_key(SHELF),
                coordinate.clone(),
            )
            .with_kind(kind)
            .with_source(SourceLocation::new("box.ts", 1).expect("site"))
            .with_excerpt(SourceExcerpt::capture_bounded("item: number")),
        );
    }
    let view = root(rows);
    let library = Library::from_view(view.clone(), backend_library::Cursor::for_view_root(&view))
        .expect("immutable fixture");
    let mut server = server(library);
    let resolved = invoke(
        &mut server,
        "backend.resolve",
        json!({"query":"item","limit":200}),
    );
    assert_eq!(resolved["isError"], false, "{resolved}");
    let records = resolved["structuredContent"]["records"]
        .as_array()
        .expect("actual name rows");
    assert_eq!(records.len(), 2);
    let facets = records
        .iter()
        .map(|row| row["identity"]["semantic_data"].clone())
        .collect::<Vec<_>>();
    assert_ne!(facets[0], facets[1]);
    let ambiguous = invoke(
        &mut server,
        "backend.document",
        json!({"coordinate":coordinate}),
    );
    assert_eq!(ambiguous["isError"], true, "coordinate remains ambiguous");
    for (record, facet) in records.iter().zip(facets) {
        for (tool, answer) in [
            ("backend.document", "page"),
            ("backend.source", "page"),
            ("backend.graph", "records"),
            ("backend.related", "records"),
        ] {
            for named in [true, false] {
                for with_coordinate in [false, true] {
                    let mut arguments = json!({"selected_symbol":facet,"detail":"full"});
                    if with_coordinate {
                        arguments["coordinate"] = json!(coordinate);
                    }
                    let result = if named {
                        invoke(&mut server, tool, arguments)
                    } else {
                        invoke(
                            &mut server,
                            "backend.call",
                            json!({"name":tool,"arguments":arguments}),
                        )
                    };
                    assert_eq!(result["isError"], false, "{tool}: {result}");
                    assert_eq!(result["structuredContent"]["answer"], answer);
                    let identity = if answer == "page" {
                        &result["structuredContent"]["identity"]
                    } else {
                        &result["structuredContent"]["records"][0]["identity"]
                    };
                    assert_eq!(identity["semantic_data"], facet);
                    assert_eq!(identity["coordinate"], coordinate);
                    if answer == "records" {
                        assert_eq!(
                            result["structuredContent"]["records"][0]["kind"],
                            record["kind"]
                        );
                    }
                }
            }
            let mismatch = invoke(
                &mut server,
                tool,
                json!({"selected_symbol":facet,"coordinate":"/foreign::box.ts:1::item"}),
            );
            assert_eq!(mismatch["isError"], true, "{tool}: {mismatch}");
        }
    }
    for bad in [
        json!({"kind":"selected-symbol-id","value":vec![0;32]}),
        json!({"kind":"selected-symbol-id","value":vec![255;32]}),
        json!({"kind":"selected-symbol-id","value":vec![1;31]}),
        json!("display abbreviation"),
    ] {
        for tool in [
            "backend.document",
            "backend.source",
            "backend.graph",
            "backend.related",
        ] {
            let result = invoke(&mut server, tool, json!({"selected_symbol":bad}));
            assert_eq!(result["isError"], true, "{tool}: {result}");
        }
    }
}

#[test]
fn selected_graph_continuation_refuses_changed_id_or_coordinate_assertion() {
    let coordinate = format!("{SHELF}::box.ts:1::item");
    let center = symbol_key("selected graph center");
    let neighbor = symbol_key("selected graph neighbor");
    let rows = vec![
        Row::new(RowId::Package(package_key(SHELF)), basis(), SHELF),
        Row::in_package(
            RowId::Symbol(center),
            basis(),
            package_key(SHELF),
            coordinate.clone(),
        ),
        Row::in_package(
            RowId::Symbol(neighbor),
            basis(),
            package_key(SHELF),
            format!("{SHELF}::box.ts:2::neighbor"),
        )
        .with_parent(center),
    ];
    let view = root(rows);
    let library = Library::from_view(view.clone(), backend_library::Cursor::for_view_root(&view))
        .expect("real catalog");
    let mut server = server(library);
    let selector = json!({"kind":"selected-symbol-id","value":center.as_bytes()});
    let first = invoke(
        &mut server,
        "backend.graph",
        json!({"selected_symbol":selector,"coordinate":coordinate,"limit":1}),
    );
    assert_eq!(first["isError"], false, "{first}");
    let cursor = first["structuredContent"]["nextCursor"]
        .as_str()
        .expect("actual owner-issued successor");
    let (issued_token, (issued, _)) = server
        .product
        .continuations
        .iter()
        .next()
        .map(|(token, value)| (token.clone(), *value))
        .expect("actual retained cursor");
    assert!(issued.cursor().query_offset() > 0);
    assert_eq!(
        server
            .product
            .decode_continuation(&issued_token)
            .expect("issued bytes"),
        issued
    );
    let changed_token = format!(
        "{}{}",
        &issued_token[..issued_token.len() - 1],
        if issued_token.ends_with('0') {
            '1'
        } else {
            '0'
        },
    );
    for unissued in [
        String::new(),
        "pc1-not-hex".to_owned(),
        format!("{issued_token}00"),
        changed_token,
    ] {
        assert!(
            server.product.decode_continuation(&unissued).is_err(),
            "unissued/tampered cursor"
        );
    }
    for changed in [
        json!({"selected_symbol":{"kind":"selected-symbol-id","value":neighbor.as_bytes()},"coordinate":coordinate,"limit":1,"cursor":cursor}),
        json!({"selected_symbol":selector,"coordinate":format!("{coordinate}-wrong"),"limit":1,"cursor":cursor}),
        json!({"selected_symbol":selector,"limit":1,"cursor":cursor}),
    ] {
        let result = invoke(&mut server, "backend.graph", changed);
        assert_eq!(
            result["isError"], true,
            "changed selected cursor operand: {result}"
        );
    }
    let next = invoke(
        &mut server,
        "backend.graph",
        json!({"selected_symbol":selector,"coordinate":coordinate,"limit":1,"cursor":cursor}),
    );
    assert_eq!(next["isError"], false, "unchanged exact successor: {next}");
    assert_eq!(
        next["structuredContent"]["records"]
            .as_array()
            .expect("successor rows")
            .len(),
        1
    );
    assert_eq!(
        first["structuredContent"]["records"]
            .as_array()
            .expect("first rows")
            .len(),
        1
    );
    assert_ne!(
        first["structuredContent"]["records"][0]["identity"]["semantic_data"],
        next["structuredContent"]["records"][0]["identity"]["semantic_data"]
    );
    for expected in [center, neighbor] {
        assert!([&first, &next].iter().any(|page|
            page["structuredContent"]["records"][0]["identity"]["semantic_data"]["value"] == json!(expected.as_bytes())),
            "exact center and neighbor, never substituted rows");
    }
    server.product.library = retained_library();
    assert!(matches!(
        server.product.decode_continuation(&issued_token),
        Err(ClientError::StaleCursor)
    ));
}

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

    fn page(&self, limit: u16, continuation: Option<PageContinuation>) -> PageRequest {
        let page = PageRequest::new(
            self.library.revision_root(),
            QueryLimit::new(limit).expect("bounded route credit"),
        );
        continuation.map_or(page, |next| page.with_continuation(next))
    }
}

impl Engine for RetainedProduct {
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

    fn surface(&mut self, _command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        Err(ClientError::CommandFailed(
            backend_library::CommandFailure::MutationRequiresOwner,
        ))
    }
}

impl Product for RetainedProduct {
    fn encode_continuation(
        &mut self,
        _continuation: PageContinuation,
    ) -> Result<String, ClientError> {
        Err(ClientError::Protocol(
            "this finite fixture owes no continuation".to_owned(),
        ))
    }

    fn decode_continuation(&mut self, _token: &str) -> Result<PageContinuation, ClientError> {
        Err(ClientError::StaleCursor)
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
    retained_library_with(false)
}

/// The same catalog with a second, separately sited definition of the class
/// name, so the name is genuinely ambiguous.
fn ambiguous_library() -> Library {
    retained_library_with(true)
}

fn retained_library_with(second_definition: bool) -> Library {
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
    if second_definition {
        rows.push(
            Row::in_package(
                RowId::Symbol(symbol_key("second retained class fixture")),
                basis(),
                package_key(SHELF),
                format!("{SHELF}::semantic::{}::{NAME}", "ef".repeat(32)),
            )
            .with_kind(declaration.kind())
            .with_signature(HEADER)
            .with_source(
                backend_library::SourceLocation::new("httpie/legacy/argparser.py", 12)
                    .expect("one-based site"),
            )
            .with_excerpt(declaration.source_excerpt().clone())
            .with_document([Fragment::Text(declaration.documentation().to_owned())]),
        );
    }
    for display in [NAME, "external semantic target"] {
        for index in 0..2 {
            let symbol = symbol_key(&format!("independent retained external {display}/{index}"));
            // Built the way the view builder builds an external target: a
            // label and nothing else, not even a document.
            rows.push(
                Row::new(
                    RowId::Symbol(symbol),
                    basis(),
                    format!(
                        "{SHELF}::external::{}::{display}",
                        RowId::Symbol(symbol).stable_key(),
                    ),
                )
                .with_document(Box::<[Fragment]>::default()),
            );
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
        // Two external placeholders carry each name and nothing else: a name
        // resolution omits them and a search summarizes them in one line, so
        // neither lists them as records.
        for (query, count) in [(NAME, 2), ("external semantic target", 0)] {
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
                if tool == "backend.search" {
                    assert_eq!(
                        result["structuredContent"]["externals"]["count"], 2,
                        "{result}"
                    );
                    assert!(
                        text_of(&result).contains("+ 2 external members: "),
                        "{}",
                        text_of(&result)
                    );
                } else {
                    assert!(
                        result["structuredContent"].get("externals").is_none(),
                        "{result}"
                    );
                    assert!(
                        !text_of(&result).contains("external members"),
                        "{}",
                        text_of(&result)
                    );
                }
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
                    // A page with no record has no records to point at.
                    assert_eq!(
                        text_of(&result).contains("structuredContent.records"),
                        count > 0,
                        "{}",
                        text_of(&result)
                    );
                    assert!(!text_of(&result).contains(HEADER));
                    assert!(records.iter().all(|row| row.get("signature").is_none()));
                }
            }
        }
        // A placeholder is not listed, but it is still a row that reads: its
        // page names the same operand and explains that no source was kept.
        let placeholders = library
            .view()
            .row_refs()
            .filter(|row| backend_library::is_inert_external_placeholder(row))
            .map(|row| row.label.clone())
            .collect::<Vec<_>>();
        assert_eq!(placeholders.len(), 4);
        for coordinate in &placeholders {
            let document = invoke(
                &mut server,
                "backend.document",
                json!({"coordinate":coordinate,"detail":detail}),
            );
            assert_eq!(document["isError"], false, "{document}");
            assert_eq!(
                document["structuredContent"]["identity"]["coordinate"],
                coordinate.as_str()
            );
            assert_eq!(
                document["structuredContent"]["source_fault"]["cause"],
                "not-captured"
            );
            assert!(document["structuredContent"].get("signature").is_none());
            // Asked for by its own coordinate, the placeholder is the answer.
            let graph = invoke(
                &mut server,
                "backend.graph",
                json!({"coordinate":coordinate,"limit":200,"detail":detail}),
            );
            assert_eq!(graph["isError"], false, "{graph}");
            assert_eq!(
                graph["structuredContent"]["records"][0]["identity"]["coordinate"],
                coordinate.as_str()
            );
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
            2,
            "the two external placeholders named {NAME} are summarized, not listed"
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
fn a_name_one_sited_declaration_defines_is_that_declaration_whatever_type_rows_repeat_it() {
    // The class has a site; the unannotated type row of the same name has
    // none, and the external placeholders are not declarations.
    let library = retained_library();
    let mut server = server(library);
    let by_name = invoke(&mut server, "backend.document", json!({"coordinate":NAME}));
    assert_eq!(by_name["isError"], false, "{by_name}");
    let coordinate = format!("{SHELF}::semantic::{}::{NAME}", "ab".repeat(32));
    assert_eq!(
        by_name["structuredContent"]["identity"]["coordinate"],
        coordinate
    );
}

#[test]
fn a_name_two_declarations_share_is_refused_with_one_exact_call_per_candidate() {
    let library = ambiguous_library();
    for detail in ["summary", "standard", "full"] {
        let mut server = server(library.clone());
        for tool in [
            "backend.document",
            "backend.source",
            "backend.graph",
            "backend.references",
        ] {
            let refused = invoke(
                &mut server,
                tool,
                json!({"coordinate":NAME,"detail":detail}),
            );
            assert_eq!(refused["isError"], true, "{tool}: {refused}");
            let fault = &refused["structuredContent"];
            assert_eq!(fault["slug"], "ambiguous", "{tool}: {refused}");
            assert_eq!(fault["operand"], NAME);
            let candidates = fault["candidates"].as_array().expect("listed candidates");
            // The two sited classes carry the name; the unannotated type row
            // stands for a class's type and the external placeholders named the
            // same are not declarations, so neither is a candidate.
            assert_eq!(candidates.len(), 2, "{tool}: {fault}");
            let text = text_of(&refused);
            assert!(text.starts_with(&format!("✗ ambiguous `{NAME}`")), "{text}");
            for candidate in candidates {
                let coordinate = candidate["coordinate"].as_str().expect("exact coordinate");
                assert!(!coordinate.contains("::external::"), "{candidate}");
                assert!(text.contains(coordinate), "{text}");
                // The retry names the same tool with the exact coordinate; the
                // read-only fixture can run every retry except references,
                // which needs an owner.
                let call = &candidate["call"];
                assert_eq!(call["name"], tool, "{call}");
                assert_eq!(call["arguments"]["coordinate"], coordinate);
                if tool != "backend.references" {
                    let retried = invoke(&mut server, tool, call["arguments"].clone());
                    assert_eq!(retried["isError"], false, "{call}: {retried}");
                }
            }
        }
    }
}

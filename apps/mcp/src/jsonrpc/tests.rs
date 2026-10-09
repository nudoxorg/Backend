//! MCP surface tests.
//!
//! These cases assert *content*, never counts: the tool a registry row is
//! reachable as, the Markdown an agent reads, the exact coordinate it passes
//! back, the slug and the next tool call a refusal carries. A fixture engine
//! stands in for the daemon, so every assertion here is about what this surface
//! decides rather than about what a live index happens to hold.

#![allow(clippy::expect_used, clippy::naive_bytecount, clippy::panic)]

use super::tools::QUERY_TOOL;
use super::*;
use backend_library::{
    Basis, COMMANDS, CommandReply, CompileExecutionIntent, Coverage, DeclarationKind, Document,
    Fragment, Freshness, Frontier, IndexCancelReceipt, IndexCancelStatus, IndexJobObservation,
    IndexJobOutcome, IndexJobProgressEvent, IndexJobProgressKind, IndexJobStage, IndexJobTerminal,
    IndexJobTicket, IndexProgressPage, IndexSearchCursor, IndexSearchPage, IndexSearchResultCount,
    IndexStartResult, Intent, Lane, Outline, OutlineExtent, OutlineNode, PackageReference,
    ProjectionPage, Reason, Row, RowId, SourceAvailability, SourceExcerpt, SourceExcerptExtent,
    SourceLocation, SurfaceReply, ViewRoot, ViewSnapshot, object_version, package_key, symbol_key,
    view_key, view_state_root,
};
use backend_present::{domain_name, grammar_for};

const PROJECT: &str = "/abs/polyglot";
const MODULE: &str = "/abs/polyglot::src/lib.rs";
const DECLARATION: &str = "/abs/polyglot::src/lib.rs:2::ferris";
const MISSING: &str = "/abs/polyglot::src/lib.rs:999::nothing";

// ---------------------------------------------------------------------------
// the fixture engine
// ---------------------------------------------------------------------------

/// A fixture product with one project, one module, and one declaration.
#[derive(Default)]
struct Fake {
    /// When set, every probe reports the endpoint as unreachable.
    offline: bool,
    /// A registered project with no published declarations has no outline.
    empty_outline: bool,
    /// Keep genuine outline transport/proof failures distinct from absence.
    outline_error: Option<ClientError>,
    /// Exact source availability returned by the document/source boundary.
    document_override: Option<Document>,
    /// Optional graph rows used to exercise the complete-page admission cap.
    graph_rows: Option<Box<[GraphQueryRow]>>,
    /// Identity-free typed failed reply from the direct graph-page route.
    graph_failure: Option<backend_library::CommandFailure>,
    /// Typed failure from the generic graph query route.
    graph_query_error: Option<ClientError>,
    /// Make the graph fixture return one authenticated continuation page.
    graph_continue: bool,
    /// Owner-issued continuation retained by the fixture encoder.
    next_continuation: Option<PageContinuation>,
    /// Make an otherwise valid owner cursor stale at decode time.
    stale_cursor: bool,
    /// Inject a typed decode failure only after an actual catalog cursor was issued.
    continuation_error: Option<ClientError>,
    /// Optional product reply used by the high-fanout surface budget case.
    surface_reply: Option<SurfaceReply>,
    /// Typed client failure returned by the surface boundary.
    surface_error: Option<ClientError>,
    /// Typed failure at graph-page, prompt-revision, or continuation-encoding admission.
    adapter_error: Option<ClientError>,
    /// Exact boundary reached by an adapter-error route.
    adapter_boundary: Option<&'static str>,
    /// Exercise the opaque owner cursor family behind `backend.surface`.
    surface_index_search_pages: bool,
    /// Override the opaque owner cursor for size-boundary cases.
    surface_index_search_owner_cursor: Option<String>,
    /// Make the owner reject an index-search cursor as stale.
    surface_index_search_stale: bool,
    /// Owner cursors that reached the durable index-search surface.
    surface_index_search_seen: Vec<Option<String>>,
    /// Exact typed surface commands reaching the owner boundary.
    surface_commands: Vec<SurfaceCommand>,
    /// Number of graph requests that reached the product boundary.
    graph_query_calls: usize,
    /// Exact index operands that reached the owner admission boundary.
    index_paths: Vec<String>,
    /// Number of command probes that reached the product boundary.
    probe_calls: usize,
    /// Real catalog projection used to exercise owner-issued page contracts.
    page_catalog: Option<backend_library::Library>,
    /// Each cursor keeps its actual selected owner root at issuance.
    catalog_cursors: BTreeMap<String, (PageContinuation, ViewStateRoot)>,
    /// Actual page bounds admitted by the catalog adapter.
    page_limits: Vec<u16>,
}

fn basis() -> Basis {
    Basis::new(view_state_root(&[]), object_version(b"source"))
}

fn index_job_ticket() -> IndexJobTicket {
    IndexJobTicket::new(
        std::num::NonZeroU64::new(17).expect("nonzero ticket"),
        [7; 16],
        PackageReference::parse("pkg:cargo/serde@1.0.228").expect("pinned package"),
    )
}

fn ticket_value(ticket: &IndexJobTicket) -> Value {
    serde_json::to_value(ticket).expect("owner ticket serializes")
}

fn root(rows: Vec<Row>) -> ViewRoot {
    let basis = basis();
    ViewRoot::new_incomplete(
        view_key(b"mcp-fixture"),
        basis,
        Frontier::new(
            basis.branch,
            basis.log,
            basis.schema,
            view_state_root(&[]),
            0,
        ),
        rows,
        vec![Coverage::Unavailable {
            lane: Lane::Semantic,
            reason: Reason::Unconfigured,
        }],
    )
    .expect("fixture view root")
}

fn snapshot(rows: Vec<Row>) -> ViewSnapshot {
    ViewSnapshot {
        root: root(rows),
        freshness: Freshness::Current,
        next: None,
        graph_relations: None,
        rich_graph: None,
    }
}

fn package_row() -> Row {
    Row::new(RowId::Package(package_key(PROJECT)), basis(), PROJECT)
}

fn module_row() -> Row {
    Row::in_package(
        RowId::Symbol(symbol_key(MODULE)),
        basis(),
        package_key(PROJECT),
        MODULE,
    )
    .with_kind(DeclarationKind::Module)
}

fn declaration_row() -> Row {
    Row::in_package(
        RowId::Symbol(symbol_key(DECLARATION)),
        basis(),
        package_key(PROJECT),
        DECLARATION,
    )
    .with_parent(symbol_key(MODULE))
    .with_kind(DeclarationKind::Function)
    .with_signature("pub fn ferris() -> Beacon")
    .with_document([Fragment::Text("Lights the beacon.".to_owned())])
    .with_source(SourceLocation::new("src/lib.rs", 2).expect("one-based location"))
    .with_excerpt(
        SourceExcerpt::captured(
            "pub fn ferris() -> Beacon {\n    Beacon\n}",
            SourceExcerptExtent::Complete,
        )
        .expect("bounded excerpt"),
    )
}

fn document() -> Document {
    let mut document = Document::new(
        symbol_key(MODULE),
        view_state_root(&[]),
        [Fragment::Text("Lights the beacon.".to_owned())],
    )
    .with_location(SourceAvailability::Captured(
        SourceLocation::new("src/lib.rs", 2).expect("one-based location"),
    ))
    .with_excerpt(
        SourceExcerpt::captured(
            "pub fn ferris() -> Beacon {\n    Beacon\n}",
            SourceExcerptExtent::Complete,
        )
        .expect("bounded excerpt"),
    );
    document.symbol = symbol_key(MODULE);
    document.signature = Some("pub mod lib".to_owned());
    document
}

fn unreachable() -> ClientError {
    ClientError::Io("no such file or directory".to_owned())
}

impl Engine for Fake {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.adapter_boundary = Some("revision");
        if let Some(error) = self.adapter_error.take() {
            return Err(error);
        }
        Ok(view_state_root(&[]))
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        let library = backend_library::Library::new();
        Ok(HealthReport::from_root(library.view(), library.cursor()))
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        self.probe_calls += 1;
        if let Probe::Index(path) | Probe::IndexWithExecutionIntent { path, .. } = probe {
            self.index_paths.push(path.to_owned());
        }
        if self.offline {
            return Err(unreachable());
        }
        let reply = match probe {
            Probe::Packages => CommandReply::Packages(snapshot(vec![
                package_row(),
                module_row(),
                declaration_row(),
            ])),
            Probe::Index(_) => {
                CommandReply::Added(Intent::request_package(package_key(PROJECT)).id())
            }
            Probe::IndexWithExecutionIntent { .. } => {
                CommandReply::Added(Intent::request_package(package_key(PROJECT)).id())
            }
            Probe::Remove(_) => {
                CommandReply::Removed(Intent::remove_package(package_key(PROJECT)).id())
            }
            Probe::Document(at) | Probe::Source(at) => {
                if at == MISSING {
                    return Err(ClientError::CommandFailed(
                        backend_library::CommandFailure::NotFound,
                    ));
                }
                CommandReply::Document(self.document_override.clone().unwrap_or_else(document))
            }
            Probe::Related(at) | Probe::Graph(at) => {
                if at == MISSING {
                    return Err(ClientError::CommandFailed(
                        backend_library::CommandFailure::NotFound,
                    ));
                }
                CommandReply::Graph(snapshot(vec![declaration_row()]))
            }
            Probe::Search { .. } => CommandReply::Search(snapshot(vec![declaration_row()])),
            Probe::Names { .. } => CommandReply::Names(snapshot(vec![declaration_row()])),
            Probe::Outline(_) => {
                if let Some(error) = self.outline_error.take() {
                    return Err(error);
                }
                if self.empty_outline {
                    return Err(ClientError::CommandFailed(
                        backend_library::CommandFailure::NotFound,
                    ));
                }
                CommandReply::Outline(
                    Outline::new(
                        package_key(PROJECT),
                        view_state_root(&[]),
                        OutlineNode {
                            symbol: symbol_key(MODULE),
                            children: vec![OutlineNode {
                                symbol: symbol_key(DECLARATION),
                                children: Box::new([]),
                            }]
                            .into_boxed_slice(),
                        },
                    )
                    .with_extent(OutlineExtent::Complete),
                )
            }
            Probe::OutlinePage { .. } => CommandReply::ProjectionPage(ProjectionPage {
                snapshot: snapshot(vec![module_row(), declaration_row()]),
                terminal: PageTerminal::Complete,
            }),
        };
        Ok(ReplyDto::new(1, reply))
    }

    fn probe_page(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let Some(catalog) = &self.page_catalog else {
            if continuation.is_some() {
                return Err(ClientError::StaleCursor);
            }
            return self.probe(probe);
        };
        let (text, limit, names) = match probe {
            Probe::Search { text, limit } => (text, limit, false),
            Probe::Names { text, limit } => (text, limit, true),
            _ => return self.probe(probe),
        };
        self.page_limits.push(limit);
        let credit = backend_library::QueryLimit::new(limit).expect("admitted limit");
        let reply = if names {
            let mut query = backend_library::NameQuery::new(text, catalog.revision_root(), credit);
            if let Some(cursor) = continuation {
                query = query.with_cursor(cursor.cursor());
            }
            catalog.names(&query).map(CommandReply::Names)
        } else {
            let mut query = backend_library::Query::new(text, catalog.revision_root(), credit);
            if let Some(cursor) = continuation {
                query = query.with_cursor(cursor.cursor());
            }
            selected_search_page(catalog, &query).map(CommandReply::Search)
        }
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok(ReplyDto::new(1, reply))
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        self.surface_commands.push(command.clone());
        if let Some(error) = self.surface_error.take() {
            return Err(error);
        }
        if let Some(reply) = self.surface_reply.take() {
            return Ok(reply);
        }
        match command {
            SurfaceCommand::IndexStart { package, .. } => {
                self.index_paths.push(package.as_str().to_owned());
                if self.offline {
                    return Err(unreachable());
                }
                Ok(SurfaceReply::IndexStarted(IndexStartResult::Started {
                    ticket: IndexJobTicket::new(std::num::NonZeroU64::MIN, [7; 16], package),
                    stage: backend_library::IndexJobStage::Scanning,
                }))
            }
            SurfaceCommand::IndexSearch { cursor, .. } if self.surface_index_search_pages => {
                self.surface_index_search_seen
                    .push(cursor.as_ref().map(|cursor| cursor.as_str().to_owned()));
                if cursor.is_some() && self.surface_index_search_stale {
                    return Err(ClientError::StaleCursor);
                }
                let expected_cursor = self
                    .surface_index_search_owner_cursor
                    .as_deref()
                    .unwrap_or("maven-owner-v4");
                if let Some(cursor) = &cursor
                    && cursor.as_str() != expected_cursor
                {
                    return Err(ClientError::Protocol(
                        "fixture received a non-owner index-search cursor".to_owned(),
                    ));
                }
                let next_cursor = cursor.is_none().then(|| {
                    IndexSearchCursor::new(expected_cursor.to_owned()).expect("owner cursor")
                });
                Ok(SurfaceReply::IndexSearchPage(IndexSearchPage {
                    snapshot: [42; 32],
                    evaluated_at_millis: 17,
                    hits: Box::new([]),
                    next_cursor,
                    result_count: IndexSearchResultCount::AtLeast(1),
                }))
            }
            SurfaceCommand::Subscriptions => Ok(SurfaceReply::Subscriptions(Box::new([]))),
            SurfaceCommand::TreeOpen {
                subject,
                parent,
                title,
                opener,
            } => Ok(SurfaceReply::TreeOpened(backend_library::TreeNodeRecord {
                id: backend_library::TreeNodeId::new(std::num::NonZeroU64::MIN),
                parent,
                subject,
                title: title.unwrap_or_else(|| ProductText::new("fixture node").expect("title")),
                opener,
                active: true,
            })),
            SurfaceCommand::References { target } => Ok(SurfaceReply::References {
                target,
                references: Box::new([backend_library::ReferenceRecord {
                    site: backend_library::ProductText::new("pkg::semantic::caller")
                        .expect("site text"),
                    target: backend_library::SemanticLinkTarget::Local {
                        declaration: backend_library::SemanticDeclarationIdentity {
                            family: [1; 16],
                            variant: [2; 16],
                        },
                    },
                    relation: backend_library::SemanticLinkKind::Calls,
                    evidence: backend_library::SemanticLinkEvidence {
                        confidence: backend_library::SemanticConfidence::Compiler,
                        source: Some(backend_library::SemanticSourceSpan {
                            file: backend_library::ProductText::new("src/main.rs")
                                .expect("path text"),
                            start: 40,
                            end: 46,
                        }),
                    },
                }]),
            }),
            SurfaceCommand::Diff { .. } => Err(ClientError::CommandFailed(
                backend_library::CommandFailure::InvalidQuery(
                    "the package is not indexed at this revision".to_owned(),
                ),
            )),
            other => Err(ClientError::Protocol(format!(
                "fixture has no reply for {:?}",
                other.id()
            ))),
        }
    }
}

impl Product for Fake {
    fn encode_continuation(
        &mut self,
        continuation: backend_library::PageContinuation,
    ) -> Result<String, ClientError> {
        self.adapter_boundary = Some("encode_continuation");
        if let Some(error) = self.adapter_error.take() {
            return Err(error);
        }
        if let Some(catalog) = &self.page_catalog {
            let token = format!("catalog-page-{}", continuation.cursor().query_offset());
            self.catalog_cursors
                .insert(token.clone(), (continuation, catalog.revision_root()));
            return Ok(token);
        }
        if self.next_continuation == Some(continuation) {
            Ok("fixture-page-1".to_owned())
        } else {
            Err(ClientError::Protocol("unknown fixture cursor".to_owned()))
        }
    }

    fn graph_page(
        &mut self,
        coordinate: String,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        if let Some(failure) = &self.graph_failure {
            return Ok(ReplyDto::new(1, CommandReply::Failed(failure.clone())));
        }

        self.adapter_boundary = Some("graph_page");
        if let Some(error) = self.adapter_error.take() {
            return Err(error);
        }
        if let Some(catalog) = &self.page_catalog {
            self.page_limits.push(limit);
            let mut request = backend_library::PageRequest::new(
                catalog.revision_root(),
                backend_library::QueryLimit::new(limit).expect("admitted limit"),
            );
            if let Some(cursor) = continuation {
                request = request.with_continuation(cursor);
            }
            let page = catalog
                .graph_page(symbol_key(&coordinate), request)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            return Ok(ReplyDto::new(1, CommandReply::ProjectionPage(page)));
        }
        if coordinate == MISSING {
            return Err(ClientError::CommandFailed(
                backend_library::CommandFailure::NotFound,
            ));
        }
        Ok(ReplyDto::new(
            1,
            CommandReply::ProjectionPage(ProjectionPage {
                snapshot: snapshot(vec![declaration_row()]),
                terminal: PageTerminal::Complete,
            }),
        ))
    }

    fn decode_continuation(
        &mut self,
        token: &str,
    ) -> Result<backend_library::PageContinuation, ClientError> {
        if let Some(error) = &self.continuation_error {
            return Err(error.clone());
        }
        if let Some(catalog) = &self.page_catalog {
            let (cursor, root) = self
                .catalog_cursors
                .get(token)
                .ok_or_else(|| ClientError::Protocol("unknown catalog cursor".to_owned()))?;
            if *root != catalog.revision_root() {
                return Err(ClientError::StaleCursor);
            }
            return Ok(*cursor);
        }
        if token == "fixture-page-1" {
            if self.stale_cursor {
                return Err(ClientError::StaleCursor);
            }
            self.next_continuation
                .ok_or_else(|| ClientError::Protocol("fixture has no cursor".to_owned()))
        } else {
            Err(ClientError::Protocol("unknown fixture cursor".to_owned()))
        }
    }

    fn graph_query(
        &mut self,
        _: AdmittedGraphQueryInput,
        _: u16,
        continuation: Option<backend_library::PageContinuation>,
    ) -> Result<GraphQueryPage, ClientError> {
        if let Some(error) = &self.graph_query_error {
            return Err(error.clone());
        }

        self.graph_query_calls += 1;
        if let Some(rows) = &self.graph_rows {
            return Ok(GraphQueryPage {
                revision: view_state_root(&[]).into(),
                source: basis().object,
                rows: rows.clone(),
                terminal: PageTerminal::Complete,
            });
        }
        if self.graph_continue {
            if continuation.is_none() {
                let continuation = PageContinuation::from_cursor(backend_library::Cursor::at(
                    view_state_root(&[]),
                    1,
                ));
                self.next_continuation = Some(continuation);
                return Ok(GraphQueryPage {
                    revision: view_state_root(&[]).into(),
                    source: basis().object,
                    rows: Box::new([]),
                    terminal: PageTerminal::More(continuation),
                });
            }
            assert_eq!(continuation, self.next_continuation);
        }
        Ok(GraphQueryPage {
            revision: view_state_root(&[]).into(),
            source: basis().object,
            rows: Box::new([]),
            terminal: PageTerminal::Complete,
        })
    }
}

// ---------------------------------------------------------------------------
// driving the server
// ---------------------------------------------------------------------------
fn ready(product: Fake) -> Server<Fake> {
    let mut server = Server::with_authority(product, PROJECT.to_owned(), [9; 32]);
    server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .expect("initialize response");
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    server
}

fn request(server: &mut Server<Fake>, method: &str, params: &Value) -> Value {
    let message = json!({ "jsonrpc": "2.0", "id": 9, "method": method, "params": params });
    server
        .handle(
            serde_json::to_vec(&message)
                .expect("encode request")
                .as_slice(),
        )
        .map(|reply| encoded_stdio_reply(reply))
        .expect("a request receives a response")
}

fn encoded_stdio_reply(reply: Value) -> Value {
    let mut line = Vec::new();
    write_message(&mut line, &reply).expect("production stdio encoder");
    serde_json::from_slice(&line).expect("encoded stdio response")
}

fn call(server: &mut Server<Fake>, tool: &str, arguments: &Value) -> Value {
    request(
        server,
        "tools/call",
        &json!({ "name": tool, "arguments": arguments }),
    )["result"]
        .clone()
}

#[test]
fn semantic_shapes_actual_jsonrpc_preserves_full_view_in_summary_and_full() {
    let operands: Value = serde_json::from_str(include_str!(
        "../../../../crates/library/fixtures/semantic-shape-read.json"
    ))
    .expect("shared operands fixture");
    for detail in ["summary", "full"] {
        let selected: backend_library::SemanticShapeReadRequest =
            serde_json::from_value(operands.clone()).expect("request");
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticVersions(Box::new([selected
                .source()
                .clone()]))),
            ..Fake::default()
        });
        let resolved = call(
            &mut server,
            "backend.resolve",
            &json!({"query":"ferris","detail":detail}),
        );
        let selector =
            resolved["structuredContent"]["records"][0]["identity"]["semantic_data"].clone();
        assert_eq!(selector["kind"], "selected-symbol-id");
        assert_eq!(selector["value"], json!(symbol_key(DECLARATION).as_bytes()));
        let versions = call(
            &mut server,
            "backend.semantic_versions",
            &json!({"package":"/abs/shape-fixture","detail":detail}),
        );
        let source = versions["structuredContent"]["semantic_data"]["value"][0].clone();
        assert_eq!(source, operands["source"]);
        let mut shape_operands = operands.clone();
        shape_operands["source"] = source;
        shape_operands["symbols"] = json!([selector["value"]]);
        let mut export_view: Value = serde_json::from_str(include_str!(
            "../../../../crates/library/fixtures/semantic-shape-egress-view.json"
        ))
        .expect("untrusted unavailable display fixture");
        export_view["batch"]["entries"][0]["symbol"]["id"] = json!(backend_library::encode_id(
            symbol_key(DECLARATION).as_bytes()
        ));
        let export: backend_library::SemanticShapeExport = serde_json::from_value(export_view)
            .expect("closed display selector from actual resolve output");
        server.product.surface_reply = Some(SurfaceReply::SemanticShapes(export.clone()));
        let result = call(
            &mut server,
            "backend.semantic_shapes",
            &json!({"request":shape_operands,"detail":detail}),
        );
        assert_ne!(result["isError"], true, "{result}");
        assert_eq!(
            result["structuredContent"]["semantic_data"]["value"],
            serde_json::to_value(&export).expect("view")
        );
        assert_eq!(server.product.surface_commands.len(), 2);
        assert!(
            matches!(&server.product.surface_commands[1], SurfaceCommand::SemanticShapes { request }
            if request.symbols() == &[*symbol_key(DECLARATION).as_bytes()])
        );
    }
    let mut server = ready(Fake::default());
    let mut wrong = operands.clone();
    wrong["symbols"] = json!([{"kind":"canonical","id":"06".repeat(32)}]);
    let result = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.semantic_shapes","arguments":{"request":wrong}}),
    );
    assert_eq!(result["result"]["isError"], true, "{result}");
    assert_eq!(result["result"]["structuredContent"]["slug"], "usage");
    assert_eq!(result["result"]["structuredContent"]["cause"], "malformed");
    assert!(server.product.surface_commands.is_empty());
}

#[test]
fn semantic_versions_jsonrpc_preserves_captured_semver_and_matches_the_cli_projection() {
    let packet: Value = serde_json::from_str(include_str!(
        "../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("complete captured public source packet");
    let reply: SurfaceReply = serde_json::from_value(packet["surface"].clone())
        .expect("complete source DTO; no wire certificate is fabricated");
    let answer = Answer::Product(Box::new(backend_present::product_view(&reply)));
    let package = packet["surface"]["data"][0]["package"]["value"]
        .as_str()
        .expect("exact captured local package");
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let mut server = ready(Fake {
            surface_reply: Some(reply.clone()),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.semantic_versions","arguments":{"package":package,"detail":detail.name()}}),
        );
        assert_context_bounded(&response);
        let result = &response["result"];
        assert_eq!(result["isError"], false, "{response}");
        assert_eq!(
            result["structuredContent"]["semantic_data"]["value"],
            packet["surface"]["data"]
        );
        assert!(
            result["structuredContent"]["records"][0]["history_status"]["proof"]
                .get("images")
                .is_none()
        );
        // The CLI adapter regression compares its actual JSON bytes to this
        // same production encoder and its Markdown to this shared renderer.
        let cli_projection = backend_present::encode_answer(
            &answer,
            detail,
            None,
            backend_present::DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("shared CLI/MCP product projection");
        let cli_projection: Value =
            serde_json::from_slice(&cli_projection.bytes).expect("CLI JSON");
        assert_eq!(result["structuredContent"], cli_projection);
        assert_eq!(
            text_of(result),
            backend_present::bounded_text(&backend_present::markdown::answer(&answer))
        );
        assert!(
            matches!(&server.product.surface_commands[0], SurfaceCommand::SemanticVersions { package: observed } if observed.as_str() == package)
        );
    }
}

#[test]
fn semantic_versions_jsonrpc_keeps_true_oversize_refusals_atomic() {
    let packet: Value = serde_json::from_str(include_str!(
        "../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("complete captured public source packet");
    let reply: SurfaceReply =
        serde_json::from_value(packet["surface"].clone()).expect("complete public source DTO");
    let SurfaceReply::SemanticVersions(records) = reply else {
        panic!("captured semantic versions")
    };
    // Repeating an actual display operand exercises only egress budgeting. It
    // does not assert that a compiler owner published duplicate generations.
    for detail in ["summary", "standard", "full"] {
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticVersions(
                vec![records[0].clone(), records[0].clone()].into_boxed_slice(),
            )),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.semantic_versions","arguments":{"package":records[0].package.as_str(),"detail":detail}}),
        );
        assert_context_bounded(&response);
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["structuredContent"]["cause"],
            "oversized"
        );
        assert!(
            response["result"]["structuredContent"]
                .get("semantic_data")
                .is_none()
        );
    }
}

#[test]
fn semantic_shapes_jsonrpc_refuses_oversized_view_without_dropping_required_facts() {
    // This is an untrusted egress fixture at the surface budget seam, not a
    // synthetic certificate-admitted compiler product or a corpus pass.
    let mut view: Value = serde_json::from_str(include_str!(
        "../../../../crates/library/fixtures/semantic-shape-egress-view.json"
    ))
    .expect("view");
    let mut operands: Value = serde_json::from_str(include_str!(
        "../../../../crates/library/fixtures/semantic-shape-read.json"
    ))
    .expect("operands");
    // Every text field remains under its public bound. Thirty-two display-only
    // closure-unavailable entries retain their exact source operand and exceed
    // the MCP packet budget without fabricating available compiler facts.
    let package = format!("/abs/{}", "x".repeat(3500));
    view["source"]["package"]["value"] = json!(package);
    operands["source"]["package"]["value"] = json!(package);
    operands["source"]["selected_source_frontier"]["package"]["value"] = json!(package);
    view["max_bytes"] = json!(262144);
    operands["max_bytes"] = json!(262144);
    let source: backend_library::SemanticVersionRecord =
        serde_json::from_value(operands["source"].clone()).expect("bounded source operand");
    let origin = backend_library::SemanticShapeSourceOrigin {
        source: backend_library::SemanticShapeSelection::from_selected(&source)
            .expect("selected fixture source"),
        selection_root: [8; 32],
        semantic_image_bytes: backend_library::SemanticImagePayloadBytes::new(4096)
            .expect("bounded fixture extent"),
        image: None,
    };
    let commitment = backend_library::semantic_shape_source_key(&origin);
    let commitment_hex: String = commitment
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let origin_view = json!({
        "source": view["source"], "selection_root": origin.selection_root,
        "semantic_image_bytes": 4096, "image": null,
        "source_commitment": commitment_hex,
    });
    let entries: Vec<_> = (1u8..=32)
        .map(|id| {
            json!({
                "symbol":{"kind":{"kind":"selected"},"id":format!("{id:02x}").repeat(32)},
                "identity":null,"origin":origin_view,
                "fact":{"state":"unavailable","data":"missing_image_fact"},
            })
        })
        .collect();
    view["batch"]["entries"] = json!(entries);
    operands["symbols"] = json!((1u8..=32).map(|id| [id; 32]).collect::<Vec<_>>());
    let export: backend_library::SemanticShapeExport =
        serde_json::from_value(view).expect("bounded view");
    for detail in ["summary", "full"] {
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticShapes(export.clone())),
            ..Fake::default()
        });
        let result = call(
            &mut server,
            "backend.semantic_shapes",
            &json!({"request":operands,"detail":detail}),
        );
        assert_context_bounded(&result);
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["cause"], "oversized");
        assert!(result["structuredContent"].get("semantic_data").is_none());
    }
}

fn text_of(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .expect("a tool result carries one text block")
        .to_owned()
}

fn tool_named<'a>(tools: &'a Value, name: &str) -> &'a Value {
    tools
        .as_array()
        .expect("tools is an array")
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("no tool named `{name}`"))
}

fn assert_context_bounded(response: &Value) {
    let bytes = serde_json::to_vec(response)
        .expect("JSON-RPC response serializes")
        .len()
        .saturating_add(1);
    assert!(
        bytes <= DEFAULT_RESPONSE_BUDGET_BYTES,
        "response is {} bytes, above the {} byte context budget: {response}",
        bytes,
        DEFAULT_RESPONSE_BUDGET_BYTES
    );
}

fn paging_catalog(count: usize) -> backend_library::Library {
    let mut rows = vec![package_row(), module_row()];
    rows.extend((0..count).map(|index| {
        let coordinate = format!("{MODULE}:{}::Session_{index:03}", index + 2);
        Row::in_package(
            RowId::Symbol(symbol_key(&coordinate)),
            basis(),
            package_key(PROJECT),
            coordinate,
        )
        .with_parent(symbol_key(MODULE))
    }));
    // This is a genuine incomplete catalog projection; no compiler or
    // complete-coverage authority is asserted by this paging fixture.
    let view = root(rows);
    let cursor = backend_library::Cursor::for_view_root(&view);
    backend_library::Library::from_view(view, cursor).expect("catalog projection")
}

fn selected_search_page(
    catalog: &backend_library::Library,
    query: &backend_library::Query,
) -> Result<ViewSnapshot, backend_library::LibraryError> {
    // Use the same producer projection seam as the application search
    // service: canonical row storage and relevance order remain distinct.
    let prefix = catalog.search_ranked(&backend_library::Query::new(
        query.text(),
        catalog.revision_root(),
        backend_library::QueryLimit::new(backend_library::QueryLimit::MAX).expect("bounded prefix"),
    ))?;
    catalog.search_from_ranked_ids(query, prefix.order())
}

#[test]
fn default_collection_pages_keep_owner_order_exactly_once_and_reject_new_snapshots() {
    for tool in ["backend.search", "backend.resolve", "backend.graph"] {
        let catalog = paging_catalog(61);
        let credit = backend_library::QueryLimit::new(200).expect("bounded oracle page");
        let snapshot = match tool {
            "backend.search" => selected_search_page(
                &catalog,
                &backend_library::Query::new("Session", catalog.revision_root(), credit),
            )
            .expect("complete selected search"),
            "backend.resolve" => catalog
                .names(&backend_library::NameQuery::new(
                    "Session",
                    catalog.revision_root(),
                    credit,
                ))
                .expect("complete selected names"),
            _ => {
                catalog
                    .graph_page(
                        symbol_key(MODULE),
                        backend_library::PageRequest::new(catalog.revision_root(), credit),
                    )
                    .expect("complete selected neighborhood")
                    .snapshot
            }
        };
        let mut expected = record_list("selected", &snapshot)
            .records()
            .iter()
            .map(|record| record.identity().coordinate().as_str().to_owned())
            .collect::<Vec<_>>();
        if tool == "backend.graph" {
            // Neighborhood snapshots sort identities within each selected
            // page. The root/parent prefix precedes children in owner page
            // selection, so compare the actual owner order at this credit.
            let membership = expected
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            expected.clear();
            let mut cursor = None;
            loop {
                let mut page = backend_library::PageRequest::new(
                    catalog.revision_root(),
                    backend_library::QueryLimit::new(backend_present::DEFAULT_LIMIT)
                        .expect("default credit"),
                );
                if let Some(next) = cursor {
                    page = page.with_continuation(next);
                }
                let page = catalog
                    .graph_page(symbol_key(MODULE), page)
                    .expect("owner page");
                expected.extend(
                    record_list("selected", &page.snapshot)
                        .records()
                        .iter()
                        .map(|record| record.identity().coordinate().as_str().to_owned()),
                );
                let PageTerminal::More(next) = page.terminal else {
                    break;
                };
                cursor = Some(next);
            }
            assert_eq!(
                expected
                    .iter()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>(),
                membership
            );
        }
        let mut server = ready(Fake {
            page_catalog: Some(catalog),
            ..Fake::default()
        });
        let mut arguments = if tool == "backend.graph" {
            json!({"coordinate": MODULE})
        } else {
            json!({"query": "Session"})
        };
        let mut seen = Vec::new();
        let mut first_cursor = None;
        for _ in 0..10 {
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name": tool, "arguments": arguments}),
            );
            assert_context_bounded(&response);
            let result = &response["result"];
            assert_eq!(result["isError"], false, "{tool}: {response}");
            assert_eq!(result["structuredContent"]["detail"], "summary");
            let records = result["structuredContent"]["records"]
                .as_array()
                .expect("complete typed rows");
            assert!(records.len() <= usize::from(backend_present::DEFAULT_LIMIT));
            seen.extend(records.iter().map(|row| {
                row["identity"]["coordinate"]
                    .as_str()
                    .expect("exact coordinate")
                    .to_owned()
            }));
            let Some(cursor) = result["structuredContent"]["nextCursor"].as_str() else {
                break;
            };
            assert_eq!(result["structuredContent"]["more"], true);
            let preview = text_of(result);
            assert!(
                preview.contains(&format!("Continue `{tool}` with the same arguments")),
                "{tool}: {preview}"
            );
            assert!(
                preview.contains("copy `structuredContent.nextCursor` to `arguments.cursor`"),
                "{tool}: {preview}"
            );
            assert!(
                preview.contains(&format!("`{cursor}`")),
                "{tool}: {preview}"
            );
            assert!(!preview.contains("raise `limit`"), "{tool}: {preview}");
            first_cursor.get_or_insert_with(|| cursor.to_owned());
            arguments["cursor"] = json!(cursor);
        }
        assert_eq!(
            seen, expected,
            "{tool} preserves owner order, no skipped/duplicate rows"
        );
        assert_eq!(
            server.product.page_limits,
            vec![backend_present::DEFAULT_LIMIT; 3]
        );
        let first_cursor = first_cursor.expect("ordinary default continues");

        // The same query spelling and MAC cannot make an old projection
        // current when the underlying catalog changes.
        server.product.page_catalog = Some(paging_catalog(62));
        arguments["cursor"] = json!(first_cursor);
        let stale = request(
            &mut server,
            "tools/call",
            &json!({"name": tool, "arguments": arguments}),
        );
        let fault = tool_failure(&stale, "cursor-mismatch");
        assert_eq!(fault["cause"], "moved");
        assert!(text_of(&stale["result"]).contains("restart the query"));
    }
}

#[test]
fn combined_budget_shortens_only_the_readable_preview_with_explicit_provenance() {
    // Egress-only values exercise escaped-byte measurement and UTF-8
    // boundaries. They claim no owner-admitted source or compiler evidence.
    let cursor = "mcp1-complete-signed-cursor";
    let structured = json!({"records": [{"coordinate": "λאב".repeat(5000)}], "coverage": "unavailable", "nextCursor": cursor});
    let text = "\"\\\nλאב".repeat(3000);
    let result = tool_result(&text, structured.clone(), false);
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"], structured);
    let preview = text_of(&result);
    assert!(preview.contains("readable preview shortened"));
    assert!(preview.contains("complete page and any nextCursor"));
    assert!(text.starts_with(preview.split("\n\n…").next().expect("UTF-8 prefix")));
    assert_context_bounded(&success(json!(17), result));

    let too_large = json!({"records": ["x".repeat(MCP_RESULT_BUDGET_BYTES)]});
    let refused = tool_result(&text, too_large, false);
    assert_eq!(refused["isError"], true);
    assert_eq!(refused["structuredContent"]["cause"], "oversized");
    assert!(refused["structuredContent"].get("records").is_none());
}

fn setup_compiler_failure() -> backend_library::PackageCompilerFailure {
    use backend_library::interface::{CompilerTerminal, SourceAuthority};
    use backend_semantic::vocabulary::{Language, NativeTool, Stage};
    use backend_version::{ContentId, SourceFactDomain};

    let bytes = b"export function welcome(): string { return 'hello'; }";
    backend_library::PackageCompilerFailure::from_package_terminal(
        "src/main.ts",
        &CompilerTerminal::Toolchain {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(bytes),
                byte_len: u32::try_from(bytes.len()).expect("small source"),
            },
            language: Language::TypeScript,
            stage: Stage::LowerIr,
            selected: NativeTool::TypeScriptCompiler,
            configured: None,
        },
    )
    .expect("valid compiler terminal")
    .expect("setup refusal projects")
}

fn tool_failure<'a>(response: &'a Value, slug: &str) -> &'a Value {
    assert!(
        response.get("error").is_none(),
        "known tool failure is a result: {response}"
    );
    assert_eq!(response["result"]["isError"], true, "{response}");
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["answer"], "fault", "{response}");
    assert_eq!(structured["slug"], slug, "{response}");
    assert_context_bounded(response);
    structured
}
fn compiler_error_routes() -> Vec<(&'static str, Value, &'static str)> {
    let command = SurfaceCommand::IndexStart {
        package: PackageReference::parse(PROJECT).expect("project reference"),
        execution_intent: CompileExecutionIntent::Interactive,
    };
    vec![
        (
            INDEX_START_TOOL,
            json!({"package": PROJECT, "detail": "full"}),
            PROJECT,
        ),
        (
            INDEX_PROGRESS_TOOL,
            json!({"ticket": ticket_value(&index_job_ticket()), "detail": "full"}),
            "pkg:cargo/serde@1.0.228",
        ),
        (
            INDEX_CANCEL_TOOL,
            json!({"ticket": ticket_value(&index_job_ticket()), "detail": "full"}),
            "pkg:cargo/serde@1.0.228",
        ),
        (
            SURFACE_TOOL,
            json!({"command": command, "detail": "full"}),
            PROJECT,
        ),
    ]
}

fn compiler_client_error(failure: &backend_library::PackageCompilerFailure) -> ClientError {
    ClientError::CommandFailed(backend_library::CommandFailure::CompilerRefused {
        detail: "RAW SOURCE DIAGNOSTIC AND LEGACY JSON".repeat(2_000),
        failure: failure.clone(),
    })
}

fn adapter_error_routes() -> Vec<(&'static str, Value, &'static str, &'static str)> {
    vec![
        (
            "tools/call",
            json!({"name": "backend.graph", "arguments": {"coordinate": DECLARATION, "limit": 1}}),
            DECLARATION,
            "graph_page",
        ),
        (
            "prompts/get",
            json!({"name": "backend.explore", "arguments": {"query": "welcome"}}),
            PROJECT,
            "revision",
        ),
        (
            "tools/call",
            json!({"name": QUERY_TOOL, "arguments": {"query": "{ Declaration { coordinate @output } }", "limit": 1}}),
            PROJECT,
            "encode_continuation",
        ),
    ]
}

#[test]
fn adapter_errors_preserve_typed_compiler_facts_at_exact_boundaries() {
    let failure = setup_compiler_failure();
    for (method, params, operand, boundary) in adapter_error_routes() {
        let mut server = ready(Fake {
            adapter_error: Some(compiler_client_error(&failure)),
            graph_continue: true,
            ..Fake::default()
        });
        let response = request(&mut server, method, &params);
        let (structured, detail) = if method == "tools/call" {
            (
                tool_failure(&response, "compiler-refused"),
                text_of(&response["result"]),
            )
        } else {
            let data = &response["error"]["data"];
            assert_eq!(data["kind"], "compiler-refused", "{boundary}: {response}");
            (
                &data["structuredContent"],
                data["detail"].as_str().expect("RPC explanation").to_owned(),
            )
        };
        assert_eq!(server.product.adapter_boundary, Some(boundary));
        assert_eq!(
            structured["compiler_failure"],
            serde_json::to_value(&failure).expect("exact facts")
        );
        assert_eq!(structured["operand"], operand);
        assert_eq!(
            structured["compiler_tool_requirement"]["configuration_variable"],
            "NUDOX_TSC"
        );
        assert!(detail.contains("src/main.ts: setup/toolchain_configuration_mismatch"));
        assert!(!detail.contains("RAW SOURCE DIAGNOSTIC"));
        assert!(!detail.contains("content:"));
        assert!(detail.len() < 500);
        assert_context_bounded(&response);
    }
}

#[test]
fn adapter_errors_keep_valid_json_and_coordinate_protocol_strings_unproven() {
    let failure = setup_compiler_failure();
    let encoded = failure
        .encode_bounded_json()
        .expect("bounded compiler JSON");
    assert_eq!(
        backend_library::PackageCompilerFailure::decode_bounded_json(&encoded)
            .expect("valid compiler JSON"),
        failure
    );
    for message in [
        String::from_utf8(encoded).expect("compiler JSON is UTF-8"),
        "/abs/trap::src/hidden.ts:12::Secret: library record not found".to_owned(),
    ] {
        for (method, params, operand, boundary) in adapter_error_routes() {
            let mut server = ready(Fake {
                adapter_error: Some(ClientError::Protocol(message.clone())),
                graph_continue: true,
                ..Fake::default()
            });
            let response = request(&mut server, method, &params);
            let data = &response["error"]["data"];
            let structured = &data["structuredContent"];
            assert_eq!(server.product.adapter_boundary, Some(boundary));
            assert_eq!(data["kind"], "protocol", "{boundary}: {response}");
            assert_eq!(structured["cause"], "unproven");
            assert_eq!(structured["operand"], operand);
            assert!(structured.get("compiler_failure").is_none());
            assert!(structured.get("compiler_tool_requirement").is_none());
            assert_context_bounded(&response);
        }
    }
}

#[test]
fn adapter_errors_keep_continuation_transport_refusal_separate_from_compiler_facts() {
    let (method, params, operand, boundary) =
        adapter_error_routes().pop().expect("continuation route");
    let mut server = ready(Fake {
        adapter_error: Some(ClientError::Transport(
            backend_replication::ReplicationError::MessageTooLarge,
        )),
        graph_continue: true,
        ..Fake::default()
    });
    let response = request(&mut server, method, &params);
    let structured = tool_failure(&response, "transport");
    assert_eq!(server.product.adapter_boundary, Some(boundary));
    assert_eq!(server.product.graph_query_calls, 1);
    assert_eq!(structured["cause"], "oversized");
    assert_eq!(structured["operand"], operand);
    assert!(structured.get("compiler_failure").is_none());
    assert!(structured.get("compiler_tool_requirement").is_none());
    assert_context_bounded(&response);
}

#[test]
fn surface_errors_preserve_typed_compiler_facts_across_all_job_routes() {
    let failure = setup_compiler_failure();
    for (tool, arguments, operand) in compiler_error_routes() {
        let mut server = ready(Fake {
            surface_error: Some(compiler_client_error(&failure)),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name": tool, "arguments": arguments}),
        );
        let structured = tool_failure(&response, "compiler-refused");
        assert_eq!(
            structured["compiler_failure"],
            serde_json::to_value(&failure).expect("exact facts")
        );
        assert_eq!(structured["cause"], "refused");
        assert_eq!(structured["operand"], operand);
        assert_eq!(
            structured["compiler_tool_requirement"]["configuration_variable"],
            "NUDOX_TSC"
        );
        assert_eq!(
            structured["compiler_tool_requirement"]["configuration_required"],
            true
        );
        let detail = text_of(&response["result"]);
        assert!(detail.contains("src/main.ts: setup/toolchain_configuration_mismatch"));
        assert!(detail.contains("Set NUDOX_TSC to an absolute path"));
        assert!(!detail.contains("RAW SOURCE DIAGNOSTIC"));
        assert!(!detail.contains("content:"));
        assert!(!detail.contains("facts="));
        assert!(detail.len() < 500);
        assert_eq!(server.product.surface_commands.len(), 1);
        assert_context_bounded(&response);
    }
}

#[test]
fn surface_errors_keep_valid_compiler_json_and_coordinates_as_unproven_protocol() {
    let failure = setup_compiler_failure();
    let encoded = failure
        .encode_bounded_json()
        .expect("bounded compiler JSON");
    assert_eq!(
        backend_library::PackageCompilerFailure::decode_bounded_json(&encoded)
            .expect("valid compiler JSON"),
        failure
    );
    for message in [
        String::from_utf8(encoded).expect("compiler JSON is UTF-8"),
        "/abs/trap::src/hidden.ts:12::Secret: library record not found".to_owned(),
    ] {
        for (tool, arguments, operand) in compiler_error_routes() {
            let mut server = ready(Fake {
                surface_error: Some(ClientError::Protocol(message.clone())),
                ..Fake::default()
            });
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name": tool, "arguments": arguments}),
            );
            let data = &response["error"]["data"];
            let structured = &data["structuredContent"];
            assert_eq!(data["kind"], "protocol", "{tool}: {response}");
            assert_eq!(structured["cause"], "unproven");
            assert_eq!(structured["operand"], operand);
            assert!(structured.get("compiler_failure").is_none());
            assert!(structured.get("compiler_tool_requirement").is_none());
            assert_context_bounded(&response);
        }
    }
}

// ---------------------------------------------------------------------------
// registry completeness
// ---------------------------------------------------------------------------

#[test]
fn every_registry_row_is_reachable_as_exactly_one_tool() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = &listed["result"]["tools"];
    const SESSION: &[&str] = &[
        "packages",
        "add",
        "remove",
        "health",
        "outline",
        "search",
        "resolve",
        "show",
        "source",
        "read",
        "references",
        "graph",
        "package",
        "index-search",
    ];
    for name in SESSION {
        let spec = COMMANDS
            .iter()
            .find(|spec| spec.name == *name)
            .unwrap_or_else(|| panic!("session row `{name}` left the registry"));
        let grammar = grammar_for(spec.name)
            .unwrap_or_else(|| panic!("registry row `{}` has no grammar", spec.name));
        let tool = tool_named(tools, grammar.tool());
        assert_eq!(tool["title"], spec.title, "`{}` title", spec.name);
        let description = tool["description"].as_str().unwrap_or_default();
        assert!(
            description.starts_with(spec.description),
            "`{}` must lead with its registry sentence: {description}",
            spec.name
        );
        assert!(
            description.len() > spec.description.len() + 20,
            "`{}` must also say when to reach for it",
            spec.name
        );
        assert_eq!(
            tool["_meta"]["backend/domain"],
            domain_name(spec.domain),
            "`{}` domain",
            spec.name
        );
        assert_eq!(
            tool["annotations"]["readOnlyHint"],
            Value::Bool(!grammar.is_write()),
            "`{}` read-only hint",
            spec.name
        );
        let required = tool["inputSchema"]["required"]
            .as_array()
            .expect("required is an array");
        let mut expected = grammar
            .positional()
            .iter()
            .filter(|argument| argument.is_required() || grammar.tool() == "backend.index")
            .map(|argument| Value::String(argument.name().to_owned()))
            .collect::<Vec<_>>();
        if grammar.tool() == "backend.index" {
            expected = vec![Value::String("path".to_owned())];
        }
        assert_eq!(required, &expected, "`{}` required operands", spec.name);
        for argument in grammar.positional().iter().chain(grammar.options()) {
            assert!(
                !tool["inputSchema"]["properties"][argument.json_name()].is_null(),
                "`{}` omits operand `{}`",
                spec.name,
                argument.name()
            );
        }
    }
    let names = tools
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "backend.packages",
            "backend.index",
            "backend.remove",
            "backend.status",
            "backend.outline",
            "backend.search",
            "backend.resolve",
            "backend.document",
            "backend.source",
            "backend.read",
            "backend.references",
            "backend.graph",
            "backend.package",
            "backend.index_search",
            "backend.index_start",
            "backend.index_progress",
            "backend.index_cancel",
        ]
    );
}

#[test]
fn owner_index_job_tools_advertise_exact_tickets_and_immediate_progress() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = &listed["result"]["tools"];

    let start = tool_named(tools, INDEX_START_TOOL);
    assert_eq!(start["inputSchema"]["required"], json!(["package"]));
    assert_eq!(
        start["inputSchema"]["properties"]["execution_intent"]["enum"],
        json!(["interactive", "background"])
    );
    assert_eq!(start["annotations"]["readOnlyHint"], false);

    let progress = tool_named(tools, INDEX_PROGRESS_TOOL);
    assert_eq!(
        progress["inputSchema"]["oneOf"][0]["required"],
        json!(["ticket"])
    );
    assert_eq!(
        progress["inputSchema"]["oneOf"][1]["required"],
        json!(["operation_key"])
    );
    assert_eq!(
        progress["inputSchema"]["properties"]["ticket"]["oneOf"][0]["properties"]["owner_epoch"]["minItems"],
        16
    );
    assert_eq!(
        progress["inputSchema"]["properties"]["after_sequence"]["default"],
        0
    );
    assert_eq!(progress["annotations"]["readOnlyHint"], true);
    assert_eq!(
        progress["outputSchema"]["properties"]["surface"]["required"],
        json!(["result", "data"])
    );

    let cancel = tool_named(tools, INDEX_CANCEL_TOOL);
    assert_eq!(cancel["inputSchema"]["required"], json!(["ticket"]));
    assert_eq!(cancel["annotations"]["readOnlyHint"], false);
    assert!(
        tools
            .as_array()
            .expect("tools array")
            .iter()
            .all(|tool| tool["name"] != "backend.index_await")
    );
    assert!(
        progress["description"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown-after-restart")
    );
    assert!(
        cancel["description"]
            .as_str()
            .unwrap_or_default()
            .contains("not terminal")
    );
}

#[test]
fn keyed_index_progress_preserves_durable_owner_evidence_and_rejects_mixed_identities() {
    let key = backend_library::IndexOperationKey::from_bytes([0x51; 32]).expect("key");
    let observation = backend_library::IndexOperationObservation::Known(
        backend_library::IndexOperationStatus::new(
            key,
            PackageReference::parse(PROJECT).expect("package"),
            CompileExecutionIntent::Interactive,
            backend_library::IndexOperationState::Accepted,
        ),
    );
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::IndexOperationStatus(observation.clone())),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({"operation_key":key}),
    );
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["structuredContent"]["surface"]["result"],
        "index-operation-status"
    );
    assert_eq!(
        result["structuredContent"]["surface"]["data"],
        serde_json::to_value(&observation).expect("typed evidence")
    );
    assert_eq!(
        server.product.surface_commands,
        [SurfaceCommand::IndexOperationStatus { operation_key: key }]
    );
    assert!(text_of(&result).contains("accepted"));
    assert!(!text_of(&result).contains("index operation published"));
    for input in [
        json!({"operation_key":key,"ticket":ticket_value(&index_job_ticket())}),
        json!({"operation_key":key,"after_sequence":0}),
        json!({"operation_key":"00".repeat(32)}),
    ] {
        let mut server = ready(Fake::default());
        let result = call(&mut server, INDEX_PROGRESS_TOOL, &input);
        assert_eq!(result["isError"], true);
        assert!(server.product.surface_commands.is_empty());
    }
}

#[test]
fn index_start_keeps_the_exact_owner_ticket_and_routes_purls_to_the_owner_job_api() {
    let ticket = index_job_ticket();
    let reply = SurfaceReply::IndexStarted(IndexStartResult::Started {
        ticket: ticket.clone(),
        stage: IndexJobStage::Acquiring,
    });
    let fake = Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    };
    let mut server = ready(fake);
    let result = call(
        &mut server,
        INDEX_START_TOOL,
        &json!({
            "package": "pkg:cargo/serde@1.0.228",
            "execution_intent": "background"
        }),
    );

    assert_eq!(result["isError"], false);
    let wire = serde_json::to_value(&reply).expect("typed reply");
    assert_eq!(
        result["structuredContent"]["surface"]["result"],
        wire["result"]
    );
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["kind"],
        "started"
    );
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        serde_json::to_value(IndexStartResult::Started {
            ticket: ticket.clone(),
            stage: IndexJobStage::Acquiring,
        })
        .expect("start projection")
    );
    assert!(text_of(&result).contains("stage acquiring"));
    assert!(text_of(&result).contains(&ticket_value(&ticket).to_string()));
    assert_context_bounded(&result);
    assert!(matches!(
        server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexStart {
            package: PackageReference::Purl(package),
            execution_intent: CompileExecutionIntent::Background,
        }] if package.as_str() == "pkg:cargo/serde@1.0.228"
    ));
}

#[test]
fn index_progress_returns_bounded_events_and_the_exact_next_sequence() {
    let ticket = index_job_ticket();
    let profile =
        backend_library::SemanticLanguageProfile::from_name("rust").expect("closed rust profile");
    let reply = SurfaceReply::IndexProgress(IndexJobObservation::Pending(IndexProgressPage {
        ticket: ticket.clone(),
        stage: IndexJobStage::Compiling,
        events: vec![IndexJobProgressEvent {
            ticket: ticket.clone(),
            sequence: 3,
            kind: IndexJobProgressKind::ProfileStarted {
                profile,
                ordinal: 1,
                total: 2,
            },
        }]
        .into_boxed_slice(),
        next_sequence: 3,
        truncated: true,
        has_more: false,
    }));
    let mut server = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({ "ticket": ticket_value(&ticket), "after_sequence": 2 }),
    );

    assert_eq!(result["isError"], false);
    let wire = serde_json::to_value(&reply).expect("typed observation");
    assert_eq!(
        result["structuredContent"]["surface"]["result"],
        wire["result"]
    );
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        wire["data"]
    );
    let text = text_of(&result);
    assert!(text.contains("older progress events aged out"), "{text}");
    assert!(text.contains("rust profile started"), "{text}");
    assert!(text.contains("\"after_sequence\":3"), "{text}");
    assert!(text.contains(&ticket_value(&ticket).to_string()), "{text}");
    assert_context_bounded(&result);
    assert!(matches!(
        server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexProgress {
            ticket: seen,
            after_sequence: 2,
        }] if seen == &ticket
    ));
}

#[test]
fn index_progress_maximum_owner_page_fits_the_combined_mcp_result_budget() {
    let ticket = index_job_ticket();
    let profile =
        backend_library::SemanticLanguageProfile::from_name("rust").expect("closed rust profile");
    let events = (1..=backend_library::MAX_INDEX_PROGRESS_EVENTS as u64)
        .map(|sequence| IndexJobProgressEvent {
            ticket: ticket.clone(),
            sequence,
            kind: IndexJobProgressKind::ProfileAdmitted {
                profile,
                ordinal: 1,
                total: 1,
            },
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let reply = SurfaceReply::IndexProgress(IndexJobObservation::Pending(IndexProgressPage {
        ticket: ticket.clone(),
        stage: IndexJobStage::Publishing,
        events,
        next_sequence: backend_library::MAX_INDEX_PROGRESS_EVENTS as u64,
        truncated: true,
        has_more: true,
    }));
    let mut server = ready(Fake {
        surface_reply: Some(reply),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({ "ticket": ticket_value(&ticket), "after_sequence": 0 }),
    );
    assert_eq!(result["isError"], false);
    assert_context_bounded(&result);
    let text = text_of(&result);
    assert!(text.contains("more events available"), "{text}");
    assert!(text.contains("\"after_sequence\":16"), "{text}");
}

#[test]
fn index_progress_distinguishes_prior_owner_tickets_and_terminal_refusals() {
    let ticket = index_job_ticket();
    let unknown = SurfaceReply::IndexProgress(IndexJobObservation::Unknown {
        ticket: ticket.clone(),
        current_owner_epoch: [8; 16],
    });
    let mut server = ready(Fake {
        surface_reply: Some(unknown.clone()),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({ "ticket": ticket_value(&ticket) }),
    );
    assert_eq!(result["isError"], false);
    let wire = serde_json::to_value(&unknown).expect("unknown observation");
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        wire["data"]
    );
    assert!(text_of(&result).contains("unknown after owner restart"));

    let terminal = SurfaceReply::IndexProgress(IndexJobObservation::Terminal(IndexJobTerminal {
        ticket: ticket.clone(),
        outcome: IndexJobOutcome::Refused(
            backend_library::ProductText::new("compiler input was refused").expect("reason"),
        ),
    }));
    server.product.surface_reply = Some(terminal.clone());
    let result = call(
        &mut server,
        INDEX_PROGRESS_TOOL,
        &json!({ "ticket": ticket_value(&ticket), "after_sequence": 3 }),
    );
    assert_eq!(result["isError"], true);
    let wire = serde_json::to_value(&terminal).expect("terminal observation");
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        wire["data"]
    );
    let text = text_of(&result);
    assert!(text.contains("outcome refused"), "{text}");
    assert!(text.contains("compiler input was refused"), "{text}");
}

#[test]
fn dependency_refusal_marks_tool_failure_and_keeps_the_exact_requested_package() {
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::Dependencies(
            backend_library::DependencyFacts::Unavailable(
                backend_library::ProductText::from_static("package is not recorded"),
            ),
        )),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        "backend.dependencies",
        &json!({"package": "pkg:npm/react@19.1.0"}),
    );
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["answer"], "product");
    assert_eq!(
        result["structuredContent"]["fault"]["operand"],
        "pkg:npm/react@19.1.0"
    );
    assert_eq!(
        result["structuredContent"]["fault"]["call"]["arguments"]["package"],
        "pkg:npm/react@19.1.0"
    );
    assert!(text_of(&result).contains("package is not recorded"));
}

#[test]
fn copied_ticket_string_uses_the_object_decoder_and_preserves_the_owner_ticket() {
    let ticket = index_job_ticket();
    let mut server = ready(Fake::default());
    for (tool, reply, expected_command) in [
        (
            INDEX_PROGRESS_TOOL,
            SurfaceReply::IndexProgress(IndexJobObservation::Unknown {
                ticket: ticket.clone(),
                current_owner_epoch: [8; 16],
            }),
            SurfaceCommand::IndexProgress {
                ticket: ticket.clone(),
                after_sequence: 17,
            },
        ),
        (
            INDEX_CANCEL_TOOL,
            SurfaceReply::IndexCancellation(IndexCancelReceipt {
                ticket: ticket.clone(),
                status: IndexCancelStatus::Requested,
            }),
            SurfaceCommand::IndexCancel {
                ticket: ticket.clone(),
            },
        ),
    ] {
        for value in [
            ticket_value(&ticket),
            json!(ticket_value(&ticket).to_string()),
        ] {
            // Fake.surface consumes one supplied response per owner call.
            server.product.surface_reply = Some(reply.clone());
            let mut arguments = json!({"ticket": value});
            if tool == INDEX_PROGRESS_TOOL {
                arguments["after_sequence"] = json!(17);
            }
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name":tool, "arguments":arguments}),
            );
            let result = &response["result"];
            assert_eq!(result["isError"], false, "{response}");
            assert_eq!(
                result["structuredContent"]["surface"]["index_job"]["value"],
                serde_json::to_value(&reply).expect("same exact reply")["data"]
            );
            assert_eq!(
                server.product.surface_commands.last(),
                Some(&expected_command)
            );
        }
        let bad = request(
            &mut server,
            "tools/call",
            &json!({"name": tool, "arguments": {"ticket": "7"}}),
        );
        assert_eq!(tool_failure(&bad, "usage")["operand"], "ticket");
    }
    assert_eq!(
        server.product.surface_commands.len(),
        4,
        "malformed tickets do not reach the owner"
    );
}

#[test]
fn missing_index_path_returns_actionable_structured_usage_before_owner_work() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name": "backend.index", "arguments": {}}),
    );
    assert_eq!(tool_failure(&response, "usage")["operand"], "path");
    assert!(text_of(&response["result"]).contains("absolute path"));
    assert_eq!(server.product.probe_calls, 0);
}

#[test]
fn index_cancel_preserves_terminal_races_and_rejects_fabricated_tickets() {
    let ticket = index_job_ticket();
    let terminal = IndexJobTerminal {
        ticket: ticket.clone(),
        outcome: IndexJobOutcome::Published,
    };
    let reply = SurfaceReply::IndexCancellation(IndexCancelReceipt {
        ticket: ticket.clone(),
        status: IndexCancelStatus::Terminal(terminal),
    });
    let mut server = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        INDEX_CANCEL_TOOL,
        &json!({ "ticket": ticket_value(&ticket) }),
    );
    assert_eq!(result["isError"], false);
    let wire = serde_json::to_value(&reply).expect("terminal cancellation race");
    assert_eq!(result["structuredContent"]["surface"]["data"], wire["data"]);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        wire["data"]
    );
    assert!(text_of(&result).contains("outcome published"));
    assert!(matches!(
        server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexCancel { ticket: seen }] if seen == &ticket
    ));

    let requested = SurfaceReply::IndexCancellation(IndexCancelReceipt {
        ticket: ticket.clone(),
        status: IndexCancelStatus::Requested,
    });
    server.product.surface_reply = Some(requested.clone());
    let result = call(
        &mut server,
        INDEX_CANCEL_TOOL,
        &json!({ "ticket": ticket_value(&ticket) }),
    );
    assert_eq!(result["isError"], false);
    let requested_wire = serde_json::to_value(&requested).expect("requested receipt");
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"],
        requested_wire["data"]
    );
    let text = text_of(&result);
    assert!(text.contains("cancellation was requested"), "{text}");
    assert!(text.contains("cancellation ticket:"), "{text}");

    let malformed = request(
        &mut server,
        "tools/call",
        &json!({
            "name": INDEX_CANCEL_TOOL,
            "arguments": {
                "ticket": {
                    "id": 17,
                    "owner_epoch": [7],
                    "package": { "kind": "purl", "value": "pkg:cargo/serde@1.0.228" }
                }
            }
        }),
    );
    assert_eq!(tool_failure(&malformed, "usage")["operand"], "ticket");
    assert_eq!(server.product.surface_commands.len(), 2);
}

#[test]
fn generic_surface_progress_and_cancel_keep_the_request_ticket_projection() {
    let ticket = index_job_ticket();
    let reply = SurfaceReply::IndexCancellation(IndexCancelReceipt {
        ticket: ticket.clone(),
        status: IndexCancelStatus::Requested,
    });
    let mut server = ready(Fake {
        surface_reply: Some(reply),
        ..Fake::default()
    });
    let result = call(
        &mut server,
        SURFACE_TOOL,
        &json!({
            "command": {
                "operation": "index-cancel",
                "ticket": ticket_value(&ticket)
            }
        }),
    );
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["kind"],
        "cancellation"
    );
    assert_eq!(
        result["structuredContent"]["surface"]["index_job"]["value"]["ticket"],
        ticket_value(&ticket)
    );
    assert!(text_of(&result).contains(&ticket_value(&ticket).to_string()));
    assert!(matches!(
        server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexCancel { ticket: seen }] if seen == &ticket
    ));
}

#[test]
fn blocking_owner_await_is_not_exposed_over_mcp() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({
            "name": "backend.index_await",
            "arguments": { "ticket": ticket_value(&index_job_ticket()) }
        }),
    );
    tool_failure(&response, "usage");
    assert!(
        text_of(&response["result"]).contains("Use backend.index_progress for bounded polling")
    );

    let command = SurfaceCommand::IndexAwait {
        ticket: index_job_ticket(),
    };
    let generic = request(
        &mut server,
        "tools/call",
        &json!({
            "name": SURFACE_TOOL,
            "arguments": { "command": serde_json::to_value(command).expect("await command") }
        }),
    );
    tool_failure(&generic, "usage");
    assert!(
        generic["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("immediate bounded polling"))
    );
    assert!(server.product.surface_commands.is_empty());
}

#[test]
fn the_session_tool_list_leads_with_packages_and_index() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names.first().copied(), Some("backend.packages"));
    assert_eq!(names.get(1).copied(), Some("backend.index"));
    assert!(
        names
            .iter()
            .all(|name| *name != "backend.surface" && *name != "backend.query"),
        "the session list must not advertise the escape hatches: {names:?}"
    );
}

#[test]
fn registry_lookup_tools_are_advertised_with_bounded_typed_inputs() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = &listed["result"]["tools"];

    let package = tool_named(tools, "backend.package");
    assert_eq!(package["inputSchema"]["required"], json!(["package"]));
    assert_eq!(
        package["inputSchema"]["properties"]["package"]["type"],
        "string"
    );
    assert_eq!(package["inputSchema"]["additionalProperties"], false);

    let search = tool_named(tools, "backend.index_search");
    assert_eq!(search["inputSchema"]["required"], json!(["query"]));
    assert_eq!(
        search["inputSchema"]["properties"]["query"]["type"],
        "string"
    );
    assert_eq!(search["inputSchema"]["properties"]["limit"]["minimum"], 1);
    assert_eq!(search["inputSchema"]["properties"]["limit"]["maximum"], 200);
    assert_eq!(
        search["inputSchema"]["properties"]["cursor"]["type"],
        "string"
    );
    assert_eq!(search["inputSchema"]["additionalProperties"], false);
}

#[test]
fn registry_schema_primitive_types_are_enforced_before_the_product_boundary() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = &listed["result"]["tools"];
    assert_eq!(
        tool_named(tools, "backend.search")["inputSchema"]["properties"]["limit"]["type"],
        "integer"
    );
    assert_eq!(
        tool_named(tools, "backend.read")["inputSchema"]["properties"]["coordinate"]["type"],
        "array"
    );

    for (tool, arguments, operand) in [
        (
            "backend.search",
            json!({"query":"ferris","limit":"1"}),
            "limit",
        ),
        ("backend.search", json!({"query":7}), "query"),
        (
            "backend.read",
            json!({"coordinate":DECLARATION}),
            "coordinate",
        ),
    ] {
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":tool,"arguments":arguments}),
        );
        let result = &response["result"];
        assert_eq!(result["isError"], true, "{tool} {arguments}: {response}");
        assert_eq!(result["structuredContent"]["answer"], "fault");
        assert_eq!(result["structuredContent"]["slug"], "usage");
        assert_eq!(result["structuredContent"]["operand"], operand);
        assert_context_bounded(&response);
    }
    assert_eq!(server.product.probe_calls, 0);

    let accepted = call(
        &mut server,
        "backend.search",
        &json!({"query":"ferris","limit":1}),
    );
    assert_eq!(accepted["isError"], false);
    assert_eq!(server.product.probe_calls, 1);
}

#[test]
fn missing_required_mcp_argument_names_the_tool_field_not_cli_usage() {
    let mut server = ready(Fake::default());
    for (arguments, operand, expected_detail) in [
        (json!({}), "query", "arguments.query"),
        (json!({"unknown":"x"}), "unknown", "takes no argument"),
        (
            json!({"query":7,"unknown":"x"}),
            "query",
            "must be a string",
        ),
    ] {
        let result = call(&mut server, "backend.search", &arguments);
        assert_eq!(result["isError"], true, "{arguments}");
        assert_eq!(result["structuredContent"]["answer"], "fault");
        assert_eq!(result["structuredContent"]["slug"], "usage");
        assert_eq!(result["structuredContent"]["operand"], operand);
        let message = text_of(&result);
        assert!(message.contains(expected_detail), "{message}");
        assert!(!message.contains("usage: backend search"), "{message}");
    }
    assert_eq!(server.product.probe_calls, 0);
}

#[test]
fn advertised_registry_lookups_reach_the_typed_product_commands() {
    let mut package_server = ready(Fake {
        surface_reply: Some(SurfaceReply::Package(Box::new([]))),
        ..Fake::default()
    });
    let package = call(
        &mut package_server,
        "backend.package",
        &json!({"package":"pkg:cargo/serde@1.0.228"}),
    );
    assert_eq!(package["isError"], false);
    assert!(matches!(
        package_server.product.surface_commands.as_slice(),
        [SurfaceCommand::Package { package }]
            if package.as_str() == "pkg:cargo/serde@1.0.228"
    ));

    let mut search_server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let search = call(
        &mut search_server,
        "backend.index_search",
        &json!({"query":"serde","limit":3}),
    );
    assert_eq!(search["isError"], false);
    assert!(matches!(
        search_server.product.surface_commands.as_slice(),
        [SurfaceCommand::IndexSearch { query, limit: 3, cursor: None }]
            if query.as_str() == "serde"
    ));
}

#[test]
fn index_requires_its_advertised_path_before_owner_admission() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tool = tool_named(&listed["result"]["tools"], "backend.index");
    assert_eq!(tool["inputSchema"]["required"], json!(["path"]));
    assert_eq!(tool["inputSchema"]["properties"]["path"]["type"], "string");
    assert_eq!(tool["inputSchema"]["properties"]["path"]["minLength"], 1);

    for params in [
        json!({ "name": "backend.index" }),
        json!({ "name": "backend.index", "arguments": {} }),
        json!({ "name": "backend.index", "arguments": { "detail": "full" } }),
        json!({ "name": "backend.index", "arguments": { "path": null } }),
        json!({ "name": "backend.index", "arguments": { "path": 7 } }),
        json!({ "name": "backend.index", "arguments": { "path": [] } }),
        json!({ "name": "backend.index", "arguments": { "path": "" } }),
    ] {
        let response = request(&mut server, "tools/call", &params);
        assert_eq!(response["id"], 9);
        let fault = tool_failure(&response, "usage");
        assert_eq!(fault["slug"], "usage");
        assert_eq!(fault["operand"], "path");
        assert_eq!(fault["cause"], "malformed");
        assert!(
            fault["detail"]
                .as_str()
                .expect("typed path guidance")
                .contains("backend.index requires arguments.path")
        );
        assert!(
            response["result"]["structuredContent"]["detail"]
                .as_str()
                .expect("shared path guidance")
                .contains("absolute path")
        );
        assert!(server.product.index_paths.is_empty(), "{params}");
        assert!(server.product.surface_commands.is_empty(), "{params}");
        assert_eq!(server.product.adapter_boundary, None, "{params}");
    }

    let explicit = "/abs/explicit-index-project";
    let accepted = call(&mut server, "backend.index", &json!({ "path": explicit }));
    assert_eq!(accepted["isError"], false);
    assert_eq!(server.product.index_paths, [explicit]);
}

#[test]
fn index_tool_advertises_and_validates_execution_intent() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tool = tool_named(&listed["result"]["tools"], "backend.index");
    assert_eq!(
        tool["inputSchema"]["properties"]["execution_intent"]["enum"],
        json!(["interactive", "background"])
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["execution_intent"]["default"],
        "interactive"
    );
    assert!(tool["inputSchema"]["properties"]["execution-intent"].is_null());

    let accepted = call(
        &mut server,
        "backend.index",
        &json!({ "path": PROJECT, "execution_intent": "background" }),
    );
    assert_eq!(accepted["isError"], false);

    let malformed = call(
        &mut server,
        "backend.index",
        &json!({ "path": PROJECT, "execution_intent": "remote" }),
    );
    assert_eq!(malformed["isError"], true);
    assert!(
        malformed["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("choose interactive or background")
    );

    let wrong_type = call(
        &mut server,
        "backend.index",
        &json!({ "path": PROJECT, "execution_intent": ["background"] }),
    );
    assert_eq!(wrong_type["isError"], true);
    assert!(
        wrong_type["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("must be a string")
    );
}

#[test]
fn document_and_source_tools_advertise_their_useful_default_detail() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    for name in ["backend.document", "backend.source"] {
        let tool = tool_named(&listed["result"]["tools"], name);
        assert_eq!(
            tool["inputSchema"]["properties"]["detail"]["default"], "standard",
            "{name} must advertise the same useful default that execution applies"
        );
    }
    let search = tool_named(&listed["result"]["tools"], "backend.search");
    assert_eq!(
        search["inputSchema"]["properties"]["detail"]["default"],
        "summary"
    );
}

#[test]
fn explicit_source_refuses_unavailable_text_while_document_retains_its_native_identity() {
    let coordinate = format!("{PROJECT}::semantic::{}::Environment", "0b".repeat(32));
    let native = symbol_key("compiler-owned-Environment");
    for (location, excerpt, cause) in [
        (
            SourceAvailability::NotCaptured,
            SourceExcerpt::NotCaptured,
            "not-captured",
        ),
        (
            SourceAvailability::Captured(
                SourceLocation::new("httpie/context.py", 46).expect("captured site"),
            ),
            SourceExcerpt::NotHydrated,
            "not-resident",
        ),
        (
            SourceAvailability::Unconfigured,
            SourceExcerpt::Unconfigured,
            "unconfigured",
        ),
    ] {
        let document = Document::new(
            native,
            view_state_root(&[]),
            [Fragment::Text(
                "Retained compiler documentation.".to_owned(),
            )],
        )
        .with_location(location)
        .with_excerpt(excerpt);
        let mut server = ready(Fake {
            document_override: Some(document),
            ..Fake::default()
        });
        let args = json!({"coordinate":coordinate,"detail":"full"});
        let page = call(&mut server, "backend.document", &args);
        assert_eq!(page["isError"], false);
        assert_eq!(page["structuredContent"]["answer"], "page");
        assert_eq!(
            page["structuredContent"]["identity"]["coordinate"],
            coordinate
        );
        assert_eq!(
            page["structuredContent"]["identity"]["semantic_data"]["value"],
            json!(native.as_bytes())
        );
        assert_eq!(page["structuredContent"]["source_fault"]["cause"], cause);
        assert!(text_of(&page).contains("Retained compiler documentation."));

        let source = call(&mut server, "backend.source", &args);
        assert_eq!(source["isError"], true);
        assert_eq!(source["structuredContent"]["answer"], "fault");
        assert_eq!(source["structuredContent"]["slug"], "source-unavailable");
        assert_eq!(source["structuredContent"]["cause"], cause);
        assert_eq!(source["structuredContent"]["operand"], coordinate);
    }

    let mut server = ready(Fake::default());
    let captured = call(
        &mut server,
        "backend.source",
        &json!({"coordinate":DECLARATION,"detail":"full"}),
    );
    assert_eq!(captured["isError"], false);
    assert_eq!(
        captured["structuredContent"]["source"]["path"],
        "src/lib.rs"
    );
    assert_eq!(captured["structuredContent"]["source"]["line"], 2);
    assert_eq!(
        captured["structuredContent"]["source"]["lines"][0],
        "pub fn ferris() -> Beacon {"
    );
    assert!(captured["structuredContent"]["source_fault"].is_null());
}

#[test]
fn the_session_list_hides_the_surface_escape_hatch() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools");
    assert!(
        tools.iter().all(|tool| tool["name"] != SURFACE_TOOL),
        "backend.surface is not part of the session tool list"
    );
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

#[test]
fn a_document_call_returns_the_markdown_page_and_the_typed_answer() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.document",
        &json!({ "coordinate": DECLARATION }),
    );
    assert_eq!(result["isError"], false);
    let text = text_of(&result);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("# polyglot › src/lib.rs:2 › ferris")
    );
    assert!(
        lines
            .get(1)
            .is_some_and(|line| line.contains(" · rust · key ") && line.ends_with("src/lib.rs:2")),
        "meta line: {:?}",
        lines.get(1)
    );
    assert_eq!(
        lines.get(2).copied(),
        Some("`/abs/polyglot::src/lib.rs:2::ferris`")
    );
    assert!(text.contains("```rust\npub mod lib\n```"), "{text}");
    assert!(text.contains("Lights the beacon."), "{text}");
    assert!(text.contains("## members"), "{text}");
    assert!(text.contains("## source"), "{text}");
    assert!(text.contains("   2 pub fn ferris() -> Beacon {"), "{text}");
    assert!(!text.contains('|'), "no Markdown table may appear: {text}");
    assert_eq!(result["structuredContent"]["answer"], "page");
    assert_eq!(
        result["structuredContent"]["identity"]["coordinate"],
        DECLARATION
    );
    assert_eq!(
        result["structuredContent"]["identity"]["trail"],
        "polyglot › src/lib.rs:2 › ferris"
    );
    assert_eq!(
        result["structuredContent"]["signature"], "pub mod lib",
        "a document's default projection must include the code it was requested to show"
    );
    assert_eq!(result["structuredContent"]["source"]["extent"], "complete");
    assert!(
        result["structuredContent"]["source"]["lines"]
            .as_array()
            .is_some_and(|lines| lines
                .iter()
                .any(|line| line == "pub fn ferris() -> Beacon {")),
        "the default document projection lost its bounded source excerpt: {result}"
    );
}

#[test]
fn a_search_call_leads_with_honest_coverage_then_two_lines_per_record() {
    let mut server = ready(Fake::default());
    let result = call(&mut server, "backend.search", &json!({ "query": "ferris" }));
    let text = text_of(&result);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("~lanes exact· names· graph· semantic✗ unconfigured"),
        "{text}"
    );
    assert_eq!(
        lines.get(1).copied(),
        Some("polyglot › src/lib.rs:2 › ferris  ·  function · rust")
    );
    assert!(
        lines
            .get(2)
            .is_some_and(|line| line.starts_with("`/abs/polyglot::src/lib.rs:2::ferris`")),
        "{text}"
    );
    assert_eq!(result["structuredContent"]["answer"], "records");
    assert_eq!(
        result["structuredContent"]["readiness"], "unknown",
        "three lanes said nothing and one is unconfigured: neither is a failure"
    );
}

#[test]
fn mcp_text_is_the_same_shared_markdown_the_cli_renders() {
    let mut direct = Fake::default();
    let answer = backend_present::answer(
        &mut direct,
        &Request::Search {
            text: "ferris".to_owned(),
            limit: 25,
        },
    )
    .expect("fixture search answer");
    let expected = bounded_text(&backend_present::markdown::answer(&answer));

    let mut server = ready(Fake::default());
    let result = call(&mut server, "backend.search", &json!({ "query": "ferris" }));
    assert_eq!(text_of(&result), expected);
}

#[test]
fn an_outline_call_is_a_tree_of_names_never_bare_digests() {
    let mut server = ready(Fake::default());
    let result = call(&mut server, "backend.outline", &json!({}));
    let text = text_of(&result);
    assert!(text.contains("▦ lib.rs"), "{text}");
    assert!(text.contains("  ƒ ferris"), "{text}");
    assert!(text.contains("2 declaration(s) · complete"), "{text}");
    let digest = backend_present::KeyTag::from_key(symbol_key(DECLARATION).as_bytes()).to_string();
    assert!(
        !text.contains(&digest),
        "a resolved node must never fall back to its digest: {text}"
    );
    assert_eq!(result["structuredContent"]["answer"], "outline");
}

#[test]
fn status_is_a_rollup_a_reader_can_hold_not_a_capability_dump() {
    let mut server = ready(Fake::default());
    let result = call(&mut server, "backend.status", &json!({}));
    let text = text_of(&result);
    assert!(
        text.lines().count() <= 25,
        "status must stay readable, got {} lines:\n{text}",
        text.lines().count()
    );
    assert!(text.contains("oracles 0/"), "{text}");
    assert!(text.contains("no-manifest"), "{text}");
    assert!(text.contains("embedding unconfigured"), "{text}");
    assert!(text.contains("~lanes"), "{text}");
    assert_eq!(result["structuredContent"]["answer"], "status");
    assert!(
        result["structuredContent"]["capabilities"]["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("oracles 0/"))
    );
}

#[test]
fn the_packages_call_states_readiness_for_every_project() {
    let mut server = ready(Fake::default());
    let result = call(&mut server, "backend.packages", &json!({}));
    let text = text_of(&result);
    assert!(text.contains("polyglot"), "{text}");
    assert!(text.contains("`/abs/polyglot`"), "{text}");
    assert!(text.contains("rust 2"), "{text}");
    assert_eq!(result["structuredContent"]["answer"], "shelf");
    assert_eq!(
        result["structuredContent"]["projects"][0]["identity"]["coordinate"],
        PROJECT
    );
}

// ---------------------------------------------------------------------------
// faults
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_coordinate_is_refused_with_the_operand_and_a_next_tool_call() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.document",
        &json!({ "coordinate": MISSING }),
    );
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(
        text.starts_with("✗ not-found `/abs/polyglot::src/lib.rs:999::nothing`"),
        "{text}"
    );
    assert!(
        text.contains("no record is published at that identity"),
        "{text}"
    );
    assert_eq!(result["structuredContent"]["answer"], "fault");
    assert_eq!(result["structuredContent"]["slug"], "not-found");
    assert_eq!(
        result["structuredContent"]["operand"], MISSING,
        "the operand is never elided"
    );
    // The next step is the exact call an agent can paste back, not advice.
    assert_eq!(
        result["structuredContent"]["call"]["name"], "backend.search",
        "{result}"
    );
    assert_eq!(
        result["structuredContent"]["call"]["arguments"]["query"], "nothing",
        "the search is the reader's own word, not an invented one"
    );
    assert!(
        text.contains("→ `{\"name\":\"backend.search\",\"arguments\":{\"query\":\"nothing\"}}`"),
        "{text}"
    );
}

#[test]
fn an_unreachable_endpoint_is_a_fault_with_the_retry_affordance() {
    let mut server = ready(Fake {
        offline: true,
        ..Fake::default()
    });
    let result = call(&mut server, "backend.search", &json!({ "query": "ferris" }));
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(text.starts_with("✗ endpoint `ferris`"), "{text}");
    assert_eq!(result["structuredContent"]["cause"], "unreachable");
}

#[test]
fn a_malformed_operand_names_the_argument_and_the_closed_set() {
    let mut server = ready(Fake::default());
    let refused = call(
        &mut server,
        "backend.search",
        &json!({ "query": "ferris", "limit": 900 }),
    );
    assert_eq!(refused["isError"], true);
    assert_eq!(refused["structuredContent"]["slug"], "usage");
    assert_eq!(refused["structuredContent"]["operand"], "limit");
    assert!(
        text_of(&refused).contains("`900`"),
        "the refused value is quoted back"
    );

    let unknown = call(
        &mut server,
        "backend.search",
        &json!({ "query": "ferris", "depth": 3 }),
    );
    assert_eq!(unknown["isError"], true);
    assert_eq!(unknown["structuredContent"]["operand"], "depth");
}

#[test]
fn an_affordance_is_the_exact_next_tool_call_an_agent_can_paste() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.related",
        &json!({ "coordinate": MISSING }),
    );
    assert_eq!(result["isError"], true);
    let fault = &result["structuredContent"];
    if let Some(call) = fault.get("call").filter(|value| !value.is_null()) {
        assert!(
            call["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("backend.")),
            "an affordance names a real tool: {call}"
        );
        assert!(text_of(&result).contains("→ `{"), "{}", text_of(&result));
    }
}

#[test]
fn an_unknown_tool_is_a_protocol_error_not_a_result() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({ "name": "backend.nope", "arguments": {} }),
    );
    assert_eq!(response["error"]["code"], -32602);
}

// ---------------------------------------------------------------------------
// the escape hatch and the typed lane
// ---------------------------------------------------------------------------

#[test]
fn the_references_tool_serves_occurrence_sites_with_source_spans() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        "backend.references",
        &json!({ "coordinate": "pkg::semantic::callee" }),
    );
    assert_eq!(result["isError"], false);
    let rendered = text_of(&result);
    assert!(
        rendered.contains("pkg::semantic::caller"),
        "the referencing site must be named: {rendered}"
    );
    assert!(
        rendered.contains("src/main.rs [bytes40..46)"),
        "the captured source span must be present: {rendered}"
    );
    assert!(
        rendered.to_lowercase().contains("compiler"),
        "the authority class is provenance: {rendered}"
    );
}

#[test]
fn the_surface_escape_hatch_returns_the_typed_reply_and_a_readable_block() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        SURFACE_TOOL,
        &json!({ "command": { "operation": "subscriptions" } }),
    );
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["structuredContent"]["surface"]["result"],
        "subscriptions"
    );
    assert!(text_of(&result).starts_with("# subscriptions"), "{result}");
}

#[test]
fn a_refused_surface_operation_reports_its_typed_cause() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({
            "name": SURFACE_TOOL,
            "arguments": { "command": serde_json::to_value(SurfaceCommand::Diff {
                from: backend_library::PackageReference::parse("pkg:cargo/example@1.0.0")
                    .expect("admit older package"),
                to: backend_library::PackageReference::parse("pkg:cargo/example@2.0.0")
                    .expect("admit newer package"),
            })
            .expect("encode the diff command") }
        }),
    );
    tool_failure(&response, "invalid-query");
    assert!(
        response["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("package is not indexed")),
        "{response}"
    );
}

#[test]
fn the_typed_query_lane_reports_its_terminal_and_revision() {
    let mut server = ready(Fake::default());
    let result = call(
        &mut server,
        QUERY_TOOL,
        &json!({ "query": "{ Declaration { coordinate @output } }" }),
    );
    assert_eq!(result["isError"], false);
    assert!(text_of(&result).starts_with("~query 0 row(s) · complete · revision "));
    assert_eq!(result["structuredContent"]["terminal"], "complete");
}

#[test]
fn graph_query_limits_are_rejected_before_the_product_boundary() {
    let mut server = ready(Fake::default());
    let oversized_query = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query": "x".repeat(backend_library::MAX_GRAPH_QUERY_BYTES + 1)
            }
        }),
    );
    tool_failure(&oversized_query, "usage");
    assert!(
        oversized_query["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|message| message.contains("query exceeds"))
    );
    assert_eq!(server.product.graph_query_calls, 0);

    let mut variables = Map::new();
    for index in 0..=backend_library::MAX_GRAPH_QUERY_FIELDS {
        variables.insert(format!("v{index}"), json!(index));
    }
    let too_many_variables = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query": "{ Declaration { coordinate @output } }",
                "variables": Value::Object(variables)
            }
        }),
    );
    tool_failure(&too_many_variables, "usage");
    assert!(
        too_many_variables["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|message| message.contains("field-count bound"))
    );
    assert_eq!(server.product.graph_query_calls, 0);

    let mut nested = Value::String("x".to_owned());
    for _ in 0..=backend_library::MAX_GRAPH_VALUE_DEPTH {
        nested = Value::Array(vec![nested]);
    }
    let too_deep = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query": "{ Declaration { coordinate @output } }",
                "variables": {"nested": nested}
            }
        }),
    );
    tool_failure(&too_deep, "usage");
    assert!(
        too_deep["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|message| message.contains("nesting bound"))
    );
    assert_eq!(server.product.graph_query_calls, 0);

    let too_large = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query": "{ Declaration { coordinate @output } }",
                "variables": {"text": "x".repeat(backend_library::MAX_GRAPH_VALUE_BYTES + 1)}
            }
        }),
    );
    tool_failure(&too_large, "usage");
    assert!(
        too_large["result"]["structuredContent"]["detail"]
            .as_str()
            .is_some_and(|message| message.contains("byte bound"))
    );
    assert_eq!(server.product.graph_query_calls, 0);
}

// ---------------------------------------------------------------------------
// resources
// ---------------------------------------------------------------------------

#[test]
fn resources_are_the_same_markdown_their_tools_return() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "resources/list", &json!({}));
    let resources = listed["result"]["resources"]
        .as_array()
        .expect("resources is an array");
    assert!(
        resources
            .iter()
            .all(|resource| resource["mimeType"] == "text/markdown"),
        "{listed}"
    );
    let templates = request(&mut server, "resources/templates/list", &json!({}));
    assert!(
        templates["result"]["resourceTemplates"]
            .as_array()
            .expect("templates")
            .iter()
            .any(|template| template["uriTemplate"] == "backend://outline/{path}"),
        "outlines remain addressable without claiming they exist: {templates}"
    );
    assert_eq!(
        server.product.probe_calls, 0,
        "metadata needs no owner read"
    );
    assert!(
        resources
            .iter()
            .any(|resource| resource["uri"] == "backend://tools/catalog"),
        "the capability-dependent route catalog is always addressable"
    );

    let workspace = request(
        &mut server,
        "resources/read",
        &json!({ "uri": "backend://workspace/current" }),
    );
    let text = workspace["result"]["contents"][0]["text"]
        .as_str()
        .expect("workspace resource text");
    let status = call(&mut server, "backend.status", &json!({}));
    assert_eq!(text, text_of(&status), "one renderer, one answer");
}

#[test]
fn tool_catalog_names_callable_routes_without_advertising_unready_surfaces() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let advertised: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(advertised.len(), 17);
    assert!(
        advertised
            .iter()
            .all(|name| tools::tool_route(name).is_some()),
        "every advertised schema has a registered route"
    );

    let catalog = request(
        &mut server,
        "resources/read",
        &json!({"uri":"backend://tools/catalog"}),
    );
    let text = catalog["result"]["contents"][0]["text"]
        .as_str()
        .expect("catalog text");
    for name in [
        "backend.dependencies",
        "backend.dependents",
        "backend.diff",
        "backend.package_profile",
        "backend.package_source_membership",
        "backend.package_versions",
        "backend.related",
        "backend.semantic_shapes",
        "backend.semantic_versions",
    ] {
        assert!(text.contains(&format!("`{name}`")), "catalog omits {name}");
        assert!(
            !advertised.contains(&name),
            "unready route {name} was listed"
        );
        assert!(
            tools::tool_route(name).is_some(),
            "catalog route {name} is not callable"
        );
    }
    let package_versions_schema = text
        .split("`backend.package_versions` —")
        .nth(1)
        .and_then(|entry| entry.split_once("```json\n"))
        .and_then(|(_, schema)| schema.split_once("\n```").map(|(schema, _)| schema))
        .and_then(|schema| serde_json::from_str::<Value>(schema).ok())
        .expect("catalog provides a parseable route input schema");
    assert_eq!(
        package_versions_schema["properties"]["package"]["type"],
        "string"
    );
    assert_eq!(package_versions_schema["required"], json!(["package"]));
    for name in [QUERY_TOOL, SURFACE_TOOL] {
        assert!(text.contains(&format!("`{name}`")), "catalog omits {name}");
        assert!(!advertised.contains(&name));
        assert!(tools::tool_route(name).is_some());
    }
    assert!(text.contains("`backend.index_await`"));
    assert_eq!(
        tools::tool_route("backend.index_await")
            .map(|route| matches!(route, tools::ToolRoute::RefusedIndexAwait)),
        Some(true)
    );
    assert!(tools::tool_route("backend.type_graph").is_none());
    assert!(!text.contains("backend.type_graph"));
    assert!(text.contains("backend://schema/query"));
}

#[test]
fn the_query_card_carries_worked_queries_over_the_declared_schema() {
    let mut server = ready(Fake::default());
    let card = request(
        &mut server,
        "resources/read",
        &json!({ "uri": "backend://schema/query" }),
    );
    let text = card["result"]["contents"][0]["text"]
        .as_str()
        .expect("schema card text")
        .to_owned();
    let queries = resources::worked_queries(&text);
    assert!(queries.len() >= 4, "the card must work through examples");
    for query in &queries {
        assert!(
            query.contains("@output"),
            "a worked query outputs something: {query}"
        );
        assert!(
            ["Declaration", "Project", "Item", "ExternalTarget"]
                .iter()
                .any(|edge| query.contains(edge)),
            "a worked query starts at a declared edge: {query}"
        );
    }
    assert!(text.contains("`coordinate` is the exact string"), "{text}");
}

#[test]
fn a_missing_resource_is_refused_rather_than_guessed() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "resources/read",
        &json!({ "uri": "backend://nope/x" }),
    );
    assert_eq!(response["error"]["code"], -32002);
}

#[test]
fn outline_resource_templates_preserve_readable_and_absent_projects() {
    let uri = "backend://outline/%2Fabs%2Fpolyglot";
    let mut published = ready(Fake::default());
    let readable = request(&mut published, "resources/read", &json!({"uri": uri}));
    assert!(
        readable["result"]["contents"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("ferris")),
        "{readable}"
    );

    let mut empty = ready(Fake {
        empty_outline: true,
        ..Fake::default()
    });
    let listed = request(&mut empty, "resources/list", &json!({}));
    let resources = listed["result"]["resources"].as_array().expect("list");
    assert!(
        !resources.iter().any(|resource| resource["uri"] == uri),
        "{listed}"
    );
    assert!(
        resources
            .iter()
            .any(|resource| resource["uri"] == "backend://workspace/current")
    );
    let missing = request(&mut empty, "resources/read", &json!({"uri": uri}));
    assert_eq!(missing["error"]["code"], -32002, "{missing}");
    assert_eq!(missing["error"]["data"]["kind"], "not-found", "{missing}");
    assert!(
        missing["error"]["data"]["detail"]
            .as_str()
            .is_some_and(|text| text.contains("no outline is published")),
        "{missing}"
    );
}

#[test]
fn resource_discovery_needs_no_owner_and_outline_reads_preserve_protocol_failure() {
    let mut offline = ready(Fake {
        offline: true,
        ..Fake::default()
    });
    for method in ["resources/list", "resources/templates/list"] {
        let listed = request(&mut offline, method, &json!({}));
        assert!(listed.get("error").is_none(), "{listed}");
    }
    assert_eq!(offline.product.probe_calls, 0);
    let mut server = ready(Fake {
        outline_error: Some(ClientError::Protocol("invalid outline proof".to_owned())),
        ..Fake::default()
    });
    let read = request(
        &mut server,
        "resources/read",
        &json!({
            "uri": "backend://outline/%2Fabs%2Fpolyglot"
        }),
    );
    assert_eq!(read["error"]["code"], -32603, "{read}");
    assert_eq!(read["error"]["data"]["kind"], "protocol", "{read}");
}

#[test]
fn tree_open_routes_record_the_initialized_mcp_client() {
    let mut server = ready(Fake::default());
    let opened = call(
        &mut server,
        "backend.tree_open",
        &json!({
            "subject": "declaration", "value": DECLARATION
        }),
    );
    assert_eq!(opened["isError"], false, "{opened}");
    let [
        SurfaceCommand::TreeOpen {
            opener: TreeOpener::Mcp(client),
            ..
        },
    ] = server.product.surface_commands.as_slice()
    else {
        panic!("typed MCP opener");
    };
    assert_eq!(client.as_str(), "test");
    assert!(text_of(&opened).contains("mcp"), "{opened}");
    let mut raw_command = server.product.surface_commands[0].clone();
    let SurfaceCommand::TreeOpen { opener, .. } = &mut raw_command else {
        panic!("tree open")
    };
    *opener = TreeOpener::Cli;
    let opened = call(&mut server, SURFACE_TOOL, &json!({"command": raw_command}));
    assert_eq!(opened["isError"], false, "{opened}");
    assert!(
        server
            .product
            .surface_commands
            .iter()
            .all(|command| matches!(
                command, SurfaceCommand::TreeOpen { opener: TreeOpener::Mcp(client), .. }
                if client.as_str() == "test"
            ))
    );
}

// ---------------------------------------------------------------------------
// protocol
// ---------------------------------------------------------------------------

#[test]
fn initialize_negotiates_only_the_library_supported_protocols() {
    for (offered, expected) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-03-26", "2025-03-26"),
        ("2025-11-25", MCP_PROTOCOL_VERSION),
        ("2026-07-28", MCP_PROTOCOL_VERSION),
    ] {
        let mut server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
        let response = request(
            &mut server,
            "initialize",
            &json!({
                "protocolVersion": offered, "capabilities": {},
                "clientInfo": {"name": "qa13", "version": "1"}
            }),
        );
        assert_eq!(
            response["result"]["protocolVersion"], expected,
            "{response}"
        );
        assert_eq!(server.product.probe_calls, 0);
    }
}

#[test]
fn initialized_notification_cannot_bypass_initialize() {
    let mut server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    let rejected = server
        .handle(br#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"backend://workspace/current"}}"#)
        .expect("pre-initialize requests receive an error");
    assert_eq!(rejected["error"]["code"], -32002);
    assert!(server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .is_some());
    let premature = server
        .handle(br#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#)
        .expect("requests before the acknowledgement receive an error");
    assert_eq!(premature["error"]["code"], -32002);
}

#[test]
fn cancellation_notifications_are_silent_and_do_not_poison_the_session() {
    let mut server = ready(Fake::default());
    assert!(server
        .handle(br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9,"reason":"client stopped waiting"}}"#)
        .is_none());
    let result = call(&mut server, "backend.search", &json!({ "query": "ferris" }));
    assert_eq!(result["isError"], false);
}

#[test]
fn deeply_nested_json_is_rejected_before_tool_admission() {
    let mut server = ready(Fake::default());
    let mut nested = Value::String("x".to_owned());
    for _ in 0..=super::codec::MAX_JSON_DEPTH {
        nested = Value::Array(vec![nested]);
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/call",
        "params": {
            "name": QUERY_TOOL,
            "arguments": {
                "query": "{ Declaration { coordinate @output } }",
                "variables": { "nested": nested }
            }
        }
    });
    let response = server
        .handle(&serde_json::to_vec(&request).expect("nested request is JSON"))
        .expect("invalid requests receive a response");
    assert_eq!(response["error"]["code"], -32600);
    assert_eq!(server.product.graph_query_calls, 0);
}

#[test]
fn the_handshake_reports_the_stable_protocol_and_its_instructions() {
    let mut server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
    let initialized = server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .expect("initialize response");
    assert_eq!(
        initialized["result"]["protocolVersion"],
        MCP_PROTOCOL_VERSION
    );
    let instructions = initialized["result"]["instructions"]
        .as_str()
        .unwrap_or_default();
    assert!(instructions.contains("backend.index"), "{instructions}");
    assert!(instructions.contains("absolute path"), "{instructions}");
    assert!(instructions.contains("backend.document"), "{instructions}");
    assert!(instructions.contains("nudox add ."), "{instructions}");
    assert!(
        instructions.contains("nudox search \"error handling\""),
        "{instructions}"
    );
    assert!(
        instructions.contains("claude mcp add --scope user --transport stdio nudox"),
        "{instructions}"
    );
    assert!(
        instructions.contains("--project '${CLAUDE_PROJECT_DIR:-.}'"),
        "{instructions}"
    );
    assert!(
        instructions.contains("claude mcp get nudox"),
        "{instructions}"
    );
    assert!(instructions.contains("backend.package"), "{instructions}");
    assert!(
        instructions.contains("backend.index_search"),
        "{instructions}"
    );
    assert!(
        instructions.contains("backend://workspace/current"),
        "{instructions}"
    );
}

#[test]
fn an_explicit_project_is_active_even_when_the_launch_directory_differs() {
    let mut server = Server::with_invocation_context(
        Fake::default(),
        PROJECT.to_owned(),
        Some("/another/checkout".to_owned()),
        [9; 32],
    );
    let initialized = server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .expect("initialize response");
    let instructions = initialized["result"]["instructions"]
        .as_str()
        .unwrap_or_default();
    assert!(
        instructions.contains("selected project: /abs/polyglot"),
        "{instructions}"
    );
    assert!(
        instructions.contains("selected project is active"),
        "{instructions}"
    );
    assert!(instructions.contains("process working directory: /another/checkout"));
    assert!(!instructions.contains("configuration mismatch"));

    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    let status = request(
        &mut server,
        "resources/read",
        &json!({ "uri": "backend://workspace/current" }),
    );
    let text = status["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("selected project: /abs/polyglot"), "{text}");
    assert!(text.contains("selected project is active"), "{text}");
    assert!(!text.contains("configuration mismatch"), "{text}");
    let indexed = call(&mut server, "backend.index", &json!({"path": PROJECT}));
    assert_eq!(indexed["isError"], false, "{indexed}");
    assert_eq!(server.product.index_paths, [PROJECT]);
}

#[test]
fn newline_codec_is_bounded_and_compact() {
    let mut reader = io::BufReader::new(&b"{}\n"[..]);
    assert_eq!(read_line(&mut reader).expect("read"), Some(b"{}".to_vec()));
    let mut output = Vec::new();
    write_message(&mut output, &json!({"jsonrpc":"2.0","id":1,"result":{}})).expect("write");
    assert_eq!(output.last(), Some(&b'\n'));
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
}

#[test]
fn newline_codec_rejects_malformed_and_oversized_jsonrpc_frames() {
    let mut oversized = vec![b'x'; crate::MAX_MCP_REQUEST_FRAME + 1];
    oversized.push(b'\n');
    let mut reader = io::BufReader::new(oversized.as_slice());
    assert!(read_line(&mut reader).is_err());

    let unterminated = vec![b'x'; crate::MAX_MCP_REQUEST_FRAME + 1];
    let mut reader = io::BufReader::new(unterminated.as_slice());
    assert!(read_line(&mut reader).is_err());

    let mut output = Vec::new();
    let oversized_result = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": "x".repeat(crate::MAX_MCP_RESPONSE_FRAME + 1)
    });
    write_message(&mut output, &oversized_result).expect("oversized reply becomes bounded fault");
    assert!(output.len() <= DEFAULT_RESPONSE_BUDGET_BYTES);
    let reply: Value = serde_json::from_slice(&output).expect("bounded refusal");
    assert_eq!(reply["error"]["code"], -32000);
    assert_eq!(reply["id"], 1);
}

#[test]
#[cfg(feature = "token-budget")]
fn bounded_response_serialization_preserves_the_calling_request_id() {
    let id = json!("request-λ-42");
    let response = token_budget_rpc_response(id.clone(), "ok", json!({"answer":"product"}), false);
    assert_eq!(response["id"], id);
    assert_eq!(response["result"]["isError"], false);
}

#[test]
fn continuation_authority_binds_context_and_owner_payload() {
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [7; 32]);
    let token = server
        .sign_cursor_token("pc1-owner-issued", b"query-a")
        .expect("admitted key");
    assert_eq!(
        server
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .as_deref(),
        Some("pc1-owner-issued")
    );
    // Verification is deliberately replayable: retrying a read page is safe,
    // while the MAC still prevents moving that page to another authority
    // context. This is the property a reconnecting MCP client needs.
    assert_eq!(
        server
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .as_deref(),
        Some("pc1-owner-issued")
    );
    assert!(
        server
            .verify_cursor_token(&token, b"query-b")
            .expect("admitted key")
            .is_none()
    );
    let mut tampered = token.clone();
    let final_byte = tampered.pop().expect("MAC byte");
    tampered.push(if final_byte == '0' { '1' } else { '0' });
    assert!(
        server
            .verify_cursor_token(&tampered, b"query-a")
            .expect("admitted key")
            .is_none()
    );
    let other_workspace = Server::with_authority(Fake::default(), "/other".to_owned(), [8; 32]);
    assert!(
        other_workspace
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .is_none()
    );
    // Restarting with the persisted authority accepts the token; rotation
    // invalidates every old token immediately, including one with identical
    // workspace and query context.
    let restarted = Server::with_authority(Fake::default(), PROJECT.to_owned(), [7; 32]);
    assert_eq!(
        restarted
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .as_deref(),
        Some("pc1-owner-issued")
    );
    let rotated = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
    assert!(
        rotated
            .verify_cursor_token(&token, b"query-a")
            .expect("admitted key")
            .is_none()
    );
    let expired = server
        .sign_cursor_token_at(unix_seconds().saturating_sub(1), "pc1-old", b"query-a")
        .expect("admitted key");
    assert!(
        server
            .verify_cursor_token(&expired, b"query-a")
            .expect("admitted key")
            .is_none()
    );
    let expires_now = server
        .sign_cursor_token_at(unix_seconds(), "pc1-now", b"query-a")
        .expect("admitted key");
    assert!(
        server
            .verify_cursor_token(&expires_now, b"query-a")
            .expect("admitted key")
            .is_none()
    );
}

#[test]
fn continuation_context_binds_workspace_query_limit_and_detail() {
    let mut first = Map::new();
    first.insert("query".to_owned(), json!("ferris"));
    first.insert("limit".to_owned(), json!(25));
    let mut second = first.clone();
    second.insert("query".to_owned(), json!("beacon"));
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context(PROJECT, "backend.search", &second, Detail::Summary)
    );
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context(PROJECT, "backend.search", &first, Detail::Full)
    );
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context(PROJECT, "backend.document", &first, Detail::Summary)
    );
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context("/other", "backend.search", &first, Detail::Summary)
    );
    let mut spaced = first.clone();
    spaced.insert("query".to_owned(), json!("  ferris   "));
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &first, Detail::Summary),
        continuation_context(PROJECT, "backend.search", &spaced, Detail::Summary)
    );
    let mut different_limit = first;
    different_limit.insert("limit".to_owned(), json!(26));
    assert_ne!(
        continuation_context(PROJECT, "backend.search", &spaced, Detail::Summary),
        continuation_context(PROJECT, "backend.search", &different_limit, Detail::Summary)
    );
}

#[test]
fn every_metadata_route_stays_within_the_context_budget() {
    let mut server = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
    let initialized = server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
        .expect("initialize response");
    assert_context_bounded(&initialized);
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );

    let routes = [
        ("ping", json!({})),
        ("tools/list", json!({})),
        ("resources/list", json!({})),
        ("resources/templates/list", json!({})),
        ("prompts/list", json!({})),
        (
            "prompts/get",
            json!({"name":"backend.explore","arguments":{"query":"λ אב"}}),
        ),
        (
            "resources/read",
            json!({"uri":"backend://workspace/current"}),
        ),
        ("resources/read", json!({"uri":"backend://schema/query"})),
        (
            "tools/call",
            json!({"name":"backend.packages","arguments":{}}),
        ),
        (
            "tools/call",
            json!({"name":"backend.search","arguments":{"query":"λ אב"}}),
        ),
        (
            "tools/call",
            json!({"name":"backend.document","arguments":{"coordinate":MODULE}}),
        ),
        (
            "tools/call",
            json!({"name":QUERY_TOOL,"arguments":{"query":"{ Declaration { coordinate @output } }"}}),
        ),
    ];
    for (method, params) in routes {
        let response = request(&mut server, method, &params);
        assert_context_bounded(&response);
    }
    let huge_prompt = json!({
        "jsonrpc":"2.0",
        "id": "id-".to_owned() + &"אב".repeat(DEFAULT_RESPONSE_BUDGET_BYTES),
        "method":"prompts/get",
        "params":{"name":"backend.explore","arguments":{"query":"long"}}
    });
    let response = server
        .handle(&serde_json::to_vec(&huge_prompt).expect("large prompt request"))
        .expect("large prompt response");
    assert_context_bounded(&encoded_stdio_reply(response));
}

#[test]
fn wire_budget_metadata_counts_the_complete_escaped_jsonrpc_line() {
    let source_text = "λ אב🙂\n".repeat(DEFAULT_RESPONSE_BUDGET_BYTES);
    let structured = json!({
        "answer":"records",
        "records":[{"coordinate":"/selected/current"}],
        "more":true,
        "nextCursor":"opaque-owner-cursor-page-two",
        "budget":{"bytes":34050,"estimatedTokens":8513,"bytesPerToken":4}
    });
    let result = tool_result(&source_text, structured.clone(), false);
    let response = encoded_stdio_reply(success(json!("request-אב🙂"), result));
    assert_context_bounded(&response);
    assert_eq!(response["result"]["structuredContent"], structured);
    assert_eq!(response["result"]["isError"], false);

    let preview = text_of(&response["result"]);
    assert!(preview.starts_with("λ אב🙂\n"));
    assert!(preview.len() < source_text.len());
    assert!(preview.ends_with(
        "… readable preview shortened to fit this response; structuredContent contains the complete page and any nextCursor."
    ));

    let wire_budget = &response["result"]["_meta"]["backend/wireBudget"];
    let actual_bytes = serde_json::to_vec(&response)
        .expect("response serializes")
        .len()
        .saturating_add(1);
    assert_eq!(wire_budget["bytes"], actual_bytes);
    assert_eq!(
        wire_budget["estimatedTokens"],
        estimate_tokens(actual_bytes)
    );
    assert_eq!(wire_budget["bytesPerToken"], ESTIMATED_BYTES_PER_TOKEN);
    assert_eq!(wire_budget["limitBytes"], DEFAULT_RESPONSE_BUDGET_BYTES);
    assert_eq!(wire_budget["scope"], "complete_jsonrpc_line");

    let large_id = format!("request-{}", "אב🙂".repeat(2_000));
    let large_structured = json!({
        "answer":"records",
        "rows":"s".repeat(31_000),
        "more":true,
        "nextCursor":"opaque-owner-cursor-page-two"
    });
    let large_text = "λ אב🙂\n".repeat(6_000);
    let large_result = tool_result(&large_text, large_structured.clone(), false);
    let large_response = encoded_stdio_reply(success(json!(large_id.clone()), large_result));
    assert_eq!(large_response["id"], large_id);
    assert_eq!(large_response["result"]["isError"], false);
    assert_eq!(
        large_response["result"]["structuredContent"],
        large_structured
    );
    assert!(text_of(&large_response["result"]).ends_with(
        "… readable preview shortened to fit this response; structuredContent contains the complete page and any nextCursor."
    ));
    assert_context_bounded(&large_response);
    let large_wire_bytes = serde_json::to_vec(&large_response)
        .expect("large-id response serializes")
        .len()
        .saturating_add(1);
    assert_eq!(
        large_response["result"]["_meta"]["backend/wireBudget"]["bytes"],
        large_wire_bytes
    );
}

#[test]
fn graph_continuation_round_trip_is_bounded_and_authorized() {
    for limit in [1, 200] {
        let mut server = ready(Fake {
            graph_continue: true,
            ..Fake::default()
        });
        let first = call(
            &mut server,
            QUERY_TOOL,
            &json!({"query":"{ Declaration { coordinate @output } }","limit":limit}),
        );
        assert_context_bounded(&first);
        assert_eq!(first["isError"], false);
        assert_eq!(first["structuredContent"]["terminal"], "limit_reached");
        let cursor = first["structuredContent"]["nextCursor"]
            .as_str()
            .expect("first graph page carries a cursor")
            .to_owned();
        let preview = text_of(&first);
        assert!(
            preview.contains(
                "Continue `backend.query` with the same `query`, `variables`, and `limit`"
            )
        );
        assert!(preview.contains("copy `structuredContent.nextCursor` into `arguments.cursor`"));
        assert!(preview.contains(&format!("`{cursor}`")));
        assert!(!preview.contains("raise `limit`"), "{preview}");
        assert!(!preview.contains("narrow the query"), "{preview}");

        let second = call(
            &mut server,
            QUERY_TOOL,
            &json!({
                "query":"{ Declaration { coordinate @output } }",
                "limit":limit,
                "cursor":cursor
            }),
        );
        assert_context_bounded(&second);
        assert_eq!(second["isError"], false);
        assert_eq!(second["structuredContent"]["terminal"], "complete");
    }
}

#[test]
fn graph_page_text_does_not_treat_cancellation_or_missing_cursor_as_limit_advice() {
    let cancelled = GraphQueryPage {
        revision: view_state_root(&[]).into(),
        source: basis().object,
        rows: Box::new([]),
        terminal: PageTerminal::Cancelled,
    };
    let cancelled_text = super::graph_page_text(&cancelled, None);
    assert!(cancelled_text.contains("query cancelled before completion"));
    assert!(!cancelled_text.contains("raise `limit`"));
    assert!(!cancelled_text.contains("more rows"));

    let more_without_token = GraphQueryPage {
        terminal: PageTerminal::More(PageContinuation::from_cursor(backend_library::Cursor::new())),
        ..cancelled
    };
    let unavailable_text = super::graph_page_text(&more_without_token, None);
    assert!(unavailable_text.contains("no continuation cursor was issued"));
    assert!(!unavailable_text.contains("raise `limit`"));
}

#[test]
fn surface_index_search_cursor_round_trips_through_the_mcp_projection() {
    let mut first_server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let first = call(
        &mut first_server,
        SURFACE_TOOL,
        &json!({
            "command": {
                "operation": "index-search",
                "query": "maven",
                "limit": 1
            },
            "detail": "full"
        }),
    );
    assert_eq!(first["isError"], false);
    assert_context_bounded(&first);
    let cursor = first["structuredContent"]["surface"]["data"]["next_cursor"]
        .as_str()
        .expect("index-search page carries a nested cursor")
        .to_owned();
    assert!(cursor.starts_with("mcp1-"));
    assert!(!cursor.contains("maven-owner-v4"));
    assert!(text_of(&first).contains(&cursor));
    assert!(text_of(&first).contains("`command.cursor`"));
    assert!(!text_of(&first).contains("`--cursor`"));
    assert!(
        !serde_json::to_string(&first)
            .expect("tool result JSON")
            .contains("maven-owner-v4")
    );

    let structured = &first["structuredContent"];
    assert_eq!(structured["surface"]["result"], "index-search-page");
    let measured = structured["budget"]["bytes"]
        .as_u64()
        .expect("typed surface carries measured bytes");
    let measured = usize::try_from(measured).expect("measured bytes fit this host");
    assert_eq!(
        serde_json::to_vec(structured)
            .expect("structured result JSON")
            .len(),
        measured,
        "the signed cursor is inside the budgeted typed projection"
    );
    let final_reply_bytes = serde_json::to_vec(&first)
        .expect("complete MCP response including Markdown")
        .len();
    assert!(
        final_reply_bytes <= DEFAULT_RESPONSE_BUDGET_BYTES,
        "structured surface and signed-token Markdown fit together: {final_reply_bytes}"
    );

    // The server-side MAC is portable across an MCP process restart when its
    // authority secret and project selection are unchanged.
    let mut restarted = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let second = call(
        &mut restarted,
        SURFACE_TOOL,
        &json!({
            "command": {
                "operation": "index-search",
                "query": "maven",
                "limit": 1,
                "cursor": cursor
            },
            "detail": "full"
        }),
    );
    assert_eq!(second["isError"], false);
    assert_eq!(
        restarted.product.surface_index_search_seen,
        vec![Some("maven-owner-v4".to_owned())]
    );
    assert!(second["structuredContent"]["surface"]["data"]["next_cursor"].is_null());
    assert!(!text_of(&second).contains("next cursor"));
}

#[test]
fn index_search_tool_round_trips_mcp_cursors_at_summary_and_full_detail() {
    for detail in ["summary", "full"] {
        let mut server = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        let first = call(
            &mut server,
            "backend.index_search",
            &json!({"query":"maven","limit":1,"detail":detail}),
        );
        assert_eq!(first["isError"], false, "detail={detail}");
        assert_context_bounded(&first);
        let cursor = first["structuredContent"]["index_search_page"]["next_cursor"]
            .as_str()
            .expect("product page carries the nested cursor")
            .to_owned();
        assert!(cursor.starts_with("mcp1-"));
        assert_eq!(
            first["structuredContent"]["nextCursor"].as_str(),
            Some(cursor.as_str())
        );
        assert!(text_of(&first).contains(&cursor));
        assert!(text_of(&first).contains("`backend.index_search`"));
        assert!(
            !serde_json::to_string(&first)
                .expect("tool result JSON")
                .contains("maven-owner-v4")
        );
        let structured = &first["structuredContent"];
        let measured = usize::try_from(
            structured["budget"]["bytes"]
                .as_u64()
                .expect("typed product carries measured bytes"),
        )
        .expect("measured bytes fit this host");
        assert_eq!(
            serde_json::to_vec(structured)
                .expect("structured product JSON")
                .len(),
            measured,
            "both signed cursor projections are inside the measured budget"
        );

        let second = call(
            &mut server,
            "backend.index_search",
            &json!({
                "query":"maven",
                "limit":1,
                "detail":detail,
                "cursor":cursor
            }),
        );
        assert_eq!(second["isError"], false, "detail={detail}");
        assert_eq!(
            server.product.surface_index_search_seen,
            vec![None, Some("maven-owner-v4".to_owned())]
        );
        assert!(
            second["structuredContent"]["index_search_page"]
                .get("next_cursor")
                .is_none()
        );
        assert!(second["structuredContent"].get("nextCursor").is_none());
        assert!(!text_of(&second).contains("next cursor"));

        // The MCP authority signature survives a process reconstruction when
        // the persisted secret and selected project are unchanged.
        let mut restarted = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        let after_restart = call(
            &mut restarted,
            "backend.index_search",
            &json!({
                "query":"maven",
                "limit":1,
                "detail":detail,
                "cursor":cursor
            }),
        );
        assert_eq!(after_restart["isError"], false, "detail={detail}");
        assert_eq!(
            restarted.product.surface_index_search_seen,
            vec![Some("maven-owner-v4".to_owned())]
        );
        assert!(
            after_restart["structuredContent"]
                .get("nextCursor")
                .is_none()
        );
    }
}

#[test]
fn index_search_cursor_round_trips_between_servers_with_the_same_workspace_authority() {
    let mut first_server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let first = call(
        &mut first_server,
        "backend.index_search",
        &json!({"query":"maven","limit":1,"detail":"summary"}),
    );
    let cursor = first["structuredContent"]["index_search_page"]["next_cursor"]
        .as_str()
        .expect("first page carries a signed cursor")
        .to_owned();

    // MCP server processes reconstructed from the same workspace authority
    // and exact project/query context can resume durable owner cursors.
    let mut same_authority = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let resumed = call(
        &mut same_authority,
        "backend.index_search",
        &json!({
            "query":"maven",
            "limit":1,
            "detail":"summary",
            "cursor":cursor
        }),
    );
    assert_eq!(resumed["isError"], false);
    assert_eq!(
        same_authority.product.surface_index_search_seen,
        vec![Some("maven-owner-v4".to_owned())]
    );

    // The same token cannot be replayed under a different workspace key or
    // project selection, and neither rejected request reaches the owner.
    for mismatch in ["authority", "project"] {
        let mut other = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        match mismatch {
            "authority" => other.cursor_secret = [8; 32].into(),
            "project" => other.project = "/other-project".to_owned(),
            _ => unreachable!(),
        }
        let response = request(
            &mut other,
            "tools/call",
            &json!({
                "name":"backend.index_search",
                "arguments":{
                    "query":"maven",
                    "limit":1,
                    "detail":"summary",
                    "cursor":cursor
                }
            }),
        );
        tool_failure(&response, "usage");
        assert_eq!(
            response["result"]["structuredContent"]["detail"],
            "cursor is unknown, expired, or belongs to another workspace authority"
        );
        assert!(other.product.surface_index_search_seen.is_empty());
    }
}

#[test]
fn oversized_signed_index_search_cursor_projection_is_refused_without_truncation() {
    let owner_cursor = "x".repeat(26 * 1024);
    let mut server = ready(Fake {
        surface_index_search_pages: true,
        surface_index_search_owner_cursor: Some(owner_cursor.clone()),
        ..Fake::default()
    });
    let response = call(
        &mut server,
        "backend.index_search",
        &json!({"query":"maven","limit":1,"detail":"summary"}),
    );
    assert_context_bounded(&response);
    assert_eq!(response["isError"], true);
    assert_eq!(response["structuredContent"]["answer"], "fault");
    assert_eq!(response["structuredContent"]["cause"], "oversized");
    assert!(
        !serde_json::to_string(&response)
            .expect("refusal JSON")
            .contains(&owner_cursor)
    );
    assert!(!text_of(&response).contains(&owner_cursor));
}

#[test]
fn index_search_tool_cursor_rejects_context_changes_and_raw_owner_tokens() {
    for mutation in [
        "query",
        "limit",
        "detail",
        "project",
        "authority",
        "tamper",
        "expired",
        "raw",
    ] {
        let mut server = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        let first = call(
            &mut server,
            "backend.index_search",
            &json!({"query":"maven","limit":1,"detail":"summary"}),
        );
        let cursor = first["structuredContent"]["nextCursor"]
            .as_str()
            .expect("first page carries a signed cursor")
            .to_owned();
        let mut arguments = json!({
            "query": "maven",
            "limit": 1,
            "detail": "summary",
            "cursor": cursor
        });
        match mutation {
            "query" => arguments["query"] = json!("spring"),
            "limit" => arguments["limit"] = json!(2),
            "detail" => arguments["detail"] = json!("full"),
            "project" => server.project = "/other-project".to_owned(),
            "authority" => server.cursor_secret = [8; 32].into(),
            "tamper" => {
                let token = arguments["cursor"].as_str().expect("cursor token");
                let mut changed = token.to_owned();
                let final_byte = changed.pop().expect("MAC byte");
                changed.push(if final_byte == '0' { '1' } else { '0' });
                arguments["cursor"] = json!(changed);
            }
            "expired" => {
                let context_arguments = json!({"query":"maven","limit":1,"detail":"summary"});
                let context = continuation_context(
                    PROJECT,
                    "backend.index_search",
                    context_arguments.as_object().expect("tool arguments"),
                    Detail::Summary,
                );
                arguments["cursor"] = json!(
                    server
                        .sign_cursor_token_at(
                            unix_seconds().saturating_sub(1),
                            "maven-owner-v4",
                            &context
                        )
                        .expect("admitted key")
                );
            }
            "raw" => arguments["cursor"] = json!("maven-owner-v4"),
            _ => unreachable!(),
        }
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.index_search","arguments":arguments}),
        );
        tool_failure(&response, "usage");
        assert_eq!(server.product.surface_index_search_seen.len(), 1);
    }
}

#[test]
fn index_search_cursor_cannot_cross_between_named_and_generic_mcp_tools() {
    let mut named = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let named_first = call(
        &mut named,
        "backend.index_search",
        &json!({"query":"maven","limit":1}),
    );
    let named_cursor = named_first["structuredContent"]["nextCursor"]
        .as_str()
        .expect("named tool cursor")
        .to_owned();
    let generic_replay = request(
        &mut named,
        "tools/call",
        &json!({
            "name": SURFACE_TOOL,
            "arguments": {
                "command": {
                    "operation": "index-search",
                    "query": "maven",
                    "limit": 1,
                    "cursor": named_cursor
                }
            }
        }),
    );
    tool_failure(&generic_replay, "usage");
    assert_eq!(named.product.surface_index_search_seen.len(), 1);

    let mut generic = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let generic_first = call(
        &mut generic,
        SURFACE_TOOL,
        &json!({"command":{"operation":"index-search","query":"maven","limit":1}}),
    );
    let generic_cursor = generic_first["structuredContent"]["surface"]["data"]["next_cursor"]
        .as_str()
        .expect("generic surface cursor")
        .to_owned();
    let named_replay = request(
        &mut generic,
        "tools/call",
        &json!({
            "name": "backend.index_search",
            "arguments": {"query":"maven","limit":1,"cursor":generic_cursor}
        }),
    );
    tool_failure(&named_replay, "usage");
    assert_eq!(generic.product.surface_index_search_seen.len(), 1);
}

#[test]
fn stale_named_index_search_cursor_is_a_restartable_refusal() {
    let mut server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let first = call(
        &mut server,
        "backend.index_search",
        &json!({"query":"maven","limit":1}),
    );
    let cursor = first["structuredContent"]["nextCursor"]
        .as_str()
        .expect("first page carries a cursor")
        .to_owned();
    server.product.surface_index_search_stale = true;
    let response = call(
        &mut server,
        "backend.index_search",
        &json!({"query":"maven","limit":1,"cursor":cursor}),
    );
    assert_eq!(response["isError"], true);
    assert_eq!(response["structuredContent"]["answer"], "fault");
    assert_eq!(response["structuredContent"]["slug"], "cursor-mismatch");
    assert!(text_of(&response).contains("restart the query"));
    assert_eq!(
        server.product.surface_index_search_seen,
        vec![None, Some("maven-owner-v4".to_owned())]
    );
}

#[test]
fn surface_index_search_cursor_binds_query_limit_project_and_detail() {
    for mutation in [
        "query",
        "limit",
        "detail",
        "project",
        "authority",
        "tamper",
        "expired",
        "raw",
    ] {
        let mut server = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        let first_command = json!({
            "operation": "index-search",
            "query": "maven",
            "limit": 1
        });
        let first = call(
            &mut server,
            SURFACE_TOOL,
            &json!({
                "command": first_command.clone(),
                "detail": "summary"
            }),
        );
        let cursor = first["structuredContent"]["surface"]["data"]["next_cursor"]
            .as_str()
            .expect("index-search page carries a nested cursor")
            .to_owned();
        let mut command = json!({
            "operation": "index-search",
            "query": "maven",
            "limit": 1,
            "cursor": cursor
        });
        let mut detail = "summary";
        match mutation {
            "query" => command["query"] = json!("spring"),
            "limit" => command["limit"] = json!(2),
            "detail" => detail = "full",
            "project" => server.project = "/other-project".to_owned(),
            "authority" => server.cursor_secret = [8; 32].into(),
            "tamper" => {
                let token = command["cursor"].as_str().expect("cursor token");
                let mut changed = token.to_owned();
                let final_byte = changed.pop().expect("MAC byte");
                changed.push(if final_byte == '0' { '1' } else { '0' });
                command["cursor"] = json!(changed);
            }
            "expired" => {
                let context_arguments_value = json!({"command": first_command.clone()});
                let context_arguments = context_arguments_value
                    .as_object()
                    .expect("surface arguments");
                let context =
                    continuation_context(PROJECT, SURFACE_TOOL, context_arguments, Detail::Summary);
                command["cursor"] = json!(
                    server
                        .sign_cursor_token_at(
                            unix_seconds().saturating_sub(1),
                            "maven-owner-v4",
                            &context
                        )
                        .expect("admitted key")
                );
            }
            "raw" => command["cursor"] = json!("maven-owner-v4"),
            _ => unreachable!(),
        }
        let response = request(
            &mut server,
            "tools/call",
            &json!({
                "name": SURFACE_TOOL,
                "arguments": {"command": command, "detail": detail}
            }),
        );
        tool_failure(&response, "usage");
        assert_eq!(server.product.surface_index_search_seen.len(), 1);
    }
}

#[test]
fn stale_surface_index_search_cursor_is_reported_as_restartable() {
    let mut first_server = ready(Fake {
        surface_index_search_pages: true,
        ..Fake::default()
    });
    let first = call(
        &mut first_server,
        SURFACE_TOOL,
        &json!({
            "command": {
                "operation": "index-search",
                "query": "maven",
                "limit": 1
            }
        }),
    );
    let cursor = first["structuredContent"]["surface"]["data"]["next_cursor"]
        .as_str()
        .expect("first page carries a cursor")
        .to_owned();
    let mut stale_owner = ready(Fake {
        surface_index_search_pages: true,
        surface_index_search_stale: true,
        ..Fake::default()
    });
    let response = request(
        &mut stale_owner,
        "tools/call",
        &json!({
            "name": SURFACE_TOOL,
            "arguments": {
                "command": {
                    "operation": "index-search",
                    "query": "maven",
                    "limit": 1,
                    "cursor": cursor
                }
            }
        }),
    );
    let fault = tool_failure(&response, "cursor-mismatch");
    assert_eq!(fault["cause"], "moved");
    assert!(text_of(&response["result"]).contains("restart the query"));
}

#[test]
fn stale_graph_roots_are_reported_as_a_restartable_cursor_error() {
    let mut server = ready(Fake {
        graph_continue: true,
        ..Fake::default()
    });
    let first = call(
        &mut server,
        QUERY_TOOL,
        &json!({"query":"{ Declaration { coordinate @output } }","limit":1}),
    );
    let cursor = first["structuredContent"]["nextCursor"]
        .as_str()
        .expect("first page carries a cursor")
        .to_owned();
    server.product.stale_cursor = true;
    let response = request(
        &mut server,
        "tools/call",
        &json!({
            "name": QUERY_TOOL,
            "arguments": {
                "query":"{ Declaration { coordinate @output } }",
                "limit":1,
                "cursor":cursor
            }
        }),
    );
    let fault = tool_failure(&response, "cursor-mismatch");
    assert_eq!(fault["cause"], "moved");
    assert!(text_of(&response["result"]).contains("restart the query"));
}

#[test]
fn high_fanout_graph_and_surface_pages_refuse_atomically() {
    let rows = (0..256)
        .map(|index| {
            let mut fields = std::collections::BTreeMap::new();
            fields.insert(
                "coordinate".to_owned(),
                GraphValue::String(format!("pkg::module_{index}::{}", "λאב".repeat(96))),
            );
            fields.insert(
                "summary".to_owned(),
                GraphValue::String("high fanout graph record ".repeat(32)),
            );
            GraphQueryRow::new(fields).expect("admitted graph row")
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let mut server = ready(Fake {
        graph_rows: Some(rows),
        ..Fake::default()
    });
    let graph = call(
        &mut server,
        QUERY_TOOL,
        &json!({"query":"{ Declaration { coordinate @output } }"}),
    );
    assert_context_bounded(&graph);
    assert_eq!(graph["isError"], true);
    assert_eq!(graph["structuredContent"]["cause"], "oversized");

    let references = (0..256)
        .map(|index| backend_library::ReferenceRecord {
            site: backend_library::ProductText::new(format!(
                "pkg::caller_{index}::{}",
                "x".repeat(160)
            ))
            .expect("site text"),
            target: backend_library::SemanticLinkTarget::Local {
                declaration: backend_library::SemanticDeclarationIdentity {
                    family: [1; 16],
                    variant: [2; 16],
                },
            },
            relation: backend_library::SemanticLinkKind::Calls,
            evidence: backend_library::SemanticLinkEvidence {
                confidence: backend_library::SemanticConfidence::Compiler,
                source: Some(backend_library::SemanticSourceSpan {
                    file: backend_library::ProductText::new("src/main.rs").expect("path text"),
                    start: index,
                    end: index + 1,
                }),
            },
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::References {
            target: backend_library::ProductText::new("pkg::semantic::callee")
                .expect("target text"),
            references,
        }),
        ..Fake::default()
    });
    let surface = call(
        &mut server,
        SURFACE_TOOL,
        &json!({
            "command": serde_json::to_value(SurfaceCommand::References {
                target: backend_library::ProductText::new("pkg::semantic::callee")
                    .expect("target text")
            }).expect("encode references command")
        }),
    );
    assert_context_bounded(&surface);
    assert_eq!(surface["isError"], true);
    assert_eq!(surface["structuredContent"]["cause"], "oversized");
}

#[test]
fn recognized_tools_share_typed_missing_input_results_before_any_owner_call() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let mut required_tools = listed["result"]["tools"]
        .as_array()
        .expect("tool catalog")
        .iter()
        .filter(|tool| {
            tool["inputSchema"]["required"]
                .as_array()
                .is_some_and(|required| !required.is_empty())
        })
        .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
        .collect::<Vec<_>>();
    required_tools.extend([QUERY_TOOL.to_owned(), SURFACE_TOOL.to_owned()]);
    assert!(
        required_tools.len() >= 8,
        "exercise advertised and catalog-only required operands"
    );
    for name in required_tools {
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":name, "arguments":{}}),
        );
        let fault = tool_failure(&response, "usage");
        assert_eq!(fault["cause"], "malformed", "{name}: {response}");
        assert!(
            !fault["detail"]
                .as_str()
                .expect("required input guidance")
                .is_empty()
        );
        assert_eq!(server.product.probe_calls, 0, "{name}");
        assert_eq!(server.product.graph_query_calls, 0, "{name}");
        assert!(server.product.index_paths.is_empty(), "{name}");
        assert!(server.product.surface_commands.is_empty(), "{name}");
    }
}

#[test]
fn malformed_call_tool_envelopes_and_unknown_routes_remain_rpc_errors() {
    let mut server = ready(Fake::default());
    for params in [
        json!([]),
        json!({}),
        json!({"name":7}),
        json!({"name":"backend.index", "arguments":null}),
        json!({"name":"backend.index", "arguments":[]}),
        json!({"name":"backend.index", "arguments":"bad envelope"}),
        json!({"name":"backend.not-a-tool", "arguments":{}}),
    ] {
        let response = request(&mut server, "tools/call", &params);
        assert_eq!(response["error"]["code"], -32602, "{params}: {response}");
        assert!(response.get("result").is_none(), "{params}: {response}");
        assert_eq!(response["id"], 9);
        assert_eq!(server.product.probe_calls, 0);
        assert!(server.product.surface_commands.is_empty());
    }
}

#[test]
fn authenticated_continuation_decode_preserves_domain_and_server_fault_distinctions() {
    for (error, slug, protocol) in [
        (ClientError::StaleCursor, "cursor-mismatch", false),
        (ClientError::CursorMismatch, "cursor-mismatch", false),
        (
            ClientError::Transport(backend_replication::ReplicationError::MessageTooLarge),
            "transport",
            false,
        ),
        (
            ClientError::Protocol("retained owner proof is corrupt".to_owned()),
            "protocol",
            true,
        ),
        (
            ClientError::RequestMismatch {
                expected: 1,
                observed: 2,
            },
            "request-mismatch",
            true,
        ),
        (ClientError::FreshnessMismatch, "freshness", true),
        (ClientError::IncoherentView, "incoherent-view", true),
    ] {
        let mut server = ready(Fake {
            page_catalog: Some(paging_catalog(3)),
            ..Fake::default()
        });
        let first = call(
            &mut server,
            "backend.search",
            &json!({"query":"Session", "limit":1}),
        );
        let cursor = first["structuredContent"]["nextCursor"]
            .as_str()
            .expect("actual catalog continuation")
            .to_owned();
        server.product.continuation_error = Some(error);
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.search", "arguments":{"query":"Session", "limit":1, "cursor":cursor}}),
        );
        if protocol {
            assert_eq!(response["error"]["code"], -32603, "{response}");
            assert_eq!(response["error"]["data"]["structuredContent"]["slug"], slug);
            assert!(response.get("result").is_none());
        } else {
            tool_failure(&response, slug);
        }
        assert_eq!(
            server.product.page_limits,
            [1],
            "a refused cursor never executes another page"
        );
    }
}

#[test]
fn generic_surface_dependency_refusal_keeps_exact_package_action_and_typed_fault() {
    let package = PackageReference::parse("pkg:npm/react@19.1.0").expect("package");
    let command = SurfaceCommand::Dependencies {
        package: package.clone(),
    };
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::Dependencies(
            backend_library::DependencyFacts::Unavailable(
                backend_library::ProductText::from_static("dependency evidence was not captured"),
            ),
        )),
        ..Fake::default()
    });
    let result = call(&mut server, SURFACE_TOOL, &json!({"command":command}));
    assert_eq!(result["isError"], true);
    let fault = &result["structuredContent"]["fault"];
    assert_eq!(fault["slug"], "lane-unavailable");
    assert_eq!(fault["cause"], "not-captured");
    assert_eq!(fault["operand"], package.as_str());
    assert_eq!(
        fault["call"],
        json!({"name":"backend.package", "arguments":{"package":package.as_str()}})
    );
    assert!(text_of(&result).contains(package.as_str()));
    assert_eq!(server.product.surface_commands, [command]);
}

#[test]
fn direct_and_generic_graph_routes_preserve_typed_domain_and_protocol_failures() {
    for failure in [
        backend_library::CommandFailure::NotFound,
        backend_library::CommandFailure::WrongBasis {
            expected: view_state_root(&[("fixture".to_owned(), "current-view".to_owned())]).into(),
            observed: view_state_root(&[("fixture".to_owned(), "stale-view".to_owned())]).into(),
        },
    ] {
        let expected = if matches!(failure, backend_library::CommandFailure::NotFound) {
            "not-found"
        } else {
            "wrong-basis"
        };
        let expected_operand = match &failure {
            backend_library::CommandFailure::WrongBasis { expected, observed } => format!(
                "{} (owner holds {})",
                &encode_id(observed.as_bytes())[..8],
                &encode_id(expected.as_bytes())[..8]
            ),
            _ => DECLARATION.to_owned(),
        };
        let mut direct = ready(Fake {
            graph_failure: Some(failure.clone()),
            ..Fake::default()
        });
        let response = request(
            &mut direct,
            "tools/call",
            &json!({"name":"backend.graph", "arguments":{"coordinate":DECLARATION, "limit":1}}),
        );
        let fault = tool_failure(&response, expected);
        assert_eq!(fault["operand"], expected_operand);
        if expected == "not-found" {
            assert_eq!(
                fault["call"],
                json!({"name":"backend.search", "arguments":{"query":"ferris"}})
            );
        }
        let mut generic = ready(Fake {
            graph_query_error: Some(ClientError::CommandFailed(failure)),
            ..Fake::default()
        });
        let response = request(
            &mut generic,
            "tools/call",
            &json!({"name":QUERY_TOOL, "arguments":{"query":"{ Declaration { coordinate @output } }"}}),
        );
        tool_failure(&response, expected);
    }
    let mut corrupt = ready(Fake {
        graph_failure: Some(backend_library::CommandFailure::IncoherentView(
            "corrupt selected relation".to_owned(),
        )),
        ..Fake::default()
    });
    let response = request(
        &mut corrupt,
        "tools/call",
        &json!({"name":"backend.graph", "arguments":{"coordinate":DECLARATION}}),
    );
    assert_eq!(response["error"]["code"], -32603);
    assert_eq!(
        response["error"]["data"]["structuredContent"]["slug"],
        "incoherent-view"
    );
    assert!(response.get("result").is_none());
}

#[test]
fn partial_terminal_keeps_error_flag_exact_ticket_partition_and_receipt_in_the_same_mcp_wire_result()
 {
    use backend_library::{
        IndexJobOutcome, IndexJobPartialPublication, IndexOperationFailureReason,
        IndexOperationKey, IndexOperationObservation, IndexOperationProfileRefusal,
        IndexOperationPublicationReceipt, IndexOperationSemanticCoverage,
        IndexOperationSemanticProfileState, IndexOperationSemanticUnavailableReason,
        IndexOperationSourceCaptureReceipt, IndexOperationSourceProfile, IndexOperationState,
        IndexOperationStatus, IndexSourceCaptureSummary, ProductText, SemanticLanguageProfile,
    };
    // This tests closed serialization and caller admission, not an actual
    // compiler generation. The view carries no complete-coverage authority.
    let package = PackageReference::parse("/abs/mixed-docs").expect("package");
    let ticket = IndexJobTicket::new(
        std::num::NonZeroU64::new(31).expect("ticket"),
        [19; 16],
        package.clone(),
    );
    let py = SemanticLanguageProfile::from_name("python").expect("Python");
    let ts = SemanticLanguageProfile::from_name("typescript").expect("TypeScript");
    let mut profiles = vec![
        IndexOperationSourceProfile {
            profile: py,
            source_version: [4; 32],
            input_digest: [5; 32],
            observation_sequence: 2,
            source_count: 236,
            state: IndexOperationSemanticProfileState::Published {
                generation: [6; 32],
                coverage: IndexOperationSemanticCoverage::Complete,
            },
        },
        IndexOperationSourceProfile {
            profile: ts,
            source_version: [4; 32],
            input_digest: [7; 32],
            observation_sequence: 3,
            source_count: 626,
            state: IndexOperationSemanticProfileState::Unavailable {
                reason: IndexOperationSemanticUnavailableReason::Rejected,
            },
        },
    ];
    profiles.sort_by_key(|profile| profile.profile);
    let view = root(Vec::new());
    let partial = IndexJobPartialPublication {
        package: package.clone(),
        receipt: IndexOperationPublicationReceipt::from_published_view(
            Some([8; 32]),
            [9; 32],
            [10; 32],
            3,
            &view,
            backend_library::Cursor::for_view_root(&view),
        )
        .expect("structurally admitted receipt"),
        source_capture: IndexSourceCaptureSummary {
            producer_package: package,
            request_identity: [1; 32],
            commit_identity: [2; 32],
            workspace_root: [3; 32],
            workspace_sequence: 2,
            profiles: profiles.into_boxed_slice(),
        },
        refused_profiles: vec![IndexOperationProfileRefusal {
            profile: ts,
            reason: IndexOperationSemanticUnavailableReason::Rejected,
            compiler_failure: Some(setup_compiler_failure()),
        }]
        .into_boxed_slice(),
    };
    partial.admit().expect("complete typed partition");
    let observation = IndexJobObservation::Terminal(IndexJobTerminal {
        ticket: ticket.clone(),
        outcome: IndexJobOutcome::PartiallyPublished(partial.clone()),
    });
    let reply = SurfaceReply::IndexProgress(observation.clone());
    let command = SurfaceCommand::IndexProgress {
        ticket: ticket.clone(),
        after_sequence: 0,
    };
    let dto = ReplyDto::new(701, CommandReply::Surface(reply.clone()));
    let bytes = serde_json::to_vec(&dto).expect("closed backend DTO");
    let decoded =
        backend_library::decode_reply_body(&bytes).expect("genuine closed backend wire decode");
    backend_library::admit_reply(
        &backend_library::CommandDto::new(701, backend_library::Command::Surface(command)),
        &decoded,
    )
    .expect("exact caller ticket and partial payload admission");
    for detail in ["summary", "full"] {
        let mut server = ready(Fake {
            surface_reply: Some(reply.clone()),
            ..Fake::default()
        });
        let wire = request(
            &mut server,
            "tools/call",
            &json!({"name":INDEX_PROGRESS_TOOL, "arguments":{"ticket":ticket, "after_sequence":0, "detail":detail}}),
        );
        assert!(wire.get("error").is_none(), "{wire}");
        assert_eq!(wire["result"]["isError"], true);
        let content = &wire["result"]["structuredContent"];
        assert_eq!(
            content["surface"]["data"],
            serde_json::to_value(&observation).expect("exact observation")
        );
        assert_eq!(
            content["surface"]["index_job"],
            serde_json::to_value(backend_present::IndexJobProjection::Progress(
                observation.clone()
            ))
            .expect("exact job projection")
        );
        assert_eq!(content["fault"]["slug"], "partially-published");
        assert_eq!(
            content["fault"]["partial_publication"],
            serde_json::to_value(&partial)
                .expect("exact receipt, partition, refusals and source tuple")
        );
        assert_eq!(
            content["fault"]["partial_publication"]["receipt"],
            serde_json::to_value(&partial.receipt).expect("receipt")
        );
        let text = text_of(&wire["result"]);
        assert!(text.contains("python: published 236 source files with Complete coverage"));
        assert!(text.contains("typescript: unavailable"));
        assert!(text.contains("NUDOX_TSC"));
        assert_context_bounded(&wire);
        assert_eq!(server.product.surface_commands.len(), 1);
    }
    let expected_cause = backend_present::product_view(&reply)
        .fault()
        .expect("legacy partial fault")
        .cause()
        .sentence()
        .to_owned();
    let key = IndexOperationKey::from_bytes([0x41; 32]).expect("durable key");
    let capture = IndexOperationSourceCaptureReceipt::from_checked_parts(
        key,
        partial.source_capture.commit_identity,
        partial.source_capture.workspace_root,
        partial.source_capture.workspace_sequence,
        partial.source_capture.profiles.clone(),
    )
    .expect("exact durable source receipt");
    let partial_status = IndexOperationStatus::new(
        key,
        partial.package.clone(),
        CompileExecutionIntent::Interactive,
        IndexOperationState::PartiallyPublished {
            receipt: partial.receipt.clone(),
            refused_profiles: partial.refused_profiles.clone(),
        },
    )
    .with_source_capture(Some(capture));
    for (status, is_error) in [
        (partial_status.clone(), true),
        (
            IndexOperationStatus::new(
                key,
                partial.package.clone(),
                CompileExecutionIntent::Interactive,
                IndexOperationState::Accepted,
            ),
            false,
        ),
        (
            IndexOperationStatus::new(
                key,
                partial.package.clone(),
                CompileExecutionIntent::Interactive,
                IndexOperationState::Failed {
                    reason: IndexOperationFailureReason::Cancelled,
                    detail: ProductText::from_static("cancelled before commit"),
                    compiler_failure: None,
                },
            ),
            false,
        ),
    ] {
        let operation = IndexOperationObservation::Known(status);
        for started in [false, true] {
            let (command, reply, name, arguments) = if started {
                (
                    SurfaceCommand::IndexOperationStart {
                        operation_key: key,
                        package: partial.package.clone(),
                        execution_intent: CompileExecutionIntent::Interactive,
                    },
                    SurfaceReply::IndexOperationStarted(operation.clone()),
                    INDEX_START_TOOL,
                    json!({"package":partial.package.as_str(), "operation_key":key.to_hex()}),
                )
            } else {
                (
                    SurfaceCommand::IndexOperationStatus { operation_key: key },
                    SurfaceReply::IndexOperationStatus(operation.clone()),
                    INDEX_PROGRESS_TOOL,
                    json!({"operation_key":key.to_hex()}),
                )
            };
            let dto = ReplyDto::new(702, CommandReply::Surface(reply.clone()));
            let bytes = serde_json::to_vec(&dto).expect("actual durable backend wire");
            let decoded = backend_library::decode_reply_body(&bytes).expect("durable wire decode");
            backend_library::admit_reply(
                &backend_library::CommandDto::new(702, backend_library::Command::Surface(command)),
                &decoded,
            )
            .expect("exact operation, package and intent admission before projection");
            for detail in ["summary", "full"] {
                let mut arguments = arguments.clone();
                arguments["detail"] = json!(detail);
                let mut server = ready(Fake {
                    surface_reply: Some(reply.clone()),
                    ..Fake::default()
                });
                let wire = request(
                    &mut server,
                    "tools/call",
                    &json!({"name":name, "arguments":arguments}),
                );
                assert!(wire.get("error").is_none(), "{wire}");
                assert_eq!(wire["result"]["isError"], is_error, "{wire}");
                let content = &wire["result"]["structuredContent"];
                assert_eq!(
                    content["surface"]["data"],
                    serde_json::to_value(&operation).expect("complete exact operation")
                );
                if is_error {
                    let fault = backend_present::product_view(&reply);
                    assert_eq!(
                        fault
                            .fault()
                            .expect("durable partial fault")
                            .cause()
                            .sentence(),
                        expected_cause
                    );
                    assert_eq!(content["fault"]["slug"], "partially-published");
                    assert!(
                        content["fault"]["partial_publication"].is_null(),
                        "no manufactured legacy source tuple"
                    );
                    assert_eq!(
                        content["surface"]["data"]["detail"]["source_capture"],
                        serde_json::to_value(&partial_status.source_capture)
                            .expect("retained exact capture")
                    );
                    let text = text_of(&wire["result"]);
                    assert!(
                        text.contains("python: published 236 source files with Complete coverage")
                    );
                    assert!(text.contains("typescript: unavailable"));
                    assert!(text.contains("Set NUDOX_TSC"));
                    assert!(text.contains(&key.to_hex()));
                } else {
                    assert!(content["fault"].is_null());
                }
                assert_context_bounded(&wire);
            }
        }
    }
}

/// Real private state for deferred cursor-authority admission. No owner is
/// started: token authentication itself is the behavior under test.
struct LazyAuthorityFixture {
    root: std::path::PathBuf,
    project: std::path::PathBuf,
    paths: backend_runtime::WorkspacePaths,
}

impl LazyAuthorityFixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("fixture clock")
            .as_nanos();
        let base = if cfg!(unix) {
            std::path::PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let name = format!("mcp-lazy-{}-{nonce:x}", std::process::id());
        let root = base.join(&name);
        let project = base.join(format!("{name}-project"));
        // /tmp is deliberately other-writable: it is not an admissible
        // application-data parent. Own a private fixture parent first, then
        // let the real WorkspacePaths admission create its state child.
        std::fs::create_dir(&root).expect("owned fixture parent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("private fixture parent permissions");
        }
        #[cfg(windows)]
        backend_platform::win32::security::restrict_to_current_user(&root)
            .expect("private fixture parent ACL");
        std::fs::create_dir(&project).expect("fixture project");
        let paths = backend_runtime::WorkspacePaths::discover(
            Some(project.clone()),
            Some(root.join("state")),
            None,
        )
        .expect("selected fixture workspace");
        Self {
            root,
            project,
            paths,
        }
    }
}

impl Drop for LazyAuthorityFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.project);
    }
}

#[test]
fn lazy_cursor_authority_is_durable_once_and_replayable_in_a_fresh_server() {
    let fixture = LazyAuthorityFixture::new();
    let authority = CursorAuthority::workspace(&fixture.paths);
    let first = Server::with_authority(Fake::default(), PROJECT.to_owned(), authority.clone());
    assert!(!fixture.paths.data().exists());
    let token = first
        .sign_cursor_token("pc1-real-owner-token", b"same-request")
        .expect("first cursor durably admits its authority");
    let persisted = read_authority_secret(fixture.paths.authority_secret()).expect("durable key");
    assert_eq!(authority.key().expect("same admitted authority"), persisted);
    let second = Server::with_authority(
        Fake::default(),
        PROJECT.to_owned(),
        CursorAuthority::workspace(&fixture.paths),
    );
    assert_eq!(
        second
            .verify_cursor_token(&token, b"same-request")
            .expect("cold process reads durable authority")
            .as_deref(),
        Some("pc1-real-owner-token")
    );
    assert_eq!(
        read_authority_secret(fixture.paths.authority_secret()).expect("unchanged authority"),
        persisted
    );
}

#[test]
fn lazy_cursor_authority_retries_after_repair_without_process_key_fallback() {
    let fixture = LazyAuthorityFixture::new();
    std::fs::write(fixture.paths.data(), b"not a workspace directory")
        .expect("unavailable durable workspace");
    let authority = CursorAuthority::workspace(&fixture.paths);
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), authority.clone());
    let first = server
        .sign_cursor_token("pc1-owner-token", b"query")
        .expect_err("a durable authority refusal must not mint an ephemeral cursor");
    assert_eq!(first.kind, "transport");
    assert!(
        first
            .detail
            .as_deref()
            .is_some_and(|detail| !detail.is_empty())
    );
    let repeated = server
        .sign_cursor_token("pc1-owner-token", b"query")
        .expect_err("the unchanged failure retains its original cause");
    assert_eq!(repeated.detail, first.detail);
    std::fs::remove_file(fixture.paths.data()).expect("retire unavailable fixture");
    let token = server
        .sign_cursor_token("pc1-owner-token", b"query")
        .expect("repaired workspace can admit its first durable authority");
    let persisted = read_authority_secret(fixture.paths.authority_secret()).expect("durable key");
    assert_eq!(authority.key().expect("admitted authority"), persisted);
    assert_eq!(
        server
            .verify_cursor_token(&token, b"query")
            .expect("durable token")
            .as_deref(),
        Some("pc1-owner-token")
    );
}

#[test]
fn simultaneous_lazy_cursor_admission_uses_one_durable_identity() {
    let fixture = LazyAuthorityFixture::new();
    let shared = CursorAuthority::workspace(&fixture.paths);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers = (0..8)
        .map(|index| {
            let authority = if index % 2 == 0 {
                shared.clone()
            } else {
                CursorAuthority::workspace(&fixture.paths)
            };
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                authority.key().expect("simultaneous durable admission")
            })
        })
        .collect::<Vec<_>>();
    let persisted = workers
        .into_iter()
        .map(|worker| worker.join().expect("admission worker"))
        .collect::<Vec<_>>();
    let durable = read_authority_secret(fixture.paths.authority_secret()).expect("durable key");
    assert!(persisted.into_iter().all(|key| key == durable));
    assert_eq!(shared.key().expect("stable shared authority"), durable);
}

#[test]
fn malformed_or_expired_cursor_does_not_admit_a_workspace_authority() {
    let fixture = LazyAuthorityFixture::new();
    let server = Server::with_authority(
        Fake::default(),
        PROJECT.to_owned(),
        CursorAuthority::workspace(&fixture.paths),
    );
    for token in [
        "not-a-cursor",
        "mcp1-0-b3duZXI-00000000000000000000000000000000",
    ] {
        assert!(
            server
                .verify_cursor_token(token, b"query")
                .expect("invalid input")
                .is_none()
        );
    }
    assert!(!fixture.paths.data().exists());
}

#[test]
fn cold_cursor_verification_never_initializes_authority_for_a_forged_token() {
    let fixture = LazyAuthorityFixture::new();
    let authority = CursorAuthority::workspace(&fixture.paths);
    let server = Server::with_authority(Fake::default(), PROJECT.to_owned(), authority.clone());
    let future = unix_seconds().saturating_add(900);
    for token in [
        format!("mcp1-{future}-cGMxLWZvcmdlZA-{}", "00".repeat(16)),
        format!("mcp1-{future}-cGMxLWZvcmdlZA-not-a-canonical-mac"),
        format!("mcp1-{future}-invalid*base64-{}", "00".repeat(16)),
    ] {
        assert!(
            server
                .verify_cursor_token(&token, b"query")
                .expect("read-only verification")
                .is_none()
        );
        assert!(
            !fixture.paths.data().exists(),
            "untrusted continuation cannot provision an authority"
        );
    }
    assert!(
        authority
            .verification_key()
            .expect("missing durable key")
            .is_none()
    );
    let token = server
        .sign_cursor_token("pc1-valid", b"query")
        .expect("actual signing request can admit a durable key");
    assert_eq!(
        server
            .verify_cursor_token(&token, b"query")
            .expect("post-sign verification")
            .as_deref(),
        Some("pc1-valid")
    );
    assert!(fixture.paths.authority_secret().is_file());
}

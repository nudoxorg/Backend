//! MCP surface tests.
//!
//! These cases assert *content*, never counts: the tool a registry row is
//! reachable as, the Markdown an agent reads, the exact coordinate it passes
//! back, the slug and the next tool call a refusal carries. A fixture engine
//! stands in for the daemon, so every assertion here is about what this surface
//! decides rather than about what a live index happens to hold.

#![allow(clippy::expect_used, clippy::naive_bytecount, clippy::panic)]

use super::*;
use backend_library::{
    Basis, COMMANDS, CommandReply, Coverage, DeclarationKind, Document, Fragment, Freshness,
    Frontier, Intent, Lane, Outline, OutlineExtent, OutlineNode, ProjectionPage, Reason, Row,
    RowId, SourceAvailability, SourceExcerpt, SourceExcerptExtent, SourceLocation, ViewRoot,
    ViewSnapshot, object_version, package_key, symbol_key, view_key, view_state_root,
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
    /// Optional graph rows used to exercise the complete-page admission cap.
    graph_rows: Option<Box<[GraphQueryRow]>>,
    /// Make the graph fixture return one authenticated continuation page.
    graph_continue: bool,
    /// Owner-issued continuation retained by the fixture encoder.
    next_continuation: Option<PageContinuation>,
    /// Make an otherwise valid owner cursor stale at decode time.
    stale_cursor: bool,
    /// Optional product reply used by the high-fanout surface budget case.
    surface_reply: Option<SurfaceReply>,
    /// Number of graph requests that reached the product boundary.
    graph_query_calls: usize,
}

fn basis() -> Basis {
    Basis::new(view_state_root(&[]), object_version(b"source"))
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
        Ok(view_state_root(&[]))
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        let library = backend_library::Library::new();
        Ok(HealthReport::from_root(library.view(), library.cursor()))
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
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
            Probe::Remove(_) => {
                CommandReply::Removed(Intent::remove_package(package_key(PROJECT)).id())
            }
            Probe::Document(at) | Probe::Source(at) => {
                if at == MISSING {
                    return Err(ClientError::CommandFailed(
                        backend_library::CommandFailure::NotFound,
                    ));
                }
                CommandReply::Document(document())
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
            Probe::Outline(_) => CommandReply::Outline(
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
            ),
            Probe::OutlinePage { .. } => CommandReply::ProjectionPage(ProjectionPage {
                snapshot: snapshot(vec![module_row(), declaration_row()]),
                terminal: PageTerminal::Complete,
            }),
        };
        Ok(ReplyDto::new(1, reply))
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        if let Some(reply) = self.surface_reply.take() {
            return Ok(reply);
        }
        match command {
            SurfaceCommand::Subscriptions => Ok(SurfaceReply::Subscriptions(Box::new([]))),
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
        if self.next_continuation == Some(continuation) {
            Ok("fixture-page-1".to_owned())
        } else {
            Err(ClientError::Protocol("unknown fixture cursor".to_owned()))
        }
    }

    fn decode_continuation(
        &mut self,
        token: &str,
    ) -> Result<backend_library::PageContinuation, ClientError> {
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
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
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
        .expect("a request receives a response")
}

fn call(server: &mut Server<Fake>, tool: &str, arguments: &Value) -> Value {
    request(
        server,
        "tools/call",
        &json!({ "name": tool, "arguments": arguments }),
    )["result"]
        .clone()
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
    let bytes = serde_json::to_vec(response).expect("JSON-RPC response serializes");
    assert!(
        bytes.len() <= DEFAULT_RESPONSE_BUDGET_BYTES,
        "response is {} bytes, above the {} byte context budget: {response}",
        bytes.len(),
        DEFAULT_RESPONSE_BUDGET_BYTES
    );
}

// ---------------------------------------------------------------------------
// registry completeness
// ---------------------------------------------------------------------------

#[test]
fn every_registry_row_is_reachable_as_exactly_one_tool() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let tools = &listed["result"]["tools"];
    for spec in COMMANDS {
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
        let expected = grammar
            .positional()
            .iter()
            .filter(|argument| argument.is_required())
            .map(|argument| Value::String(argument.name().to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(required, &expected, "`{}` required operands", spec.name);
        for argument in grammar.positional().iter().chain(grammar.options()) {
            assert!(
                !tool["inputSchema"]["properties"][argument.name()].is_null(),
                "`{}` omits operand `{}`",
                spec.name,
                argument.name()
            );
        }
    }
    assert_eq!(
        tools.as_array().map(Vec::len),
        Some(COMMANDS.len() + 2),
        "one tool per registry row, plus backend.query and backend.surface"
    );
}

#[test]
fn the_tool_table_is_grouped_by_domain_in_registry_order() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let domains: Vec<String> = listed["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .filter_map(|tool| tool["_meta"]["backend/domain"].as_str())
        .map(ToOwned::to_owned)
        .collect();
    let mut seen: Vec<&str> = Vec::new();
    for domain in &domains[..COMMANDS.len()] {
        if seen.last().copied() != Some(domain.as_str()) {
            assert!(
                !seen.contains(&domain.as_str()),
                "the {domain} domain is emitted in two runs"
            );
            seen.push(domain);
        }
    }
    assert_eq!(
        seen,
        vec!["library", "registry", "home", "session", "system"]
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
fn the_surface_escape_hatch_still_advertises_every_typed_operation() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    let surface = tool_named(&listed["result"]["tools"], SURFACE_TOOL);
    let advertised =
        surface["inputSchema"]["properties"]["command"]["properties"]["operation"]["enum"]
            .as_array()
            .expect("the operation enum");
    let expected = COMMANDS
        .iter()
        .filter(|spec| spec.is_surface())
        .map(|spec| Value::String(spec.name.to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(advertised, &expected);
    assert_eq!(
        surface["inputSchema"]["properties"]["detail"]["enum"],
        json!(["summary", "standard", "full"]),
        "the escape hatch must advertise the presentation control it accepts"
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
        text.contains("→ `{\"arguments\":{\"query\":\"nothing\"},\"name\":\"backend.search\"}`"),
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
        rendered.contains("src/main.rs:40-46"),
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
    assert_eq!(response["error"]["code"], -32603);
    assert!(
        response["error"]["data"]["detail"]
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
    assert_eq!(oversized_query["error"]["code"], -32602);
    assert!(
        oversized_query["error"]["data"]["detail"]
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
    assert_eq!(too_many_variables["error"]["code"], -32602);
    assert!(
        too_many_variables["error"]["data"]["detail"]
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
    assert_eq!(too_deep["error"]["code"], -32602);
    assert!(
        too_deep["error"]["data"]["detail"]
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
    assert_eq!(too_large["error"]["code"], -32602);
    assert!(
        too_large["error"]["data"]["detail"]
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
    assert!(
        resources
            .iter()
            .any(|resource| resource["uri"] == "backend://outline/backend%3A%2F%2F".to_owned() + "")
            || resources.iter().any(|resource| resource["uri"]
                .as_str()
                .is_some_and(|uri| uri.starts_with("backend://outline/"))),
        "the shelf contributes one outline resource per project: {listed}"
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

// ---------------------------------------------------------------------------
// protocol
// ---------------------------------------------------------------------------

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
    assert_eq!(initialized["result"]["protocolVersion"], STABLE_PROTOCOL);
    let instructions = initialized["result"]["instructions"]
        .as_str()
        .unwrap_or_default();
    assert!(
        instructions.contains("backend://workspace/current"),
        "{instructions}"
    );
    assert!(instructions.contains("backend.document"), "{instructions}");
}

#[test]
fn the_mcp_context_exposes_a_project_and_launch_directory_mismatch() {
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
        instructions.contains("configuration mismatch"),
        "{instructions}"
    );

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
    assert!(text.contains("configuration mismatch"), "{text}");
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
    assert!(write_message(&mut output, &oversized_result).is_err());
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
    let token = server.sign_cursor_token("pc1-owner-issued", b"query-a");
    assert_eq!(
        server.verify_cursor_token(&token, b"query-a"),
        Some("pc1-owner-issued")
    );
    // Verification is deliberately replayable: retrying a read page is safe,
    // while the MAC still prevents moving that page to another authority
    // context. This is the property a reconnecting MCP client needs.
    assert_eq!(
        server.verify_cursor_token(&token, b"query-a"),
        Some("pc1-owner-issued")
    );
    assert!(server.verify_cursor_token(&token, b"query-b").is_none());
    let tampered = token.replace("pc1-owner-issued", "pc1-owner-tampered");
    assert!(server.verify_cursor_token(&tampered, b"query-a").is_none());
    let other_workspace = Server::with_authority(Fake::default(), "/other".to_owned(), [8; 32]);
    assert!(
        other_workspace
            .verify_cursor_token(&token, b"query-a")
            .is_none()
    );
    // Restarting with the persisted authority accepts the token; rotation
    // invalidates every old token immediately, including one with identical
    // workspace and query context.
    let restarted = Server::with_authority(Fake::default(), PROJECT.to_owned(), [7; 32]);
    assert_eq!(
        restarted.verify_cursor_token(&token, b"query-a"),
        Some("pc1-owner-issued")
    );
    let rotated = Server::with_authority(Fake::default(), PROJECT.to_owned(), [9; 32]);
    assert!(rotated.verify_cursor_token(&token, b"query-a").is_none());
    let expired =
        server.sign_cursor_token_at(unix_seconds().saturating_sub(1), "pc1-old", b"query-a");
    assert!(server.verify_cursor_token(&expired, b"query-a").is_none());
    let expires_now = server.sign_cursor_token_at(unix_seconds(), "pc1-now", b"query-a");
    assert!(
        server
            .verify_cursor_token(&expires_now, b"query-a")
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
    assert_context_bounded(&response);
}

#[test]
fn graph_continuation_round_trip_is_bounded_and_authorized() {
    let mut server = ready(Fake {
        graph_continue: true,
        ..Fake::default()
    });
    let first = call(
        &mut server,
        QUERY_TOOL,
        &json!({"query":"{ Declaration { coordinate @output } }","limit":1}),
    );
    assert_context_bounded(&first);
    assert_eq!(first["isError"], false);
    assert_eq!(first["structuredContent"]["terminal"], "limit_reached");
    let cursor = first["structuredContent"]["nextCursor"]
        .as_str()
        .expect("first graph page carries a cursor")
        .to_owned();

    let second = call(
        &mut server,
        QUERY_TOOL,
        &json!({
            "query":"{ Declaration { coordinate @output } }",
            "limit":1,
            "cursor":cursor
        }),
    );
    assert_context_bounded(&second);
    assert_eq!(second["isError"], false);
    assert_eq!(second["structuredContent"]["terminal"], "complete");
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
    assert_eq!(response["error"]["code"], -32010);
    assert_eq!(response["error"]["data"]["kind"], "stale_cursor");
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

//! MCP protocol and bounded codec tests.

#![allow(clippy::expect_used, clippy::naive_bytecount, clippy::panic)]

use super::*;
use backend_library::{
    Basis, CommandFailure, CommandReply, Fragment, Frontier, Intent, Row, RowId, SourceLocation,
    ViewRoot, object_version, package_key, symbol_key, view_key, view_state_root,
};

#[derive(Default)]
struct Fake;

impl Product for Fake {
    fn health(&mut self) -> Result<HealthReport, String> {
        let view = empty_view();
        Ok(HealthReport::from_root(
            &view,
            backend_library::Cursor::for_view_root(&view),
        ))
    }

    fn revision(&mut self) -> Result<ViewStateRoot, String> {
        Ok(empty_view().root())
    }
    fn packages(&mut self) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn index(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn remove(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn search(&mut self, _: &str, _: u16) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn names(&mut self, _: &str, _: u16) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn document(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn source(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn outline(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn graph(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn related(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn diff(
        &mut self,
        _: PackageReference,
        _: PackageReference,
    ) -> Result<Box<[DiffRecord]>, String> {
        let source = backend_library::SemanticDeclarationIdentity {
            family: [1; 16],
            variant: [2; 16],
        };
        Ok(vec![DiffRecord {
            label: backend_library::ProductText::new("example::call")
                .map_err(|error| error.to_string())?,
            change: backend_library::DeclarationChange::Changed,
            before: Some(source),
            after: Some(source),
            links: vec![backend_library::SemanticLinkDelta::EvidenceChanged {
                from: source,
                target: backend_library::SemanticLinkTarget::Foreign {
                    declaration: [3; 16],
                    variant: None,
                },
                relation: backend_library::SemanticLinkKind::Calls,
                before: backend_library::SemanticLinkEvidence {
                    confidence: backend_library::SemanticConfidence::Syntactic,
                    source: None,
                },
                after: backend_library::SemanticLinkEvidence {
                    confidence: backend_library::SemanticConfidence::Compiler,
                    source: None,
                },
            }]
            .into_boxed_slice(),
        }]
        .into_boxed_slice())
    }
    fn graph_query(
        &mut self,
        _: String,
        _: std::collections::BTreeMap<String, GraphValue>,
        _: u16,
    ) -> Result<GraphQueryPage, String> {
        let view = empty_view();
        Ok(GraphQueryPage {
            revision: view.root().into(),
            source: view.basis().object,
            rows: Box::new([]),
            terminal: PageTerminal::Complete,
        })
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, String> {
        match command {
            SurfaceCommand::Subscriptions => Ok(SurfaceReply::Subscriptions(Box::new([]))),
            _ => Err("unused surface fixture".to_owned()),
        }
    }
}

fn empty_view() -> ViewRoot {
    let root = view_state_root(&[]);
    let basis = Basis::new(root, object_version(b"source"));
    ViewRoot::new_incomplete(
        view_key(b"mcp-test"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, root, 0),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty view")
}

#[test]
fn handshake_lists_complete_tool_surface() {
    let mut server = Server::new(Fake, "/project".to_owned());
    let initialized = server.handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#).expect("response");
    assert_eq!(initialized["result"]["protocolVersion"], STABLE_PROTOCOL);
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    let tools = server
        .handle(br#"{"jsonrpc":"2.0","id":"tools","method":"tools/list","params":{}}"#)
        .expect("response");
    assert_eq!(tools["result"]["tools"].as_array().map(Vec::len), Some(14));
    let surface = tools["result"]["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == "backend.surface"))
        .expect("complete surface tool");
    assert_eq!(
        surface["inputSchema"]["properties"]["command"]["properties"]["operation"]["enum"]
            .as_array()
            .map(Vec::len),
        Some(24)
    );
    assert_eq!(tools["id"], "tools");
}

#[test]
fn generic_surface_tool_reaches_a_typed_daemon_operation() {
    let mut server = Server::new(Fake, "/project".to_owned());
    assert!(server.handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#).is_some());
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    let response = server
        .handle(br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"backend.surface","arguments":{"command":{"operation":"subscriptions"}}}}"#)
        .expect("response");
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["surface"]["result"],
        "subscriptions"
    );
}

#[test]
fn diff_tool_admits_package_references_and_returns_the_typed_surface() {
    let mut server = Server::new(Fake, "/project".to_owned());
    assert!(server.handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#).is_some());
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    let response = server
        .handle(br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"backend.diff","arguments":{"from":"pkg:cargo/example@1.0.0","to":"pkg:cargo/example@2.0.0"}}}"#)
        .expect("response");
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["surface"]["result"],
        "diff"
    );
    let row = &response["result"]["structuredContent"]["surface"]["data"][0];
    assert_eq!(row["label"], "example::call");
    assert_eq!(row["before"]["family"], json!(vec![1; 16]));
    assert_eq!(row["links"][0]["change"], "evidence-changed");
    assert_eq!(row["links"][0]["relation"], "calls");
    assert_eq!(row["links"][0]["before"]["confidence"], "syntactic");
    assert_eq!(row["links"][0]["after"]["confidence"], "compiler");

    let rejected = server
        .handle(br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"backend.diff","arguments":{"from":"pkg:cargo/example","to":"pkg:cargo/example@2.0.0"}}}"#)
        .expect("response");
    assert_eq!(rejected["error"]["code"], -32602);
}

#[test]
fn initialized_notification_cannot_bypass_initialize_and_resource_is_pinned() {
    let mut server = Server::new(Fake, "/project".to_owned());
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
        .handle(br#"{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"backend://workspace/current"}}"#)
        .expect("requests before initialized acknowledgement receive an error");
    assert_eq!(premature["error"]["code"], -32002);
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    let resource = server.handle(br#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"backend://workspace/current"}}"#).expect("response");
    let text = resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("revision"));
    assert!(text.contains("/project"));
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
fn status_exposes_unavailable_coverage_instead_of_claiming_readiness() {
    let library = backend_library::Library::new();
    let CommandReply::Readiness(report) = library
        .execute(backend_library::Command::Health)
        .expect("health")
    else {
        panic!("health reply");
    };
    let status = readiness_value(&report, "/project");
    assert_eq!(status["readiness"], "unavailable");
    assert_eq!(status["coverage"][0]["state"], "unavailable");
    assert_eq!(status["coverage"][0]["lane"], "exact");
    assert_eq!(status["coverage"][0]["reason"], "no_index");
    assert!(
        status["capabilities"]
            .as_array()
            .is_some_and(|capabilities| !capabilities.is_empty())
    );
    assert_eq!(status["capabilities"][0]["lifecycle"], "unavailable");
    assert_eq!(
        status["capabilities"][0]["unavailableReason"],
        "no_manifest"
    );
}

#[test]
fn index_reply_reports_an_accepted_intent_and_pending_completion() {
    let intent = Intent::request_package(package_key("pkg")).id();
    let (value, is_error) = reply_value(ReplyDto::new(1, CommandReply::Added(intent)));
    assert!(!is_error);
    assert_eq!(value["accepted"], true);
    assert_eq!(value["operation"], "index");
    assert_eq!(value["completion"], "pending");
    assert!(value.get("indexed").is_none());
}

#[test]
fn rows_expose_typed_lifecycle_and_source_availability() {
    let root = view_state_root(&[]);
    let basis = Basis::new(root, object_version(b"source"));
    let row = Row::new(RowId::Symbol(symbol_key("pkg::Thing")), basis, "pkg::Thing")
        .with_source(SourceLocation::new("src/lib.rs", 9).expect("source"));
    let value = row_value(&row);
    assert_eq!(value["state"], "ready");
    assert_eq!(value["source"]["available"], true);
    assert_eq!(value["source"]["state"], "captured");
    assert_eq!(value["source"]["path"], "src/lib.rs");
    assert_eq!(value["source"]["startLine"], 9);
    assert_eq!(value["excerpt"]["state"], "not_captured");

    let missing = Row::new(RowId::Symbol(symbol_key("pkg::Other")), basis, "pkg::Other");
    assert_eq!(row_value(&missing)["source"]["available"], false);
    assert_eq!(row_value(&missing)["source"]["state"], "not_captured");
}

#[test]
fn document_projection_exposes_its_typed_source_availability() {
    let document = Document::new(
        symbol_key("pkg::Thing"),
        view_state_root(&[]),
        [Fragment::Text("documentation".to_owned())],
    )
    .with_location(SourceAvailability::Captured(
        SourceLocation::new("src/lib.rs", 11).expect("source"),
    ))
    .with_excerpt(
        backend_library::SourceExcerpt::captured(
            "fn thing() {}",
            backend_library::SourceExcerptExtent::Complete,
        )
        .expect("excerpt"),
    );
    let value = document_value(&document);
    assert_eq!(value["source"]["available"], true);
    assert_eq!(value["source"]["state"], "captured");
    assert_eq!(value["source"]["path"], "src/lib.rs");
    assert_eq!(value["source"]["startLine"], 11);
    assert!(value["source"].get("bytes").is_none());
    assert_eq!(value["excerpt"]["state"], "captured");
    assert_eq!(value["excerpt"]["text"], "fn thing() {}");
    assert_eq!(value["excerpt"]["extent"], "complete");
}

#[test]
fn command_failures_have_a_typed_error_shape() {
    let (value, is_error) = reply_value(ReplyDto::error(1, "offline"));
    assert!(is_error);
    assert_eq!(value["error"]["kind"], "command");
    assert_eq!(value["error"]["message"], "offline");

    let (value, is_error) = reply_value(ReplyDto::new(
        2,
        CommandReply::Failed(CommandFailure::CursorMismatch),
    ));
    assert!(is_error);
    assert_eq!(value["error"]["kind"], "cursor_mismatch");
    assert_eq!(value["error"]["message"], "cursor does not match the view");
}

#[test]
fn projection_pages_expose_explicit_completion() {
    let page = ProjectionPage {
        snapshot: ViewSnapshot {
            root: empty_view(),
            freshness: backend_library::Freshness::Current,
            next: None,
        },
        terminal: PageTerminal::Complete,
    };
    let (value, is_error) = reply_value(ReplyDto::new(1, CommandReply::ProjectionPage(page)));
    assert!(!is_error);
    assert_eq!(value["terminal"]["state"], "complete");
}

#[test]
fn snapshots_expose_coverage_and_resource_reads_reject_command_errors() {
    let library = backend_library::Library::new();
    let CommandReply::Packages(packages) = library
        .execute(backend_library::Command::Packages)
        .expect("packages")
    else {
        panic!("health reply");
    };
    let snapshot = snapshot_value(
        "search",
        &ViewSnapshot {
            root: packages.root,
            freshness: backend_library::Freshness::Current,
            next: None,
        },
    );
    assert_eq!(snapshot["readiness"], "unavailable");
    assert_eq!(snapshot["coverage"][0]["reason"], "no_index");

    let error = resource_value(ReplyDto::error(1, "not found")).expect_err("resource error");
    assert_eq!(error.kind, "transport");
    assert_eq!(error.detail.as_deref(), Some("not found"));
}

#[test]
fn outline_projection_emits_the_complete_forest_and_extent() {
    let root = view_state_root(&[]);
    let outline = backend_library::Outline::new(
        package_key("pkg"),
        root,
        OutlineNode {
            symbol: symbol_key("pkg::First"),
            children: Box::new([]),
        },
    )
    .with_additional_roots([OutlineNode {
        symbol: symbol_key("pkg::Second"),
        children: Box::new([]),
    }])
    .with_extent(OutlineExtent::Truncated);
    let (value, is_error) = reply_value(ReplyDto::new(1, CommandReply::Outline(outline)));
    assert!(!is_error);
    assert_eq!(value["roots"].as_array().map(Vec::len), Some(2));
    assert_eq!(value["extent"], "truncated");
}

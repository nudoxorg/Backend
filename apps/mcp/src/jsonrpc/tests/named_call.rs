//! The compact tool must preserve the selected registered route's contracts.

use super::super::tools::{CALL_TOOL, CATALOG_RESOURCE_URI};
use super::*;

fn forwarded(server: &mut Server<Fake>, name: &str, arguments: &Value) -> Value {
    call(
        server,
        CALL_TOOL,
        &json!({"name": name, "arguments": arguments}),
    )
}

#[test]
fn compact_discovery_has_one_closed_registry_derived_target_set() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    assert_context_bounded(&listed);
    let tools = listed["result"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 18);
    assert_eq!(tools.last().expect("compact tool")["name"], CALL_TOOL);
    let tool = tool_named(&listed["result"]["tools"], CALL_TOOL);
    let schema = &tool["inputSchema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["name", "arguments"]));
    assert!(schema["properties"].get("detail").is_none());
    let names = schema["properties"]["name"]["enum"]
        .as_array()
        .expect("closed names");
    for grammar in backend_present::GRAMMARS {
        assert!(names.contains(&json!(grammar.tool())), "{}", grammar.tool());
    }
    assert_eq!(names.len(), backend_present::GRAMMARS.len() + 1);
    assert!(names.contains(&json!(QUERY_TOOL)));
    for excluded in [SURFACE_TOOL, CALL_TOOL, "backend.no_such_tool"] {
        assert!(!names.contains(&json!(excluded)));
    }
    assert_eq!(
        tool["_meta"]["backend/catalog"]["uri"],
        CATALOG_RESOURCE_URI
    );
    assert_eq!(tool["annotations"]["readOnlyHint"], false);
    assert_eq!(tool["annotations"]["destructiveHint"], true);
    assert_eq!(tool["annotations"]["idempotentHint"], false);
    let catalog = request(
        &mut server,
        "resources/read",
        &json!({"uri": CATALOG_RESOURCE_URI}),
    );
    let text = catalog["result"]["contents"][0]["text"]
        .as_str()
        .expect("catalog");
    assert!(
        text.contains(
            tool["_meta"]["backend/catalog"]["schema"]
                .as_str()
                .expect("schema version")
        )
    );
    assert!(text.contains("`backend.call`"));
    assert_eq!(server.product.probe_calls, 0);
    assert!(server.product.surface_commands.is_empty());
}

#[test]
fn compact_calls_preserve_original_defaults_results_and_mutation_commands() {
    for (name, arguments) in [
        ("backend.document", json!({"coordinate": DECLARATION})),
        ("backend.source", json!({"coordinate": DECLARATION})),
        ("backend.search", json!({"query": "ferris"})),
        ("backend.status", json!({})),
        (
            QUERY_TOOL,
            json!({"query": "{ Declaration { coordinate @output } }"}),
        ),
        ("backend.remove", json!({"path": PROJECT})),
    ] {
        let direct = call(&mut ready(Fake::default()), name, &arguments);
        let mut server = ready(Fake::default());
        let compact = forwarded(&mut server, name, &arguments);
        assert_eq!(compact, direct, "selected route {name}");
        assert_eq!(compact["isError"], false, "{name}: {compact}");
    }
}

#[test]
fn compact_catalog_reads_preserve_typed_operands_refusals_and_semantic_data() {
    let package = "pkg:npm/react@19.1.0";
    let reply = SurfaceReply::Dependencies(backend_library::DependencyFacts::Unavailable(
        ProductText::from_static("dependency evidence was not captured"),
    ));
    let mut direct = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let original = call(
        &mut direct,
        "backend.dependencies",
        &json!({"package": package}),
    );
    let mut compact = ready(Fake {
        surface_reply: Some(reply),
        ..Fake::default()
    });
    let result = forwarded(
        &mut compact,
        "backend.dependencies",
        &json!({"package": package}),
    );
    assert_eq!(
        result["structuredContent"]["records"],
        original["structuredContent"]["records"]
    );
    assert_eq!(
        result["structuredContent"]["fault"]["cause"],
        original["structuredContent"]["fault"]["cause"]
    );
    let advice = &result["structuredContent"]["fault"]["call"];
    assert_eq!(
        advice,
        &json!({"name": CALL_TOOL, "arguments": {"name": "backend.package", "arguments": {"package": package}}})
    );
    assert!(text_of(&result).contains("backend.call"));
    assert!(!text_of(&result).contains("→ `backend.package("));
    compact.product.surface_reply = Some(SurfaceReply::Package(Box::new([])));
    let advice_result = request(&mut compact, "tools/call", advice);
    assert!(advice_result.get("error").is_none());
    assert!(
        matches!(compact.product.surface_commands.last(), Some(SurfaceCommand::Package { package: actual }) if actual.as_str() == package)
    );
    compact.product.surface_commands.pop();
    assert_eq!(
        compact.product.surface_commands,
        direct.product.surface_commands
    );
    assert!(matches!(compact.product.surface_commands.as_slice(),
        [SurfaceCommand::Dependencies { package: actual }] if actual.as_str() == package));
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["structuredContent"]["fault"]["cause"],
        "not-captured"
    );
    assert_eq!(result["structuredContent"]["fault"]["operand"], package);

    let packet: Value = serde_json::from_str(include_str!(
        "../../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("captured semantic packet");
    let reply: SurfaceReply =
        serde_json::from_value(packet["surface"].clone()).expect("typed reply");
    let package = packet["surface"]["data"][0]["package"]["value"]
        .as_str()
        .expect("captured package");
    let arguments = json!({"package": package, "detail": "full"});
    let mut direct = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let original = call(&mut direct, "backend.semantic_versions", &arguments);
    let mut compact = ready(Fake {
        surface_reply: Some(reply),
        ..Fake::default()
    });
    let result = forwarded(&mut compact, "backend.semantic_versions", &arguments);
    assert_eq!(result, original);
    assert_context_bounded(&result);
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["cause"], "oversized");
    assert!(result["structuredContent"].get("semantic_data").is_none());
    assert_eq!(
        compact.product.surface_commands,
        direct.product.surface_commands
    );
}

#[test]
fn compact_outer_and_target_admission_never_coerce_or_dispatch_invalid_inputs() {
    let mut server = ready(Fake::default());
    for arguments in [
        json!({}),
        json!({"name": CALL_TOOL, "arguments": {}}),
        json!({"name": SURFACE_TOOL, "arguments": {"command": {"operation": "subscriptions"}}}),
        json!({"name": "backend.no_such_tool", "arguments": {}}),
        json!({"name": "backend.status"}),
        json!({"name": "backend.status", "arguments": null}),
        json!({"name": "backend.status", "arguments": []}),
        json!({"name": "backend.status", "arguments": "{}"}),
        json!({"name": "backend.status", "arguments": {}, "detail": "full"}),
        json!({"name": "backend.dependencies", "arguments": {"package": "pkg:npm/react@19.1.0", "unknown": true}}),
        json!({"name": "backend.index", "arguments": {}}),
        json!({"name": "backend.package_graph", "arguments": {"package": "pkg:cargo/serde@1.0.228", "direction": "dependencies", "limit": "128"}}),
        json!({"name": QUERY_TOOL, "arguments": {"query": "{ Declaration { name @output } }", "variables": {"bad": {"object": true}}}}),
    ] {
        let refused = call(&mut server, CALL_TOOL, &arguments);
        assert_eq!(refused["isError"], true, "{arguments}: {refused}");
        assert_eq!(
            refused["structuredContent"]["slug"], "usage",
            "{arguments}: {refused}"
        );
    }
    let retired = forwarded(
        &mut server,
        "backend.index_await",
        &json!({"ticket": ticket_value(&index_job_ticket())}),
    );
    assert_eq!(retired["isError"], true);
    assert!(text_of(&retired).contains("backend.index_progress"));
    assert_eq!(server.product.probe_calls, 0);
    assert_eq!(server.product.graph_query_calls, 0);
    assert!(server.product.surface_commands.is_empty());
}

#[test]
fn compact_queries_resume_original_named_cursors_and_reject_changed_targets() {
    let mut server = ready(Fake {
        graph_continue: true,
        ..Fake::default()
    });
    let mut arguments = json!({"query": "{ Declaration { coordinate @output } }", "limit": 1});
    let first = forwarded(&mut server, QUERY_TOOL, &arguments);
    assert_eq!(first["isError"], false);
    arguments["cursor"] = first["structuredContent"]["nextCursor"].clone();
    let second = call(&mut server, QUERY_TOOL, &arguments);
    assert_eq!(second["isError"], false);
    assert_eq!(second["structuredContent"]["terminal"], "complete");
    assert_eq!(server.product.graph_query_calls, 2);

    let mut server = ready(Fake {
        graph_continue: true,
        ..Fake::default()
    });
    arguments
        .as_object_mut()
        .expect("arguments")
        .remove("cursor");
    let first = call(&mut server, QUERY_TOOL, &arguments);
    arguments["cursor"] = first["structuredContent"]["nextCursor"].clone();
    let changed = forwarded(&mut server, "backend.search", &arguments);
    assert_eq!(changed["isError"], true);
    assert_eq!(server.product.graph_query_calls, 1);
    let second = forwarded(&mut server, QUERY_TOOL, &arguments);
    assert_eq!(second["isError"], false);
    assert_eq!(server.product.graph_query_calls, 2);
}

#[test]
fn compact_calls_keep_original_request_lifetime_deadline_and_target_retry() {
    let context = RequestContext::new();
    let original_deadline = context
        .read_deadline(std::time::Duration::from_secs(30))
        .expect("deadline");
    let mut server = ready(Fake::default());
    let params = json!({"name": CALL_TOOL, "arguments": {"name": QUERY_TOOL, "arguments": {"query": "{ Declaration { coordinate @output } }"}}});
    let message = json!({"jsonrpc": "2.0", "id": 41, "method": "tools/call", "params": params});
    let reply = server
        .handle_value(message.clone(), Some(context.clone()))
        .expect("query reply");
    assert_eq!(reply["result"]["isError"], false);
    let admitted = server
        .product
        .request_contexts
        .last()
        .expect("original caller scope");
    assert!(admitted.same_request(&context));
    assert_eq!(
        admitted
            .read_deadline(std::time::Duration::from_secs(900))
            .expect("same deadline"),
        original_deadline
    );
    context.cancel(backend_client::RequestCancellationReason::Explicit);
    let cancelled = server
        .handle_value(message, Some(context))
        .expect("cancelled reply");
    assert_eq!(cancelled["error"]["code"], -32800);
    assert_eq!(server.product.graph_query_calls, 1);

    let mut server = ready(Fake {
        surface_error: Some(ClientError::from(
            backend_runtime::RuntimeError::StartTimeout("/socket".into()),
        )),
        ..Fake::default()
    });
    let arguments = json!({"package": "pkg:npm/react@19.1.0", "detail": "full"});
    let result = forwarded(&mut server, "backend.dependencies", &arguments);
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["structuredContent"]["call"],
        json!({"name": CALL_TOOL, "arguments": {"name": "backend.dependencies", "arguments": arguments}})
    );
    server.product.surface_reply = Some(SurfaceReply::Dependencies(
        backend_library::DependencyFacts::Unavailable(ProductText::from_static(
            "dependency evidence was not captured",
        )),
    ));
    let retried = request(
        &mut server,
        "tools/call",
        &result["structuredContent"]["call"],
    );
    assert!(retried.get("error").is_none());
    assert_eq!(server.product.surface_commands.len(), 2);
    assert_eq!(
        server.product.surface_commands[0],
        server.product.surface_commands[1]
    );
}

#[test]
fn compact_query_budget_refuses_the_same_complete_payload_without_partial_rows() {
    let rows = (0..256)
        .map(|index| {
            let fields = std::collections::BTreeMap::from([
                (
                    "coordinate".to_owned(),
                    GraphValue::String(format!("pkg::module_{index}::{}", "λאב".repeat(96))),
                ),
                (
                    "summary".to_owned(),
                    GraphValue::String("high fanout graph record ".repeat(32)),
                ),
                (
                    "nested_targets".to_owned(),
                    GraphValue::List(Box::new([GraphValue::List(Box::new([GraphValue::List(
                        Box::new([
                            GraphValue::String(format!("target_{index}")),
                            GraphValue::Unsigned(index),
                        ]),
                    )]))])),
                ),
            ]);
            GraphQueryRow::new(fields).expect("admitted graph row")
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let arguments = json!({"query": "{ Declaration { coordinate @output } }"});
    let mut direct = ready(Fake {
        graph_rows: Some(rows.clone()),
        ..Fake::default()
    });
    let original = request(
        &mut direct,
        "tools/call",
        &json!({"name": QUERY_TOOL, "arguments": arguments}),
    );
    let mut compact = ready(Fake {
        graph_rows: Some(rows),
        ..Fake::default()
    });
    let result = request(
        &mut compact,
        "tools/call",
        &json!({"name": CALL_TOOL, "arguments": {"name": QUERY_TOOL, "arguments": arguments}}),
    );
    assert_eq!(result, original);
    assert_context_bounded(&result);
    assert_eq!(result["result"]["isError"], true);
    assert_eq!(result["result"]["structuredContent"]["cause"], "oversized");
    assert!(result["result"]["structuredContent"].get("rows").is_none());
}

#[test]
fn compact_queries_expose_complete_copyable_continuations_at_all_details() {
    for detail in ["summary", "standard", "full"] {
        let mut server = ready(Fake {
            graph_continue: true,
            ..Fake::default()
        });
        let arguments = json!({"query":"query($names: [String!]) { Declaration { name @filter(op: \"one_of\", value: [\"$names\"]) @output } }", "variables":{"names":["ferris","main"]}, "limit":1, "detail":detail});
        let first = forwarded(&mut server, QUERY_TOOL, &arguments);
        assert_eq!(first["isError"], false, "{first}");
        assert_context_bounded(&first);
        assert!(text_of(&first).contains("Continue `backend.call`"));
        assert!(!text_of(&first).contains("Continue `backend.query`"));
        let next = &first["structuredContent"]["continuation_call"];
        let mut expected = arguments.clone();
        expected["cursor"] = first["structuredContent"]["nextCursor"].clone();
        assert_eq!(
            next,
            &json!({"name":CALL_TOOL,"arguments":{"name":QUERY_TOOL,"arguments":expected}})
        );
        let second = request(&mut server, "tools/call", next);
        assert_eq!(second["result"]["isError"], false, "{second}");
        assert_eq!(
            second["result"]["structuredContent"]["terminal"],
            "complete"
        );
        assert!(
            second["result"]["structuredContent"]
                .get("continuation_call")
                .is_none()
        );
        assert_eq!(
            server.product.graph_query_inputs[0],
            server.product.graph_query_inputs[1]
        );
    }
}

#[test]
fn compact_nonquery_pages_expose_copyable_original_operands_and_effective_limits() {
    for detail in ["summary", "full"] {
        let mut server = ready(Fake {
            surface_index_search_pages: true,
            ..Fake::default()
        });
        let arguments = json!({"query":"maven","limit":1,"detail":detail});
        let first = forwarded(&mut server, "backend.index_search", &arguments);
        assert_eq!(first["isError"], false, "{first}");
        assert!(text_of(&first).contains("Continue `backend.call`"));
        assert!(!text_of(&first).contains("Continue `backend.index_search`"));
        let next = &first["structuredContent"]["continuation_call"];
        let mut expected = arguments;
        expected["cursor"] = first["structuredContent"]["nextCursor"].clone();
        assert_eq!(
            next,
            &json!({"name":CALL_TOOL,"arguments":{"name":"backend.index_search","arguments":expected}})
        );
        let second = request(&mut server, "tools/call", next);
        assert_eq!(second["result"]["isError"], false, "{second}");
        assert_eq!(
            server.product.surface_index_search_seen,
            [None, Some(FIXTURE_INDEX_CURSOR.to_owned())]
        );
        assert!(
            second["result"]["structuredContent"]
                .get("continuation_call")
                .is_none()
        );

        let mut server = ready(Fake {
            page_catalog: Some(paging_catalog(6)),
            ..Fake::default()
        });
        let first = forwarded(
            &mut server,
            "backend.outline",
            &json!({"path":PROJECT,"limit":1,"detail":detail}),
        );
        assert_eq!(first["isError"], false, "{first}");
        assert!(text_of(&first).contains("Continue `backend.call`"));
        let next = &first["structuredContent"]["continuation_call"];
        assert_eq!(next["name"], CALL_TOOL);
        assert_eq!(next["arguments"]["name"], "backend.outline");
        assert_eq!(next["arguments"]["arguments"]["path"], PROJECT);
        assert_eq!(next["arguments"]["arguments"]["limit"], 1);
        let second = request(&mut server, "tools/call", next);
        assert_eq!(second["result"]["isError"], false, "{second}");
        assert_eq!(server.product.page_limits, [1, 1]);
    }
}

#[test]
fn compact_fitting_semantic_shape_view_preserves_exact_typed_data() {
    // A captured unavailable display view proves projection parity, not compiler availability.
    let operands: Value = serde_json::from_str(include_str!(
        "../../../../../crates/library/fixtures/semantic-shape-read.json"
    ))
    .expect("shared request");
    let export: backend_library::SemanticShapeExport = serde_json::from_str(include_str!(
        "../../../../../crates/library/fixtures/semantic-shape-egress-view.json"
    ))
    .expect("shared view");
    for detail in ["summary", "full"] {
        let arguments = json!({"request":operands,"detail":detail});
        let mut direct = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticShapes(export.clone())),
            ..Fake::default()
        });
        let original = call(&mut direct, "backend.semantic_shapes", &arguments);
        let mut compact = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticShapes(export.clone())),
            ..Fake::default()
        });
        let result = forwarded(&mut compact, "backend.semantic_shapes", &arguments);
        assert_eq!(result, original);
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(
            result["structuredContent"]["semantic_data"]["value"],
            json!(export)
        );
        assert_eq!(
            compact.product.surface_commands,
            direct.product.surface_commands
        );
        assert_context_bounded(&result);
    }
}

#[test]
fn compact_large_query_and_variables_are_complete_or_atomically_refused() {
    for size in [128, 8000] {
        let arguments = json!({
            "query": format!("# {}\n{{ Declaration {{ coordinate @output }} }}", "λאב".repeat(size / 2)),
            "variables": {"names": ["λאב".repeat(size), "second operand"], "depth": 3},
            "limit": 1,
            "detail": "full",
        });
        let mut server = ready(Fake {
            graph_continue: true,
            ..Fake::default()
        });
        let result = forwarded(&mut server, QUERY_TOOL, &arguments);
        assert_context_bounded(&result);
        if size == 128 {
            assert_eq!(result["isError"], false, "{result}");
            let next = &result["structuredContent"]["continuation_call"];
            let mut expected = arguments.clone();
            expected["cursor"] = result["structuredContent"]["nextCursor"].clone();
            assert_eq!(next["arguments"]["arguments"], expected);
            let second = request(&mut server, "tools/call", next);
            assert_eq!(second["result"]["isError"], false, "{second}");
        } else {
            assert_eq!(result["isError"], true, "{result}");
            assert_eq!(result["structuredContent"]["cause"], "oversized");
            assert!(
                result["structuredContent"]
                    .get("continuation_call")
                    .is_none()
            );
            assert!(result["structuredContent"].get("rows").is_none());
        }
    }
}

#[test]
fn compact_startup_retry_retains_durable_mutation_identity_and_requested_detail() {
    let key = backend_library::IndexOperationKey::from_bytes([0x79; 32]).expect("key");
    let ticket = index_job_ticket();
    let arguments = json!({"package":ticket.package().as_str(), "operation_key":key, "execution_intent":"background", "detail":"full"});
    let mut server = ready(Fake {
        surface_error: Some(ClientError::from(
            backend_runtime::RuntimeError::StartTimeout("/socket".into()),
        )),
        ..Fake::default()
    });
    let first = forwarded(&mut server, INDEX_START_TOOL, &arguments);
    let retry = &first["structuredContent"]["call"];
    assert_eq!(
        retry,
        &json!({"name":CALL_TOOL,"arguments":{"name":INDEX_START_TOOL,"arguments":arguments}})
    );
    assert!(text_of(&first).contains("Retry the exact tool call"));
    let status = backend_library::IndexOperationStatus::new(
        key,
        ticket.package().clone(),
        CompileExecutionIntent::Background,
        backend_library::IndexOperationState::Active {
            ticket,
            stage: backend_library::IndexJobStage::Acquiring,
        },
    );
    server.product.surface_reply = Some(SurfaceReply::IndexOperationStarted(
        backend_library::IndexOperationObservation::Known(status),
    ));
    let second = request(&mut server, "tools/call", retry);
    assert_eq!(second["result"]["isError"], false, "{second}");
    assert_eq!(server.product.surface_commands.len(), 2);
    assert_eq!(
        server.product.surface_commands[0],
        server.product.surface_commands[1]
    );
}

#[test]
fn startup_reader_calls_retain_tagged_fault_output_contract_and_wire_budget() {
    for compact in [false, true] {
        let error = ClientError::from(backend_runtime::RuntimeError::StartTimeout(
            "/socket".into(),
        ));
        let fault = Fault::from_client_error(&error, backend_present::Operand::Whole);
        let mut server = ready(Fake {
            surface_error: Some(error),
            ..Fake::default()
        });
        let tools = request(&mut server, "tools/list", &json!({}));
        let answer_schema =
            &tool_named(&tools["result"]["tools"], "backend.document")["outputSchema"];
        let arguments = json!({"package":"pkg:npm/react@19.1.0", "detail":"full"});
        let selected = json!({"name":"backend.dependencies","arguments":arguments});
        let params = if compact {
            json!({"name":CALL_TOOL,"arguments":selected})
        } else {
            selected
        };
        let response = request(&mut server, "tools/call", &params);
        assert_context_bounded(&response);
        let result = &response["result"];
        let structured = &result["structuredContent"];
        assert_eq!(result["isError"], true);
        assert_eq!(structured["answer"], "fault");
        for required in answer_schema["required"]
            .as_array()
            .expect("output requirements")
        {
            assert!(
                structured.get(required.as_str().expect("name")).is_some(),
                "{required}: {structured}"
            );
        }
        assert!(
            answer_schema["properties"]["answer"]["enum"]
                .as_array()
                .expect("closed tag")
                .contains(&structured["answer"])
        );
        let canonical = fault_value(&fault);
        for (field, value) in canonical.as_object().expect("tagged canonical fault") {
            if field != "shell" && field != "call" {
                assert_eq!(&structured[field], value, "fault field {field}");
            }
        }
        assert_eq!(structured["call"], params);
        assert!(structured.get("shell").is_none());
        let budget = &result["_meta"]["backend/wireBudget"];
        assert!(budget["bytes"].as_u64().expect("actual wire bytes") > 0);
        assert_eq!(budget["limitBytes"], DEFAULT_RESPONSE_BUDGET_BYTES);
        assert_eq!(budget["scope"], "complete_jsonrpc_line");
        let _typed: backend_present::FaultDto =
            serde_json::from_value(structured.clone()).expect("required typed fault fields");
    }
}

fn native_graph_fixture() -> (
    backend_library::PackageGraphPage,
    backend_library::PackageGraphPage,
) {
    use backend_library::{
        DependencyAuthority, DependencyEvidence, DependencyScope, PACKAGE_GRAPH_PAGE_SCHEMA,
        PackageDependencyRecord, PackageDependencyTarget, PackageGraphCursor,
        PackageGraphDirection, PackageGraphKnowledge, PackageGraphPage, PackageGraphPageRequest,
        PackageGraphPageTerminal, PackageGraphSourceAuthority, PackageGraphSourceKey,
        RegistryEcosystem,
    };
    let package = PackageReference::parse("pkg:cargo/serde@1.0.228").expect("package");
    let authority = PackageGraphSourceAuthority::Local([7; 32]);
    let source = PackageGraphSourceKey::new(package.clone(), authority);
    let record = PackageDependencyRecord::new_with_source_authority(
        package.clone(),
        authority,
        PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde_derive", "=1.0.228", None)
            .expect("target"),
        DependencyScope::Runtime,
        false,
        DependencyEvidence {
            authority: DependencyAuthority::LocalManifest,
            frontier: [7; 32],
            provenance: [8; 32],
        },
    );
    let request = PackageGraphPageRequest::new(
        package.clone(),
        PackageGraphDirection::Dependencies,
        Some(authority),
        1,
    )
    .expect("request");
    let cursor = PackageGraphCursor {
        schema: PACKAGE_GRAPH_PAGE_SCHEMA,
        limit: 1,
        seal: [0; 32],
        view_root: [9; 32],
        facts_witness: [10; 32],
        recipe: request.recipe(Some(&source)),
        catalog_snapshot: None,
        source: Some(source.clone()),
        after_edge_id: record.facts_version,
    };
    let first = PackageGraphPage {
        schema: PACKAGE_GRAPH_PAGE_SCHEMA,
        limit: 1,
        view_root: [9; 32],
        facts_witness: [10; 32],
        catalog_snapshot: None,
        package,
        direction: PackageGraphDirection::Dependencies,
        source: Some(source),
        knowledge: PackageGraphKnowledge::Known,
        rows: Box::new([record]),
        terminal: PackageGraphPageTerminal::More(cursor),
    };
    first.admit_for(&request).expect("native typed page");
    let mut second = first.clone();
    second.rows = Box::new([]);
    second.terminal = PackageGraphPageTerminal::Complete;
    let PackageGraphPageTerminal::More(cursor) = &first.terminal else {
        panic!("cursor")
    };
    second
        .admit_for(&request.with_cursor(cursor.clone()))
        .expect("terminal typed page");
    (first, second)
}

#[test]
fn catalog_native_json_graph_cursor_reaches_lowering_and_copyable_direct_or_compact_call() {
    use backend_library::{PackageGraphPageTerminal, SurfaceCommand};
    use backend_present::ContinuationDomain;
    let grammar = grammar_for("package-graph").expect("grammar");
    assert_eq!(
        grammar.continuation_domain(),
        Some(ContinuationDomain::NativeOwnerJson)
    );
    let mut discovery = ready(Fake::default());
    let catalog = request(
        &mut discovery,
        "resources/read",
        &json!({"uri":CATALOG_RESOURCE_URI}),
    );
    let text = catalog["result"]["contents"][0]["text"]
        .as_str()
        .expect("catalog");
    let schema = text
        .split("`backend.package_graph` —")
        .nth(1)
        .and_then(|entry| entry.split_once("```json\n"))
        .and_then(|(_, schema)| schema.split_once("\n```").map(|(schema, _)| schema))
        .and_then(|schema| serde_json::from_str::<Value>(schema).ok())
        .expect("exact catalog input schema");
    assert_eq!(schema["properties"]["cursor"]["type"], "object");
    for compact in [false, true] {
        for detail in ["summary", "standard", "full"] {
            let (first_page, second_page) = native_graph_fixture();
            let PackageGraphPageTerminal::More(expected_cursor) = &first_page.terminal else {
                panic!("more page")
            };
            let authority = first_page
                .source
                .as_ref()
                .expect("source")
                .authority
                .selector();
            let arguments = json!({"package":first_page.package.as_str(),"direction":"dependencies","authority":authority,"limit":1,"detail":detail});
            let mut server = ready(Fake {
                surface_replies: std::collections::VecDeque::from([
                    SurfaceReply::PackageGraphPage(first_page.clone()),
                    SurfaceReply::PackageGraphPage(second_page.clone()),
                ]),
                ..Fake::default()
            });
            let first = if compact {
                forwarded(&mut server, "backend.package_graph", &arguments)
            } else {
                call(&mut server, "backend.package_graph", &arguments)
            };
            assert_eq!(first["isError"], false, "{first}");
            assert_context_bounded(&first);
            assert_eq!(
                first["structuredContent"]["package_graph_page"],
                json!(first_page)
            );
            assert!(first["structuredContent"].get("nextCursor").is_none());
            let next = &first["structuredContent"]["continuation_call"];
            let mut expected_arguments = arguments;
            expected_arguments["cursor"] = json!(expected_cursor);
            let expected = json!({"name":"backend.package_graph","arguments":expected_arguments});
            assert_eq!(
                next,
                &if compact {
                    json!({"name":CALL_TOOL,"arguments":expected})
                } else {
                    expected
                }
            );
            assert!(text_of(&first).contains("structuredContent.continuation_call"));
            if compact {
                assert!(text_of(&first).contains("Continue `backend.call`"));
            }
            let second = request(&mut server, "tools/call", next);
            assert_eq!(second["result"]["isError"], false, "{second}");
            assert_eq!(
                second["result"]["structuredContent"]["package_graph_page"],
                json!(second_page)
            );
            assert!(
                second["result"]["structuredContent"]
                    .get("continuation_call")
                    .is_none()
            );
            assert!(
                matches!(&server.product.surface_commands[1], SurfaceCommand::PackageGraphPage{request} if request.cursor.as_ref()==Some(expected_cursor) && request.limit==1 && request.package==first_page.package && request.authority==Some(first_page.source.as_ref().expect("source").authority))
            );
            let SurfaceCommand::PackageGraphPage { request } = &server.product.surface_commands[1]
            else {
                panic!("typed graph")
            };
            second_page
                .admit_for(request)
                .expect("unchanged native guards");
            let mut changed = request.clone();
            changed.limit = 2;
            assert!(
                second_page.admit_for(&changed).is_err(),
                "native recipe retains original page limit"
            );
            let mut changed = request.clone();
            changed.cursor.as_mut().expect("native cursor").recipe = [0; 32];
            assert!(
                second_page.admit_for(&changed).is_err(),
                "native recipe is never reconstructed by the wrapper"
            );
        }
    }
}

#[test]
fn native_json_cursor_rejects_wrong_shapes_unknown_fields_and_wrong_schema_before_owner_call() {
    let (page, _) = native_graph_fixture();
    let backend_library::PackageGraphPageTerminal::More(cursor) = page.terminal else {
        panic!("cursor")
    };
    let encoded = json!(cursor);
    let mut unknown = encoded.clone();
    unknown["unknown"] = json!(true);
    let mut wrong_schema = encoded.clone();
    wrong_schema["schema"] = json!(0);
    let mut server = ready(Fake::default());
    for cursor in [
        json!(serde_json::to_string(&encoded).expect("JSON text")),
        json!("mcp2-wrong-family"),
        json!([]),
        json!(null),
        unknown,
        wrong_schema,
    ] {
        let result = forwarded(
            &mut server,
            "backend.package_graph",
            &json!({"package":"pkg:cargo/serde@1.0.228","direction":"dependencies","limit":1,"cursor":cursor}),
        );
        assert_eq!(result["isError"], true, "{result}");
    }
    assert!(server.product.surface_commands.is_empty());
    let unrelated = forwarded(
        &mut server,
        "backend.dependencies",
        &json!({"package":"pkg:cargo/serde@1.0.228","cursor":encoded}),
    );
    assert_eq!(unrelated["isError"], true, "{unrelated}");
    assert_eq!(unrelated["structuredContent"]["slug"], "usage");
    assert!(server.product.surface_commands.is_empty());
}

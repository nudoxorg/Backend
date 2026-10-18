//! The unchanged real 42-image capture exercises the production receipt and gates.
use super::*;

fn captured() -> backend_library::SemanticVersionRecord {
    let packet: Value = serde_json::from_str(include_str!(
        "../../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("unchanged real 42-image capture");
    serde_json::from_value(packet["surface"]["data"][0].clone()).expect("complete native record")
}
fn selection(source: &backend_library::SemanticVersionRecord) -> SurfaceCommand {
    SurfaceCommand::SelectSemanticVersion {
        package: source.package.clone(),
        coordinate: source.coordinate.clone(),
        profile: source.profile,
        generation: source.generation,
    }
}
fn arguments(source: &backend_library::SemanticVersionRecord, detail: Detail) -> Value {
    json!({
        "package":source.package.as_str(), "coordinate":source.coordinate.as_str(),
        "profile":source.profile.name().expect("closed profile"),
        "generation":encode_id(&source.generation.to_bytes()), "detail":detail.name()
    })
}
fn invocation(
    exposure: McpExposure,
    source: &backend_library::SemanticVersionRecord,
    detail: Detail,
) -> Value {
    exposure.expose_call(
        json!({"name":"backend.select_semantic_version","arguments":arguments(source,detail)}),
    )
}

#[test]
fn semantic_selection_receipt_preserves_large_native_record_and_admits_every_mcp_exposure() {
    let source = captured();
    let reply = SurfaceReply::SemanticVersionSelected(source.clone());
    backend_library::admit_surface_reply(&selection(&source), &reply)
        .expect("real selected record");
    let view = backend_present::product_view_for_command(&selection(&source), &reply);
    let answer = Answer::Product(Box::new(view));
    let payload = encode_answer(&answer, Detail::Full, None, DEFAULT_RESPONSE_BUDGET_BYTES)
        .expect("complete CLI/native projection still fits its byte contract");
    let cli: backend_present::ProductDto = serde_json::from_slice(&payload.bytes).expect("CLI DTO");
    assert_eq!(
        cli.semantic_data,
        Some(backend_present::ProductSemanticData::Versions(Box::new([
            source.clone()
        ])))
    );
    let structured =
        serde_json::from_slice(&payload.bytes).expect("complete legacy structured data");
    let legacy = wire_admission::ResponseBudget::new(&json!(9))
        .prepare(complete_tool_result("", structured, false));
    assert!(
        !legacy.fits(),
        "the old complete record must reproduce the actual budget defect even with no preview"
    );
    assert!(legacy.tokens().expect("real capture tokens") > MCP_CL100K_TOKEN_LIMIT);
    assert!(
        legacy.bytes() <= DEFAULT_RESPONSE_BUDGET_BYTES,
        "this regression isolates the token gate"
    );

    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
            let mut server = ready(Fake {
                surface_replies: std::collections::VecDeque::from([reply.clone(), reply.clone()]),
                ..Fake::default()
            });
            // Reconfirm an already selected generation. No changed/new-write claim.
            for _ in 0..2 {
                let response = request(
                    &mut server,
                    "tools/call",
                    &invocation(exposure, &source, detail),
                );
                assert_context_bounded(&response);
                assert!(wire_tokens(&response).expect("actual envelope") <= MCP_CL100K_TOKEN_LIMIT);
                assert!(response.get("error").is_none(), "{response}");
                let result = &response["result"];
                assert_eq!(result["isError"], false);
                let receipt = &result["structuredContent"]["selection_receipt"];
                assert_eq!(receipt["schema"], "backend.semantic-selection-receipt.v1");
                assert_eq!(receipt["package"], json!(source.package));
                let card = super::super::tools::catalog_tool("backend.select_semantic_version")
                    .expect("advertised selector");
                let package_schema = &card["outputSchema"]["properties"]["selection_receipt"]["properties"]
                    ["package"];
                assert_eq!(package_schema["type"], "object");
                assert!(
                    receipt["package"].is_object(),
                    "actual tagged package must match advertised object"
                );
                assert_eq!(package_schema["required"], json!(["kind", "value"]));
                assert!(
                    package_schema["properties"]["kind"]["enum"]
                        .as_array()
                        .expect("closed kinds")
                        .contains(&receipt["package"]["kind"])
                );
                assert_eq!(package_schema["properties"]["value"]["type"], "string");
                assert!(receipt["package"]["value"].is_string());
                assert_eq!(
                    serde_json::from_value::<PackageReference>(receipt["package"].clone())
                        .expect("exact typed package kind"),
                    source.package
                );

                assert_eq!(receipt["coordinate"], json!(source.coordinate));
                assert_eq!(receipt["profile"], json!(source.profile));
                assert_eq!(receipt["generation"], json!(source.generation));
                assert_eq!(receipt["selected"], true);
                assert!(
                    serde_json::from_value::<backend_library::SemanticVersionRecord>(
                        receipt.clone()
                    )
                    .is_err(),
                    "a receipt cannot masquerade as a complete semantic record"
                );
                assert!(receipt.get("changed").is_none());
                assert!(receipt.get("history_status").is_none());
                assert!(
                    result["structuredContent"].get("semantic_data").is_none(),
                    "a receipt is not a truncated version record"
                );
            }
            assert_eq!(
                server.product.surface_commands,
                vec![selection(&source), selection(&source)],
                "each receipt owes exactly one mutation and no post-commit read"
            );
        }
        // The expert native-command MCP route shares the same bounded receipt.
        let mut server = ready(Fake {
            surface_reply: Some(reply.clone()),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":SURFACE_TOOL,"arguments":{"command":selection(&source),"detail":detail.name()}}),
        );
        assert_eq!(
            response["result"]["structuredContent"]["selection_receipt"]["generation"],
            json!(source.generation),
            "{response}"
        );
        assert_eq!(response["result"]["isError"], false);
        assert_context_bounded(&response);
        assert!(wire_tokens(&response).expect("expert receipt tokens") <= MCP_CL100K_TOKEN_LIMIT);
        assert_eq!(
            response["result"]["structuredContent"]["metadata_call"],
            json!({"name":SURFACE_TOOL,"arguments":{"command":SurfaceCommand::SemanticVersions {package:source.package.clone(),limit:1,cursor:None},"detail":"full"}}),
            "expert reads preserve the exact tagged package namespace"
        );
        assert_eq!(server.product.surface_commands, vec![selection(&source)]);
    }
}

#[test]
fn semantic_selection_receipt_metadata_call_recovers_the_complete_captured_history() {
    let source = captured();
    for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticVersionSelected(source.clone())),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &invocation(exposure, &source, Detail::Full),
        );
        let next = response["result"]["structuredContent"]["metadata_call"].clone();
        let expected=exposure.expose_call(json!({"name":"backend.semantic_versions","arguments":{"package":source.package.as_str(),"limit":1,"detail":"full"}}));
        assert_eq!(
            next, expected,
            "the exposed read is copyable, not a cursor on the mutation"
        );
        assert_eq!(server.product.surface_commands, vec![selection(&source)]);
        let page = native_transport_source_page(&source);
        server.product.surface_reply = Some(SurfaceReply::SemanticVersionsPage(page.clone()));
        let response = request(&mut server, "tools/call", &next);
        assert_eq!(response["result"]["isError"], false, "{response}");
        let returned: backend_library::SemanticVersionsPage = serde_json::from_value(
            response["result"]["structuredContent"]["semantic_data"]["value"].clone(),
        )
        .expect("metadata page");
        assert_eq!(returned, page);
        let backend_library::SemanticVersionDescriptor::Available { reference } =
            &returned.entries[0]
        else {
            panic!("real source available")
        };
        assert_eq!(reference.metadata.coordinate, source.coordinate);
        assert_eq!(reference.metadata.profile, source.profile);
        assert_eq!(reference.metadata.generation, source.generation);
        let backend_library::SemanticSourceHistory::Published { proof, .. } = &reference.history
        else {
            panic!("published capture")
        };
        assert_eq!(proof.image_count, 42, "whole history count is retained");
        let mut union = backend_library::SemanticSourceAssembly::new(reference.source.clone())
            .expect("exact union");
        let mut next=exposure.expose_call(json!({"name":"backend.semantic_source","arguments":{"request":{"reference":reference}}}));
        server.product.semantic_source_document = Some(std::sync::Arc::new(
            backend_library::SemanticSourceDocument::new(&source).expect("complete capture"),
        ));
        loop {
            let response = request(&mut server, "tools/call", &next);
            assert_eq!(response["result"]["isError"], false, "{response}");
            assert_context_bounded(&response);
            assert!(wire_tokens(&response).expect("source tokens") <= MCP_CL100K_TOKEN_LIMIT);
            let chunk: backend_library::SemanticSourceChunk = serde_json::from_value(
                response["result"]["structuredContent"]["semantic_data"]["value"].clone(),
            )
            .expect("native byte range");
            union.append(&chunk).expect("strict monotonic union");
            if chunk.complete {
                break;
            }
            next = response["result"]["structuredContent"]["continuation_call"].clone();
        }
        assert_eq!(
            union.finish().expect("digest-checked complete source"),
            source,
            "all metadata and all42 image proofs remain accessible"
        );
    }
}

#[test]
fn semantic_selection_receipt_refuses_unencodable_envelope_before_mutation() {
    let source = captured();
    let id = json!("\u{1}".repeat(4096));
    let control = wire_admission::ResponseBudget::new(&id).measure(complete_tool_result(
        "",
        json!({}),
        false,
    ));
    assert!(control.tokens().expect("actual escaped ID tokens") > MCP_CL100K_TOKEN_LIMIT);
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::SemanticVersionSelected(source.clone())),
        ..Fake::default()
    });
    let message = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":invocation(McpExposure::RegisteredCall,&source,Detail::Full)});
    let bytes = serde_json::to_vec(&message).expect("request");
    assert!(
        bytes.len() < DEFAULT_RESPONSE_BUDGET_BYTES,
        "request itself fits"
    );
    let response = server
        .handle(&bytes)
        .map(encoded_stdio_reply)
        .expect("response");
    assert!(
        response.get("error").is_some() || response["result"]["isError"] == true,
        "{response}"
    );
    assert!(
        server.product.surface_commands.is_empty(),
        "an unencodable acknowledgement cannot commit a selection"
    );
    assert!(
        server.product.surface_reply.is_some(),
        "the native mutation was never invoked"
    );
    assert_context_bounded(&response);
}

#[test]
fn semantic_selection_receipt_never_confirms_a_different_lane_or_an_owner_failure() {
    let source = captured();
    let mut different = source.clone();
    different.generation = backend_library::SemanticGenerationId::new([99; 32]);
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::SemanticVersionSelected(source.clone())),
        ..Fake::default()
    });
    let response = request(
        &mut server,
        "tools/call",
        &invocation(McpExposure::Direct, &different, Detail::Full),
    );
    assert!(
        response.get("error").is_some() || response["result"]["isError"] == true,
        "{response}"
    );
    assert!(
        response["result"]["structuredContent"]
            .get("selection_receipt")
            .is_none()
    );
    let mut server = ready(Fake {
        surface_error: Some(ClientError::CommandFailed(
            backend_library::CommandFailure::InvalidQuery("generation is not retained".to_owned()),
        )),
        ..Fake::default()
    });
    let response = request(
        &mut server,
        "tools/call",
        &invocation(McpExposure::RegisteredCall, &source, Detail::Full),
    );
    assert!(
        response.get("error").is_some() || response["result"]["isError"] == true,
        "{response}"
    );
    assert!(
        response["result"]["structuredContent"]
            .get("selection_receipt")
            .is_none()
    );
    assert_eq!(server.product.surface_commands, vec![selection(&source)]);
}

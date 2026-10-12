//! Named-token and native-page laws; immutable synthetic catalog, no compiler
//! admission or production runtime claim. The measured cb57 raw is retained.
use super::*;
use std::collections::BTreeSet;

fn dense_catalog(count: usize, version: u8) -> backend_library::Library {
    let source = Basis::new(view_state_root(&[version]), object_version(&[version]));
    let rows = (0..count)
        .map(|i| {
            let coordinate = format!(
                "{PROJECT}::semantic::{}::Café_אב_{i:04}",
                backend_library::encode_id(symbol_key(&format!("native-row-{i}")).as_bytes())
            );
            Row::in_package(
                RowId::Symbol(symbol_key(&coordinate)),
                source,
                package_key(PROJECT),
                coordinate,
            )
            .with_kind(DeclarationKind::Struct)
        })
        .chain(std::iter::once(Row::new(
            RowId::Package(package_key(PROJECT)),
            source,
            PROJECT,
        )))
        .collect();
    let root = ViewRoot::new_incomplete(
        view_key(b"dense-outline"),
        source,
        Frontier::new(source.branch, source.log, source.schema, source.root, 0),
        rows,
        vec![Coverage::Unavailable {
            lane: Lane::Exact,
            reason: Reason::NoIndex,
        }],
    )
    .expect("no compiler coverage minted");
    let cursor = backend_library::Cursor::for_view_root(&root);
    backend_library::Library::from_view(root, cursor).expect("real immutable catalog")
}
fn coordinates(value: &Value, out: &mut BTreeSet<String>) {
    if let Some(coordinate) = value.get("coordinate").and_then(Value::as_str) {
        assert!(out.insert(coordinate.to_owned()), "duplicate declaration");
    }
    if let Some(children) = value.get("children").and_then(Value::as_array) {
        for child in children {
            coordinates(child, out);
        }
    }
}
#[test]
fn actual_cb57_full_outline_has_17680_tokens_and_cannot_pass_named_wire_admission() {
    let raw = include_str!("../fixtures/httpie-outline-cb57.raw").trim_end_matches('\n');
    assert_eq!(cl100k_tokens(raw).expect("actual reference frame"), 17_680);
    let response: Value = serde_json::from_str(raw).expect("actual frame");
    assert!(!wire_fits(&response, ResponseTransport::StdioLine));
    for transport in [ResponseTransport::StdioLine, ResponseTransport::HttpBody] {
        let bounded = bound_rpc_reply(response.clone(), transport);
        assert!(wire_fits(&bounded, transport));
        assert!(
            bounded.get("error").is_some(),
            "structured 200 declarations are not prefix-cut"
        );
    }
}
#[test]
fn full_outline_budget_selects_native_pages_and_complete_union_without_truncating_operands() {
    let catalog = dense_catalog(320, 1);
    let expected: BTreeSet<_> = catalog
        .view()
        .rows()
        .iter()
        .filter(|row| matches!(row.id, RowId::Symbol(_)))
        .map(|row| row.label.clone())
        .collect();
    let mut server = ready(Fake {
        page_catalog: Some(catalog),
        ..Fake::default()
    });
    let first = call(
        &mut server,
        "backend.outline",
        &json!({"path":PROJECT,"detail":"full"}),
    );
    assert_eq!(first["isError"], false, "{first}");
    let limit = first["structuredContent"]["page"]["limit"]
        .as_u64()
        .expect("effective limit");
    assert!(
        limit < 200,
        "dense native coordinates force a real smaller first page"
    );
    let source_root = first["structuredContent"]["page"]["source_root"].clone();
    let mut union = BTreeSet::new();
    let mut result = first;
    let mut pages = 0;
    loop {
        let typed: backend_present::OutlineDto =
            serde_json::from_value(result["structuredContent"].clone())
                .expect("actual public typed decode");
        assert_eq!(typed.page.as_ref().expect("page").limit, limit as u16);
        assert_eq!(
            result["structuredContent"]["page"]["source_root"],
            source_root
        );
        for root in result["structuredContent"]["roots"]
            .as_array()
            .expect("roots")
        {
            coordinates(root, &mut union);
        }
        let cursor = result["structuredContent"]["nextCursor"]
            .as_str()
            .map(str::to_owned);
        pages += 1;
        assert!(pages < 100, "bounded page progress");
        let Some(cursor) = cursor else {
            break;
        };
        assert_eq!(
            text_of(&result).matches(&cursor).count(),
            0,
            "opaque proof occurs once, only in typed content"
        );
        let changed_limit = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.outline","arguments":{"path":PROJECT,"limit":limit + 1,"detail":"full","cursor":cursor}}),
        );
        assert!(
            changed_limit.get("error").is_some() || changed_limit["result"]["isError"] == true,
            "different query credit is refused"
        );
        result = call(
            &mut server,
            "backend.outline",
            &json!({"path":PROJECT,"limit":limit,"detail":"full","cursor":cursor}),
        );
        assert_eq!(result["isError"], false, "{result}");
    }
    assert_eq!(union, expected);
    assert!(pages > 1);
}
#[test]
fn outline_budget_reread_refuses_a_new_source_and_cancelled_original_lifetime() {
    let mut server = ready(Fake {
        page_catalog: Some(dense_catalog(320, 1)),
        outline_source_after_first_page: Some(dense_catalog(320, 2)),
        ..Fake::default()
    });
    let reply = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.outline","arguments":{"path":PROJECT,"detail":"full"}}),
    );
    assert!(
        reply.get("error").is_some() || reply["result"]["isError"] == true,
        "{reply}"
    );
    assert!(
        server.product.page_limits.len() >= 2,
        "actual adaptive re-read reached"
    );
    let lifetime = RequestContext::new();
    lifetime.cancel(RequestCancellationReason::Explicit);
    let calls = server.product.page_limits.len();
    let reply = server.handle_value(json!({"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"backend.outline","arguments":{"path":PROJECT}}}), Some(lifetime)).expect("cancel reply");
    assert!(reply.get("error").is_some());
    assert_eq!(
        calls,
        server.product.page_limits.len(),
        "cancelled request never probes owner"
    );
}
#[test]
fn whole_envelope_counts_unicode_punctuation_id_and_escaping_and_preserves_small_typed_fields() {
    let typed = json!({"answer":"records","coordinate":"/α::café","more":true,"nextCursor":"actual-owner-proof"});
    let reply = success(
        json!("id-".to_owned() + &"אב🙂".repeat(500)),
        tool_result(&"\\\"🙂[]{}:".repeat(6000), typed.clone(), false),
    );
    for transport in [ResponseTransport::StdioLine, ResponseTransport::HttpBody] {
        let bounded = bound_rpc_reply(reply.clone(), transport);
        assert!(wire_fits(&bounded, transport));
        assert_eq!(bounded["result"]["structuredContent"], typed);
        assert_eq!(
            bounded["result"]["_meta"]["backend/wireBudget"]["tokenPolicy"],
            CL100K_POLICY
        );
    }
}

#[test]
fn original_byte_legal_dense_payload_is_honestly_refused_without_losing_request_identity() {
    let id = format!("request-{}", "אב🙂".repeat(2_000));
    let structured = json!({"answer":"records", "rows":"s".repeat(31_000),
        "more":true, "nextCursor":"opaque-owner-cursor-page-two"});
    let reply = success(
        json!(id.clone()),
        tool_result(&"λ אב🙂\n".repeat(6_000), structured, false),
    );
    let minimum = fit_tool_preview(
        attach_wire_budget(reply.clone(), ResponseTransport::StdioLine),
        ResponseTransport::StdioLine,
    );
    assert!(
        transport_serialized_bytes(&minimum, ResponseTransport::StdioLine)
            <= MCP_RESULT_BUDGET_BYTES
    );
    assert!(wire_tokens(&minimum).expect("exact density") > MCP_CL100K_TOKEN_LIMIT);
    let bounded = bound_rpc_reply(reply, ResponseTransport::StdioLine);
    assert!(bounded.get("error").is_some());
    assert_eq!(bounded["id"], id);
    assert!(wire_fits(&bounded, ResponseTransport::StdioLine));
    assert!(
        bounded["error"]["data"]["detail"]
            .as_str()
            .expect("bounded cause")
            .contains(CL100K_POLICY)
    );
}

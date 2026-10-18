//! Named-token and native-page laws; immutable synthetic catalog, no compiler
//! admission or production runtime claim. The measured cb57 raw is retained.
use super::*;
use std::collections::BTreeSet;

fn dense_catalog(count: usize, version: u8) -> backend_library::Library {
    let source = Basis::new(
        view_state_root(&[("dense-outline".to_owned(), version.to_string())]),
        object_version(&[version]),
    );
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
    let typed = json!({"answer":"records","coordinate":"/α::café","more":true,"nextCursor":"actual-owner-proof",
        "continuation_call":{"name":"backend.call","arguments":{"name":"backend.search","arguments":{
            "query":"literal ' \" \n scope","limit":3,"cursor":"actual-owner-proof"}}}});
    let reply = success(
        json!("id-".to_owned() + &"אב🙂".repeat(500)),
        tool_result(&"\\\"🙂[]{}:".repeat(6000), typed.clone(), false),
    );
    for transport in [ResponseTransport::StdioLine, ResponseTransport::HttpBody] {
        let bounded = bound_rpc_reply(reply.clone(), transport);
        assert!(wire_fits(&bounded, transport));
        assert_eq!(bounded["result"]["structuredContent"], typed);
        assert!(
            text_of(&bounded["result"])
                .contains("when present, copy structuredContent.continuation_call unchanged"),
            "both transport preview fits preserve the executable continuation hint"
        );
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

#[test]
fn outline_selector_bytes_shrink_native_first_page_and_preserve_complete_ids() {
    use super::super::tools::CALL_TOOL;
    // Identical authored display coordinates deliberately cannot stand in for
    // exact IDs. The old node shape fits 200 rows; retained selectors do not.
    let source = Basis::new(view_state_root(&[]), object_version(b"selector-budget"));
    let coordinate = format!("{PROJECT}::ui/prism.min.js:3::{}", "u".repeat(40));
    let symbols = (0..320)
        .map(|i| symbol_key(&format!("selected-outline-{i}")))
        .collect::<Vec<_>>();
    let mut rows = symbols
        .iter()
        .map(|id| {
            Row::in_package(
                RowId::Symbol(*id),
                source,
                package_key(PROJECT),
                &coordinate,
            )
            .with_kind(DeclarationKind::Function)
        })
        .collect::<Vec<_>>();
    rows.push(Row::new(
        RowId::Package(package_key(PROJECT)),
        source,
        PROJECT,
    ));
    let root = ViewRoot::new_incomplete(
        view_key(b"selector-budget"),
        source,
        Frontier::new(source.branch, source.log, source.schema, source.root, 0),
        rows,
        vec![Coverage::Unavailable {
            lane: Lane::Exact,
            reason: Reason::NoIndex,
        }],
    )
    .expect("no compiler coverage minted");
    let catalog = backend_library::Library::from_view(
        root.clone(),
        backend_library::Cursor::for_view_root(&root),
    )
    .expect("immutable catalog");
    let expected = symbols
        .iter()
        .map(|id| backend_library::encode_id(id.as_bytes()))
        .collect::<BTreeSet<_>>();
    let mut producer = Fake {
        page_catalog: Some(catalog.clone()),
        ..Fake::default()
    };
    let answer = answer_paged(
        &mut producer,
        &Request::OutlinePage {
            path: PROJECT.to_owned(),
            limit: 200,
        },
        None,
    )
    .expect("real native first-page producer");
    let complete = encode_answer(
        &answer,
        Detail::Full,
        None,
        backend_present::MAX_RESPONSE_BUDGET_BYTES,
    )
    .expect("complete boundary projection");
    assert!(
        complete.bytes.len() > DEFAULT_RESPONSE_BUDGET_BYTES,
        "new structured DTO itself crosses the cap, before envelope admission"
    );
    let mut without_selectors: Value = serde_json::from_slice(&complete.bytes).expect("JSON");
    let nodes = without_selectors["roots"]
        .as_array_mut()
        .expect("flat roots");
    assert_eq!(nodes.len(), 200);
    for node in nodes {
        assert!(
            node.as_object_mut()
                .expect("node")
                .remove("semantic_data")
                .is_some()
        );
    }
    assert!(
        serde_json::to_vec(&without_selectors)
            .expect("old shape")
            .len()
            < DEFAULT_RESPONSE_BUDGET_BYTES,
        "only removing the new selector fields restores structured admission"
    );
    assert!(encode_answer(&answer, Detail::Full, None, DEFAULT_RESPONSE_BUDGET_BYTES).is_err());

    for forwarded in [false, true] {
        let mut server = ready(Fake {
            page_catalog: Some(catalog.clone()),
            ..Fake::default()
        });
        let invoke = |server: &mut Server<Fake>, arguments: Value| {
            let params = if forwarded {
                json!({"name":CALL_TOOL,"arguments":{"name":"backend.outline","arguments":arguments}})
            } else {
                json!({"name":"backend.outline","arguments":arguments})
            };
            request(server, "tools/call", &params)
        };
        let mut frame = invoke(
            &mut server,
            json!({"path":PROJECT,"limit":200,"detail":"full"}),
        );
        assert_eq!(frame["result"]["isError"], false, "{frame}");
        assert!(
            server.product.page_limits.len() >= 2,
            "actual structured overflow must reach another native page request"
        );
        assert_eq!(server.product.page_limits[0], 200);
        let page = &frame["result"]["structuredContent"];
        let limit = page["page"]["limit"].as_u64().expect("effective limit");
        assert!(limit < 200);
        let basis = page["page"]["source_root"].clone();
        let mut ids = BTreeSet::new();
        let mut cursors = BTreeSet::new();
        let mut pages = 0;
        loop {
            assert!(wire_fits(&frame, ResponseTransport::StdioLine));
            assert!(wire_fits(&frame, ResponseTransport::HttpBody));
            let page = &frame["result"]["structuredContent"];
            assert_eq!(page["page"]["limit"], limit);
            assert_eq!(page["page"]["source_root"], basis);
            let nodes = page["roots"].as_array().expect("roots");
            assert_eq!(page["declarations"], nodes.len());
            for node in nodes {
                assert_eq!(node["coordinate"], coordinate);
                assert_eq!(node["semantic_data"]["kind"], "selected-symbol-id");
                let bytes: [u8; 32] =
                    serde_json::from_value(node["semantic_data"]["value"].clone())
                        .expect("exact selected ID");
                assert!(
                    ids.insert(backend_library::encode_id(&bytes)),
                    "no exact ID repeated"
                );
            }
            pages += 1;
            assert!(pages <= symbols.len(), "bounded progress");
            let Some(cursor) = page["nextCursor"].as_str().map(str::to_owned) else {
                assert_eq!(page["page"]["more"], false);
                assert_eq!(page["extent"], "complete");
                break;
            };
            assert_eq!(page["page"]["more"], true);
            assert!(cursors.insert(cursor.clone()));
            if pages == 1 {
                let wrong = invoke(
                    &mut server,
                    json!({"path":PROJECT,"limit":limit+1,
                    "detail":"full","cursor":cursor}),
                );
                assert!(
                    wrong.get("error").is_some() || wrong["result"]["isError"] == true,
                    "issued continuation cannot silently change its limit"
                );
            }
            frame = invoke(
                &mut server,
                json!({"path":PROJECT,"limit":limit,
                "detail":"full","cursor":cursor}),
            );
            assert_eq!(frame["result"]["isError"], false, "{frame}");
        }
        assert_eq!(ids, expected);
        assert!(pages > 1);
    }
}


#[test]
fn late_token_dense_outline_continuations_shrink_delivery_keep_credit_and_preserve_every_full_id() {
    let source = Basis::new(view_state_root(&[]), object_version(b"late-outline-density"));
    // Alternating letters and digits are separate cl100k regex pieces. A run
    // of one repeated letter compresses and does not exercise token admission.
    let dense_segment = "p0".repeat(125);
    assert_eq!(cl100k_tokens(&dense_segment).expect("named dense fixture tokenizer"), 250);
    let ids = (0..200).map(|n| symbol_key(&format!("late-outline-id-{n}"))).collect::<Vec<_>>();
    let mut rows = ids.iter().enumerate().map(|(n, id)| {
        let coordinate = if n < 50 { format!("{PROJECT}::A{n:03}") }
            else if n < 100 { format!("{PROJECT}::z/{}::Late", dense_segment) }
            else { format!("{PROJECT}::zzTail{n:03}") };
        Row::in_package(RowId::Symbol(*id), source, package_key(PROJECT), coordinate)
            .with_kind(DeclarationKind::Function)
    }).collect::<Vec<_>>();
    rows.push(Row::new(RowId::Package(package_key(PROJECT)), source, PROJECT));
    let root = ViewRoot::new_incomplete(view_key(b"late-outline-density"), source,
        Frontier::new(source.branch, source.log, source.schema, source.root, 0), rows,
        vec![Coverage::Unavailable { lane: Lane::Exact, reason: Reason::NoIndex }]).expect("incomplete native fixture");
    let catalog = backend_library::Library::from_view(root.clone(), backend_library::Cursor::for_view_root(&root)).expect("native catalog");
    let credit = backend_library::QueryLimit::new(50).expect("actual failing issued credit");
    let first_request = backend_library::PageRequest::new(catalog.revision_root(), credit);
    let first = catalog.outline_page(package_key(PROJECT), first_request).expect("small first page");
    let dense = catalog.outline_page(package_key(PROJECT), first_request.with_continuation(
        PageContinuation::from_cursor(first.snapshot.next.expect("next")))).expect("native dense later page");
    assert_eq!(dense.snapshot.root.row_count(), 50);
    let dense_answer = backend_present::catalog_page_answer(&Request::OutlinePage { path: PROJECT.to_owned(), limit: 50 },
        ReplyDto::new(1, CommandReply::ProjectionPage(dense))).expect("real admitted projection");
    let encoded = encode_answer(&dense_answer, Detail::Full, None, DEFAULT_RESPONSE_BUDGET_BYTES).expect("complete native DTO fits byte policy");
    let structured: Value = serde_json::from_slice(&encoded.bytes).expect("typed JSON");
    let witness = attach_wire_budget(success(json!(60), complete_tool_result("", structured, false)), ResponseTransport::StdioLine);
    assert!(transport_serialized_bytes(&witness, ResponseTransport::StdioLine) < MCP_RESULT_BUDGET_BYTES,
        "the actual later-page boundary is token density, not byte overflow");
    assert!(wire_tokens(&witness).expect("named actual tokenizer") > MCP_CL100K_TOKEN_LIMIT);
    let expected = ids.iter().map(|id| backend_library::encode_id(id.as_bytes())).collect::<BTreeSet<_>>();
    for forwarded in [false, true] {
        let mut server = ready(Fake { page_catalog: Some(catalog.clone()), ..Fake::default() });
        let invoke = |server: &mut Server<Fake>, args: Value| request(server, "tools/call", &if forwarded {
            json!({"name":"backend.call","arguments":{"name":"backend.outline","arguments":args}})
        } else { json!({"name":"backend.outline","arguments":args}) });
        let mut frame = invoke(&mut server, json!({"path":PROJECT,"limit":50,"detail":"full"}));
        assert_eq!(frame["result"]["isError"], false, "{frame}");
        assert_eq!(server.product.page_windows.len(), 1, "first page already fits at 50");
        let basis = frame["result"]["structuredContent"]["page"]["source_root"].clone();
        let mut selected = BTreeSet::new();
        let mut cursors = BTreeSet::new();
        let mut pages = 0;
        let mut delivered = Vec::new();
        loop {
            assert!(frame.get("error").is_none(), "{frame}");
            assert!(wire_fits(&frame, ResponseTransport::StdioLine));
            assert!(wire_fits(&frame, ResponseTransport::HttpBody));
            let value = &frame["result"]["structuredContent"];
            let typed: backend_present::OutlineDto = serde_json::from_value(value.clone()).expect("unchanged public outline DTO");
            assert_eq!(typed.page.as_ref().expect("page").limit, 50, "issued credit never changes");
            assert_eq!(value["page"]["source_root"], basis);
            let mut pending = value["roots"].as_array().expect("roots").iter().collect::<Vec<_>>();
            let mut count = 0_usize;
            while let Some(node) = pending.pop() {
                let id: [u8; 32] = serde_json::from_value(node["semantic_data"]["value"].clone()).expect("complete 32-byte selected ID");
                assert_eq!(node["semantic_data"]["kind"], "selected-symbol-id");
                let id = backend_library::encode_id(&id);
                assert_eq!(node["key"], &id[..8]);
                assert!(selected.insert(id), "no selectable identity repeated");
                assert!(node["coordinate"].as_str().expect("whole coordinate").starts_with(PROJECT));
                if let Some(children) = node["children"].as_array() { pending.extend(children); }
                count += 1;
            }
            assert_eq!(value["declarations"], count);
            delivered.push(count);
            pages += 1;
            assert!(pages <= ids.len());
            let Some(cursor) = value["nextCursor"].as_str().map(str::to_owned) else {
                assert_eq!(value["page"]["more"], false);
                assert_eq!(value["extent"], "complete");
                break;
            };
            assert!(cursors.insert(cursor.clone()));
            if pages == 1 {
                let wrong = invoke(&mut server, json!({"path":PROJECT,"limit":49,"detail":"full","cursor":cursor}));
                assert!(wrong.get("error").is_some() || wrong["result"]["isError"] == true);
            }
            frame = invoke(&mut server, json!({"path":PROJECT,"limit":50,"detail":"full","cursor":cursor}));
            assert_eq!(frame["result"]["isError"], false, "{frame}");
        }
        assert_eq!(selected, expected);
        assert!(delivered.windows(2).any(|pair| pair == [25, 50]),
            "a sparse tail must re-expand to50 after a successful dense delivery25, without skipping its authenticated predecessor");
        assert!(server.product.page_windows.iter().any(|window| window.issued().get() == 50 && window.delivery().get() < 50),
            "a later page must use an actually smaller native delivery under the same credit");
        assert!(server.product.page_limits.iter().all(|limit| *limit == 50));
        assert!(pages > 3);
    }
}


#[test]
fn outline_owner_byte_refusal_retries_same_frontier_and_never_forgets_failed_read_revision() {
    for changed_failure_basis in [false, true] {
        let catalog = dense_catalog(17, 1);
        let mut server = ready(Fake { page_catalog: Some(catalog), catalog_delivery_ceiling: Some(1),
            outline_refusal_basis: changed_failure_basis.then(|| dense_catalog(17, 2).revision_root().into()),
            ..Fake::default() });
        let first = call(&mut server, "backend.outline", &json!({"path":PROJECT,"limit":4,"detail":"full"}));
        assert_eq!(first["isError"], false, "{first}");
        let cursor = first["structuredContent"]["nextCursor"].as_str().expect("issued cursor").to_owned();
        let input = *server.product.catalog_frontiers.last().expect("first frontier");
        assert!(input.is_none());
        let before = server.product.page_windows.len();
        let continued = request(&mut server, "tools/call", &json!({"name":"backend.outline", "arguments":{
            "path":PROJECT,"limit":4,"detail":"full","cursor":cursor
        }}));
        let attempts = &server.product.page_windows[before..];
        assert_eq!(attempts.iter().map(|w| w.delivery().get()).collect::<Vec<_>>(), [4, 2, 1],
            "typed native byte refusal has its own real bounded retry path");
        assert!(attempts.iter().all(|w| w.issued().get() == 4));
        let frontiers = &server.product.catalog_frontiers[before..];
        assert!(frontiers[0].is_some());
        assert!(frontiers.iter().all(|frontier| frontier == &frontiers[0]), "every attempt reuses the original input, never a refused successor");
        if changed_failure_basis {
            assert!(continued.get("error").is_some() || continued["result"]["isError"] == true, "{continued}");
            assert!(continued.to_string().contains("source changed"), "failed-read source cannot reset before the successful retry");
        } else {
            assert_eq!(continued["result"]["isError"], false, "{continued}");
            let page = &continued["result"]["structuredContent"];
            assert_eq!(page["page"]["limit"], 4);
            assert_eq!(page["declarations"], 1);
            assert_eq!(page["page"]["source_root"], first["structuredContent"]["page"]["source_root"]);
            assert!(wire_fits(&continued, ResponseTransport::StdioLine));
        }
    }
}

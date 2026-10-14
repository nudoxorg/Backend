//! Native catalog pages under actual whole-envelope pressure.
use super::*;

fn captured_record_answer(captured: &Value) -> Answer {
    let page: backend_present::RecordListDto =
        serde_json::from_value(captured.clone()).expect("actual captured presentation page");
    let records = page
        .records
        .iter()
        .map(|record| {
            let key = symbol_key(&record.identity.coordinate);
            let Some(backend_present::IdentitySemanticData::SelectedSymbolId(selected)) =
                &record.identity.semantic_data
            else {
                panic!("capture must retain the original selected row identity");
            };
            // These particular structural coordinates reproduce their actual row
            // keys. Display ID bytes are never promoted into native authority.
            assert_eq!(key.as_bytes(), selected.bytes());
            let mut row = Row::new(RowId::Symbol(key), basis(), &record.identity.coordinate);
            row.kind = Some(match record.kind.as_deref().expect("captured kind") {
                "class" => DeclarationKind::Class,
                "function" => DeclarationKind::Function,
                "method" => DeclarationKind::Method,
                "import" => DeclarationKind::Import,
                "module" => DeclarationKind::Module,
                other => panic!("unhandled captured kind: {other}"),
            });
            assert_eq!(record.state.as_deref(), Some("ready"));
            row.signature = record.signature.clone();
            row.score = record.score;
            let projected = backend_present::Record::from_row(&row);
            if let Some(summary) = &record.summary {
                projected.with_summary(summary)
            } else {
                projected
            }
        })
        .collect::<Vec<_>>();
    // Basis is unused by this presentation-only replay. No owner, snapshot,
    // native query, or coverage proof is manufactured from captured JSON.
    let coverage = backend_present::CoverageLine::new(
        &[
            Coverage::Complete,
            Coverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Unconfigured,
            },
        ],
        Some(records.len() as u64),
    );
    let mut list =
        backend_present::RecordList::new(&page.query, coverage, records.into_boxed_slice())
            .with_more(page.more);
    if let Some(status) = page.semantic_search {
        list = list.with_semantic_search_status(status);
    }
    let answer = Answer::Records(Box::new(list));
    // Summary captures intentionally omit signature/project/key display
    // facets; compare the complete captured detail via the production codec.
    let detail = Detail::parse(captured["detail"].as_str().expect("captured detail"))
        .expect("known captured detail");
    let replay =
        encode_answer_with_call(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES, None)
            .expect("captured rows without continuation fit the typed payload");
    let replay: Value = serde_json::from_slice(&replay.bytes).expect("production projection");
    for field in [
        "query",
        "coverage",
        "readiness",
        "records",
        "more",
        "semantic_search",
    ] {
        assert_eq!(
            replay[field], captured[field],
            "captured {field} must remain exact"
        );
    }
    answer
}

#[test]
fn actual_searxng_capture_counts_the_complete_response_and_requires_one_copy_of_the_native_proof() {
    // These bytes are immutable outputs of actual e315 CLI/MCP processes, not
    // fabricated product rows. This test exercises projection admission only;
    // native reruns and source authority require the managed runtime cohort.
    let captured: Value = serde_json::from_str(include_str!(
        "../fixtures/searxng-e315-search-budget/cli-page-25.json"
    ))
    .expect("actual native first page");
    let request: Value = serde_json::from_str(include_str!(
        "../fixtures/searxng-e315-search-budget/request.json"
    ))
    .expect("actual persistent MCP request");
    let refusal: Value = serde_json::from_str(include_str!(
        "../fixtures/searxng-e315-search-budget/response.json"
    ))
    .expect("actual whole-envelope refusal");
    assert_eq!(request["id"], 269);
    assert_eq!(
        request["params"]["arguments"],
        json!({"query":"Search","limit":25})
    );
    assert_eq!(
        refusal["error"]["data"]["structuredContent"]["cause"],
        "oversized"
    );
    assert!(
        refusal["error"]["data"]["detail"]
            .as_str()
            .expect("actual observed tokens")
            .contains("13496")
    );
    assert_context_bounded(&refusal);
    let answer = captured_record_answer(&captured);
    assert_eq!(
        captured["records"].as_array().expect("actual rows").len(),
        25
    );
    assert_eq!(captured["more"], true);
    let original_rows = captured["records"].clone();
    let cursor = captured["nextCursor"]
        .as_str()
        .expect("actual pc4 native proof");
    assert_eq!(cursor.len(), 12_238);
    for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
        let arguments = request["params"]["arguments"]
            .as_object()
            .expect("actual operands");
        let invocation = McpInvocation::new(exposure, "backend.search", arguments);
        let next = invocation.continuation_call(cursor);
        let project = |duplicate: bool| {
            let payload = encode_answer_with_call(
                &answer,
                Detail::Standard,
                duplicate.then_some(cursor),
                DEFAULT_RESPONSE_BUDGET_BYTES,
                Some(&next),
            )
            .expect("byte-bounded actual projection");
            let structured: Value =
                serde_json::from_slice(&payload.bytes).expect("actual rows and native proof");
            assert_eq!(
                structured["records"], original_rows,
                "no captured coordinate or source field omitted"
            );
            complete_tool_result(
                "Actual immutable native search page; copy structuredContent.continuation_call.",
                structured,
                false,
            )
        };
        let budget = wire_admission::ResponseBudget::new(&request["id"]);
        let duplicate = budget.measure(project(true));
        assert!(
            !duplicate.fits(),
            "duplicated complete native proof cannot fit this capture"
        );
        let single = budget.measure(project(false));
        let frame = serde_json::to_string(single.value()).expect("complete encoded frame");
        assert_eq!(
            frame.matches(cursor).count(),
            1,
            "the exact portable native proof occurs once"
        );
        assert!(single.bytes() < duplicate.bytes());
        // Smaller does not mean admitted: this complete native 25-row
        // standard page can still require an actual smaller owner read.
        assert!(
            !single.fits(),
            "25-row capture still exceeds the whole-response budget"
        );
        let call = &single.value()["result"]["structuredContent"]["continuation_call"];
        let selected = if exposure == McpExposure::RegisteredCall {
            &call["arguments"]
        } else {
            call
        };
        assert_eq!(selected["arguments"]["cursor"], cursor);
        assert_eq!(selected["arguments"]["query"], "Search");
        assert_eq!(
            selected["arguments"]["limit"], 25,
            "transport projection never invents a different native page boundary"
        );
        // Lower bounds with this SAME 25-row proof are diagnostics only:
        // removing rows while retaining its after-25 boundary would skip
        // coordinates and is never an admissible owner page or resume call.
        // The next test proves admission with actual smaller native pages.
        for count in [0, 1, 3, 25] {
            let mut diagnostic = captured.clone();
            diagnostic["records"] = Value::Array(
                original_rows
                    .as_array()
                    .expect("captured rows")
                    .iter()
                    .take(count)
                    .cloned()
                    .collect(),
            );
            let answer = captured_record_answer(&diagnostic);
            let payload = encode_answer_with_call(
                &answer,
                Detail::Standard,
                None,
                DEFAULT_RESPONSE_BUDGET_BYTES,
                Some(&next),
            )
            .expect("diagnostic projection");
            let structured = serde_json::from_slice(&payload.bytes).expect("diagnostic payload");
            let measured = budget.measure(complete_tool_result(
                "Captured proof token floor; diagnostic only, not a native page.",
                structured,
                false,
            ));
            println!(
                "captured25-proof floor (diagnostic only): {exposure:?}, rows={count}, bytes={}, tokens={:?}, admitted={}",
                measured.bytes(),
                measured.tokens(),
                measured.fits()
            );
        }
    }
}

#[test]
fn actual_searxng_three_row_pages_are_admitted_with_their_exact_owned_resume_calls() {
    // Actual persistent e315 reads, including the resumed request. This proves
    // transport projection of these two pages; it does not claim completion
    // of the Search query or authenticate a captured cursor in a new owner.
    let captures = [
        (
            "backend.search",
            [
                (
                    include_str!(
                        "../fixtures/searxng-e315-search-budget/page3/search-real-first3.request.json"
                    ),
                    include_str!(
                        "../fixtures/searxng-e315-search-budget/page3/search-real-first3.response.json"
                    ),
                ),
                (
                    include_str!(
                        "../fixtures/searxng-e315-search-budget/page3/search-real-next3.request.json"
                    ),
                    include_str!(
                        "../fixtures/searxng-e315-search-budget/page3/search-real-next3.response.json"
                    ),
                ),
            ],
        ),
        (
            "backend.resolve",
            [
                (
                    include_str!(
                        "../fixtures/searxng-e315-search-budget/page3/resolve-real-first3.request.json"
                    ),
                    include_str!(
                        "../fixtures/searxng-e315-search-budget/page3/resolve-real-first3.response.json"
                    ),
                ),
                (
                    include_str!(
                        "../fixtures/searxng-e315-search-budget/page3/resolve-real-next3.request.json"
                    ),
                    include_str!(
                        "../fixtures/searxng-e315-search-budget/page3/resolve-real-next3.response.json"
                    ),
                ),
            ],
        ),
    ];
    for (tool, pages) in captures {
        for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
            let mut prior_cursor = None;
            let mut coordinates = std::collections::BTreeSet::new();
            for (request, response) in pages {
                let request: Value = serde_json::from_str(request).expect("actual request");
                let response: Value = serde_json::from_str(response).expect("actual response");
                assert_eq!(response["id"], request["id"]);
                assert_eq!(response["result"]["isError"], false);
                assert_context_bounded(&response);
                let arguments = request["params"]["arguments"]
                    .as_object()
                    .expect("actual native query operands");
                assert_eq!(request["params"]["name"], tool);
                assert_eq!(arguments["query"], "Search");
                assert_eq!(arguments["limit"], 3);
                if let Some(previous) = prior_cursor {
                    assert_eq!(
                        arguments["cursor"], previous,
                        "actual owner page boundary is unchanged"
                    );
                } else {
                    assert!(!arguments.contains_key("cursor"));
                }
                let captured = &response["result"]["structuredContent"];
                let rows = captured["records"]
                    .as_array()
                    .expect("actual complete native page");
                assert_eq!(rows.len(), 3);
                for row in rows {
                    assert!(
                        coordinates.insert(
                            row["identity"]["coordinate"]
                                .as_str()
                                .expect("exact selectable coordinate")
                                .to_owned()
                        )
                    );
                }
                let cursor = captured["nextCursor"]
                    .as_str()
                    .expect("actual owned continuation");
                assert!(
                    cursor.len() > 1024,
                    "exercise the single-copy production policy"
                );
                let invocation = McpInvocation::new(exposure, tool, arguments);
                let next = invocation.continuation_call(cursor);
                let answer = captured_record_answer(captured);
                let payload = encode_answer_with_call(
                    &answer,
                    Detail::Summary,
                    None,
                    DEFAULT_RESPONSE_BUDGET_BYTES,
                    Some(&next),
                )
                .expect("production typed encoder preserves the complete captured smaller page");
                let structured: Value =
                    serde_json::from_slice(&payload.bytes).expect("complete production projection");
                assert_eq!(structured["records"], captured["records"]);
                assert_eq!(structured["more"], true);
                assert!(
                    structured.get("nextCursor").is_none(),
                    "proof appears in executable call only"
                );
                let preview = markdown::summary_with_cursor(
                    &answer,
                    Some(cursor),
                    ContinuationTarget::McpInvocation(invocation),
                );
                assert!(
                    preview.ends_with(&format!("{}\n", invocation.continuation_note())),
                    "actual executable cursor must select the production continuation guidance"
                );
                assert!(!preview.contains("raise `limit`"));
                for row in rows {
                    let coordinate = row["identity"]["coordinate"]
                        .as_str()
                        .expect("complete captured coordinate");
                    assert!(preview.contains(&format!("`{coordinate}`\n")));
                }
                let budget = wire_admission::ResponseBudget::new(&request["id"]);
                let measured = budget.measure(complete_tool_result(&preview, structured, false));
                assert!(
                    measured.fits(),
                    "genuine three-row page must be admitted: {} bytes, {:?} tokens",
                    measured.bytes(),
                    measured.tokens()
                );
                assert_context_bounded(measured.value());
                println!(
                    "genuine captured page: {tool}, {exposure:?}, request={}, rows=3, bytes={}, tokens={:?}, admitted={}",
                    request["id"],
                    measured.bytes(),
                    measured.tokens(),
                    measured.fits()
                );
                let frame = serde_json::to_string(measured.value()).expect("actual whole envelope");
                assert_eq!(
                    frame.matches(cursor).count(),
                    1,
                    "original owned proof retained once"
                );
                let call = &measured.value()["result"]["structuredContent"]["continuation_call"];
                let selected = if exposure == McpExposure::RegisteredCall {
                    &call["arguments"]
                } else {
                    call
                };
                assert_eq!(selected["name"], tool);
                assert_eq!(selected["arguments"]["query"], "Search");
                assert_eq!(
                    selected["arguments"]["limit"], 3,
                    "resume size remains native-issued"
                );
                assert_eq!(selected["arguments"]["cursor"], cursor);
                prior_cursor = Some(cursor.to_owned());
            }
            assert_eq!(
                coordinates.len(),
                6,
                "complete union of the two observed pages only"
            );
        }
    }
}

fn wide_catalog(count: usize) -> backend_library::Library {
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
        .with_document([Fragment::Text("λאב🙂[]{}\\\"!".repeat(450))])
    }));
    let view = root(rows);
    let cursor = backend_library::Cursor::for_view_root(&view);
    backend_library::Library::from_view(view, cursor)
        .expect("native catalog projection, not compiler coverage")
}

#[test]
fn whole_response_budget_keeps_every_native_catalog_coordinate_through_copyable_continuations() {
    for tool in ["backend.search", "backend.resolve", "backend.graph"] {
        for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
            let count = 12;
            let mut server = ready(Fake {
                page_catalog: Some(wide_catalog(count)),
                ..Fake::default()
            });
            let arguments = if tool == "backend.graph" {
                json!({"coordinate":MODULE,"limit":12,"detail":"full"})
            } else {
                json!({"query":"Session","limit":12,"detail":"full"})
            };
            let mut next = exposure.expose_call(json!({"name":tool,"arguments":arguments}));
            let mut seen = std::collections::BTreeSet::new();
            let mut pages = 0;
            let mut previous_limit = None;
            loop {
                let response = encoded_stdio_reply(server.handle_value(json!({"jsonrpc":"2.0","id":"אב🙂[]\\\"".repeat(100),"method":"tools/call","params":next}), None).expect("actual ID"));
                assert_context_bounded(&response);
                assert_eq!(response["result"]["isError"], false, "{response}");
                let structured = &response["result"]["structuredContent"];
                let rows = structured["records"].as_array().expect("whole native page");
                assert!(!rows.is_empty(), "native page makes progress");
                for row in rows {
                    let coordinate = row["identity"]["coordinate"]
                        .as_str()
                        .expect("selectable exact coordinate");
                    assert!(
                        seen.insert(coordinate.to_owned()),
                        "coordinate cannot repeat: {coordinate}"
                    );
                }
                pages += 1;
                if !structured["more"].as_bool().expect("honest continuation") {
                    break;
                }
                next = structured["continuation_call"].clone();
                let call = if exposure == McpExposure::RegisteredCall {
                    &next["arguments"]
                } else {
                    &next
                };
                let effective = call["arguments"]["limit"]
                    .as_u64()
                    .expect("effective native bound");
                assert!(
                    effective < 12,
                    "initial page must actually be resized by the full response budget"
                );
                if let Some(previous) = previous_limit {
                    assert_eq!(effective, previous, "resumed limits stay fixed");
                }
                previous_limit = Some(effective);
                assert_eq!(call["arguments"]["detail"], "full");
                assert_eq!(call["arguments"]["cursor"], structured["nextCursor"]);
                assert!(pages <= count);
            }
            assert!(pages > 1);
            assert_eq!(seen.len(), count);
            for index in 0..count {
                assert!(seen.contains(&format!("{MODULE}:{}::Session_{index:03}", index + 2)));
            }
        }
    }
}

#[test]
fn query_preview_omits_only_complete_rows_and_reports_the_actual_omission() {
    let rows = [31_000usize, 11]
        .into_iter()
        .map(|size| {
            backend_library::GraphQueryRow::new(std::collections::BTreeMap::from([(
                "coordinate".to_owned(),
                GraphValue::String(format!("pkg::{}", "λ".repeat(size / 2))),
            )]))
            .expect("admitted field extent")
        })
        .collect::<Vec<_>>();
    let page = GraphQueryPage {
        revision: view_state_root(&[]).into(),
        source: basis().object,
        rows: rows.into_boxed_slice(),
        terminal: PageTerminal::Complete,
    };
    let text = graph_page_text(&page, None);
    assert!(text.contains("2 row(s), 0 shown, 2 omitted from text"));
    assert!(text.len() <= backend_present::MAX_PREVIEW_TEXT_BYTES);
    assert!(
        !text.contains("pkg::"),
        "no partial coordinate from the first oversized row"
    );
    assert_eq!(
        page.rows.len(),
        2,
        "readable omission does not change structured rows"
    );
}

fn dense_query_rows() -> Box<[GraphQueryRow]> {
    (0..12)
        .map(|index| {
            GraphQueryRow::new(std::collections::BTreeMap::from([
                (
                    "coordinate".to_owned(),
                    GraphValue::String(format!("pkg::Session_{index:03}")),
                ),
                (
                    "source".to_owned(),
                    GraphValue::String("λאב🙂[]{}\\\"!".repeat(450)),
                ),
            ]))
            .expect("complete synthetic query row")
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

#[test]
fn typed_query_pages_keep_all_fields_and_effective_limits_under_an_actual_large_id() {
    for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
        let all = dense_query_rows();
        let mut server = ready(Fake {
            graph_paged_rows: Some(all.clone()),
            ..Fake::default()
        });
        let query = "{ Declaration { coordinate @output signature @output(name: \"source\") } }";
        let mut next = exposure.expose_call(
            json!({"name":"backend.query","arguments":{"query":query,"limit":12,"detail":"full"}}),
        );
        let mut seen = Vec::new();
        let mut bound = None;
        let mut pages = 0;
        loop {
            let response = encoded_stdio_reply(server.handle_value(json!({"jsonrpc":"2.0","id":"אב🙂\\\"".repeat(100),"method":"tools/call","params":next}), None).expect("actual ID"));
            assert_context_bounded(&response);
            assert_eq!(response["result"]["isError"], false, "{response}");
            let structured = &response["result"]["structuredContent"];
            seen.extend(
                structured["rows"]
                    .as_array()
                    .expect("every native field")
                    .iter()
                    .cloned(),
            );
            pages += 1;
            if structured["terminal"] == "complete" {
                break;
            }
            next = structured["continuation_call"].clone();
            let call = if exposure == McpExposure::RegisteredCall {
                &next["arguments"]
            } else {
                &next
            };
            assert_eq!(call["arguments"]["query"], query);
            let effective = call["arguments"]["limit"].as_u64().expect("selected limit");
            assert!(effective < 12);
            if let Some(bound) = bound {
                assert_eq!(effective, bound);
            }
            bound = Some(effective);
            assert!(pages <= 12, "progress must be monotonic");
        }
        assert!(pages > 1);
        assert_eq!(seen.len(), all.len());
        for (observed, expected) in seen.iter().zip(all.iter()) {
            for (name, value) in expected.fields() {
                assert_eq!(
                    observed[name],
                    graph_value(value),
                    "complete projected field union"
                );
            }
        }
    }
}

#[test]
fn budget_rereads_cannot_cross_query_scope_or_reset_a_cancelled_lifetime() {
    let mut server = ready(Fake {
        graph_paged_rows: Some(dense_query_rows()),
        graph_query_source_changes: true,
        ..Fake::default()
    });
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.query","arguments":{"query":"{ Declaration { coordinate @output } }","limit":12}}),
    );
    assert!(
        response.get("error").is_some() || response["result"]["isError"] == true,
        "{response}"
    );
    assert_eq!(server.product.graph_query_calls, 2);
    for tool in [
        "backend.query",
        "backend.search",
        "backend.resolve",
        "backend.graph",
    ] {
        let lifetime = RequestContext::new();
        lifetime.cancel(RequestCancellationReason::Explicit);
        let mut server = ready(Fake {
            page_catalog: Some(wide_catalog(12)),
            graph_paged_rows: Some(dense_query_rows()),
            ..Fake::default()
        });
        let args = if tool == "backend.graph" {
            json!({"coordinate":MODULE})
        } else {
            json!({"query":"{ Declaration { coordinate @output } }"})
        };
        let response = encoded_stdio_reply(server.handle_value(json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":tool,"arguments":args}}), Some(lifetime)).expect("cancelled response"));
        assert!(
            response.get("error").is_some() || response["result"]["isError"] == true,
            "{response}"
        );
        assert_eq!(server.product.graph_query_calls, 0);
        assert!(server.product.page_limits.is_empty());
    }
}

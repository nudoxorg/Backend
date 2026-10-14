//! Complete native graph output admission, using actual JSON-RPC finalization.
use super::*;
use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyScope, PackageDependencyRecord,
    PackageDependencyTarget, PackageGraphCursor, PackageGraphDirection, PackageGraphKnowledge,
    PackageGraphPage, PackageGraphPageRequest, PackageGraphPageTerminal,
    PackageGraphSourceAuthority, PackageGraphSourceKey, RegistryEcosystem,
};

fn rows() -> Vec<PackageDependencyRecord> {
    let source = PackageReference::parse("pkg:cargo/graph_budget@1.0.0").expect("source");
    let mut rows = (0..64)
        .map(|n| {
            PackageDependencyRecord::new_with_source_authority(
                source.clone(),
                PackageGraphSourceAuthority::Local([7; 32]),
                PackageDependencyTarget::new(
                    RegistryEcosystem::Cargo,
                    &format!("dependency_{n:03}"),
                    &format!(">=1; {}", "依存関係🦀".repeat(120)),
                    None,
                )
                .expect("realistic long Unicode requirement"),
                DependencyScope::Runtime,
                false,
                DependencyEvidence {
                    authority: DependencyAuthority::LocalManifest,
                    frontier: [7; 32],
                    provenance: [8; 32],
                },
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row.facts_version);
    rows
}

// This fixture exercises presentation and exact wire budgeting, not owner
// authentication. Native hostile/frontier/cold-key proof lives in library and
// the actual production adapter tests.
pub(super) fn page_for(
    request: &PackageGraphPageRequest,
    rows: &[PackageDependencyRecord],
) -> PackageGraphPage {
    let source = PackageGraphSourceKey::new(rows[0].source.clone(), rows[0].source_authority);
    let start = request.cursor.as_ref().map_or(0, |cursor| {
        rows.partition_point(|row| row.facts_version <= cursor.after_edge_id)
    });
    let end = (start + usize::from(request.limit)).min(rows.len());
    let selected = rows[start..end].to_vec().into_boxed_slice();
    let terminal = if end < rows.len() {
        PackageGraphPageTerminal::More(PackageGraphCursor {
            schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [9; 32],
            facts_witness: [10; 32],
            recipe: request.recipe(Some(&source)),
            catalog_snapshot: request.catalog_snapshot,
            source: Some(source.clone()),
            limit: request.limit,
            after_edge_id: selected.last().expect("positive page").facts_version,
            seal: [4; 32],
        })
    } else {
        PackageGraphPageTerminal::Complete
    };
    let page = PackageGraphPage {
        schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
        view_root: [9; 32],
        facts_witness: [10; 32],
        catalog_snapshot: request.catalog_snapshot,
        package: request.package.clone(),
        direction: request.direction,
        source: Some(source),
        limit: request.limit,
        knowledge: PackageGraphKnowledge::Known,
        rows: selected,
        terminal,
    };
    page.admit_for(request).expect("complete native fixture");
    page
}

#[test]
fn default_graph_page_shrinks_before_admission_and_exact_copy_calls_cover_all_edges() {
    let original = rows();
    let expected = original
        .iter()
        .map(|row| row.facts_version)
        .collect::<Vec<_>>();
    for compact in [false, true] {
        for detail in ["summary", "standard", "full"] {
            let mut server = ready(Fake {
                native_graph_rows: Some(original.clone()),
                ..Fake::default()
            });
            let arguments = json!({"package":original[0].source.as_str(),"direction":"dependencies","detail":detail});
            let mut next = if compact {
                json!({"name":super::super::tools::CALL_TOOL,"arguments":{"name":"backend.package_graph","arguments":arguments}})
            } else {
                json!({"name":"backend.package_graph","arguments":arguments})
            };
            let mut found = Vec::new();
            let mut effective = None;
            loop {
                let reply = request(&mut server, "tools/call", &next);
                assert!(reply.get("error").is_none(), "{reply}");
                assert_eq!(reply["result"]["isError"], false, "{reply}");
                assert!(wire_fits(&reply, ResponseTransport::StdioLine));
                assert!(wire_tokens(&reply).expect("named tokenizer") <= MCP_CL100K_TOKEN_LIMIT);
                let page: PackageGraphPage = serde_json::from_value(
                    reply["result"]["structuredContent"]["package_graph_page"].clone(),
                )
                .expect("complete exact owner page");
                assert!(
                    page.limit < 64,
                    "dense first page must adapt before emitting"
                );
                if let Some(limit) = effective {
                    assert_eq!(page.limit, limit, "resume never changes issued limit");
                }
                effective = Some(page.limit);
                for row in &page.rows {
                    assert_eq!(
                        original
                            .iter()
                            .find(|edge| edge.facts_version == row.facts_version),
                        Some(row),
                        "no canonical requirement/evidence is omitted"
                    );
                }
                found.extend(page.rows.iter().map(|row| row.facts_version));
                match page.terminal {
                    PackageGraphPageTerminal::More(cursor) => {
                        next = reply["result"]["structuredContent"]["continuation_call"].clone();
                        let args = if compact {
                            &next["arguments"]["arguments"]
                        } else {
                            &next["arguments"]
                        };
                        assert_eq!(args["limit"], page.limit);
                        assert_eq!(args["cursor"], json!(cursor));
                        assert_eq!(args["detail"], detail);
                        assert!(
                            text_of(&reply["result"])
                                .contains("structuredContent.continuation_call")
                        );
                    }
                    PackageGraphPageTerminal::Complete => break,
                    PackageGraphPageTerminal::Cancelled => panic!("not cancelled"),
                }
            }
            assert_eq!(
                found, expected,
                "complete union has every original edge once"
            );
            let issued = server
                .product
                .surface_commands
                .iter()
                .filter_map(|command| match command {
                    SurfaceCommand::PackageGraphPage { request } => Some(request),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(issued[0].limit, 64, "native default is unchanged");
            for request in issued
                .into_iter()
                .filter(|request| request.cursor.is_some())
            {
                assert_eq!(Some(request.limit), effective);
            }
        }
    }
}

#[test]
fn cancellation_after_native_graph_reply_precedes_success_and_oversized_fallback() {
    for raw in [false, true] {
        for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
            for limit in [1, 64] {
                let original = rows();
                let mut server = ready(Fake {
                    native_graph_rows: Some(original.clone()),
                    graph_cancel_after_native_reply: Some(RequestCancellationReason::Explicit),
                    ..Fake::default()
                });
                let params = if raw {
                    exposure.expose_call(json!({"name":"backend.surface","arguments":{"command":SurfaceCommand::PackageGraphPage {
                        request:PackageGraphPageRequest::new(original[0].source.clone(), PackageGraphDirection::Dependencies, None, limit).expect("native request")
                    },"detail":"full"}}))
                } else {
                    exposure.expose_call(json!({"name":"backend.package_graph","arguments":{"package":original[0].source.as_str(),"direction":"dependencies","limit":limit,"detail":"full"}}))
                };
                let context = RequestContext::new();
                let _deadline = context
                    .read_deadline(std::time::Duration::from_secs(30))
                    .expect("original absolute deadline");
                let response = encoded_stdio_reply(
                    server
                        .handle_value(
                            json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":params}),
                            Some(context.clone()),
                        )
                        .expect("original cancellation response"),
                );
                if raw && exposure == McpExposure::RegisteredCall {
                    // The compact route never admits the private raw surface.
                    // This refusal cannot be mistaken for native cancellation.
                    assert_eq!(context.cancellation_reason(), None);
                    assert!(response.get("error").is_none(), "{response}");
                    assert_eq!(response["result"]["isError"], true, "{response}");
                    let fault = &response["result"]["structuredContent"];
                    assert_eq!(fault["slug"], "usage");
                    assert_eq!(fault["operand"], "name");
                    assert_eq!(fault["cause"], "malformed");
                    assert!(fault.get("package_graph_page").is_none());
                    assert!(fault.get("continuation_call").is_none());
                    assert!(fault.get("shell").is_none());
                    assert!(server.product.surface_commands.is_empty());
                    assert_context_bounded(&response);
                    continue;
                }
                assert_eq!(
                    context.cancellation_reason(),
                    Some(RequestCancellationReason::Explicit)
                );
                assert_eq!(response["id"], 9);
                assert!(response.get("result").is_none(), "{response}");
                assert_eq!(response["error"]["code"], -32800, "{response}");
                assert_eq!(response["error"]["message"], "Request cancelled");
                let stopped = &response["error"]["data"];
                assert_eq!(
                    stopped["reason"], "request explicitly cancelled by client",
                    "original stop wins over successful or oversized reply: {response}"
                );
                // The transport finalizes cancellation instead of publishing
                // either a graph page or an oversized fallback affordance.
                assert!(stopped.get("structuredContent").is_none());
                assert!(stopped.get("package_graph_page").is_none());
                assert!(stopped.get("continuation_call").is_none());
                assert!(stopped.get("shell").is_none());
                assert_eq!(
                    server.product.surface_commands.len(),
                    1,
                    "a stopped caller cannot trigger a smaller re-read"
                );
                assert_eq!(
                    server
                        .product
                        .request_contexts
                        .last()
                        .expect("same scope")
                        .remaining_read_budget()
                        .expect_err("original stop remains"),
                    ClientError::RequestCancelled(RequestCancellationReason::Explicit)
                );

                assert_context_bounded(&response);
            }
        }
    }
}

#[test]
fn effective_graph_limit_and_original_authority_remain_copyable_in_all_cli_views() {
    let rows = rows();
    for authority in [None, Some(rows[0].source_authority)] {
        let request = PackageGraphPageRequest::new(
            rows[0].source.clone(),
            PackageGraphDirection::Dependencies,
            authority,
            1,
        )
        .expect("reader request");
        let page = page_for(&request, &rows);
        let command = SurfaceCommand::PackageGraphPage {
            request: request.clone(),
        };
        let view = backend_present::product_view_for_owned_command(
            &command,
            SurfaceReply::PackageGraphPage(page.clone()),
        );
        for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
            let shell = view
                .package_graph_cli_continuation(detail)
                .expect("actual copyable command");
            assert!(shell.contains("--limit 1"));
            assert_eq!(shell.contains("--authority"), authority.is_some());
            for text in [
                markdown::answer_with_detail(
                    &Answer::Product(Box::new(view.clone())),
                    detail,
                    None,
                    ContinuationTarget::CliOption,
                ),
                backend_present::text::answer_with_detail(
                    &Answer::Product(Box::new(view.clone())),
                    backend_present::Theme::plain(),
                    detail,
                    None,
                    ContinuationTarget::CliOption,
                ),
            ] {
                assert!(text.contains(&shell));
                assert!(
                    text.contains(rows[0].target.requirement.as_str()),
                    "readable requirement remains exact"
                );
                assert!(text.contains("source authority:"));
                assert!(text.contains("evidence:"));
                assert!(text.contains("frontier:"));
                assert!(text.contains("provenance:"));
            }
            let answer = Answer::Product(Box::new(view.clone()));
            let bytes = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
                .expect("borrowed native encoder");
            assert_eq!(
                backend_present::encode_package_graph_json(
                    &page,
                    detail,
                    DEFAULT_RESPONSE_BUDGET_BYTES
                )
                .expect("exact borrowed CLI alternative")
                .bytes,
                bytes.bytes,
                "the fitting proof must use the actual CLI projection",
            );
            let value: Value = serde_json::from_slice(&bytes.bytes).expect("exact JSON");
            assert_eq!(value["package_graph_page"], json!(page));
            assert!(
                value["records"]
                    .as_array()
                    .expect("readable identities")
                    .iter()
                    .all(|record| !record["tags"].to_string().contains("依存")),
                "machine identity rows do not duplicate canonical requirement"
            );
        }
    }
}

#[test]
fn actual_caller_id_is_admitted_before_selection_and_resumes_refuse_atomically() {
    let rows = rows();
    let args =
        json!({"package":rows[0].source.as_str(),"direction":"dependencies","detail":"full"});
    let mut baseline = ready(Fake {
        native_graph_rows: Some(rows.clone()),
        ..Fake::default()
    });
    let first = request(
        &mut baseline,
        "tools/call",
        &json!({"name":"backend.package_graph","arguments":args}),
    );
    assert_eq!(first["result"]["isError"], false);
    let baseline_limit = first["result"]["structuredContent"]["package_graph_page"]["limit"]
        .as_u64()
        .expect("effective limit");
    // Native 3081 refused this original 30KB ID even at the minimum page:
    // 13,054 ordinary cl100k tokens. Preserve that pressure as an explicit
    // atomic-refusal case, not a false promise that every ID leaves room.
    let original_large_id = Value::String("caller".repeat(5_000));
    let mut minimum_server = ready(Fake {
        native_graph_rows: Some(rows.clone()),
        ..Fake::default()
    });
    let mut minimum_args = args.clone();
    minimum_args["limit"] = json!(1);
    let minimum = request(
        &mut minimum_server,
        "tools/call",
        &json!({"name":"backend.package_graph","arguments":minimum_args}),
    );
    assert_eq!(minimum["result"]["isError"], false, "{minimum}");
    assert_eq!(
        minimum["result"]["structuredContent"]["package_graph_page"]["limit"],
        1
    );
    assert_eq!(
        minimum["result"]["structuredContent"]["package_graph_page"]["rows"]
            .as_array()
            .expect("one whole native edge")
            .len(),
        1
    );
    let minimum_pressure =
        wire_admission::ResponseBudget::new(&original_large_id).prepare(minimum["result"].clone());
    assert!(
        !minimum_pressure.fits(),
        "original ID cannot admit even a whole limit-one page"
    );
    assert!(
        minimum_pressure
            .tokens()
            .expect("byte-bounded irreducible page")
            > MCP_CL100K_TOKEN_LIMIT
    );
    let mut original_pressure = ready(Fake {
        native_graph_rows: Some(rows.clone()),
        ..Fake::default()
    });
    let refused = original_pressure
        .handle_value(
            json!({"jsonrpc":"2.0","id":original_large_id,
        "method":"tools/call","params":{"name":"backend.package_graph","arguments":args}}),
            None,
        )
        .map(encoded_stdio_reply)
        .expect("original correlated minimum refusal");
    assert_eq!(refused["id"], original_large_id);
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(refused["result"]["structuredContent"]["cause"], "oversized");
    for structured in [
        &refused["result"]["structuredContent"],
        &refused["error"]["data"]["structuredContent"],
    ] {
        for field in [
            "package_graph_page",
            "continuation_call",
            "nextCursor",
            "shell",
        ] {
            assert!(
                structured.get(field).is_none(),
                "atomic initial refusal cannot publish {field}"
            );
        }
    }
    assert!(matches!(original_pressure.product.surface_commands.last(),
        Some(SurfaceCommand::PackageGraphPage { request }) if request.limit == 1 && request.cursor.is_none()));
    assert!(wire_fits(&refused, ResponseTransport::StdioLine));

    // Obtain the actual fixed resumed page before choosing a bounded pressure
    // ID. Measurement uses production encoding, exact metadata and cl100k;
    // no estimated byte/token ratio establishes the test's fit geometry.
    let mut resumed_probe = ready(Fake {
        native_graph_rows: Some(rows.clone()),
        ..Fake::default()
    });
    let resumed_page = request(
        &mut resumed_probe,
        "tools/call",
        &first["result"]["structuredContent"]["continuation_call"],
    );
    assert_eq!(resumed_page["result"]["isError"], false, "{resumed_page}");
    assert_eq!(
        resumed_page["result"]["structuredContent"]["package_graph_page"]["limit"], baseline_limit,
        "the calibration uses the unchanged issued resumed frontier"
    );
    assert_eq!(resumed_probe.product.surface_commands.len(), 1);
    let mut selected_id = None;
    for copies in (128..=5_000).step_by(128) {
        let id = Value::String("caller".repeat(copies));
        let budget = wire_admission::ResponseBudget::new(&id);
        let initial = budget.prepare(first["result"].clone());
        let fixed = budget.prepare(resumed_page["result"].clone());
        let single = budget.prepare(minimum["result"].clone());
        if !initial.fits() && !fixed.fits() && single.fits() {
            println!(
                "graph ID geometry: id_bytes={}, baseline_limit={}, initial={}B/{:?}, fixed={}B/{:?}, minimum={}B/{:?}",
                serde_json::to_vec(&id).expect("ID bytes").len(),
                baseline_limit,
                initial.bytes(),
                initial.tokens(),
                fixed.bytes(),
                fixed.tokens(),
                single.bytes(),
                single.tokens()
            );
            selected_id = Some(id);
            break;
        }
    }
    let large_id = selected_id.expect("bounded actual ID admits a complete minimum page but refuses baseline and fixed resumed pages");
    let mut large = ready(Fake {
        native_graph_rows: Some(rows.clone()),
        ..Fake::default()
    });
    let message = json!({"jsonrpc":"2.0","id":large_id,"method":"tools/call","params":{"name":"backend.package_graph","arguments":args}});
    let reply = large
        .handle(&serde_json::to_vec(&message).expect("actual request"))
        .map(encoded_stdio_reply)
        .expect("actual correlated response");
    assert_eq!(reply["id"], message["id"]);
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    assert!(wire_fits(&reply, ResponseTransport::StdioLine));
    assert!(
        reply["result"]["structuredContent"]["package_graph_page"]["limit"]
            .as_u64()
            .expect("actual-ID page")
            < baseline_limit,
        "a provisional Null-ID check cannot choose the same page for this measured actual ID"
    );

    let mut resumed = ready(Fake {
        native_graph_rows: Some(rows),
        ..Fake::default()
    });
    let message = json!({"jsonrpc":"2.0","id":large_id,"method":"tools/call","params":first["result"]["structuredContent"]["continuation_call"]});
    let reply = resumed
        .handle(&serde_json::to_vec(&message).expect("exact resumed request"))
        .map(encoded_stdio_reply)
        .expect("atomic refusal");
    assert!(
        reply.get("error").is_some() || reply["result"]["isError"] == true,
        "{reply}"
    );
    assert_eq!(
        reply["id"], message["id"],
        "refusal retains the exact caller ID"
    );
    for structured in [
        &reply["result"]["structuredContent"],
        &reply["error"]["data"]["structuredContent"],
    ] {
        for field in [
            "package_graph_page",
            "continuation_call",
            "nextCursor",
            "shell",
        ] {
            assert!(
                structured.get(field).is_none(),
                "atomic resumed refusal cannot publish {field}"
            );
        }
    }
    let expected_cursor: PackageGraphCursor =
        serde_json::from_value(message["params"]["arguments"]["cursor"].clone())
            .expect("exact issued native graph cursor");
    let [SurfaceCommand::PackageGraphPage { request: received }] =
        resumed.product.surface_commands.as_slice()
    else {
        panic!("an issued frontier must make exactly one native graph request");
    };
    assert_eq!(
        u64::from(received.limit),
        baseline_limit,
        "issued native limit cannot resize"
    );
    assert_eq!(
        received.cursor.as_ref(),
        Some(&expected_cursor),
        "all issued cursor fields remain unchanged"
    );
    assert!(wire_fits(&reply, ResponseTransport::StdioLine));
}

#[test]
fn graph_candidate_reads_preserve_the_original_absolute_request_deadline() {
    let rows = rows();
    let mut server = ready(Fake {
        native_graph_rows: Some(rows.clone()),
        ..Fake::default()
    });
    let scope = RequestContext::new();
    let original = scope
        .read_deadline(std::time::Duration::from_secs(60))
        .expect("original caller deadline");
    let message = json!({"jsonrpc":"2.0","id":19,"method":"tools/call","params":{"name":"backend.package_graph","arguments":{"package":rows[0].source.as_str(),"direction":"dependencies","detail":"full"}}});
    let reply = server
        .handle_value(message, Some(scope.clone()))
        .map(encoded_stdio_reply)
        .expect("bounded result");
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    assert!(
        server.product.surface_commands.len() > 1,
        "fixture must choose a smaller complete candidate"
    );
    for received in &server.product.request_contexts {
        assert_eq!(
            received
                .read_deadline(std::time::Duration::from_secs(600))
                .expect("same original scope"),
            original
        );
    }
    assert_eq!(
        scope
            .read_deadline(std::time::Duration::from_secs(600))
            .expect("not renewed"),
        original
    );
}

#[test]
fn raw_surface_graph_uses_same_selector_and_preserves_exact_nested_continuation() {
    let original = rows();
    let expected = original
        .iter()
        .map(|row| row.facts_version)
        .collect::<Vec<_>>();
    for detail in ["summary", "standard", "full"] {
        let mut server = ready(Fake {
            native_graph_rows: Some(original.clone()),
            ..Fake::default()
        });
        let first_request = PackageGraphPageRequest::new(
            original[0].source.clone(),
            PackageGraphDirection::Dependencies,
            Some(original[0].source_authority),
            64,
        )
        .expect("raw registered operator request");
        let mut next = json!({"name":SURFACE_TOOL,"arguments":{"command":SurfaceCommand::PackageGraphPage { request:first_request },"detail":detail}});
        let mut found = Vec::new();
        let mut effective = None;
        loop {
            let reply = request(&mut server, "tools/call", &next);
            assert!(reply.get("error").is_none(), "{reply}");
            assert_eq!(reply["result"]["isError"], false, "{reply}");
            assert!(wire_fits(&reply, ResponseTransport::StdioLine));
            let raw: SurfaceReply =
                serde_json::from_value(reply["result"]["structuredContent"]["surface"].clone())
                    .expect("original complete raw reply contract");
            let SurfaceReply::PackageGraphPage(page) = raw else {
                panic!("native page")
            };
            assert!(page.limit < 64);
            if let Some(limit) = effective {
                assert_eq!(page.limit, limit);
            }
            effective = Some(page.limit);
            assert!(text_of(&reply["result"]).contains(original[0].target.requirement.as_str()));
            found.extend(page.rows.iter().map(|row| row.facts_version));
            match page.terminal {
                PackageGraphPageTerminal::More(cursor) => {
                    next = reply["result"]["structuredContent"]["continuation_call"].clone();
                    assert_eq!(
                        next["name"], SURFACE_TOOL,
                        "never changes raw operator intent into a catalog target"
                    );
                    assert_eq!(next["arguments"]["detail"], detail);
                    let command: SurfaceCommand =
                        serde_json::from_value(next["arguments"]["command"].clone())
                            .expect("closed typed native command");
                    let SurfaceCommand::PackageGraphPage { request } = command else {
                        panic!("same command")
                    };
                    assert_eq!(request.cursor.as_ref(), Some(&cursor));
                    assert_eq!(request.catalog_snapshot, cursor.catalog_snapshot);
                    assert_eq!(request.limit, page.limit);
                    assert_eq!(request.authority, Some(original[0].source_authority));
                    request.admit().expect("exact next native grammar");
                }
                PackageGraphPageTerminal::Complete => break,
                PackageGraphPageTerminal::Cancelled => panic!("not cancelled"),
            }
        }
        assert_eq!(
            found, expected,
            "raw route retains every canonical edge once"
        );
    }
}

fn local_context() -> backend_present::StartupRecoveryContext {
    backend_present::StartupRecoveryContext::new(
        "/srv/project ' literal\nnext".to_owned(),
        "/srv/owner.sock".to_owned(),
        "/observed/bundle/backend-cli".to_owned(),
        backend_present::StartupRecoveryCommand::Status,
        backend_present::StartupRecoveryMode::Active,
        backend_present::StartupRecoveryWorkspace::Explicit("/srv/workspace".to_owned()),
    )
}

fn wide_single_edge() -> Vec<PackageDependencyRecord> {
    wide_single_edge_with_spelling("s", "t")
}

fn wide_single_edge_with_spelling(
    source_spelling: &str,
    target_spelling: &str,
) -> Vec<PackageDependencyRecord> {
    let source = PackageReference::parse(&format!("pkg:cargo/{}@1", source_spelling.repeat(2000)))
        .expect("bounded source coordinate");
    // Keep both the resolved PURL and its exact name near the URL bound. The
    // 3000-byte spelling used to exceed that parser bound before reaching the
    // response-budget control this fixture is meant to exercise.
    let target = target_spelling
        .repeat(backend_library::interface::MAX_PACKAGE_URL_BYTES - "pkg:cargo/@1".len());
    vec![PackageDependencyRecord::new_with_source_authority(
        source,
        PackageGraphSourceAuthority::Local([7; 32]),
        PackageDependencyTarget::new(
            RegistryEcosystem::Cargo,
            target.clone(),
            "r".repeat(backend_library::MAX_PRODUCT_TEXT_BYTES),
            Some(
                PackageReference::parse(&format!("pkg:cargo/{target}@1"))
                    .expect("bounded exact resolved target"),
            ),
        )
        .expect("bounded complete requirement"),
        DependencyScope::Runtime,
        false,
        DependencyEvidence {
            authority: DependencyAuthority::LocalManifest,
            frontier: [7; 32],
            provenance: [8; 32],
        },
    )]
}

#[test]
fn minimum_graph_refusal_offers_only_proven_local_cli_json_and_keeps_the_input() {
    // Keep the original byte and token pressure as a strict negative. A
    // readable refusal does not prove that the separate CLI JSON fits.
    // Positive spellings retain the exact source/target/requirement lengths;
    // their named token counts, rather than a relaxed cap, admit the fallback.
    let negative = wide_single_edge();
    let positive = [("a", "t"), ("s", "a"), ("a", "a")]
        .into_iter()
        .map(|(source, target)| wide_single_edge_with_spelling(source, target))
        .find(|rows| {
            let native = PackageGraphPageRequest::new(
                rows[0].source.clone(),
                PackageGraphDirection::Dependencies,
                Some(rows[0].source_authority),
                1,
            )
            .expect("same bounded native input");
            let page = page_for(&native, rows);
            [Detail::Summary, Detail::Standard, Detail::Full]
                .into_iter()
                .all(|detail| {
                    let args = json!({"limit":1});
                    let invocation = McpInvocation::new(
                        McpExposure::Direct,
                        "backend.package_graph",
                        args.as_object().expect("fixture arguments"),
                    );
                    backend_present::package_graph_json_alternative(
                        &page,
                        &native,
                        detail,
                        Some(local_context()),
                    )
                    .is_some()
                        && backend_present::package_graph_markdown(&page, detail, invocation).len()
                            > backend_present::MAX_PREVIEW_TEXT_BYTES
                })
        })
        .expect("complete CLI JSON fits while the unchanged readable byte pressure refuses");
    assert_eq!(
        positive[0].source.as_str().len(),
        negative[0].source.as_str().len()
    );
    assert_eq!(
        positive[0].target.name.as_str().len(),
        negative[0].target.name.as_str().len()
    );
    assert_eq!(
        positive[0].target.requirement,
        negative[0].target.requirement
    );
    assert_eq!(
        positive[0]
            .target
            .resolved
            .as_ref()
            .expect("resolved")
            .as_str()
            .len(),
        negative[0]
            .target
            .resolved
            .as_ref()
            .expect("resolved")
            .as_str()
            .len()
    );
    for (rows, cli_fits) in [(negative, false), (positive, true)] {
        for detail in ["summary", "standard", "full"] {
            for exposure in [0, 1, 2] {
                for attached in [false, true] {
                    let mut server = ready(Fake {
                        native_graph_rows: Some(rows.clone()),
                        cli_context: attached.then(local_context),
                        ..Fake::default()
                    });
                    let args = json!({"package": rows[0].source.as_str(),"direction":"dependencies","authority":rows[0].source_authority.selector(),"limit":1,"detail":detail});
                    let native = PackageGraphPageRequest::new(
                        rows[0].source.clone(),
                        PackageGraphDirection::Dependencies,
                        Some(rows[0].source_authority),
                        1,
                    )
                    .expect("exact input");
                    let expected_page = page_for(&native, &rows);
                    let payload = backend_present::encode_package_graph_json(
                        &expected_page,
                        Detail::parse(detail).expect("detail"),
                        DEFAULT_RESPONSE_BUDGET_BYTES,
                    )
                    .expect("complete compact CLI JSON byte projection");
                    let mut cli_wire = payload.bytes.into_vec();
                    cli_wire.push(b'\n');
                    let cli_tokens =
                        cl100k_tokens(std::str::from_utf8(&cli_wire).expect("JSON UTF-8"))
                            .expect("both complete CLI projections have valid named-token counts");
                    println!(
                        "minimum graph CLI geometry: positive={cli_fits}, detail={detail}, bytes={}, tokens={cli_tokens:?}",
                        cli_wire.len()
                    );
                    assert!(
                        cli_wire.len() <= DEFAULT_RESPONSE_BUDGET_BYTES,
                        "both complete CLI projections fit the unchanged byte cap"
                    );
                    if cli_fits {
                        assert!(
                            cli_tokens <= MCP_CL100K_TOKEN_LIMIT,
                            "positive CLI projection fits the unchanged named-token cap"
                        );
                    } else {
                        assert!(
                            cli_tokens > MCP_CL100K_TOKEN_LIMIT,
                            "original complete CLI projection refuses specifically on valid named-token pressure"
                        );
                    }
                    let advice = backend_present::package_graph_json_alternative(
                        &expected_page,
                        &native,
                        Detail::parse(detail).expect("detail"),
                        Some(local_context()),
                    );
                    assert_eq!(
                        advice.is_some(),
                        cli_fits,
                        "independent complete CLI JSON proof"
                    );
                    let params = match exposure {
                        0 => json!({"name":"backend.package_graph","arguments":args}),
                        1 => {
                            json!({"name":super::super::tools::CALL_TOOL,"arguments":{"name":"backend.package_graph","arguments":args}})
                        }
                        _ => {
                            json!({"name":"backend.surface","arguments":{"command":SurfaceCommand::PackageGraphPage{request:native.clone()},"detail":detail}})
                        }
                    };
                    let reply = request(&mut server, "tools/call", &params);
                    assert_eq!(
                        reply["result"]["isError"], true,
                        "minimum complete readable row refuses: {reply}"
                    );
                    let fault = &reply["result"]["structuredContent"];
                    assert_eq!(fault["cause"], "oversized");
                    assert!(fault.get("package_graph_page").is_none());
                    assert!(fault.get("surface").is_none());
                    assert_eq!(
                        fault.get("shell").is_some(),
                        attached && cli_fits,
                        "{reply}"
                    );
                    assert!(fault.get("call").is_none(), "JSON is not an MCP argument");
                    if attached && cli_fits {
                        let advice = advice.expect("independently proved complete JSON fits");
                        assert_eq!(
                            fault["shell"],
                            advice.shell().expect("exact existing CLI route")
                        );
                        let shell = fault["shell"].as_str().expect("shell");
                        assert!(shell.contains("--format json"));
                        assert!(shell.contains("/observed/bundle/backend-cli"));
                        assert!(!shell.contains("nudox"));
                        assert!(!shell.contains("authority.secret"));
                        assert!(
                            reply["result"]["content"][0]["text"]
                                .as_str()
                                .expect("readable refusal")
                                .contains("Run on the machine serving this request")
                        );
                    }
                    assert_eq!(
                        server.product.surface_commands.len(),
                        1,
                        "no metadata dial or repeat at limit one"
                    );
                    assert!(wire_fits(&reply, ResponseTransport::StdioLine));
                    assert!(wire_tokens(&reply).expect("tokens") <= MCP_CL100K_TOKEN_LIMIT);
                }
            }
        }
    }
}

#[test]
fn graph_json_alternative_keeps_resumed_frontier_cancel_authority_and_catalog() {
    let rows = rows();
    let mut initial = PackageGraphPageRequest::new(
        rows[0].source.clone(),
        PackageGraphDirection::Dependencies,
        Some(rows[0].source_authority),
        1,
    )
    .expect("initial");
    initial.catalog_snapshot = Some([6; 32]);
    let first = page_for(&initial, &rows);
    let PackageGraphPageTerminal::More(cursor) = first.terminal else {
        panic!("issued frontier")
    };
    let mut resumed = initial.with_cursor(cursor);
    let mut page = page_for(&resumed, &rows);
    resumed.control = backend_library::PackageGraphControl::Cancel;
    page.rows = Box::new([]);
    page.knowledge = PackageGraphKnowledge::Unknown { reason: None };
    page.terminal = PackageGraphPageTerminal::Cancelled;
    page.admit_for(&resumed)
        .expect("cancelled native frontier remains admitted");
    assert!(
        backend_present::grammar_for(backend_present::SURFACE_VERB).is_none(),
        "the dedicated CLI JSON route must not become a registered named target"
    );
    let json = backend_present::encode_package_graph_json(
        &page,
        Detail::Full,
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .expect("exact cancellation JSON projection");
    let mut wire = json.bytes.into_vec();
    wire.push(b'\n');
    assert!(wire.len() <= DEFAULT_RESPONSE_BUDGET_BYTES);
    assert!(
        backend_present::cl100k_tokens(std::str::from_utf8(&wire).expect("JSON UTF-8"))
            .expect("in-repo canonical tokenizer")
            <= backend_present::DEFAULT_RESPONSE_TOKEN_BUDGET
    );
    let advice = backend_present::package_graph_json_alternative(
        &page,
        &resumed,
        Detail::Full,
        Some(local_context()),
    )
    .expect("complete canonical JSON fits");
    let backend_present::Affordance::ReadPackageGraphJson {
        request: exact,
        detail,
        ..
    } = &advice
    else {
        panic!("typed shell alternative")
    };
    assert_eq!(exact.as_ref(), &resumed);
    assert_eq!(*detail, Detail::Full);
    assert!(advice.tool_call().is_none());
    let command = SurfaceCommand::PackageGraphPage {
        request: resumed.clone(),
    };
    let input = serde_json::to_string(&command).expect("exact registered CLI input");
    assert!(
        matches!(backend_present::lower_surface_json(&input).expect("existing CLI surface grammar"), backend_present::Request::Surface(lowered) if *lowered == command)
    );
    assert!(advice.shell().expect("shell").ends_with(&format!(
            "surface '{}')",
            serde_json::to_string(&SurfaceCommand::PackageGraphPage { request: resumed })
                .expect("exact typed input")
        )));
}

#[test]
fn graph_json_alternative_requires_metadata_and_complete_canonical_budget() {
    let rows = rows();
    let request = PackageGraphPageRequest::new(
        rows[0].source.clone(),
        PackageGraphDirection::Dependencies,
        None,
        64,
    )
    .expect("request");
    let page = page_for(&request, &rows);
    assert!(
        backend_present::package_graph_json_alternative(&page, &request, Detail::Full, None)
            .is_none()
    );
    assert!(
        backend_present::package_graph_json_alternative(
            &page,
            &request,
            Detail::Full,
            Some(local_context())
        )
        .is_none(),
        "canonical JSON itself cannot fit; no impossible promise"
    );
    assert!(
        backend_present::OwnerContext::from_local_paths(
            std::path::Path::new("relative/project"),
            std::path::Path::new("/srv/workspace"),
            std::path::Path::new("/srv/owner.sock")
        )
        .is_none()
    );
}

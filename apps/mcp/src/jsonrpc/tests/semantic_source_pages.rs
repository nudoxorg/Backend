//! Transport seams use typed real capture bytes; native owner/cold proof remains
//! in local-service. Synthetic discovery membership does not claim publication.
use super::*;
use backend_library::{
    SemanticSourceDocument, SemanticSourceRange, SemanticSourceReference,
    SemanticVersionDescriptor, SemanticVersionRecord, SemanticVersionsPage,
};

fn captured() -> SemanticVersionRecord {
    let packet: Value = serde_json::from_str(include_str!(
        "../../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("unchanged real42 capture");
    serde_json::from_value(packet["surface"]["data"][0].clone()).expect("complete typed record")
}
fn descriptor(record: &SemanticVersionRecord) -> SemanticVersionDescriptor {
    SemanticVersionDescriptor::Available {
        reference: SemanticSourceReference::capture(
            record,
            record
                .selected_source_frontier
                .as_ref()
                .map_or([8; 32], |frontier| frontier.source_relation_root),
            [34; 32],
            &[7; 32],
        )
        .expect("transport guard, not native authority"),
    }
}
pub(super) fn page(limit: u16, offset: u64, total: u64, root: u8) -> SemanticVersionsPage {
    let original = captured();
    let mut entries = Vec::new();
    for ordinal in offset..total.min(offset + u64::from(limit)) {
        let mut record = original.clone();
        if ordinal != 0 {
            // Strict native key order after the real capture's first member.
            let mut generation = original.generation.to_bytes();
            let mut carry = ordinal;
            for byte in generation.iter_mut().rev() {
                carry += u64::from(*byte);
                *byte = carry as u8;
                carry >>= 8;
            }
            assert_eq!(carry, 0, "synthetic key increment cannot wrap");
            record.generation = backend_library::SemanticGenerationId::new(generation);
            record.selected = false;
            record.selected_source_frontier = None;
            record.history_status = backend_library::SemanticHistoryPublicationStatus::NotSelected;
        }
        if let Some(frontier) = &mut record.selected_source_frontier {
            frontier.source_relation_root = [root; 32];
        }
        entries.push(SemanticVersionDescriptor::Available {
            reference: SemanticSourceReference::capture(&record, [root; 32], [34; 32], &[7; 32])
                .expect("sealed synthetic page observation"),
        });
    }
    let end = offset + entries.len() as u64;
    let more = end < total;
    let next_cursor = if more {
        let metadata = entries.last().expect("nonempty page").metadata();
        Some(
            backend_library::SemanticVersionsCursor::issue(
                backend_library::SemanticVersionsCursorClaims {
                    package: original.package.clone(),
                    source_relation_root: [root; 32],
                    semantic_relation_root: [34; 32],
                    coordinate: metadata.coordinate.clone(),
                    profile: metadata.profile,
                    generation: metadata.generation,
                    limit,
                    total,
                    offset: end,
                },
                &[7; 32],
            )
            .expect("synthetic native transport continuation"),
        )
    } else {
        None
    };
    let page = SemanticVersionsPage {
        package: original.package.clone(),
        source_relation_root: [root; 32],
        semantic_relation_root: [34; 32],
        total,
        offset,
        entries: entries.into_boxed_slice(),
        next_cursor,
        more,
        complete: !more,
    };
    page.admit().expect("self-consistent native transport page");
    page
}

#[test]
fn semantic_versions_jsonrpc_preserves_captured_semver_and_matches_the_cli_projection() {
    let original = captured();
    let discovery = native_transport_source_page(&original);
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::SemanticVersionsPage(discovery.clone())),
        ..Fake::default()
    });
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.semantic_versions","arguments":{"package":original.package.as_str(),"detail":"full"}}),
    );
    assert_context_bounded(&response);
    assert_eq!(response["result"]["isError"], false, "{response}");
    let restored: SemanticVersionsPage = serde_json::from_value(
        response["result"]["structuredContent"]["semantic_data"]["value"].clone(),
    )
    .expect("complete descriptor discovery");
    assert_eq!(restored, discovery);
    let SemanticVersionDescriptor::Available { reference } = &restored.entries[0] else {
        unreachable!()
    };
    assert_eq!(reference.metadata.coordinate, original.coordinate);
    assert_eq!(reference.metadata.generation, original.generation);
    assert_eq!(reference.metadata.complete, original.complete);
    let backend_library::SemanticSourceHistory::Published { proof, .. } = &reference.history else {
        panic!("actual complete history")
    };
    assert_eq!(proof.image_count, 42);
    let document = std::sync::Arc::new(
        SemanticSourceDocument::new(&original).expect("native captured document"),
    );
    let mut union = backend_library::SemanticSourceAssembly::new(reference.source.clone())
        .expect("exact union");
    let mut next = json!({"name":"backend.semantic_source","arguments":{"request":{"reference":reference},"detail":"full"}});
    let mut source_server = ready(Fake {
        semantic_source_document: Some(document),
        ..Fake::default()
    });
    loop {
        let response = request(&mut source_server, "tools/call", &next);
        assert_context_bounded(&response);
        assert_eq!(response["result"]["isError"], false, "{response}");
        let chunk: backend_library::SemanticSourceChunk = serde_json::from_value(
            response["result"]["structuredContent"]["semantic_data"]["value"].clone(),
        )
        .expect("exact native byte range");
        union.append(&chunk).expect("strict monotonic union");
        if chunk.complete {
            break;
        }
        next = response["result"]["structuredContent"]["continuation_call"].clone();
    }
    let reassembled = union.finish().expect("complete digest and canonical DTO");
    assert_eq!(
        reassembled, original,
        "all metadata and all42 history image proofs survive"
    );
    let reply = SurfaceReply::SemanticVersions(Box::new([reassembled]));
    let answer = Answer::Product(Box::new(backend_present::product_view(&reply)));
    let cli = encode_answer(&answer, Detail::Full, None, usize::MAX)
        .expect("archived full CLI projection");
    let cli: Value = serde_json::from_slice(&cli.bytes).expect("CLI projection");
    assert_eq!(cli["semantic_data"]["value"], json!([original]));
}

fn assert_compact_raw_surface_refused(server: &mut Server<Fake>, command: &SurfaceCommand) {
    let before = server.product.surface_commands.len();
    let response = request(
        server,
        "tools/call",
        &McpExposure::RegisteredCall.expose_call(json!({
            "name":"backend.surface", "arguments":{"command":command, "detail":"full"}
        })),
    );
    assert_context_bounded(&response);
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], true, "{response}");
    let fault = &response["result"]["structuredContent"];
    assert_eq!(fault["slug"], "usage");
    assert_eq!(fault["operand"], "name");
    assert_eq!(fault["cause"], "malformed");
    assert!(fault.get("semantic_data").is_none());
    assert!(fault.get("surface").is_none());
    assert!(fault.get("continuation_call").is_none());
    assert!(fault.get("shell").is_none());
    assert_eq!(
        server.product.surface_commands.len(),
        before,
        "unregistered route never reaches native source"
    );
}

#[test]
fn discovery_complete_coordinate_union_is_monotonic_through_direct_compact_and_raw_calls() {
    for raw in [false, true] {
        for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
            let mut server = ready(Fake {
                semantic_versions_total: Some(40),
                ..Fake::default()
            });
            if raw && exposure == McpExposure::RegisteredCall {
                assert_compact_raw_surface_refused(
                    &mut server,
                    &SurfaceCommand::SemanticVersions {
                        package: captured().package,
                        limit: 20,
                        cursor: None,
                    },
                );
            }
            // Raw surface is direct-only. The compact exposure reconstructs
            // the identical discovery via its registered semantic route.
            let raw_surface = raw && exposure == McpExposure::Direct;
            let mut next = if raw_surface {
                exposure.expose_call(json!({"name":"backend.surface","arguments":{"command":SurfaceCommand::SemanticVersions { package:captured().package, limit:20, cursor:None },"detail":"full"}}))
            } else {
                exposure.expose_call(json!({"name":"backend.semantic_versions","arguments":{"package":captured().package.as_str(),"limit":20,"detail":"full"}}))
            };
            let mut offset = 0;
            let mut last = None;
            let mut coordinates = Vec::new();
            let mut pages = 0;
            loop {
                let response = encoded_stdio_reply(server.handle_value(json!({"jsonrpc":"2.0","id":"λ🙂\\\"".repeat(150),"method":"tools/call","params":next}), None).expect("actual caller ID"));
                assert_context_bounded(&response);
                assert_eq!(response["result"]["isError"], false, "{response}");
                let structured = &response["result"]["structuredContent"];
                let page: SemanticVersionsPage = serde_json::from_value(if raw_surface {
                    structured["surface"]["data"].clone()
                } else {
                    structured["semantic_data"]["value"].clone()
                })
                .expect("complete page");
                assert_eq!(page.offset, offset);
                assert_eq!(page.total, 40);
                assert_eq!(page.source_relation_root, [8; 32]);
                assert_eq!(page.semantic_relation_root, [34; 32]);
                for entry in &page.entries {
                    let metadata = entry.metadata();
                    assert!(last.is_none_or(|last| last < metadata.generation));
                    last = Some(metadata.generation);
                    coordinates.push((metadata.coordinate.clone(), metadata.generation));
                    assert!(
                        text_of(&response["result"])
                            .lines()
                            .any(|line| line == metadata.coordinate.as_str()),
                        "complete selectable coordinate line"
                    );
                }
                offset += page.entries.len() as u64;
                pages += 1;
                if page.complete {
                    assert_eq!(offset, 40);
                    break;
                }
                next = structured["continuation_call"].clone();
                let selected = if exposure == McpExposure::RegisteredCall {
                    &next["arguments"]
                } else {
                    &next
                };
                if raw_surface {
                    let command: SurfaceCommand =
                        serde_json::from_value(selected["arguments"]["command"].clone())
                            .expect("closed nested command");
                    let SurfaceCommand::SemanticVersions { limit, cursor, .. } = command else {
                        unreachable!()
                    };
                    assert_eq!(limit as usize, page.entries.len());
                    assert_eq!(cursor, page.next_cursor);
                    assert!(selected["arguments"].get("cursor").is_none());
                } else {
                    assert_eq!(selected["arguments"]["cursor"], json!(page.next_cursor));
                }
                assert!(pages <= 40, "monotonic progress cannot loop");
            }
            assert_eq!(coordinates.len(), 40);
            assert!(pages > 1, "real budget preparation must paginate");
        }
    }
}

#[test]
fn raw_source_and_uri_resources_reconstruct_the_entire_capture_with_exact_selected_ranges() {
    let original = captured();
    let SemanticVersionDescriptor::Available { reference } = descriptor(&original) else {
        unreachable!()
    };
    for resource in [false, true] {
        for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
            let document =
                std::sync::Arc::new(SemanticSourceDocument::new(&original).expect("real42"));
            let mut server = ready(Fake {
                semantic_source_document: Some(document),
                ..Fake::default()
            });
            let mut union = backend_library::SemanticSourceAssembly::new(reference.source.clone())
                .expect("source union");
            let native = backend_library::SemanticSourceRequest {
                reference: reference.clone(),
                range: SemanticSourceRange::default(),
            };
            let mut uri = native.resource_uri().expect("canonical original URI");
            if !resource && exposure == McpExposure::RegisteredCall {
                assert_compact_raw_surface_refused(
                    &mut server,
                    &SurfaceCommand::SemanticSource {
                        request: native.clone(),
                    },
                );
            }
            let mut next = if exposure == McpExposure::Direct {
                json!({"name":"backend.surface","arguments":{"command":SurfaceCommand::SemanticSource { request:native },"detail":"full"}})
            } else {
                exposure.expose_call(json!({"name":"backend.semantic_source","arguments":{"request":native,"detail":"full"}}))
            };
            loop {
                let response = if resource {
                    request(&mut server, "resources/read", &json!({"uri":uri}))
                } else {
                    request(&mut server, "tools/call", &next)
                };
                assert_context_bounded(&response);
                assert!(response.get("error").is_none(), "{response}");
                let chunk: backend_library::SemanticSourceChunk = if resource {
                    assert_eq!(response["result"]["contents"][0]["uri"], uri);
                    serde_json::from_str(
                        response["result"]["contents"][0]["text"]
                            .as_str()
                            .expect("canonical resource JSON"),
                    )
                    .expect("source chunk")
                } else {
                    assert_eq!(response["result"]["isError"], false, "{response}");
                    serde_json::from_value(if exposure == McpExposure::Direct {
                        response["result"]["structuredContent"]["surface"]["data"].clone()
                    } else {
                        response["result"]["structuredContent"]["semantic_data"]["value"].clone()
                    })
                    .expect("raw source chunk")
                };
                assert_eq!(chunk.offset, union.offset().expect("exact byte offset"));
                union.append(&chunk).expect("no dropped or repeated bytes");
                let SurfaceCommand::SemanticSource { request: selected } = server
                    .product
                    .surface_commands
                    .last()
                    .expect("actual native request")
                else {
                    unreachable!()
                };
                assert_eq!(selected.reference, reference);
                assert_eq!(selected.range.offset, chunk.offset);
                if chunk.complete {
                    break;
                }
                if resource {
                    uri = response["result"]["_meta"]["backend/nextUri"]
                        .as_str()
                        .expect("exact executable next URI")
                        .to_owned();
                    let resumed = backend_library::SemanticSourceRequest::from_resource_uri(&uri)
                        .expect("canonical next URI");
                    assert_eq!(resumed.reference, reference);
                    assert_eq!(resumed.range.offset, chunk.next.expect("forward"));
                    assert_eq!(resumed.range.max_bytes, selected.range.max_bytes);
                } else {
                    next = response["result"]["structuredContent"]["continuation_call"].clone();
                    let call = if exposure == McpExposure::RegisteredCall {
                        &next["arguments"]
                    } else {
                        &next
                    };
                    let resumed: backend_library::SemanticSourceRequest =
                        if exposure == McpExposure::Direct {
                            assert_eq!(call["name"], "backend.surface");
                            assert!(call["arguments"].get("request").is_none());
                            let SurfaceCommand::SemanticSource { request } =
                                serde_json::from_value(call["arguments"]["command"].clone())
                                    .expect("closed raw continuation")
                            else {
                                unreachable!()
                            };
                            request
                        } else {
                            assert_eq!(next["name"], "backend.call");
                            assert_eq!(call["name"], "backend.semantic_source");
                            assert!(call["arguments"].get("command").is_none());
                            serde_json::from_value(call["arguments"]["request"].clone())
                                .expect("closed registered source continuation")
                        };
                    assert_eq!(resumed.reference, reference);
                    assert_eq!(resumed.range.offset, chunk.next.expect("forward"));
                    assert_eq!(resumed.range.max_bytes, selected.range.max_bytes);
                }
            }
            assert_eq!(union.finish().expect("full exact source digest"), original);
        }
    }
}

#[test]
fn source_copyable_calls_reconstruct_all_real42_bytes_through_both_exposures() {
    let original = captured();
    let SemanticVersionDescriptor::Available { reference } = descriptor(&original) else {
        unreachable!()
    };
    let document =
        std::sync::Arc::new(SemanticSourceDocument::new(&original).expect("exact real42 bytes"));
    for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
        let mut union =
            backend_library::SemanticSourceAssembly::new(reference.source.clone()).expect("union");
        let mut next = exposure.expose_call(json!({"name":"backend.semantic_source","arguments":{
            "request":{"reference":reference}, "detail":"full"
        }}));
        loop {
            let mut server = ready(Fake {
                semantic_source_document: Some(document.clone()),
                ..Fake::default()
            });
            let response = request(&mut server, "tools/call", &next);
            assert_context_bounded(&response);
            assert!(wire_tokens(&response).expect("whole envelope") <= MCP_CL100K_TOKEN_LIMIT);
            let result = &response["result"];
            assert_eq!(result["isError"], false, "{response}");
            let restored: backend_library::SemanticSourceChunk = serde_json::from_value(
                result["structuredContent"]["semantic_data"]["value"].clone(),
            )
            .expect("typed source chunk");
            let SurfaceCommand::SemanticSource {
                request: selected_request,
            } = server
                .product
                .surface_commands
                .last()
                .expect("selected exact request")
            else {
                unreachable!()
            };
            let range = selected_request.range.clone();
            let chunk = document.chunk(&range).expect("selected native bytes");
            assert_eq!(restored, chunk);
            union
                .append(&restored)
                .expect("exact order/identity/coverage");
            if chunk.complete {
                assert!(
                    result["structuredContent"]
                        .get("continuation_call")
                        .is_none()
                );
                break;
            }
            next = result["structuredContent"]["continuation_call"].clone();
            let selected = if exposure == McpExposure::RegisteredCall {
                &next["arguments"]
            } else {
                &next
            };
            assert_eq!(selected["name"], "backend.semantic_source");
            assert_eq!(
                selected["arguments"]["request"]["reference"],
                json!(reference)
            );
            assert_eq!(
                selected["arguments"]["request"]["range"],
                json!({"offset":chunk.next,"max_bytes":range.max_bytes})
            );
            assert_eq!(selected["arguments"]["detail"], "full");
            assert!(text_of(result).contains("structuredContent.continuation_call"));
            for command in &server.product.surface_commands {
                let SurfaceCommand::SemanticSource { request } = command else {
                    unreachable!()
                };
                assert_eq!(request.reference, reference);
                assert_eq!(request.range.offset, range.offset);
            }
        }
        assert_eq!(union.finish().expect("complete canonical digest"), original);
    }
}

#[test]
fn oversized_initial_versions_page_adapts_with_exact_native_limit_and_one_deadline() {
    let pages =
        [20, 10, 5, 3, 2, 1].map(|limit| SurfaceReply::SemanticVersionsPage(page(limit, 0, 40, 8)));
    let mut server = ready(Fake {
        surface_replies: pages.into(),
        ..Fake::default()
    });
    let lifetime = RequestContext::new();
    let deadline = lifetime
        .read_deadline(std::time::Duration::from_secs(30))
        .expect("original deadline");
    let response = encoded_stdio_reply(
        server
            .handle_value(
                json!({
                    "jsonrpc":"2.0","id":9,"method":"tools/call","params":{
                        "name":"backend.call","arguments":{
                            "name":"backend.semantic_versions","arguments":{
                                "package":captured().package.as_str(),"limit":20,"detail":"full"
                            }
                        }
                    }
                }),
                Some(lifetime.clone()),
            )
            .expect("request with original lifetime"),
    );
    assert_context_bounded(&response);
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert!(
        server.product.surface_commands.len() > 1,
        "initial complete page must actually exceed the measured budget"
    );
    let SurfaceCommand::SemanticVersions { limit, cursor, .. } = server
        .product
        .surface_commands
        .last()
        .expect("native reads")
    else {
        unreachable!()
    };
    assert!(*limit < 20 && cursor.is_none());
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["semantic_data"]["value"]["total"], 40);
    assert_eq!(
        structured["semantic_data"]["value"]["entries"]
            .as_array()
            .expect("exact entries")
            .len(),
        usize::from(*limit)
    );
    let next = &structured["continuation_call"]["arguments"];
    assert_eq!(next["name"], "backend.semantic_versions");
    assert_eq!(next["arguments"]["limit"], *limit);
    assert_eq!(next["arguments"]["cursor"], structured["nextCursor"]);
    for context in &server.product.request_contexts {
        assert_eq!(
            context
                .read_deadline(std::time::Duration::from_secs(60))
                .expect("same deadline"),
            deadline
        );
    }
    lifetime.cancel(RequestCancellationReason::Explicit);
    assert!(
        server
            .product
            .request_contexts
            .iter()
            .all(|context| context.cancellation_reason()
                == Some(RequestCancellationReason::Explicit))
    );
}

#[test]
fn adapting_versions_refuses_changed_roots_and_never_resizes_continuations() {
    let mut server = ready(Fake {
        surface_replies: [
            SurfaceReply::SemanticVersionsPage(page(20, 0, 60, 8)),
            SurfaceReply::SemanticVersionsPage(page(10, 0, 60, 9)),
        ]
        .into(),
        ..Fake::default()
    });
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.semantic_versions","arguments":{
            "package":captured().package.as_str(),"limit":20,"detail":"full"
        }}),
    );
    assert!(
        response["result"]["isError"] == true || response.get("error").is_some(),
        "{response}"
    );
    assert_eq!(server.product.surface_commands.len(), 2);
    let first = page(20, 0, 60, 8);
    let cursor = first.next_cursor.expect("owner cursor");
    let mut server = ready(Fake {
        surface_reply: Some(SurfaceReply::SemanticVersionsPage(page(20, 20, 60, 8))),
        ..Fake::default()
    });
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.semantic_versions","arguments":{
            "package":captured().package.as_str(),"limit":20,"cursor":cursor,"detail":"full"
        }}),
    );
    assert!(
        response["result"]["isError"] == true || response.get("error").is_some(),
        "{response}"
    );
    assert_eq!(
        server.product.surface_commands.len(),
        1,
        "authenticated continuation limits never resize"
    );
}

#[test]
fn current_paged_mcp_command_rejects_matching_and_foreign_legacy_arrays() {
    for foreign in [false, true] {
        let mut record = captured();
        let package = record.package.clone();
        if foreign {
            record.package = PackageReference::from_kind(
                backend_library::PackageReferenceKind::Local,
                "/abs/foreign",
            )
            .expect("foreign package");
        }
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticVersions(Box::new([record]))),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.semantic_versions","arguments":{"package":package.as_str()}}),
        );
        assert!(
            response.get("error").is_some() || response["result"]["isError"] == true,
            "{response}"
        );
        assert!(
            response["result"]["structuredContent"]
                .get("semantic_data")
                .is_none()
        );
        assert_eq!(server.product.surface_commands.len(), 1);
    }
}

#[test]
fn bounded_version_preparation_keeps_expired_and_cancelled_callers_out_of_owner_reads() {
    for cancelled in [false, true] {
        let lifetime = RequestContext::new();
        if cancelled {
            lifetime.cancel(RequestCancellationReason::Explicit);
        } else {
            lifetime
                .read_deadline(std::time::Duration::ZERO)
                .expect_err("original exhausted budget");
        }
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::SemanticVersionsPage(page(1, 0, 40, 8))),
            ..Fake::default()
        });
        let response = encoded_stdio_reply(server.handle_value(json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{
            "name":"backend.semantic_versions","arguments":{"package":captured().package.as_str()}
        }}),Some(lifetime)).expect("typed stopped request reply"));
        assert!(
            response.get("error").is_some() || response["result"]["isError"] == true,
            "{response}"
        );
        assert!(
            server.product.surface_commands.is_empty(),
            "bounded preparation cannot reset a stopped caller's lifetime"
        );
    }
}

#[test]
fn source_preparation_rejects_a_self_consistent_foreign_range_before_any_success() {
    let original = captured();
    let SemanticVersionDescriptor::Available { reference } = descriptor(&original) else {
        unreachable!()
    };
    let document = SemanticSourceDocument::new(&original).expect("real42 bytes");
    let request_native = backend_library::SemanticSourceRequest {
        reference,
        range: SemanticSourceRange {
            offset: 0,
            max_bytes: 31,
        },
    };
    for changed in 0..3 {
        let mut chunk = document
            .chunk(&SemanticSourceRange {
                offset: 31,
                max_bytes: 31,
            })
            .expect("internally valid different range");
        if changed == 1 {
            chunk.source.version[0] ^= 1;
        }
        if changed == 2 {
            chunk = document
                .chunk(&SemanticSourceRange {
                    offset: 0,
                    max_bytes: 30,
                })
                .expect("internally valid short reply");
        }
        chunk
            .admit()
            .expect("self consistency does not bind a caller");
        let command = SurfaceCommand::SemanticSource {
            request: request_native.clone(),
        };
        assert!(
            backend_library::admit_surface_reply(
                &command,
                &SurfaceReply::SemanticSource(chunk.clone())
            )
            .is_err()
        );
        for raw in [false, true] {
            let mut server = ready(Fake {
                surface_reply: Some(SurfaceReply::SemanticSource(chunk.clone())),
                ..Fake::default()
            });
            let call = if raw {
                json!({"name":"backend.surface","arguments":{"command":command}})
            } else {
                json!({"name":"backend.semantic_source","arguments":{"request":request_native}})
            };
            let response = request(&mut server, "tools/call", &call);
            assert!(
                response.get("error").is_some() || response["result"]["isError"] == true,
                "{response}"
            );
            assert_eq!(
                server.product.surface_commands.len(),
                1,
                "correlation failure cannot resize into success"
            );
        }
    }
}

#[test]
fn default_versions_credit_is_shared_and_two_complete_descriptors_need_one_owner_read() {
    let credit=backend_library::DEFAULT_SEMANTIC_VERSIONS_LIMIT;
    assert_eq!(credit,8);
    let card=super::super::tools::catalog_tool("backend.semantic_versions").expect("registered schema");
    assert_eq!(card["inputSchema"]["properties"]["limit"]["default"],credit);
    for exposure in [McpExposure::Direct,McpExposure::RegisteredCall] {
        for detail in [Detail::Summary,Detail::Standard,Detail::Full] {
            let mut server=ready(Fake{semantic_versions_total:Some(2),..Fake::default()});
            let response=request(&mut server,"tools/call",&exposure.expose_call(json!({"name":"backend.semantic_versions","arguments":{"package":captured().package.as_str(),"detail":detail.name()}})));
            assert_context_bounded(&response);
            assert!(wire_tokens(&response).unwrap()<=MCP_CL100K_TOKEN_LIMIT);
            assert_eq!(response["jsonrpc"],"2.0");assert_eq!(response["id"],9);assert!(response.get("error").is_none());
            let result=&response["result"];assert_eq!(result["isError"],false,"{response}");
            let actual:SemanticVersionsPage=serde_json::from_value(result["structuredContent"]["semantic_data"]["value"].clone()).expect("complete typed descriptors");
            assert_eq!(actual,page(credit,0,2,8));assert_eq!(actual.entries.len(),2);assert!(!actual.more);assert!(actual.complete);assert!(actual.next_cursor.is_none());
            assert!(result["structuredContent"].get("continuation_call").is_none());
            assert!(matches!(&server.product.surface_commands[..],[SurfaceCommand::SemanticVersions{limit,cursor:None,..}] if *limit==credit));
        }
    }
}

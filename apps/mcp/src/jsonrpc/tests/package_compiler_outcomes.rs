//! Every currently advertised profile stays discoverable with dense failures.
use super::*;
use backend_library::{
    IndexOperationProfileRefusal, IndexOperationSemanticProfileState as State,
    IndexOperationSemanticUnavailableReason as Reason, IndexOperationSourceProfile,
    IndexSourceCaptureSummary, PackageCompilerDiagnostic, PackageCompilerFailure,
    PackageCompilerOutcomeBasis, PackageCompilerOutcomeRead, PackageCompilerOutcomeSelector,
    PackageCompilerOutcomes, PackageCompilerProfileRefusal, SemanticLanguageProfile,
};

fn discovery() -> PackageCompilerOutcomes {
    discovery_with_profiles(
        backend_semantic::vocabulary::LanguageProfile::PRODUCT_PROFILES
            .into_iter()
            .map(SemanticLanguageProfile::new)
            .collect(),
    )
}

fn canonical_discovery() -> PackageCompilerOutcomes {
    let profiles: Vec<SemanticLanguageProfile> = (0..8)
        .flat_map(|family| (0..8).map(move |dialect| [family, dialect]))
        .filter_map(|bytes| serde_json::from_value(json!(bytes)).ok())
        .filter(|profile: &SemanticLanguageProfile| profile.profile().is_ok())
        .take(16)
        .collect();
    assert_eq!(
        profiles.len(),
        16,
        "closed canonical dialects fit the independent capture count bound"
    );
    discovery_with_profiles(profiles)
}

fn discovery_with_profiles(mut profiles: Vec<SemanticLanguageProfile>) -> PackageCompilerOutcomes {
    let package = PackageReference::parse(PROJECT).expect("local");
    let basis = PackageCompilerOutcomeBasis {
        package_key: package_key(PROJECT).to_bytes(),
        source_relation_root: [3; 32],
        source_version: [4; 32],
        capture_relation_root: [5; 32],
    };
    profiles.sort();
    let source_capture = IndexSourceCaptureSummary {
        cargo_target: None,
        producer_package: package,
        request_identity: [6; 32],
        commit_identity: [7; 32],
        workspace_root: [8; 32],
        workspace_sequence: 1,
        profiles: profiles
            .iter()
            .map(|profile| IndexOperationSourceProfile {
                profile: *profile,
                source_version: [4; 32],
                input_digest: [9; 32],
                observation_sequence: 1,
                source_count: 407,
                state: State::Unavailable {
                    reason: Reason::Rejected,
                },
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    };
    let refusals = profiles
        .into_iter()
        .map(|profile| PackageCompilerProfileRefusal {
            profile,
            reason: Reason::Rejected,
            diagnostic: PackageCompilerDiagnostic::Retained {
                read: PackageCompilerOutcomeRead::Diagnostic(PackageCompilerOutcomeSelector {
                    basis: basis.identity(),
                    profile,
                }),
            },
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    PackageCompilerOutcomes::Summary {
        coverage: None,
        basis,
        source_capture,
        refusals,
    }
}

fn maximum_failure() -> PackageCompilerFailure {
    use backend_library::interface::{CompilerAttempt, CompilerFragmentFailure, SourceAuthority};
    use backend_semantic::ir::{BuildError, EntityId};
    use backend_version::{CompileRecipeDomain, ContentId, SourceFactDomain};
    let path = "\"".repeat(PackageCompilerFailure::MAX_RELATIVE_PATH_BYTES);
    PackageCompilerFailure::from_fragment_failure(
        &path,
        CompilerAttempt {
            source: SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"captured bytes"),
                byte_len: 14,
            },
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"recipe"),
        },
        &CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
            owner: EntityId::new(7),
            start: 18,
            end: 24,
        }),
    )
    .expect("maximum escaped path is a legal full failure")
}

fn reply(outcomes: PackageCompilerOutcomes) -> SurfaceReply {
    SurfaceReply::PackageProfile {
        latest: None,
        versions: 0,
        candidate_authority: None,
        source_metadata: None,
        compiler_outcomes: Some(outcomes),
    }
}

#[test]
fn all_nine_product_profiles_and_sixteen_canonical_dialects_fit_complete_cli_mcp_envelopes() {
    let current = discovery();
    let PackageCompilerOutcomes::Summary { source_capture, .. } = &current else {
        panic!("summary")
    };
    assert_eq!(
        source_capture.profiles.len(),
        9,
        "every advertised product profile"
    );
    for discovery in [current, canonical_discovery()] {
        let PackageCompilerOutcomes::Summary {
            basis,
            source_capture,
            refusals,
            ..
        } = &discovery
        else {
            panic!("summary")
        };
        let mut reads = vec![
            (None, discovery.clone()),
            (Some(PackageCompilerOutcomeRead::Current), discovery.clone()),
        ];
        let failure = maximum_failure();
        assert!(failure.encode_bounded_json().expect("full fact").len() > 6 * 1024);
        for refusal in refusals {
            let PackageCompilerDiagnostic::Retained { read } = &refusal.diagnostic else {
                panic!("retained")
            };
            reads.push((
                Some(*read),
                PackageCompilerOutcomes::Diagnostic {
                    basis: *basis,
                    selector: *read.selector().expect("diagnostic selector"),
                    source_capture: source_capture.clone(),
                    refusal: IndexOperationProfileRefusal {
                        profile: read.selector().expect("diagnostic selector").profile,
                        reason: refusal.reason,
                        compiler_failure: Some(failure.clone()),
                    },
                },
            ));
        }
        for (selector, outcomes) in reads {
            let native = if selector.is_some() {
                SurfaceReply::PackageCompilerOutcomes(outcomes.clone())
            } else {
                reply(outcomes.clone())
            };
            backend_library::admit_surface_reply(
                &SurfaceCommand::PackageProfile {
                    package: PackageReference::parse(PROJECT).expect("local"),
                    compiler_outcome: selector,
                },
                &native,
            )
            .expect("actual reply admission");
            for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
                let answer = Answer::Product(Box::new(backend_present::product_view(&native)));
                let cli = backend_present::encode_answer(
                    &answer,
                    detail,
                    None,
                    DEFAULT_RESPONSE_BUDGET_BYTES,
                )
                .expect("complete CLI projection fits default ceiling");
                assert!(cli.bytes.len() <= 48 * 1024);
                assert_eq!(cli.budget.bytes, cli.bytes.len());
                assert!(
                    backend_present::encode_answer(&answer, detail, None, 1024).is_err(),
                    "small caller budgets refuse the whole fact instead of clipping"
                );
                backend_present::encode_answer(
                    &answer,
                    detail,
                    None,
                    backend_present::MAX_RESPONSE_BUDGET_BYTES,
                )
                .expect("same complete facts fit the existing hard ceiling");
                assert!(
                    backend_present::cl100k_tokens(std::str::from_utf8(&cli.bytes).expect("UTF8"))
                        .expect("actual token policy")
                        <= MCP_CL100K_TOKEN_LIMIT
                );
                let mut args = json!({"package":PROJECT,"detail":detail.name()});
                if let Some(selector) = selector {
                    args["compiler_outcome"] = json!(selector);
                }
                for named in [false, true] {
                    let mut server = ready(Fake {
                        surface_reply: Some(native.clone()),
                        ..Fake::default()
                    });
                    let params = if named {
                        json!({"name":"backend.call","arguments":{"name":"backend.package_profile","arguments":args}})
                    } else {
                        json!({"name":"backend.package_profile","arguments":args})
                    };
                    let response = request(&mut server, "tools/call", &params);
                    assert_context_bounded(&response);
                    assert!(
                        wire_tokens(&response).expect("whole envelope tokens")
                            <= MCP_CL100K_TOKEN_LIMIT,
                        "{response}"
                    );
                    assert_eq!(response["result"]["isError"], false, "{response}");
                    let dto: backend_present::ProductDto =
                        serde_json::from_value(response["result"]["structuredContent"].clone())
                            .expect("typed response");
                    assert_eq!(
                        dto.compiler_outcomes,
                        Some(outcomes.clone()),
                        "no truncation under any detail or route"
                    );
                    assert_eq!(
                        response["result"]["structuredContent"],
                        serde_json::from_slice::<Value>(&cli.bytes).expect("CLI JSON")
                    );
                }
            }
        }
    }
}

#[test]
fn compiler_selector_invalid_call_then_valid_and_typed_stale_keep_exact_request() {
    let summary = discovery();
    let PackageCompilerOutcomes::Summary { refusals, .. } = &summary else {
        panic!("summary")
    };
    let PackageCompilerDiagnostic::Retained { read } = &refusals[0].diagnostic else {
        panic!("retained")
    };
    let mut server = ready(Fake {
        surface_reply: Some(reply(summary.clone())),
        ..Fake::default()
    });
    let invalid = request(
        &mut server,
        "tools/call",
        &json!({"name":"backend.package_profile","arguments":{"package":PROJECT,"compiler_outcome":{"basis":"00","profile":[1,0]}}}),
    );
    assert_eq!(invalid["result"]["isError"], true, "{invalid}");
    assert!(
        server.product.surface_commands.is_empty(),
        "invalid selector never reaches owner"
    );
    let valid = call(
        &mut server,
        "backend.package_profile",
        &json!({"package":PROJECT}),
    );
    assert_eq!(valid["isError"], false, "{valid}");
    let stale = PackageCompilerOutcomes::Stale {
        package: PackageReference::parse(PROJECT).expect("local"),
    };
    server.product.surface_reply = Some(SurfaceReply::PackageCompilerOutcomes(stale.clone()));
    let selected = call(
        &mut server,
        "backend.package_profile",
        &json!({"package":PROJECT,"compiler_outcome":read}),
    );
    assert_eq!(
        selected["structuredContent"]["compiler_outcomes"],
        json!(stale)
    );
    assert!(
        matches!(server.product.surface_commands.last(),Some(SurfaceCommand::PackageProfile { compiler_outcome:Some(observed),.. }) if observed==read)
    );
    let mut invocation = backend_present::Invocation::new(
        grammar_for("package-profile").expect("registered grammar"),
    );
    invocation.push(PROJECT);
    invocation.set(
        "compiler-outcome",
        serde_json::to_string(read).expect("exact selector"),
    );
    let backend_present::Request::Surface(command) =
        backend_present::lower(&invocation, PROJECT).expect("same CLI operand")
    else {
        panic!("surface")
    };
    assert!(
        matches!(*command,SurfaceCommand::PackageProfile { compiler_outcome:Some(observed),.. } if observed==*read)
    );
}

#[test]
fn named_package_profile_dot_uses_the_selected_project_but_surface_json_stays_exact() {
    let reply = SurfaceReply::PackageProfile {
        latest: None,
        versions: 0,
        candidate_authority: None,
        source_metadata: None,
        compiler_outcomes: None,
    };
    let mut server = ready(Fake {
        surface_reply: Some(reply.clone()),
        ..Fake::default()
    });
    let named = call(
        &mut server,
        "backend.package_profile",
        &json!({"package":"."}),
    );
    assert_eq!(named["isError"], false, "{named}");
    assert!(matches!(
        server.product.surface_commands.last(),
        Some(SurfaceCommand::PackageProfile {
            package: PackageReference::Local(label),
            compiler_outcome: None,
        }) if label.as_str() == PROJECT
    ));

    server.product.surface_reply = Some(reply);
    let exact = call(
        &mut server,
        "backend.surface",
        &json!({"command": {
            "operation":"package-profile",
            "package":{"kind":"local", "value":"."}
        }}),
    );
    assert_eq!(exact["isError"], false, "{exact}");
    assert!(matches!(
        server.product.surface_commands.last(),
        Some(SurfaceCommand::PackageProfile {
            package: PackageReference::Local(label),
            compiler_outcome: None,
        }) if label.as_str() == "."
    ), "the tagged surface escape hatch preserves its exact operand");
}

#[test]
fn partial_coverage_pages_keep_complete_escaped_observations_inside_actual_cli_and_mcp_budgets()
-> Result<(), String> {
    use backend_library::{PackageCompilerCoveragePage, PackageCompilerCoverageSelector};
    use backend_semantic::ir::{SourceCoverageGapCause, SourceCoverageRow};
    let current = discovery();
    let PackageCompilerOutcomes::Summary { basis, .. } = current else {
        panic!("basis")
    };
    let selector = PackageCompilerCoverageSelector {
        outcome: PackageCompilerOutcomeSelector {
            basis: basis.identity(),
            profile: SemanticLanguageProfile::from_name("typescript").ok_or_else(|| {
                "the TypeScript profile is present in the canonical test registry".to_owned()
            })?,
        },
        catalog: [6; 32],
        generation: [7; 32],
        input_digest: [9; 32],
        offset: 1,
    };
    let path = format!("src/{}.spec.ts", "q\"".repeat(600));
    let row = SourceCoverageRow::Gap {
        path: path.clone(),
        source:
            *backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                b"spec",
            )
            .as_ref(),
        byte_len: 4,
        cause: SourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram,
        diagnostic: None,
    };
    let page = PackageCompilerCoveragePage {
        generation: selector.generation,
        input_digest: selector.input_digest,
        source_count: 50,
        completed: 41,
        row_count: 10,
        rows: vec![row.clone()].into_boxed_slice(),
        next: Some(PackageCompilerOutcomeRead::Coverage(
            PackageCompilerCoverageSelector {
                offset: 2,
                ..selector
            },
        )),
    };
    let page_bytes = serde_json::to_vec(&page)
        .map_err(|error| format!("serialize the bounded coverage page: {error}"))?;
    assert!(page_bytes.len() <= 4096);
    let package = PackageReference::parse(PROJECT)
        .map_err(|error| format!("parse the coverage test package: {error}"))?;
    let outcomes = PackageCompilerOutcomes::Coverage {
        package: package.clone(),
        basis,
        selector,
        page,
    };
    let read = PackageCompilerOutcomeRead::Coverage(selector);
    let native = SurfaceReply::PackageCompilerOutcomes(outcomes.clone());
    backend_library::admit_surface_reply(
        &SurfaceCommand::PackageProfile {
            package,
            compiler_outcome: Some(read),
        },
        &native,
    )
    .map_err(|error| format!("admit the captured coverage reply: {error:?}"))?;
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let answer = Answer::Product(Box::new(backend_present::product_view(&native)));
        let cli =
            backend_present::encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
                .expect("complete bounded CLI coverage page");
        assert!(cli.bytes.len() <= 49152);
        let cli_json = std::str::from_utf8(&cli.bytes)
            .map_err(|error| format!("encoded CLI product is valid UTF-8: {error}"))?;
        let cli_token_count = backend_present::cl100k_tokens(cli_json)
            .map_err(|error| format!("count tokens in the encoded CLI product: {error:?}"))?;
        assert!(cli_token_count <= MCP_CL100K_TOKEN_LIMIT);
        let cli_value: Value = serde_json::from_slice(&cli.bytes)
            .map_err(|error| format!("decode the encoded CLI product: {error}"))?;
        for registered in [false, true] {
            let mut server = ready(Fake {
                surface_reply: Some(native.clone()),
                ..Fake::default()
            });
            let args = json!({"package":PROJECT,"compiler_outcome":read,"detail":detail.name()});
            let params = if registered {
                json!({"name":"backend.call","arguments":{"name":"backend.package_profile","arguments":args}})
            } else {
                json!({"name":"backend.package_profile","arguments":args})
            };
            let response = request(&mut server, "tools/call", &params);
            assert_context_bounded(&response);
            let response_tokens = wire_tokens(&response)
                .map_err(|error| format!("count tokens in the MCP response: {error:?}"))?;
            assert!(response_tokens <= MCP_CL100K_TOKEN_LIMIT);
            // request() returns the actual encoded complete JSON-RPC envelope,
            // whose tools/call result owns both isError and structuredContent.
            assert_eq!(response["jsonrpc"], "2.0", "{response}");
            assert_eq!(response["id"], 9, "{response}");
            assert!(response.get("error").is_none(), "{response}");
            let result = response
                .get("result")
                .expect("correlated tools/call result");
            assert_eq!(result["isError"], false, "{response}");
            assert_eq!(
                result["structuredContent"], cli_value,
                "the complete CLI and MCP DTOs agree at the same detail"
            );
            let dto: backend_present::ProductDto =
                serde_json::from_value(result["structuredContent"].clone())
                    .map_err(|error| format!("decode the typed MCP product DTO: {error}"))?;
            let observed = dto
                .compiler_outcomes
                .expect("complete typed coverage observation");
            assert_eq!(
                observed, outcomes,
                "full path/cause/source and continuation survive final transport envelope"
            );
            let PackageCompilerOutcomes::Coverage { page, .. } = observed else {
                panic!("coverage")
            };
            assert_eq!(page.rows.as_ref(), [row.clone()].as_slice());
        }
    }
    Ok(())
}

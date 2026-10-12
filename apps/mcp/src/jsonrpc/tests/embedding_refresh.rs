//! JSON-RPC fixture controls; these do not prove model activation or live owner quiescence.
use super::*;
use backend_library::{
    EmbeddingRefreshAction as Action, EmbeddingRefreshCompilerReceipt as Compiler,
    EmbeddingRefreshFailure as Failure, EmbeddingRefreshFailureKind as FailureKind,
    EmbeddingRefreshObservation as Observation, EmbeddingRefreshPhase as Phase,
    EmbeddingRefreshTicket as Ticket, ProductText,
};

fn ticket() -> Ticket {
    Ticket {
        sequence: std::num::NonZeroU64::new(7).expect("sequence"),
        owner_epoch: 19,
        owner_fence: [0x83; 32],
        configuration: [0x27; 32],
    }
}

#[test]
fn refresh_registry_requires_explicit_mcp_action_and_discovery_never_dispatches() {
    let mut server = ready(Fake::default());
    let listed = request(&mut server, "tools/list", &json!({}));
    assert!(
        listed["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .all(|tool| tool["name"] != "backend.embedding_refresh")
    );
    let catalog = super::super::tools::catalog_markdown();
    let entry = catalog
        .split("- `backend.embedding_refresh` — ")
        .nth(1)
        .expect("registered catalog route")
        .split("\n- `")
        .next()
        .expect("entry");
    let schema: Value = serde_json::from_str(
        entry
            .split("```json\n")
            .nth(1)
            .expect("actual schema")
            .split("\n```")
            .next()
            .expect("schema end"),
    )
    .expect("catalog JSON");
    assert_eq!(schema["required"], json!(["request"]));
    assert_eq!(schema["properties"]["request"]["type"], "object");
    assert!(
        server.product.surface_commands.is_empty(),
        "initialization and catalog discovery are passive"
    );
    for arguments in [
        json!({}),
        json!({"request": "ambient config"}),
        json!({"request": {"action":"start", "configuration":ticket().configuration, "timeout_seconds":0}}),
    ] {
        let response = request(
            &mut server,
            "tools/call",
            &json!({
                "name":"backend.embedding_refresh", "arguments":arguments
            }),
        );
        assert_eq!(response["id"], 9);
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(response["result"]["structuredContent"]["slug"], "usage");
        assert!(
            server.product.surface_commands.is_empty(),
            "invalid action must not start an owner transition"
        );
    }
}

#[test]
fn refresh_jsonrpc_keeps_exact_ticket_failure_and_copied_observation_in_all_details() {
    let ticket = ticket();
    let diagnostic = "actual helper identity changed after capture; compiler is retired";
    let pending = Observation::Pending {
        ticket: ticket.clone(),
        phase: Phase::Activating,
    };
    let failed = Observation::Failed {
        ticket: ticket.clone(),
        cause: Failure {
            kind: FailureKind::Activation,
            detail: ProductText::from_static(diagnostic),
        },
        compiler: Some(Compiler::Retired {
            configuration: ticket.configuration,
        }),
    };
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        for action in [
            Action::Start {
                configuration: ticket.configuration,
                timeout_seconds: 30,
            },
            Action::Observe {
                ticket: ticket.clone(),
            },
            Action::Cancel {
                ticket: ticket.clone(),
            },
        ] {
            let mut server = ready(Fake {
                surface_reply: Some(SurfaceReply::EmbeddingRefresh(pending.clone())),
                ..Fake::default()
            });
            let response = request(
                &mut server,
                "tools/call",
                &json!({
                    "name":"backend.embedding_refresh", "arguments":{"request":action,"detail":detail.name()}
                }),
            );
            assert_context_bounded(&response);
            assert_eq!(response["id"], 9);
            let result = &response["result"];
            assert_eq!(result["isError"], false, "{response}");
            assert_eq!(
                result["structuredContent"]["embedding_refresh"],
                serde_json::to_value(&pending).expect("pending")
            );
            assert_eq!(
                server.product.surface_commands,
                vec![SurfaceCommand::EmbeddingRefresh { request: action }]
            );
            let note = result["structuredContent"]["note"]
                .as_str()
                .expect("usable observe note");
            let arguments: Value = serde_json::from_str(
                note.strip_prefix("Observe this exact operation with backend.embedding_refresh: ")
                    .expect("registered tool guidance"),
            )
            .expect("literal observation arguments");
            server.product.surface_reply = Some(SurfaceReply::EmbeddingRefresh(failed.clone()));
            let observed = request(
                &mut server,
                "tools/call",
                &json!({
                    "name":"backend.embedding_refresh", "arguments":arguments
                }),
            );
            assert_context_bounded(&observed);
            assert_eq!(observed["id"], 9);
            assert_eq!(observed["result"]["isError"], true, "{observed}");
            assert_eq!(
                observed["result"]["structuredContent"]["embedding_refresh"],
                serde_json::to_value(&failed).expect("failed")
            );
            assert_eq!(
                observed["result"]["structuredContent"]["fault"]["detail"],
                diagnostic
            );
            assert!(
                observed["result"]["content"][0]["text"]
                    .as_str()
                    .expect("Markdown")
                    .contains(diagnostic)
            );
            assert_eq!(
                server.product.surface_commands[1],
                SurfaceCommand::EmbeddingRefresh {
                    request: Action::Observe {
                        ticket: ticket.clone()
                    }
                }
            );
        }
    }
}

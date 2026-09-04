//! Exercises MCP lifecycle and registry injection without duplicating application command DTOs.

use interface_core::{ApplicationInput, CorrelationId, InputText};
use interface_protocol::{McpDecode, McpRequest, decode_mcp};

fn text(value: &str) -> InputText {
    match InputText::try_from_str(value) {
        Ok(value) => value,
        Err(error) => panic!(
            "test literal unexpectedly exceeds {}: {}",
            error.maximum, error.actual
        ),
    }
}

#[test]
fn lifecycle_requests_preserve_request_vs_notification_identity() {
    let initialize = decode_mcp(
        br#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    );
    assert!(matches!(
        initialize,
        McpDecode::Accepted(envelope)
            if envelope.id.is_some()
                && matches!(envelope.request, McpRequest::Initialize(_))
    ));

    let notification = decode_mcp(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    assert!(matches!(
        notification,
        McpDecode::Accepted(envelope)
            if envelope.id.is_none() && matches!(envelope.request, McpRequest::Initialized)
    ));
}

#[test]
fn named_tool_injects_the_registry_action_into_the_shared_raw_command() {
    let decoded = decode_mcp(
        br#"{"jsonrpc":"2.0","id":"search-1","method":"tools/call","params":{"name":"search","arguments":{"correlation":9,"snapshot":"snapshot","query":"needle","limit":2}}}"#,
    );
    assert!(matches!(
        decoded,
        McpDecode::Accepted(envelope)
            if matches!(
                envelope.request,
                McpRequest::Application(ApplicationInput::Search {
                    correlation: CorrelationId(9),
                    snapshot,
                    query,
                    limit: 2,
                }) if snapshot == text("snapshot") && query == text("needle")
            )
    ));
}

#[test]
fn registry_action_cannot_be_overridden_by_tool_arguments() {
    let decoded = decode_mcp(
        br#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"health","arguments":{"action":"search","correlation":8}}}"#,
    );
    assert!(
        matches!(decoded, McpDecode::Rejected(error) if error.code == interface_protocol::AdapterErrorCode::TooManyFields)
    );
}

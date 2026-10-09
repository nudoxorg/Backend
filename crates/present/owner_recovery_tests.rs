//! Owner recovery preserves CLI scope and never turns connection flags into MCP operands.

use crate::*;

fn recovery() -> (Fault, [String; 3]) {
    let paths = [
        "/tmp/project é ' $(printf BAD)".to_owned(),
        "/tmp/state `printf BAD`; literal".to_owned(),
        "/tmp/socket\nsecond \"line\"".to_owned(),
    ];
    let context = OwnerContext::new(paths[0].clone(), paths[1].clone(), paths[2].clone());
    (
        Fault::new(
            FaultSlug::Endpoint,
            Operand::Path(paths[2].clone()),
            Cause::new(CauseSlug::Unreachable, "the selected local owner is absent"),
            Affordance::RecoverOwner { context },
        ),
        paths,
    )
}

#[test]
fn explicit_owner_recovery_encodes_only_the_exact_shell_context() {
    let (fault, paths) = recovery();
    let shell = fault.affordance().shell().expect("selected CLI recovery");
    assert_eq!(
        shell,
        format!(
            "nudox --project {} --workspace {} --endpoint {} health",
            crate::shell::quote_argument(&paths[0]),
            crate::shell::quote_argument(&paths[1]),
            crate::shell::quote_argument(&paths[2]),
        )
    );
    assert_eq!(fault.affordance().tool_call(), None);
    let encoded = encode_serializable(
        "fault",
        Detail::Full,
        None,
        FaultDto::new(&fault),
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .expect("actual typed fault encoding");
    let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("encoded JSON");
    assert_eq!(value["shell"], shell);
    assert!(
        value.get("call").is_none(),
        "MCP cannot rebind this CLI context"
    );
    assert_eq!(value["slug"], "endpoint");
    assert_eq!(value["operand"], paths[2]);
    assert_eq!(value["cause"], "unreachable");
    assert_eq!(value["detail"], fault.cause().sentence());
    assert!(text::fault(&fault, Theme::plain()).ends_with(&format!("→ {shell}")));
    let markdown = markdown::fault(&fault);
    assert!(markdown.ends_with(&format!(
        "→ Run in a shell:\n\n    {}",
        shell.replace('\n', "\n    ")
    )));
    assert!(!markdown.contains("backend.health"));
    assert!(!markdown.contains("backend.status"));
}

#[test]
#[cfg(unix)]
fn selected_owner_recovery_survives_real_posix_shell_argv_parsing() {
    let (fault, paths) = recovery();
    let shell = fault.affordance().shell().expect("selected CLI recovery");
    let script = format!(r#"nudox() {{ printf '%s\000' "$@"; }}; {shell}"#);
    let parsed = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .output()
        .expect("actual POSIX shell");
    assert!(parsed.status.success());
    assert!(parsed.stderr.is_empty());
    let expected = [
        "--project",
        &paths[0],
        "--workspace",
        &paths[1],
        "--endpoint",
        &paths[2],
        "health",
    ];
    let mut expected_bytes = Vec::new();
    for argument in expected {
        expected_bytes.extend_from_slice(argument.as_bytes());
        expected_bytes.push(0);
    }
    assert_eq!(parsed.stdout, expected_bytes);
}

#[test]
fn bound_owner_status_is_a_real_registry_tool_with_no_scope_operands() {
    let affordance = Affordance::status();
    assert_eq!(affordance, Affordance::InspectOwner);
    let call = affordance.tool_call().expect("bound owner status");
    assert_eq!(
        call,
        serde_json::json!({"name": "backend.status", "arguments": {}})
    );
    let grammar = grammar_for_tool(call["name"].as_str().expect("tool name"))
        .expect("published MCP tool registry");
    assert_eq!(grammar.name(), "health");
    let invocation = Invocation::new(grammar);
    invocation.check().expect("zero-operand status grammar");
    assert!(matches!(
        lower(&invocation, "/selected/project").expect("actual lowering"),
        Request::Status
    ));
    assert_eq!(affordance.shell().as_deref(), Some("nudox health"));
    let fault = Fault::new(
        FaultSlug::LaneUnavailable,
        Operand::Whole,
        Cause::new(CauseSlug::Unreachable, "inspect the bound owner's status"),
        affordance,
    );
    let payload = encode_serializable(
        "fault",
        Detail::Full,
        None,
        FaultDto::new(&fault),
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .expect("actual bound status fault encoding");
    let encoded: serde_json::Value = serde_json::from_slice(&payload.bytes).expect("encoded JSON");
    assert_eq!(encoded["call"], call);
    assert!(
        markdown::fault(&fault).ends_with("→ `{\"name\":\"backend.status\",\"arguments\":{}}`")
    );
}

//! A usage refusal for an MCP call names the JSON arguments of that call.
use super::*;

#[test]
fn a_missing_stylo_name_names_the_json_arguments_not_the_cli_flags() {
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name": "backend.stylo_query", "arguments": {"package": PROJECT}}),
    );
    let fault = tool_failure(&response, "usage");
    let detail = fault["detail"].as_str().expect("detail");
    assert!(
        detail.contains("exactly one of class or custom_property"),
        "{detail}"
    );
    assert!(
        !detail.contains("--"),
        "no CLI flag in an MCP refusal: {detail}"
    );
    assert!(server.product.surface_commands.is_empty());
}

#[test]
fn both_stylo_names_are_refused_without_cli_flags() {
    // A usage refusal built for the CLI must never leak `--option` for an option
    // the called tool exposes as a JSON property.
    let mut server = ready(Fake::default());
    let response = request(
        &mut server,
        "tools/call",
        &json!({"name": "backend.stylo_query", "arguments": {"package": PROJECT, "class": "a", "custom_property": "--b"}}),
    );
    let fault = tool_failure(&response, "usage");
    let detail = fault["detail"].as_str().expect("detail");
    assert!(
        !detail.contains("--class") && !detail.contains("--custom-property"),
        "{detail}"
    );
}

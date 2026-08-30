use std::io::{self, Cursor};

use wave_application_protocol::{
    AdapterErrorCause, MAX_HEADER_LINE_BYTES, MAX_HEADER_LINES, McpDecode, decode_cli, decode_mcp,
    read_frame,
};

#[test]
fn fixed_header_bound_rejects_before_earned_body_allocation() -> io::Result<()> {
    let input = vec![b'x'; MAX_HEADER_LINE_BYTES];
    let Err(error) = read_frame(&mut Cursor::new(input)) else {
        return Err(io::Error::other("overlong header was accepted"));
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
}

#[test]
fn fixed_header_count_rejects_before_earned_body_allocation() -> io::Result<()> {
    let mut input = Vec::new();
    for _ in 0..=MAX_HEADER_LINES {
        input.extend_from_slice(b"x: y\r\n");
    }
    input.extend_from_slice(b"\r\n");
    let Err(error) = read_frame(&mut Cursor::new(input)) else {
        return Err(io::Error::other("excess headers were accepted"));
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
}

#[test]
fn parser_errors_preserve_primitive_and_json_causes() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = vec!["health".to_owned(), "not-a-number".to_owned()];
    let Err(number_error) = decode_cli(&arguments) else {
        return Err(io::Error::other("invalid correlation was accepted").into());
    };
    assert!(matches!(
        number_error.cause,
        Some(AdapterErrorCause::Number(_))
    ));

    let McpDecode::Rejected(json_error) = decode_mcp(b"{") else {
        return Err(io::Error::other("broken JSON was accepted").into());
    };
    assert!(matches!(json_error.cause, Some(AdapterErrorCause::Json(_))));
    Ok(())
}

#[test]
fn post_parse_mcp_rejections_retain_request_id() -> Result<(), Box<dyn std::error::Error>> {
    let McpDecode::Rejected(error) = decode_mcp(
        br#"{"jsonrpc":"2.0","id":91,"method":"tools/call","params":{"name":"nudox.application"}}"#,
    ) else {
        return Err(io::Error::other("malformed MCP request was accepted").into());
    };
    assert_eq!(error.id, Some(serde_json::json!(91)));
    assert_eq!(error.error.field, "arguments");
    Ok(())
}

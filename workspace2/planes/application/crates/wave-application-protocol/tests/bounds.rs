use std::io::{self, Cursor};

use wave_application_protocol::{
    AdapterErrorCause, MAX_HEADER_LINE_BYTES, MAX_HEADER_LINES, decode_cli, decode_mcp, read_frame,
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

    let Err(json_error) = decode_mcp(b"{") else {
        return Err(io::Error::other("broken JSON was accepted").into());
    };
    assert!(matches!(json_error.cause, Some(AdapterErrorCause::Json(_))));
    Ok(())
}

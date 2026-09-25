/// Central error handling for configuration files.
pub fn parse_config() -> Result<String, String> {
    Ok("configured".to_string())
}

/// Decoy that only mentions the callee name in a comment and string literal.
pub fn decoy_mention() {
    let _ = "parse_config(";
    // parse_config( is not a call
}

/// Runs the application entrypoint.
pub fn run_app() -> Result<String, String> {
    let _ = journey_helper::helper_value();
    parse_config()
}

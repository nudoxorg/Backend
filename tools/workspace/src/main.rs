//! Command-line entry point for v2 workspace and repository law validation.

use std::io::{self, Read};
use std::process::ExitCode;

fn main() -> ExitCode {
    let allow_incomplete = std::env::args().any(|argument| argument == "--allow-incomplete");
    let metadata_only = std::env::args().any(|argument| argument == "--metadata-only");
    let mut input = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut input) {
        eprintln!("failed to read Cargo metadata: {error}");
        return ExitCode::FAILURE;
    }
    let result = if metadata_only {
        backend_workspace::validate_json(&input, !allow_incomplete)
    } else {
        backend_workspace::validate_workspace_json(&input, !allow_incomplete)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(violations) => {
            for violation in violations {
                eprintln!("{violation}");
            }
            ExitCode::FAILURE
        }
    }
}

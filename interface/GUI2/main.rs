//! Runs the `nudox-gui` executable: the rewritten desktop surface of the Nudox library.
//! Process setup is kept here while product policy remains in library crates.
//! Every external failure crosses this boundary as a structured diagnostic.

use std::process::ExitCode;

fn main() -> ExitCode {
    match nudox_gui2::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("Nudox could not open: {refusal}");
            ExitCode::FAILURE
        }
    }
}

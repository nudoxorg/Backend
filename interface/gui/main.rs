//! Runs the `interface-gui` executable, which exists to read the one shared local library through a desktop reader.
//! Process setup is kept here while product policy remains in library crates.
//! Every external failure crosses this boundary as a structured diagnostic.
//! The `nudox-gui` process: the resident surface that holds the compiler host for the CLI and MCP.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    match interface_gui::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("Nudox could not open: {refusal}");
            ExitCode::FAILURE
        }
    }
}

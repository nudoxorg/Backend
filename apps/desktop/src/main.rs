//! Launchable local-first desktop host.

// A release build is a Windows GUI program: launching it opens the window and
// no console beside it. Debug builds stay console programs so `cargo run`
// keeps the owner's and the shell's diagnostics in the terminal. Either way a
// redirected stderr (`backend-desktop.exe 2> log.txt`) receives them, since a
// GUI program still inherits the handles its launcher passes.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    backend_desktop::main_entry()
}

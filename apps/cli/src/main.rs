//! Thin process wrapper around the importable CLI target.

fn main() -> std::process::ExitCode {
    backend_cli::main_entry()
}

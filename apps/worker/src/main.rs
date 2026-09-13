//! Standalone worker process entrypoint.

fn main() -> std::process::ExitCode {
    backend_worker::main_entry()
}

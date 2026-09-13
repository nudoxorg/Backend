//! Journey-owned worker process wrapper.

fn main() -> std::process::ExitCode {
    backend_worker::main_entry()
}

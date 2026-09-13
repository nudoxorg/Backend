//! Journey-owned local daemon process wrapper.

fn main() -> std::process::ExitCode {
    backend_locald::main_entry()
}

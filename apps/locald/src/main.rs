//! Standalone locald process entrypoint.
//!
//! The default compiled profile opens the durable workspace owner and serves
//! the bounded Unix endpoint. Additional product compositions can be exposed
//! through an explicit profile in the process configuration.

mod stack_profile;

fn main() -> std::process::ExitCode {
    stack_profile::capture_stderr();
    stack_profile::start();
    backend_local_service::main_entry()
}

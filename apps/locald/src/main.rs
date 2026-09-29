//! Standalone locald process entrypoint.
//!
//! The default compiled profile opens the durable workspace owner and serves
//! the bounded Unix endpoint. Additional product compositions can be exposed
//! through an explicit profile in the process configuration.

/// Windows gives the main thread a 1 MiB stack by default (Unix typically
/// gives 8 MiB); rust-analyzer's semantic authority recurses deeply enough
/// during real package compilation to overflow that, crashing the whole
/// process. Run the actual entry point on a thread sized to match what this
/// workload already relies on elsewhere.
const MAIN_STACK_BYTES: usize = 16 * 1024 * 1024;

fn main() -> std::process::ExitCode {
    std::thread::Builder::new()
        .stack_size(MAIN_STACK_BYTES)
        .spawn(backend_local_service::main_entry)
        .expect("spawn main-entry thread")
        .join()
        .expect("main-entry thread panicked")
}

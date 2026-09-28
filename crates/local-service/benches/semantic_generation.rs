//! Release measurement for one resident semantic generation.
#![allow(clippy::expect_used, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_semantic_generation();
}

//! Release measurement for a snapshot digest and a proved overlay miss.
#![allow(clippy::expect_used, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_semantic_snapshot_residence();
}

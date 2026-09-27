//! Release measurement for admitting a resident semantic image batch.
#![allow(clippy::expect_used, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_semantic_admission();
}

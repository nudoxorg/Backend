//! Release measurement for one semantic-query image validation.
#![allow(clippy::expect_used, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_semantic_query_walk();
}

//! Release measurement for resident semantic-image rows.
#![allow(clippy::expect_used, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_semantic_image_rows();
}

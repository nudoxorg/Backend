//! Release measurement for a batch of admitted semantic images.
#![allow(clippy::expect_used, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_semantic_image_batch();
}

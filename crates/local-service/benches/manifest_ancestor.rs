//! Release measurement for one shared ancestor manifest per refresh.
#![allow(clippy::expect_used, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_manifest_ancestor();
}

//! Release measurement for resident local manifest facts.
#![allow(clippy::expect_used, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_manifest_residence();
}

//! Release measurement for package-scoped view publication.
#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_package_publication();
}

//! Release measurement for a resident typed search corpus.
#![allow(clippy::expect_used, clippy::print_stdout)]

fn main() {
    backend_local_service::builtin::measure_search_corpus();
    backend_local_service::builtin::measure_search_source_page();
    backend_local_service::builtin::measure_package_source_lookup();
}

#!/usr/bin/env nu

use std/log

# Run the full test suite: Cargo workspace tests + Buck2 compiler tests.
def main [] {
    log info "Running Cargo workspace tests..."
    cargo test --workspace

    log info "Running Buck2 compiler tests..."
    # Use run-external so nushell does not expand `...` as a path glob.
    run-external "buck2" "test" "//workspace/compiler/..."
}

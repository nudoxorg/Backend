#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "Linting with Clippy (debug)..."
    cargo clippy --workspace -- -D warnings
}

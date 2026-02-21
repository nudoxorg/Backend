#!/usr/bin/env nu
use common.nu *

def main [] {
    log "🧹" "Linting with Clippy (debug)..."
    cargo clippy --workspace -- -D warnings
}

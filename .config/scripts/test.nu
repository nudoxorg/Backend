#!/usr/bin/env nu
use common.nu *

def main [] {
    log "🧪" "Running workspace tests..."
    cargo test --workspace
}

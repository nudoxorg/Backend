#!/usr/bin/env nu
use common.nu *

def main [] {
    log "📚" "Generating documentation..."
    cargo doc --workspace --no-deps
}

#!/usr/bin/env nu
use common.nu *

def main [] {
    log "📚" "Opening documentation in browser..."
    cargo doc --workspace --no-deps --open
}

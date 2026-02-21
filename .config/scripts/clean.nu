#!/usr/bin/env nu
use common.nu *

def main [] {
    log "🧹" "Cleaning build artifacts..."
    cargo clean
}

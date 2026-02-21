#!/usr/bin/env nu
use common.nu *

def main [] {
    log "🩹" "Fixing Clippy lints..."
    cargo clippy --workspace --fix --allow-dirty --allow-staged
}

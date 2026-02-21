#!/usr/bin/env nu
use common.nu *

def main [] {
    log "🔨" "Building workspace (debug)..."
    cargo build --workspace --bin $env.MAIN_PACKAGE --target $env.RUST_TARGET
}

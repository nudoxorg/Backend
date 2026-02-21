#!/usr/bin/env nu
use common.nu *

def main [] {
    log "🔎" "Checking workspace..."
    cargo check --workspace
    cargo clippy --workspace --bin $env.MAIN_PACKAGE --target $env.RUST_TARGET
    typos
}

#!/usr/bin/env nu

use common.nu *

use std/log

def main [] {
    log info "🔨 Building workspace (debug)..."
    cargo build --workspace --bin $env.MAIN_PACKAGE --target $env.RUST_TARGET
}

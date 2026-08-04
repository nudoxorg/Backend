#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "🔎 Checking workspace..."
    cargo check --workspace
    cargo clippy --workspace --bin $env.MAIN_PACKAGE --target $env.RUST_TARGET
    typos
}

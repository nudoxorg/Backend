#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info $"🚀 Building workspace \(release\) for ($env.RUST_TARGET)…"
    cargo build --workspace --release --bin $env.MAIN_PACKAGE --target $env.RUST_TARGET
}

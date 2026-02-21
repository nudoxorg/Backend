#!/usr/bin/env nu
use common.nu *

def main [] {
    log "🚀" $"Building workspace \(release\) for ($env.RUST_TARGET)…"
    cargo build --workspace --release --bin $env.MAIN_PACKAGE --target $env.RUST_TARGET
}

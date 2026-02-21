#!/usr/bin/env nu
use common.nu *

def main [...args] {
    log "▶️" $"Running ($env.MAIN_PACKAGE) \(debug\)..."
    cargo run --bin $env.MAIN_PACKAGE -- ...$args
}

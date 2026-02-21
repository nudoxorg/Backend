#!/usr/bin/env nu
use common.nu *

def main [...args] {
    log "▶️" $"Running ($env.MAIN_PACKAGE) \(release\)..."
    cargo run --bin $env.MAIN_PACKAGE --release -- ...$args
}

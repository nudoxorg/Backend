#!/usr/bin/env nu
use common.nu *

def main [...args] {
    log "🧪" $"Running workspace tests with args: ($args)"
    cargo test --workspace -- ...$args
}

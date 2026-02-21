#!/usr/bin/env nu
use common.nu *

def main [] {
    log "💅" "Formatting Rust code..."
    cargo fmt
}

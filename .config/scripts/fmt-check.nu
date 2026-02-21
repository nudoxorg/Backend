#!/usr/bin/env nu
use common.nu *

def main [] {
    log "💅" "Checking Rust code formatting..."
    cargo fmt
}

#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "📚 Opening documentation in browser..."
    cargo doc --workspace --no-deps --open
}

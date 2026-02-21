#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "📚 Generating documentation..."
    cargo doc --workspace --no-deps
}

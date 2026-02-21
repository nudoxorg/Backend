#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "🧹 Cleaning build artifacts..."
    cargo clean
}

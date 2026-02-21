#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "Running workspace tests..."
    cargo test --workspace
}

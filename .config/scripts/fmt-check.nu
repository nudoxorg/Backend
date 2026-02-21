#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "💅 Checking Rust code formatting..."
    cargo fmt
}

#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "Fixing Clippy lints..."
    cargo clippy --workspace --fix --allow-dirty --allow-staged
}

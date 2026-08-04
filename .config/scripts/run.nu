#!/usr/bin/env nu
use common.nu *

use std/log

def main [...args] {
    log info $"Running ($env.MAIN_PACKAGE) \(debug\)..."
    cargo run --bin $env.MAIN_PACKAGE -- ...$args
}

#!/usr/bin/env nu
use common.nu *

use std/log

def main [...args] {
    log info $"Running workspace tests with args: ($args)"
    cargo test --workspace -- ...$args
}

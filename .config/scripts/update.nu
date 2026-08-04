#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "Updating dependencies..."
    cargo update
}

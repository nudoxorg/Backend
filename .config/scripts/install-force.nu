#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    nu .config/scripts/build-release.nu
    
    log info $"Force installing ($env.MAIN_PACKAGE) binary..."
    cargo install --bin $env.MAIN_PACKAGE --force
}

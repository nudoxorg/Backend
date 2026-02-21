#!/usr/bin/env nu
use common.nu *

def main [] {
    nu .config/scripts/build-release.nu
    
    log "💾" $"Force installing ($env.MAIN_PACKAGE) binary..."
    cargo install --bin $env.MAIN_PACKAGE --force
}

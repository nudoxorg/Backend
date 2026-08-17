#!/usr/bin/env nu

use std/log

def main [] {
    log info "Generating rust-project.json for rust-analyzer..."
    bash nix/build/gen-rust-project.sh
}

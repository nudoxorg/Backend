#!/usr/bin/env nu
use common.nu *

def main [] {
    log "🔄" "Updating dependencies..."
    cargo update
}

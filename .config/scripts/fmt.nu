#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "💅 Formatting"

    let nix_files = (glob **/*.nix)
    let md_files = (glob **/*.md)
    log debug $"Found ($nix_files | length) Nix files and ($md_files | length) Markdown files"
    
    let main_id = (job id)

    log debug "Spawning cargo fmt"
    job spawn { cargo fmt; null | job send $main_id }
    log debug "Spawning nixfmt"
    job spawn { nixfmt ...$nix_files; null | job send $main_id }
    log debug "Spawning tombi"
    job spawn { tombi format; null | job send $main_id }
    log debug "Spawning hongdown"
    job spawn { hongdown --write ...$md_files; null | job send $main_id }

    log debug "Waiting for jobs to complete..."
    job recv
    job recv
    job recv
    job recv
    log debug "All formatting jobs completed"
}

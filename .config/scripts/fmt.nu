#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    log info "💅 Formatting"

    let nix_files = (glob **/*.nix)
    let md_files = (glob **/*.md)
    let main_id = (job id)

    job spawn { cargo fmt; null | job send $main_id }
    job spawn { nixfmt ...$nix_files; null | job send $main_id }
    job spawn { tombi format; null | job send $main_id }
    job spawn { hongdown --write ...$md_files; null | job send $main_id }

    job recv
    job recv
    job recv
    job recv
}

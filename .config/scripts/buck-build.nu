#!/usr/bin/env nu

use std/log

# Build Buck2 targets.
# With no argument builds the entire workspace (//...).
# Pass a package name (e.g. "compiler", "util/caching") to scope to that subtree.
def main [
    package?: string  # Package path under workspace/ — omit for all targets
    --release (-r)    # Build with @mode/release configuration
] {
    let target = if ($package == null) {
        "//..."
    } else {
        $"//workspace/($package)/..."
    }

    let mode_flag = if $release { ["@mode/release"] } else { [] }

    log info $"buck2 build ($target)"
    run-external "buck2" "build" ...$mode_flag $target
}

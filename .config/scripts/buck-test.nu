#!/usr/bin/env nu

use std/log

# Run Buck2 tests.
# With no argument tests the entire workspace (//...).
# Pass a package name (e.g. "compiler", "util/caching") to scope to that subtree.
def main [
    package?: string    # Package path under workspace/ — omit for all targets
    --filter (-f): string  # Test name filter passed to the test runner
] {
    let target = if ($package == null) {
        "//..."
    } else {
        $"//workspace/($package)/..."
    }

    log info $"buck2 test ($target)"

    if ($filter == null) {
        run-external "buck2" "test" $target
    } else {
        run-external "buck2" "test" $target "--" $filter
    }
}

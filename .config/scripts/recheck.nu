#!/usr/bin/env nu

# Cheap post-agent-wave verification. This intentionally does not run the full
# test matrix; use full-check.nu for that.

def run-required [label: string, command: closure] {
    print $"==> ($label)"
    let started = (date now)
    try {
        do $command
    } catch {|err|
        print --stderr $"FAIL: ($label): ($err.msg? | default ($err | to nuon))"
        exit ($err.exit_code? | default 1 | into int)
    }
    let elapsed = ((date now) - $started | into int) / 1_000_000_000
    print $"PASS: ($label) \(($elapsed)s\)"
}

def main [] {
    let total_started = (date now)

    run-required "git whitespace check" {
        ^git diff --check
    }
    run-required "root Cargo metadata" {
        ^cargo metadata --no-deps --format-version 1 --locked | ignore
    }
    if ("workspace/gui/Cargo.toml" | path exists) {
        run-required "GUI Cargo metadata" {
            ^cargo metadata --manifest-path workspace/gui/Cargo.toml --no-deps --format-version 1
                | ignore
        }
    } else {
        print "SKIP: GUI Cargo metadata (manifest not present)"
    }
    run-required "Nushell full-check parse" {
        ^nu -c 'source .config/scripts/full-check.nu'
    }
    run-required "Nushell check parse" {
        ^nu -c 'source .config/scripts/check.nu'
    }

    let total_elapsed = ((date now) - $total_started | into int) / 1_000_000_000
    print $"Recheck completed in ($total_elapsed)s"
}

#!/usr/bin/env nu

# check-ignore-reasons.nu — fail if any #[ignore] attribute has no reason.
#
# nextest does not, and cannot, gate on this: `#[ignore]` vs
# `#[ignore = "..."]` is rustc/libtest attribute metadata that nextest never
# inspects (it only sees "ignored: yes/no" via `cargo test --list`'s
# terse/json output, not the reason string). There is no key in
# `.config/nextest.toml` that makes `-P ci` itself reject a bare `#[ignore]`;
# this script IS that check, run as a gate before `-P ci` by
# `nextest-suite.nu` — see that script.
#
# A bare `#[ignore]` is a silent skip with no record of why it is safe to
# leave off, or how to turn it back on. Every #[ignore] in this repo's own
# source (crates/, workspace/, tests/fuzz/, tests/ — excluding vendor/ and
# result/, neither of which is code this repo owns) already carries a
# reason as of 2026-08-05; this script exists to keep that true as the repo
# grows, not because a violation is currently expected.
#
# Usage: nu .config/scripts/check-ignore-reasons.nu [--root <path>]

def main [
    --root: string = "."   # repo root to scan
] {
    let dirs = [
        ($root | path join "crates")
        ($root | path join "workspace")
        ($root | path join "fuzz")
        ($root | path join "tests")
    ] | where { |d| $d | path exists }

    if ($dirs | is-empty) {
        print --stderr "FAIL: none of crates/, workspace/, tests/fuzz/, tests/ exist under --root"
        exit 1
    }

    # A bare `#[ignore]` attribute: the whole line, trimmed, is exactly
    # `#[ignore]` — no `= "..."` reason. This deliberately does not match
    # `#[ignore = "..."]`, `#[ignore(...)]` (not a real attribute form but
    # harmless to allow through), or prose that merely *mentions*
    # `` `#[ignore]`d `` in a doc comment (those never match the anchored
    # `^\s*#\[ignore\]\s*$` pattern below, because they have trailing text).
    let bare = ($dirs
        | each { |d|
            let result = (^grep -rnE '^\s*#\[ignore\]\s*$' $d --include=*.rs | complete)
            $result.stdout | lines
        }
        | flatten
        | where { |l| not ($l | str contains "/vendor/") and not ($l | str contains "/result/") }
    )

    if ($bare | is-empty) {
        print "PASS: every #[ignore] under crates/, workspace/, tests/fuzz/, tests/ carries a reason"
        exit 0
    }

    print --stderr "FAIL: #[ignore] without a reason string (add `= \"why, and how to run it on purpose\"`):"
    for l in $bare {
        print --stderr $"  ($l)"
    }
    exit 1
}

# Standalone cross-compile lane runner, invoked as `nu .config/ci/cross-check.nu`
# inside the lean `.#cross` dev shell. Independent of the `backend` command so a
# compile-only lane never realizes the corpus, native compilers, or GUI closure.

# Splits the space-separated target list the cross shell exports into triples.
def split-targets [raw: string]: nothing -> list<string> {
    $raw
    | split row " "
    | each {|entry| $entry | str trim }
    | where {|entry| not ($entry | is-empty) }
}

# Compile-checks the whole workspace and all its targets for each configured
# cross target, running nothing. Proves Windows and aarch64 Linux still build
# from the CI host without a native runner. Exits non-zero if any target fails.
def main [
    --target: string # limit the run to one triple from NUDOX_CROSS_CHECK_TARGETS
    --keep-artifacts # keep each target tree instead of reclaiming its disk
]: nothing -> nothing {
    let configured = (split-targets ($env.NUDOX_CROSS_CHECK_TARGETS? | default ""))
    if ($configured | is-empty) {
        error make {msg: "NUDOX_CROSS_CHECK_TARGETS is empty; run inside the `.#cross` dev shell"}
    }
    let targets = if ($target | is-empty) {
        $configured
    } else if ($target in $configured) {
        [$target]
    } else {
        error make {msg: $"($target) is not one of: ($configured | str join ', ')"}
    }
    let target_root = ($env.CARGO_TARGET_DIR? | default ".local/target")
    let results = $targets | each {|triple|
        print $"cross: cargo check --locked --workspace --all-targets --target ($triple)"
        # Stream Cargo's own progress and diagnostics straight through.
        try {
            run-external "cargo" "check" "--locked" "--workspace" "--all-targets" "--target" $triple
        } catch {
            # LAST_EXIT_CODE keeps the exact status; the summary reports it.
        }
        let status = ($env.LAST_EXIT_CODE? | default 1)
        # A foreign target tree can reach tens of gigabytes; the shared CI disk
        # runs close to full, so reclaim it between targets unless asked not to.
        if not $keep_artifacts {
            let tree = ($target_root | path join $triple)
            if ($tree | path exists) { rm --recursive --force $tree }
        }
        {target: $triple, status: (if $status == 0 { "passed" } else { "failed" })}
    }
    print ($results | table --expand)
    let failed = ($results | where status == "failed" | get target)
    if not ($failed | is-empty) {
        error make {msg: $"cross compile check failed for: ($failed | str join ', ')"}
    }
}

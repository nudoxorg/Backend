# T1 smoke prototype: "it builds, and every surface comes alive". The
# warm-cache budget is under a minute; this runner records timings first.
# Invoked as `nu .config/ci/t1.nu` inside the lean
# `.#smoke` dev shell; run the same command locally to get the same verdict.
#
# It builds the product binaries (CLI, MCP, desktop and their locald owner) for the host
# target: no --all-targets, so none of the ~330 test binaries are linked, and
# no foreign targets. Then .config/ci/smoke.py starts each one: CLI --help,
# MCP initialize + tools/list on stdio, and the desktop drawing a first frame
# under Xvfb with Mesa's software Vulkan. Correctness is T2's job.
#
# Speed comes from a persistent build graph and target dir (CI_T1_CACHE_ROOT,
# on the CI disk) plus restored source mtimes, so only changed crates and
# their dependents rebuild. It has a separate root from the fast-lane cache.
# The managed Cargo wrapper leases the workspace build slot and target graph
# and selects incremental/compiler-cache defaults.

use lib.nu *

const SURFACES = [
    {package: "backend-cli", bin: "backend-cli"}
    {package: "backend-mcp", bin: "backend-mcp"}
    {package: "backend-desktop", bin: "backend-desktop"}
    # Not smoked here; published so T2 (.config/ci/surface.py) drives the
    # exact owner the CLI and MCP find beside them.
    {package: "backend-locald", bin: "backend-locald"}
]

def main []: nothing -> nothing {
    let started = (date now)
    let cache = $env.CI_T1_CACHE_ROOT? | default ""
    let target_dir = if ($cache | is-not-empty) {
        $cache | path join "t1-target"
    } else {
        $env.CARGO_TARGET_DIR? | default ($env.PWD | path join ".local/target")
    }
    let fast_cache = $env.CI_FAST_CACHE_ROOT? | default ""
    if ($fast_cache | is-not-empty) {
        # Check before creating or touching any T1 cache paths: the fast lane
        # may evict only its own subtree while T1 runs.
        run-bounded 5sec "python3" ($env.FILE_PWD | path join "t1_capacity.py") $target_dir "--fast-cache-root" $fast_cache "--t1-cache-root" $cache "--separation-only"
    }
    if ($cache | is-not-empty) {
        mkdir $cache
        $env.CARGO_TARGET_DIR = $target_dir
        $env.CARGO_HOME = ($cache | path join "cargo-home")
        mkdir $env.CARGO_HOME
        # The mtime manifest trusts git blobs. On a dirty tree restoring
        # those times could hide changed source from Cargo.
        let clean = (^git diff --quiet HEAD | complete)
        if $clean.exit_code != 0 {
            error make {msg: "cached T1 needs a clean tracked checkout; unset CI_T1_CACHE_ROOT for local edits"}
        }
        # A fresh checkout stamps every file with "now", which would make
        # Cargo rebuild the whole workspace. Restore the mtimes of unchanged
        # files so the incremental state above stays valid.
        ^python3 ($env.FILE_PWD | path join "restore-mtimes.py") ($cache | path join "t1-source-mtimes.json")
        if $env.LAST_EXIT_CODE != 0 { error make {msg: "source mtime restoration failed"} }
    }
    if ($cache | is-empty) { $env.CARGO_TARGET_DIR = $target_dir }
    print $"CI-TIMING lane=t1 step=cache-preparation seconds=(((date now) - $started) / 1sec | math round)"
    let bin_dir = $target_dir | path join "debug"
    let cargo = $env.BACKEND_PARALLEL_CARGO? | default "cargo"

    let capacity = ci-step "t1" "disk and memory preflight" {||
        run-bounded 10sec "python3" ($env.FILE_PWD | path join "t1_capacity.py") $target_dir "--fast-cache-root" $fast_cache "--t1-cache-root" $cache
    }

    let build_args = $SURFACES | each {|surface| ["--package" $surface.package "--bin" $surface.bin] } | flatten
    let built = if $capacity {
        ci-step "t1" "build cli, mcp, desktop, locald" {||
            # Fix the job count regardless of ambient shell settings. Four is
            # the managed wrapper's per-process bound; CI can lower it with
            # CI_T1_CARGO_JOBS. Two left a cold build at ~23 min on a 20-thread
            # host (pr-t1 615449: 1,128 crates in 1339s).
            let jobs = $env.CI_T1_CARGO_JOBS? | default "4"
            if not ($jobs =~ '^[1-4]$') {
                error make {msg: $"CI_T1_CARGO_JOBS must be 1 through 4, got ($jobs)"}
            }
            # Cached CI must explain an unexpected rebuild. This does not
            # change compiler flags or freshness decisions; Cargo emits the
            # first invalidating dependency instead of a bare Compiling line.
            let fingerprint_log = ($env.CARGO_LOG? | default (
                if ($cache | is-not-empty) { "cargo::core::compiler::fingerprint=info" } else { "" }
            ))
            print $"T1 cargo jobs: ($jobs)"
            with-env {CARGO_BUILD_JOBS: $jobs, CARGO_LOG: $fingerprint_log} {
                run-bounded 30min $cargo "build" "--locked" "--jobs" $jobs ...$build_args
            }
        }
    } else { false }
    let smoke = if $built {
        ["cli" "mcp" "gui"] | each {|surface|
            ci-step "t1" $"smoke ($surface)" {||
                run-bounded 3min "python3" ($env.FILE_PWD | path join "smoke.py") $surface "--bin-dir" $bin_dir
            }
        }
    } else {
        print "== t1: build was not admitted or failed; skipping smoke checks =="
        []
    }
    let artifact_root = $env.CI_T1_ARTIFACT_ROOT? | default ""
    # Publish successful builds even when a smoke is red. Artifact presence
    # enables T2 diagnosis; the smoke results remain part of finish-lane below.
    let published = if $built and ($artifact_root | is-not-empty) {
        ci-step "t1" "publish exact binaries and runtime" {||
            let sha = (^git rev-parse HEAD | str trim)
            run-bounded 10min "python3" ($env.FILE_PWD | path join "t1_artifacts.py") "publish" "--bin-dir" $bin_dir "--output" ($artifact_root | path join $sha)
        }
    } else { true }
    # T2 is a separate verdict. Run `test.py integration --bin-dir ...`
    # locally, or let the artifact consumer run it in CI.
    if ($cache | is-not-empty) {
        print $"CI-CACHE t1: (^du -sh $target_dir | str trim)"
    }
    let elapsed = (date now) - $started
    print $"CI-TIMING lane=t1 step=total seconds=($elapsed / 1sec | math round)"
    let results = if $capacity {
        [$capacity $built $published] | append $smoke
    } else { [$capacity] }
    finish-lane "t1" $results
}

# T1 smoke prototype: "it builds, and every surface comes alive". The
# warm-cache budget is under a minute; this runner records timings first.
# Invoked as `nu .config/ci/t1.nu` inside the lean
# `.#smoke` dev shell; run the same command locally to get the same verdict.
#
# It builds only the three product binaries (CLI, MCP, desktop) for the host
# target: no --all-targets, so none of the ~330 test binaries are linked, and
# no foreign targets. Then .config/ci/smoke.py starts each one: CLI --help,
# MCP initialize + tools/list on stdio, and the desktop drawing a first frame
# under Xvfb with Mesa's software Vulkan. Correctness is T2's job.
#
# Speed comes from one persistent incremental target dir (CI_FAST_CACHE_ROOT,
# on the CI disk) plus restored source mtimes, so only changed crates and
# their dependents rebuild. See docs/ci-rework/t1-smoke-design.md in the
# NuDox workspace.

use lib.nu *

const SURFACES = [
    {package: "backend-cli", bin: "backend-cli"}
    {package: "backend-mcp", bin: "backend-mcp"}
    {package: "backend-desktop", bin: "backend-desktop"}
]

def main []: nothing -> nothing {
    let started = (date now)
    let cache = $env.CI_FAST_CACHE_ROOT? | default ""
    if ($cache | is-not-empty) {
        mkdir $cache
        $env.CARGO_TARGET_DIR = ($cache | path join "t1-target")
        $env.CARGO_HOME = ($cache | path join "cargo-home")
        mkdir $env.CARGO_HOME
        # A fresh checkout stamps every file with "now", which would make
        # Cargo rebuild the whole workspace. Restore the mtimes of unchanged
        # files so the incremental state above stays valid.
        ^python3 ($env.FILE_PWD | path join "restore-mtimes.py") ($cache | path join "t1-source-mtimes.json")
        if $env.LAST_EXIT_CODE != 0 { error make {msg: "source mtime restoration failed"} }
    }
    let target_dir = $env.CARGO_TARGET_DIR? | default ($env.PWD | path join ".local/target")
    let bin_dir = $target_dir | path join "debug"

    let build_args = $SURFACES | each {|surface| ["--package" $surface.package "--bin" $surface.bin] } | flatten
    let built = ci-step "t1" "build cli, mcp, desktop" {||
        run-bounded 30min "cargo" "build" "--locked" ...$build_args
    }
    let smoke = if $built {
        ["cli" "mcp" "gui"] | each {|surface|
            ci-step "t1" $"smoke ($surface)" {||
                run-bounded 3min "python3" ($env.FILE_PWD | path join "smoke.py") $surface "--bin-dir" $bin_dir
            }
        }
    } else {
        print "== t1: build failed; skipping smoke checks =="
        []
    }
    if ($cache | is-not-empty) {
        print $"CI-CACHE t1: (^du -sh $target_dir | str trim)"
    }
    let elapsed = (date now) - $started
    print $"CI-TIMING lane=t1 step=total seconds=($elapsed / 1sec | math round)"
    finish-lane "t1" ([$built] | append $smoke)
}

# T3 bootstrap: retained caller lifetime/failure laws, outside the PR path.
# This deliberately does not claim the full release/platform/corpus closure.
use lib.nu *

def main []: nothing -> nothing {
    let cargo = $env.BACKEND_PARALLEL_CARGO? | default "cargo"
    let cache = $env.CI_T3_CACHE_ROOT? | default ""
    if ($cache | is-not-empty) {
        let clean = (^git diff --quiet HEAD | complete)
        if $clean.exit_code != 0 { error make {msg: "cached deep checks need a clean tracked checkout"} }
        run-bounded 10sec "python3" ($env.FILE_PWD | path join "restore-mtimes.py") ($cache | path join "source-mtimes.json")
    }
    let target = $env.CARGO_TARGET_DIR? | default ($env.PWD | path join ".local/target")
    let capacity = ci-step "t3" "disk and memory preflight" {||
        run-bounded 10sec "python3" ($env.FILE_PWD | path join "t1_capacity.py") $target
    }
    let laws = if $capacity {
        ci-step "t3" "CLI saved-operation wait, failure and disconnect laws" {||
            with-env {CARGO_BUILD_JOBS: "2"} {
                run-bounded 30min $cargo "nextest" "run" "--locked" "--package" "backend-cli" "--lib" "--build-jobs" "2" "--test-threads" "2" "--no-tests=fail" "-E" "test(index_wait::)"
            }
        }
    } else { false }
    finish-lane "t3" [$capacity $laws]
}

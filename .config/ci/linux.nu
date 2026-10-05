# Linux CI lane runner, invoked as `nu .config/ci/linux.nu` inside the
# `.#compiler` dev shell. The lane is the whole native x86_64/aarch64 Linux
# gate: the root flake's structural checks, then `backend test pr` (every
# workspace test except the deep-accuracy suites and the PR quarantine).
#
# MachineConfigurations only schedules this file; what the lane runs lives
# here so a Backend PR can change it.

use lib.nu *

# Prints the process output the CLI captured for a failed step. The CLI keeps
# a child's stdout/stderr out of the log (process-require writes them beside a
# manifest instead), and the CI worktree is deleted with the container, so
# this is the only chance to show why a step failed.
def print-captured-failures []: nothing -> nothing {
    let captured = (glob ".local/process-failures/*/std*.txt")
    if ($captured | is-empty) {
        print "no .local/process-failures/*: the failure was before the CLI, so the text above is all of it"
        return
    }
    for file in $captured {
        print $"---- ($file) ----"
        # Bounded: a full cargo failure can run to tens of MB and Concourse
        # stores every log line. Keep the verdict summary plus a large tail.
        let output = open --raw $file | lines
        let verdicts = ($output | where {|line|
            ($line | str contains 'FAIL [') or ($line | str contains 'TIMEOUT [') or ($line | str contains 'tests were not run')
        })
        if not ($verdicts | is-empty) {
            print '== nextest failure summary =='
            $verdicts | last 100 | str join (char nl) | print
        }
        # Each failing test's own output comes long before the tail, and nextest
        # prints it twice (as the test fails, then in its final summary). Key
        # every stderr block by the FAIL/TIMEOUT line above it and keep one per
        # test, so every failure gets its reason, not just the first few dozen.
        let headers = (
            $output
            | enumerate
            | where {|row| $row.item =~ '^\s+(FAIL|TIMEOUT) \[' }
        )
        let blocks = (
            $output
            | enumerate
            | where {|row| $row.item | str contains 'stderr ───' }
            | each {|row|
                let above = $headers | where index < $row.index
                if ($above | is-empty) {
                    null
                } else {
                    let header = $above | last
                    {
                        key: ($header.item | str replace --regex '^\s+(FAIL|TIMEOUT) \[[^\]]*\]\s+\(\s*\d+/\d+\)\s+' '')
                        header: $header.item
                        start: $row.index
                    }
                }
            }
            | compact
            | uniq-by key
        )
        if not ($blocks | is-empty) {
            print $"== ($blocks | length) failing tests' stderr \(first 300, 20 lines each\) =="
            for block in ($blocks | first 300) {
                print $block.header
                $output | skip $block.start | first 20 | str join (char nl) | print
                print '--'
            }
        }
        # A `cargo build`/`check` failure (exit 101, no nextest FAIL/panic
        # lines) prints its rustc errors long before the tail too, often
        # buried under hundreds of trailing warnings from unrelated crates
        # that still compiled: pull every error the same way, or the tail
        # alone can show nothing but warnings for a build that never passed.
        let compile_errors = ($output | enumerate | where {|row|
            ($row.item | str starts-with 'error[') or ($row.item | str starts-with 'error: ') or ($row.item | str contains 'could not compile')
        } | get index)
        if not ($compile_errors | is-empty) {
            print $"== ($compile_errors | length) compile errors \(first 30, 40 lines each\) =="
            for start in ($compile_errors | first 30) {
                $output | skip ([
                    ($start - 2)
                    0
                ] | math max) | first 40 | str join (char nl) | print
                print '--'
            }
        }
        $output | last 300 | str join (char nl) | print
    }
}

# Fetches the registry releases the desktop registry and journey tests expect
# to find unpacked in the Cargo cache. Best effort: a failure here shows up
# as those tests' own failures, with their own messages.
def warm-registry-cache []: nothing -> nothing {
    # The Rust frontend's tests resolve this workspace itself offline, for
    # every platform: macOS- and Windows-only dependencies (accesskit_*)
    # included, which a Linux build never downloads. `cargo fetch` with no
    # --target fetches them all.
    run-external "cargo" "fetch" "--locked"
    for manifest in [
        "frontends/rust/fixtures/toml_pin/Cargo.toml"
        "apps/desktop/tests/fixtures/browse_tree/Cargo.toml"
    ] {
        run-external "cargo" "fetch" "--locked" "--manifest-path" $manifest
    }
    # Releases named by tests but pinned by no fixture lockfile.
    let scratch = (mktemp --directory)
    mkdir ($scratch | path join "src")
    "" | save ($scratch | path join "src" "lib.rs")
    "[package]\nname = \"nudox-registry-cache-warmup\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[dependencies]\ntoml = \"=0.5.11\"\nanyhow = \"=1.0.104\"\n\n[workspace]\n"
    | save ($scratch | path join "Cargo.toml")
    run-external "cargo" "fetch" "--manifest-path" ($scratch | path join "Cargo.toml")
    rm --recursive --force $scratch
    # The owner indexes each release as its own root under an offline
    # policy, which resolves that release's whole graph, dev-dependencies
    # included ("Full Cargo dependency and feature resolution is incomplete
    # under the offline policy"). Fetch each one's graph from a copy, so the
    # registry sources themselves are never written to.
    let releases = (
        ["frontends/rust/fixtures/toml_pin/Cargo.lock" "apps/desktop/tests/fixtures/browse_tree/Cargo.lock"]
        | each {|lock| open --raw $lock | from toml | get package | where {|package| ($package.source? | default "") starts-with "registry+" } | each {|package| $"($package.name)-($package.version)" } }
        | flatten
        | append ["toml-0.5.11" "anyhow-1.0.104" "log-0.4.34"]
        | uniq
    )
    let cargo_home = $env.CARGO_HOME? | default ($env.HOME? | default "" | path join ".cargo")
    for release in $releases {
        let matches = glob ($cargo_home | path join "registry" "src" "*" $release)
        let unpacked = if ($matches | is-empty) { null } else {
            $matches | first
        }
        if $unpacked == null { continue }
        let copy = (mktemp --directory)
        cp --recursive ($unpacked | path join "*" | into glob) $copy
        try { run-external "cargo" "fetch" "--manifest-path" ($copy | path join "Cargo.toml") } catch { print $"cache warm-up: could not fetch ($release)'s graph" }
        rm --recursive --force $copy
    }
}

def main [
    --skip-flake-check # run only the test step (the flake check is already cached on the host store)
]: nothing -> nothing {

    # The flake closes over ~1,100 pinned corpus archives; the default 1,024
    # soft descriptor limit is exhausted before any check fails.
    ulimit --file-descriptor-count --soft 65536
    provide-fhs-tools
    cap-build-cache
    restore-source-mtimes

    let flake = if $skip_flake_check {
        true
    } else {
        ci-step "linux" "root flake check" {||
            with-plain-tmp {|| run-external "nix" "flake" "check" "-L" "--keep-going" "path:." }
        }
    }

    # "private-debug" makes the CLI write the child's stdout/stderr beside its
    # manifest; the default "metadata-only" keeps just byte counts, so a
    # failure would reach CI as a bare exit status.
    # The owner keeps its state only under a parent this user alone can open
    # (`private state parent is not owned by this user with owner-only
    # access`). Tests make their scratch roots under /tmp with the default
    # 022 umask, so every daemon they start exited at once; macOS's per-user
    # TMPDIR hid this. 077 makes what the tests and their children create
    # owner-only, as the product's own state directories are.
    #
    # The owner's Rust authority also needs a Cargo home, which the dev shell
    # cannot name because it is per-user.
    let cargo_home = $env.CARGO_HOME? | default ($env.HOME? | default "" | path join ".cargo")
    #
    # No debug info for the test build. Each of the workspace's few hundred
    # test executables statically links its dependencies, so with the
    # profile's `debug = 1` one run wrote ~200 GB and filled ilo's 468 GB
    # disk (2026-10-05, build 54). Panics still name their file and line
    # (Rust embeds that apart from debug info); only RUST_BACKTRACE frames
    # lose line numbers. It also shortens every link.
    let test_env = {BACKEND_PROCESS_ARTIFACT_POLICY: "private-debug", CARGO_PROFILE_DEV_DEBUG: "0", CARGO_PROFILE_TEST_DEBUG: "0"}
    | merge (
        if ($cargo_home | path exists) { {NUDOX_CARGO_HOME: $cargo_home} } else { {} }
    )
    # Desktop registry and journey tests read real releases from this
    # machine's Cargo cache, as they do on a developer's Mac. A fresh CI
    # container has none, so fetch (download and unpack) them first: what
    # the two fixture projects pin, plus the releases tests name directly.
    ci-step "linux" "warm the Cargo cache for registry tests" {|| warm-registry-cache } | ignore
    stop-if-superseded "linux"
    let tests = (with-env $test_env {
        ci-step "linux" "backend test pr" {|| run-external "sh" "-c" "umask 077 && exec backend test pr" }
    })
    if not $tests { print-captured-failures }

    report-build-size
    reclaim-build-output
    if not ($flake and $tests) {
        error make {msg: "linux lane failed"}
    }
}

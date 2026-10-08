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
# cross target. Proves Windows and aarch64 Linux still build from the CI host.
# Then, unless `--compile-only`, runs each platform's `platform` test selection
# under emulation: Windows under Wine, aarch64 under QEMU user mode
# (`.config/ci/emulated.nu`). Exits non-zero if any check or test run fails.
def main [
    --target: string # limit the run to one triple from NUDOX_CROSS_CHECK_TARGETS
    --keep-artifacts # keep each target tree instead of reclaiming its disk
    --compile-only # skip the emulated (Wine, QEMU) test runs after the checks
]: nothing -> nothing {
    let configured = split-targets ($env.NUDOX_CROSS_CHECK_TARGETS? | default "")
    if ($configured | is-empty) {
        error make {msg: "NUDOX_CROSS_CHECK_TARGETS is empty; run inside the `.#cross` dev shell"}
    }
    let targets = if ($target | is-empty) {
        $configured
    } else if $target in $configured {
        [$target]
    } else {
        error make {msg: $"($target) is not one of: ($configured | str join ', ')"}
    }
    let target_root = $env.CARGO_TARGET_DIR? | default ".local/target"
    let concurrency = ($env.CI_COMPILE_CONCURRENCY? | default "1" | into int)
    let jobs = ($env.CI_COMPILE_JOBS? | default "4" | into int)
    if $concurrency < 1 or $concurrency > 3 or $jobs < 1 {
        error make {msg: "invalid compile concurrency or job count"}
    }
    let results = $targets | par-each --threads $concurrency {|triple|
        # Each target has its own host build-script graph as well as its
        # foreign outputs; parallel Cargo invocations must not share those.
        let target_dir = $target_root | path join $triple
        let started = date now
        print $"cross: cargo check --locked --workspace --all-targets --target ($triple) --jobs ($jobs)"
        # Stream Cargo's own progress and diagnostics straight through. The
        # verdict comes out of the try itself: an env change such as
        # LAST_EXIT_CODE made inside the block does not survive it, so
        # reading it afterwards reported every clean target as failed.
        let status = (try {
            with-env {CARGO_TARGET_DIR: $target_dir} {
                run-external "cargo" "check" "--locked" "--workspace" "--all-targets" "--target" $triple "--jobs" ($jobs | into string)
            }
            0
        } catch {
            1
        })
        # A foreign target tree can reach tens of gigabytes; the shared CI disk
        # runs close to full, so reclaim it between targets unless asked not to.
        if not $keep_artifacts {
            if ($target_dir | path exists) { rm --recursive --force $target_dir }
        }
        let seconds = ((date now) - $started) / 1sec | math round
        print $"CI-TIMING lane=fast target=($triple) seconds=($seconds) exit=($status)"
        {target: $triple, status: (if $status == 0 { "passed" } else { "failed" })}
    }
    print ($results | table --expand)
    let failed = $results | where status == "failed" | get target
    if not ($failed | is-empty) {
        error make {msg: $"cross compile check failed for: ($failed | str join ', ')"}
    }
    if $compile_only { return }
    # Every target compiles; now run each emulated platform's tests in its own
    # shell (`.config/ci/emulated.nu`). These live in the cross lane until the
    # CI scheduler gives them lanes of their own.
    let emulated = [
        {platform: "windows", shell: "windows-wine", triple: "x86_64-pc-windows-gnu"}
        {platform: "arm64", shell: "arm64-emu", triple: "aarch64-unknown-linux-gnu"}
    ] | each {|lane|
        # `nix develop` makes its TMPDIR inside the current one. Nested in
        # this shell's, test socket paths outgrow sun_path's 107 bytes
        # (EndpointTooLong), so start the inner shell from plain /tmp.
        let passed = (try {
            with-env {TMPDIR: "/tmp", TMP: "/tmp", TEMP: "/tmp", TEMPDIR: "/tmp"} {
                run-external "nix" "develop" $".#($lane.shell)" "-c" "nu" ".config/ci/emulated.nu" $lane.platform
            }
            true
        } catch { false })
        if not $keep_artifacts {
            let tree = $target_root | path join $lane.triple
            if ($tree | path exists) { rm --recursive --force $tree }
        }
        {platform: $lane.platform, status: (if $passed { "passed" } else { "failed" })}
    }
    print ($emulated | table --expand)
    let failed_tests = $emulated | where status == "failed" | get platform
    if not ($failed_tests | is-empty) {
        error make {msg: $"emulated tests failed for: ($failed_tests | str join ', ')"}
    }
}

#!/usr/bin/env nu

# nextest-suite.nu — THE single documented command that runs this program's
# test suite end to end through `cargo nextest`.
#
#   nu .config/scripts/nextest-suite.nu                  # fast local loop: root unit+integration only
#   nu .config/scripts/nextest-suite.nu --profile ci      # root workspace, everything, serialised correctly
#   nu .config/scripts/nextest-suite.nu --profile perf    # root workspace measurement run only
#   nu .config/scripts/nextest-suite.nu --all             # EVERYTHING: root ci + GUI + screenshots, in order
#
# Flags:
#   --profile <default|ci|perf>   root-workspace nextest profile (default: "default")
#   --gui                         also run workspace/gui's nextest-compatible suites
#                                  (shell_flow, adversarial) after the root step
#   --screenshots                 also run workspace/gui's harness=false suites
#                                  (shot_probe, screenshots) directly via `cargo test`
#                                  — nextest cannot run these; see .config/nextest-gui.toml
#   --all                         shorthand for --profile ci --gui --screenshots,
#                                  plus --run-ignored all on the root step
#   --report <path>               where perf-report.nu writes its markdown (default: perf-report.md)
#
# WHY EVERYTHING RUNS SEQUENTIALLY, NEVER BACKGROUNDED: docs/AGENTS-DOCTRINE.md §8 —
# concurrent cargo invocations against the shared `target/` corrupt the
# build-script cache (observed: `dyld: Library not loaded:
# @rpath/libclang.dylib`), and a real-crate lowering has been measured at
# 21.9s idle vs 44.6s under that same contention. This script is a single
# linear pipeline for that reason: root workspace, then GUI nextest tests,
# then GUI screenshot binaries, never two cargo processes alive at once. Two
# instances of THIS script running at the same time reintroduce exactly the
# hazard it exists to avoid — do not `run_in_background` a second copy
# against the same checkout.
#
# WHY THE IGNORE-REASON CHECK RUNS FIRST ON `--profile ci`: nextest has no
# config key for "fail if a test is `#[ignore]`d without a reason" — it never
# reads the reason string at all. `check-ignore-reasons.nu` is that check,
# run here as a gate so a CI invocation of this script fails before spending
# any time on a build if the gate would fail anyway.
#
# WHY OUTPUT IS BUFFERED, NOT STREAMED LIVE: each step's combined stdout is
# both printed (once that step finishes) and saved to a temp file, then all
# saved files are concatenated through `perf-report.nu` at the end (doctrine
# §4 / this track's constraint 4 — every integration test is also a
# benchmark via a `cost case=` line). Nushell's `complete` — the only way to
# capture an external command's output as a value here — buffers rather than
# streams; a step that takes minutes prints nothing until it finishes. The
# `==> ...` marker before each step at least says what is currently running.

def run-required [label: string, command: closure] {
    print $"==> ($label)"
    let started = (date now)
    try {
        do $command
    } catch {|err|
        let detail = ($err.msg? | default ($err | to nuon))
        let status = ($err.exit_code? | default 1 | into int)
        print --stderr $"FAIL: ($label): ($detail)"
        exit $status
    }
    let elapsed = ((date now) - $started | into int) / 1_000_000_000
    print $"PASS: ($label) \(($elapsed)s\)"
}

# Locate a `libclang.dylib` directory under /nix/store. Required at *run*
# time (not just build time) because `nudox-languages` — and anything
# that links it, e.g. `nudox-engine`'s test binaries — dynamically links
# libclang without an embedded rpath (docs/TESTING.md; the doc comment on
# `nudox-engine`'s store plane `ProducerRegistry::with_all_available`). Without
# `DYLD_LIBRARY_PATH` set to this directory, `cargo nextest list`/`run`
# aborts with `Library not loaded: @rpath/libclang.dylib` while enumerating
# `nudox-languages`'s own test binary, before any test runs.
def find-libclang-dir [] {
    if (sys host | get name) != "Darwin" {
        return null
    }
    let hits = (^find /nix/store -maxdepth 3 -iname libclang.dylib | complete)
    let lines = ($hits.stdout | lines | where { |l| $l != "" })
    if ($lines | is-empty) { null } else { $lines | first | path dirname }
}

# Run an external command, printing its output once it finishes and saving
# it to `capture_path` (appended, so the root/GUI/screenshot steps can share
# one running log if the caller wants). Returns the exit code.
def run-and-capture [capture_path: string, ...args: string] {
    let result = (^$args.0 ...($args | skip 1) | complete)
    print $result.stdout
    if ($result.stderr | str trim) != "" {
        print --stderr $result.stderr
    }
    $"($result.stdout)\n($result.stderr)\n" | save --force --append $capture_path
    $result.exit_code
}

def main [
    --profile: string = "default"   # root-workspace nextest profile: default | ci | perf
    --gui                            # also run workspace/gui's nextest suites (shell_flow, adversarial)
    --screenshots                    # also run workspace/gui's harness=false suites directly
    --all                            # everything: --profile ci --gui --screenshots --run-ignored
    --report: string = "perf-report.md"
] {
    let profile = (if $all { "ci" } else { $profile })
    let want_gui = ($all or $gui)
    let want_screenshots = ($all or $screenshots)
    let run_ignored = ($all or $profile != "default")

    if $profile not-in ["default" "ci" "perf"] {
        print --stderr $"FAIL: --profile must be default, ci, or perf \(got '($profile)'\)"
        exit 1
    }

    $env.RUSTC_BOOTSTRAP = "1"  # constraint 6: nudox-ir uses unstable macro decls

    if (sys host | get name) == "Darwin" {
        let libclang_dir = (find-libclang-dir)
        if $libclang_dir == null {
            print --stderr "FAIL: could not locate libclang.dylib under /nix/store"
            print --stderr "      nudox-languages (and anything linking it) aborts at dyld load"
            print --stderr "      time without DYLD_LIBRARY_PATH pointed at it. See docs/TESTING.md."
            exit 1
        }
        $env.LIBCLANG_PATH = $libclang_dir
        $env.DYLD_LIBRARY_PATH = $libclang_dir
        print $"    libclang: ($libclang_dir)"
    }

    let tmp_dir = ($env.TMPDIR? | default "/tmp") | path join "nudox-nextest-suite"
    mkdir $tmp_dir
    let capture = ($tmp_dir | path join "combined.txt")
    "" | save --force $capture

    if $profile == "ci" {
        run-required "ignore-reason gate (no #[ignore] without a reason)" {
            nu .config/scripts/check-ignore-reasons.nu
        }
    }

    # No package exclusions. This block records why there used to be.
    #
    # This used to read `--exclude index --exclude driver --exclude ir-vcs`
    # and cite L6 ("index has never compiled"). L6 has been RESOLVED since
    # 2026-08-05 (docs/LIMITATIONS.md:736) — `index` and `ir-vcs` both compile and
    # both have tests. Re-measured 2026-08-07: dropping the two stale
    # exclusions takes `cargo nextest list -P default` from 1227 to 2267
    # enumerated tests, i.e. the stale tourniquet was hiding ~1040 tests that
    # build today, 731 of them in `index` and 216 in `ir-vcs`. Of those,
    # 1020 of 1023 also PASS on a real run; the 3 that do not are recorded in
    # docs/TESTING.md's "Newly-visible red tests" section rather than re-hidden.
    # Doctrine §7: "a crate outside the gate stops being measured".
    #
    # `driver` is NO LONGER EXCLUDED as of 2026-08-08, and its 202 tests are
    # now measured for the first time (enumeration 2,378 -> 2,580).
    #
    # The old note here said its build was "non-deterministic on this checkout
    # rather than failing one fixed way", citing three runs in 25 minutes that
    # gave three answers. The churn was real; it was noise on top of a plain
    # missing-code bug. `driver` was missing committed source:
    # `heart::SyncError::InvalidEndpoint` did not exist at HEAD while
    # `driver/sync.rs` called it, and index's `transport` module split had
    # landed only halfway, so `driver` imported PendingSection/BlobManifest/
    # FileEntry twice each. Both fixes sat uncommitted in a working tree, so the
    # failure was invisible here and total for everyone else.
    #
    # Verified from a FRESH CLONE of HEAD with its own empty target/, which is
    # stricter than the "settled tree" recheck this comment used to ask for — a
    # build on this machine cannot tell "committed" from "present in someone's
    # working tree", and that is exactly how the bug survived.
    #
    # UPDATE 2026-08-12: the standalone `driver` package named throughout this
    # history has since been dissolved — its composition/serving code folded
    # into `index::server`, and the `driver` integration tests referenced
    # above now live under `workspace/index/tests/` and are measured as part
    # of `index`, not a separately-excludable package. No exclusion list
    # changes are needed here as a result; `index` was already unexcluded.
    #
    # `--no-fail-fast` on ci/perf (never on `default`): those two profiles
    # exist to produce a complete picture (a JUnit report, a perf table) —
    # stopping at the first failure would silently drop every benchmark
    # after it, which is worse than a slower run. `default` keeps nextest's
    # own fail-fast default on purpose: it is the fast local loop, and the
    # first failure is usually the one to look at right now.
    let root_args = (
        [nextest run -P $profile --workspace]
        ++ (if $run_ignored { [--run-ignored all --no-fail-fast] } else { [] })
    )
    print $"==> root workspace: cargo ($root_args | str join ' ')"
    let root_started = (date now)
    let root_exit = (run-and-capture $capture cargo ...$root_args)
    let root_elapsed = ((date now) - $root_started | into int) / 1_000_000_000
    mut overall_ok = ($root_exit == 0)
    if $root_exit != 0 {
        print --stderr $"FAIL: root workspace nextest run \(($root_elapsed)s, exit ($root_exit)\)"
    } else {
        print $"PASS: root workspace nextest run \(($root_elapsed)s\)"
    }

    if $want_gui {
        let gui_profile = (if $profile == "default" { "default" } else { "ci" })
        let gui_args = (
            [nextest run --manifest-path workspace/gui/Cargo.toml --config-file .config/nextest-gui.toml -P $gui_profile]
            ++ (if $run_ignored { [--run-ignored all --no-fail-fast] } else { [] })
        )
        print $"==> GUI workspace \(nextest-compatible suites only\): cargo ($gui_args | str join ' ')"
        let gui_started = (date now)
        let gui_exit = (run-and-capture $capture cargo ...$gui_args)
        let gui_elapsed = ((date now) - $gui_started | into int) / 1_000_000_000
        if $gui_exit != 0 {
            $overall_ok = false
            print --stderr $"FAIL: GUI nextest run \(($gui_elapsed)s, exit ($gui_exit)\)"
        } else {
            print $"PASS: GUI nextest run \(($gui_elapsed)s\)"
        }
    } else {
        print "NOT RUN: GUI nextest suites (pass --gui or --all to request them)"
    }

    if $want_screenshots {
        # nextest cannot run these at all (see .config/nextest-gui.toml's
        # header — verified, not assumed: including them in a nextest list
        # aborts the WHOLE invocation with exit 104, zero tests enumerated).
        # Run directly, one binary at a time, strictly after the steps above
        # so nothing else is building workspace/gui concurrently.
        print "==> GUI workspace: shot_probe (direct cargo test, harness = false)"
        let probe_started = (date now)
        let probe_args = ["cargo", "test", "--manifest-path", "workspace/gui/Cargo.toml", "--test", "shot_probe"]
        let probe_exit = (run-and-capture $capture ...$probe_args)
        let probe_elapsed = ((date now) - $probe_started | into int) / 1_000_000_000
        if $probe_exit != 0 {
            $overall_ok = false
            print --stderr $"FAIL: shot_probe \(($probe_elapsed)s\)"
        } else {
            print $"PASS: shot_probe \(($probe_elapsed)s\)"
        }

        print "==> GUI workspace: screenshots (direct cargo test, harness = false)"
        let shots_started = (date now)
        let shots_args = ["cargo", "test", "--manifest-path", "workspace/gui/Cargo.toml", "--test", "screenshots", "--", "--nocapture"]
        let shots_exit = (run-and-capture $capture ...$shots_args)
        let shots_elapsed = ((date now) - $shots_started | into int) / 1_000_000_000
        if $shots_exit != 0 {
            $overall_ok = false
            print --stderr $"FAIL: screenshots \(($shots_elapsed)s\)"
        } else {
            print $"PASS: screenshots \(($shots_elapsed)s\)"
        }
    } else {
        print "NOT RUN: GUI screenshot suites (pass --screenshots or --all to request them)"
    }

    # Doctrine §4 / this track's constraint 4: every integration test is also
    # a benchmark via `heart::cost::measured()`'s `cost case=` line.
    # perf-report.nu itself prints "no cost lines found" harmlessly if a run
    # (e.g. the plain `default` profile, which sets `success-output =
    # "never"` on purpose for a quiet fast loop) produced none.
    print "==> perf report"
    # `--stdin` is load-bearing, not decoration — see perf-report.nu's header
    # comment: without it, a script invoked as a separate `nu` process does
    # not receive the pipe as `$in` on this nushell version (confirmed
    # 2026-08-05, nushell 0.114.1) and the report step fails outright.
    open --raw $capture | nu --stdin .config/scripts/perf-report.nu --markdown $report

    if not $overall_ok {
        exit 1
    }
}

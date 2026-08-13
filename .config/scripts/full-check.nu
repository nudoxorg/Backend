#!/usr/bin/env nu

# Run the repository's full local check. Expensive/infrastructure tiers are
# opt-in and fail loudly when requested but unavailable.
#
# Every step is individually timed; a summary table of wall durations and the
# total execution time is printed at the end.

def has-command [name: string] {
    not (which $name | is-empty)
}

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

def require-command [name: string] {
    if not (has-command $name) {
        print --stderr $"FAIL: requested command is not installed: ($name)"
        exit 1
    }
}

def main [
    --nix (-n) # Also run `nix flake check`.
    --live (-l) # Run ignored tests against the configured live services.
    --nightly # Run mutation/fuzz tooling when installed.
] {
    let total_started = (date now)

    run-required "root Cargo workspace tests" {
        ^env RUSTC_BOOTSTRAP=1 cargo test --workspace --locked --no-fail-fast
    }

    run-required "fixture engine integration tests" {
        ^env RUSTC_BOOTSTRAP=1 cargo test -p nudox-engine --features fixtures --locked --no-fail-fast
    }

    run-required "test measurement support" {
        ^env RUSTC_BOOTSTRAP=1 cargo test -p nudox-test-support --locked
    }

    if (has-command "cargo") and ("workspace/gui/Cargo.toml" | path exists) {
        run-required "GUI Cargo tests" {
            ^env RUSTC_BOOTSTRAP=1 cargo test --manifest-path workspace/gui/Cargo.toml
        }
    } else {
        print "NOT RUN: GUI Cargo tests (manifest or cargo unavailable)"
    }

    let compiler_buck_manifest = "workspace/compiler/BUCK"
    if not ($compiler_buck_manifest | path exists) {
        print --stderr $"SKIP: compiler Buck tests (missing ($compiler_buck_manifest))"
    } else if not (has-command "buck2") {
        print --stderr "SKIP: compiler Buck tests (buck2 is not on PATH)"
    } else {
        run-required "compiler Buck tests" {
            run-external "buck2" "test" "//workspace/compiler/..."
        }
    }

    if $nix {
        require-command "nix"
        run-required "Nix flake checks" {
            run-external "nix" "flake" "check" "--no-update-lock-file"
        }
    } else {
        print "NOT RUN: Nix flake checks (pass --nix to request them)"
    }

    if $live {
        require-command "cargo"
        if ($env.SERVER_TEST_BACKENDS? | default "") == "" {
            print --stderr "FAIL: --live requires SERVER_TEST_BACKENDS=1 in the environment"
            exit 1
        }
        run-required "live backend socket tests" {
            ^env RUSTC_BOOTSTRAP=1 cargo test -p index --test live_socket --locked -- --ignored --nocapture
        }
        run-required "live backend download tests" {
            ^env RUSTC_BOOTSTRAP=1 cargo test -p index --test live_download --locked -- --ignored --nocapture
        }
        print "    live pipeline uses bin/nudox-serve (workspace/index package)"
    } else {
        print "NOT RUN: live backend tests (pass --live with SERVER_TEST_BACKENDS=1)"
    }

    if $nightly {
        if (has-command "cargo-mutants") {
            run-required "mutation tests for engine" {
                ^cargo mutants --package nudox-engine --no-shuffle
            }
        } else {
            print --stderr "FAIL: --nightly requested but cargo-mutants is not installed"
            exit 1
        }

        if (has-command "cargo-fuzz") {
            run-required "fuzz target inventory" {
                ^cargo fuzz list
            }
        } else {
            print --stderr "FAIL: --nightly requested but cargo-fuzz is not installed"
            exit 1
        }
    } else {
        print "NOT RUN: mutation/fuzz tooling (pass --nightly to request it)"
        print "    note: no fuzz/ tree, no fuzz targets, no screenshot or mutation coverage in default gate"
    }

    let total_elapsed = ((date now) - $total_started | into int) / 1_000_000_000
    print $"Full check completed in ($total_elapsed)s"
}

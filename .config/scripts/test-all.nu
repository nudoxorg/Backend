#!/usr/bin/env nu

use std/log

# Run the full test suite: Cargo workspace tests + Buck2 compiler tests.
def main [] {
    log info "Running Cargo workspace tests..."
    cargo test --workspace

    # ── C# sandbox gotchas (CSHARP-PLAN §7, pitfall #15) ──────────────────
    # dotnet needs a writable HOME-equivalent dir; $DOTNET_CLI_HOME must exist
    # and be writable before any `dotnet` invocation (including the genrule
    # that builds the oracle).  Same class of fix as the GOCACHE issue from
    # the Go/Java snapshot-test drive.
    #
    # Also ensure `dotnet` is on PATH so snap_csharp tests that shell out can
    # find it.  In the Nix devshell this is guaranteed by the package list;
    # outside the devshell the operator must install the .NET 10 SDK manually.
    let dotnet_home = $"($env.TMPDIR? | default "/tmp")/dotnet"
    mkdir $dotnet_home
    $env.DOTNET_CLI_HOME = $dotnet_home
    $env.DOTNET_CLI_TELEMETRY_OPTOUT = "1"
    $env.DOTNET_NOLOGO = "1"
    $env.DOTNET_SKIP_FIRST_TIME_EXPERIENCE = "1"

    log info "Running Buck2 compiler tests (including C# snap_csharp)..."
    # Use run-external so nushell does not expand `...` as a path glob.
    # //workspace/compiler/... already covers snap_csharp once the test file
    # exists; the explicit target below is kept for clarity and lets you run
    # only the C# snapshot suite via `buck2 test //workspace/compiler:snap_csharp`.
    run-external "buck2" "test" "//workspace/compiler/..."
}

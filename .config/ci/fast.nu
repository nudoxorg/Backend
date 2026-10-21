# Fast lane: the first verdict on a PR, in minutes rather than the hour the
# full test suite takes. Invoked as `nu .config/ci/fast.nu` inside the lean
# `.#cross` dev shell. It proves that the change is formatted, passes the
# cheap structural flake checks, and compiles (every target, tests included)
# for Linux x86_64, Windows and Linux aarch64. The scheduler runs it first and
# stops the PR's other lanes when it fails: a change that does not compile
# has nothing for an hour of tests to say.

use lib.nu *

# CI mounts this path as a persistent task cache. Keep that mount point while
# evicting its regenerable contents; removing the mount itself fails EBUSY.
def clear-fast-cache [cache: string]: nothing -> nothing {
    ^python3 ($env.FILE_PWD | path join "fast_cache.py") $cache
    if $env.LAST_EXIT_CODE != 0 {
        error make {msg: "fast cache eviction failed; compilation refused"}
    }
}

# Flake checks that build no workspace crate, so they cost seconds once their
# tools are in the host store. The heavy ones (control-plane, semantic-lints,
# the gui contracts) stay in the Linux lane's full `nix flake check`.
def fast-flake-checks []: nothing -> list<string> {
    [
        "formatting"
        "nushell-command"
        "ast-grep-rules"
        "agent-skills"
        "tooling-contracts"
        "telemetry-config"
        "surface-smoke-contract"
        "nu-check-failure-contract"
        "packaged-source-contract"
    ]
}

def main []: nothing -> nothing {

    # Check wall-clock expiry on every invocation, outside cached Nix checks.
    ^python3 ($env.FILE_PWD | path join "quarantine.py")
    if $env.LAST_EXIT_CODE != 0 {
        error make {msg: "test exclusion ownership/review is invalid or expired; see docs/testing/quarantine.md"}
    }

    # Fast checks keep only compile outputs, independently of the much larger
    # Linux runtime cache. Cache loss always falls back to a clean build.
    let cache = $env.CI_FAST_CACHE_ROOT? | default ""
    let limit = $env.CI_FAST_CACHE_GIB? | default "24" | into int
    if ($cache | is-not-empty) {
        mkdir $cache
        let used = (
            ^du -s --block-size=1G $cache
            | split row "\t"
            | first
            | into int
        )
        if $used > $limit {
            clear-fast-cache $cache
            print $"fast cache: reclaimed ($used) GiB; limit ($limit) GiB"
        }
        let free = (
            ^df --output=avail -B1G $cache
            | lines
            | last
            | str trim
            | into int
        )
        if $free < 40 {
            # Reclaim only this serial worker's accelerator, never sources or
            # another job's artifacts. Cache loss is a normal cold-build case.
            clear-fast-cache $cache
            let recovered = (
                ^df --output=avail -B1G $cache
                | lines
                | last
                | str trim
                | into int
            )
            if $recovered < 40 {
                print $"CI problem: only ($recovered) GiB free; fast compilation requires 40"
                exit 75
            }
        }
        $env.CARGO_TARGET_DIR = ($cache | path join "targets")
        # Registry archives and Git checkouts are reusable inputs too. Keep
        # them inside the same bounded cache to avoid every task fetching the
        # full workspace dependency graph again.
        $env.CARGO_HOME = ($cache | path join "cargo-home")
        let admission = (
            run-external "python3" ($env.FILE_PWD | path join "fast_cache.py") "--ensure-cargo-home" $cache $env.CARGO_HOME
            | complete
        )
        if ($admission.stdout | is-not-empty) { print $admission.stdout }
        if $admission.exit_code != 0 {
            error make {msg: $"fast Cargo home admission failed: ($admission.stderr)"}
        }
        $env.ZIG_GLOBAL_CACHE_DIR = ($cache | path join "zig")
        $env.ZIG_LOCAL_CACHE_DIR = ($cache | path join "zig")
        ^python3 ($env.FILE_PWD | path join "restore-mtimes.py") ($cache | path join "source-mtimes.json")
        if $env.LAST_EXIT_CODE != 0 { error make {msg: "source mtime restoration failed"} }
    }
    let system = (^nix eval --raw --impure --expr "builtins.currentSystem" | str trim)
    let installables = fast-flake-checks | each {|check| $"path:.#checks.($system).($check)" }
    let checks = ci-step "fast" "structural flake checks" {||
        with-plain-tmp {|| run-bounded 20min "python3" ($env.FILE_PWD | path join "structural-checks.py") ...$installables }
    }
    stop-if-superseded "fast"
    # Seconds, and names the file: a unit-test module that Cargo also builds as
    # a standalone test fails every --all-targets compile below with hundreds
    # of follow-on errors (canonical broke this way three times).
    let layout = ci-step "fast" "test module layout" {||
        run-bounded 2min "python3" ($env.FILE_PWD | path join "test_layout.py") $env.PWD
    }
    # Native first: a plain compile error shows up before the foreign targets
    # spend their minutes on it.
    let targets = ["x86_64-unknown-linux-gnu"] | append (
        $env.NUDOX_CROSS_CHECK_TARGETS
        | split row " "
        | where {|triple| $triple | is-not-empty }
    ) | uniq
    let compiles = ci-step "fast" $"cargo check ($targets | str join ', ')" {||
        with-env {NUDOX_CROSS_CHECK_TARGETS: ($targets | str join " ")} {
            run-bounded 30min "nu" "--no-config-file" ($env.FILE_PWD | path join "cross-check.nu") "--compile-only" ...(if ($cache | is-not-empty) { ["--keep-artifacts"] } else { [] })
        }
    }
    if ($cache | is-not-empty) {
        print $"CI-CACHE fast: (^du -sh $cache | str trim)"
        let used = (
            ^du -s --block-size=1G $cache
            | split row "\t"
            | first
            | into int
        )
        if $used > $limit {
            clear-fast-cache $cache
            print $"fast cache: evicted ($used) GiB after build; limit ($limit) GiB"
        }
    } else { reclaim-build-output }
    finish-lane "fast" [$checks $layout $compiles]
}

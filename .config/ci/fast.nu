# Fast lane: the first verdict on a PR, in minutes rather than the hour the
# full test suite takes. Invoked as `nu .config/ci/fast.nu` inside the lean
# `.#cross` dev shell. It proves that the change is formatted, passes the
# cheap structural flake checks, and compiles (every target, tests included)
# for Linux x86_64, Windows and Linux aarch64. The scheduler runs it first and
# stops the PR's other lanes when it fails: a change that does not compile
# has nothing for an hour of tests to say.

use lib.nu *

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
    ]
}

def main []: nothing -> nothing {
    # Fast checks keep only compile outputs, independently of the much larger
    # Linux runtime cache. Cache loss always falls back to a clean build.
    let cache = ($env.CI_FAST_CACHE_ROOT? | default "")
    if ($cache | is-not-empty) {
        mkdir $cache
        let limit = ($env.CI_FAST_CACHE_GIB? | default "24" | into int)
        let used = (^du -s --block-size=1G $cache | split row "\t" | first | into int)
        if $used > $limit {
            rm --recursive --force $cache
            mkdir $cache
            print $"fast cache: reclaimed ($used) GiB; limit ($limit) GiB"
        }
        $env.CARGO_TARGET_DIR = ($cache | path join "targets")
        $env.ZIG_GLOBAL_CACHE_DIR = ($cache | path join "zig")
        $env.ZIG_LOCAL_CACHE_DIR = ($cache | path join "zig")
        ^python3 ($env.FILE_PWD | path join "restore-mtimes.py") ($cache | path join "source-mtimes.json")
        if $env.LAST_EXIT_CODE != 0 { error make {msg: "source mtime restoration failed"} }
    }
    let system = (^nix eval --raw --impure --expr "builtins.currentSystem" | str trim)
    let installables = fast-flake-checks | each {|check| $"path:.#checks.($system).($check)" }
    let checks = ci-step "fast" "structural flake checks" {||
        with-plain-tmp {|| run-bounded 20min "nix" "build" "-L" "--keep-going" "--no-link" ...$installables }
    }
    stop-if-superseded "fast"
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
    } else { reclaim-build-output }
    finish-lane "fast" [$checks $compiles]
}

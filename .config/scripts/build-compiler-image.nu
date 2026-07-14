#!/usr/bin/env nu

use std/log

# Helper: import a Buck2 target via snowydeer, print store path to stdout
def import-target [label: string, desc: string] -> string {
    log info $"Importing ($desc) ($label) ..."
    let path = (^buck2 bxl //snowydeer:snowydeer.bxl:main -- --target $label | str trim)
    log info $"  ($desc) → ($path)"
    $path
}

def main [] {
    # ── Binaries ──────────────────────────────────────────────────────────
    let daemon_path = (import-target //workspace/compiler:compiler-daemon "compiler-daemon")
    let worker_path = (import-target //workspace/compiler:producer-worker "producer-worker")

    # ── Oracle resources (optional; gracefully handled if missing) ─────────
    let go_path = (try { import-target //workspace/compiler/compile/go/oracle:oracle "go-oracle" } catch { "" })
    let java_path = (try { import-target //workspace/compiler/compile/java/oracle:extractor "java-oracle" } catch { "" })
    let cs_path = (try { import-target //workspace/compiler/compile/csharp/oracle:oracle "csharp-oracle" } catch { "" })

    print ""
    log info "Building Docker image with nix2container ..."

    # Build the image with all paths injected via env vars
    let env_vars = {
        NUDOX_COMPILER_DAEMON_PATH: $daemon_path,
        NUDOX_PRODUCER_WORKER_PATH: $worker_path,
    }
    let env_vars = (if $go_path != "" { $env_vars | merge { NUDOX_GO_ORACLE_PATH: $go_path } } else { $env_vars })
    let env_vars = (if $java_path != "" { $env_vars | merge { NUDOX_JAVA_ORACLE_PATH: $java_path } } else { $env_vars })
    let env_vars = (if $cs_path != "" { $env_vars | merge { NUDOX_CSHARP_ORACLE_PATH: $cs_path } } else { $env_vars })

    ^env $env_vars nix build .#compilerImage --impure

    if ($env.LAST_EXIT_CODE) != 0 {
        log error "nix build failed"
        exit 1
    }

    print ""
    log info "Docker image tarball at ./result"
    ls -la ./result

    print ""
    print "To load into Docker:  docker load < result"
    print $"To tag:              docker tag nudox/compiler-daemon:latest nudox/compiler-daemon:<version>"
}

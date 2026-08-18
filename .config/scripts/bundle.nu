#!/usr/bin/env nu
use common.nu *

use std/log

# Package lindsey via cargo-bundle, then wrap the .app with the Nix-built
# oracles, toolchains, and embed model. `lindsey-bundle` is the flake's
# wrapper around cargo-bundle 0.11; this script is the devshell verb that
# finds it.
def main [...args] {
    log info "Bundling lindsey (cargo-bundle, then oracle wrap)…"
    if (which lindsey-bundle | is-empty) {
        build_error "lindsey-bundle is not on PATH — enter the Nix devshell (`nix develop`) so cargo-bundle 0.11 and the oracle wrap are provided"
    }
    ^lindsey-bundle ...$args
}

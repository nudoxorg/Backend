#!/usr/bin/env nu
use common.nu *

use std/log

def main [] {
    # Call build-release first? Or assume called?
    # Original 'install' recipe calls 'build-release' first.
    # We should follow that dependency.
    # To invoke another script from here, use `nu .config/scripts/build-release.nu`?
    # Or just execute cargo build... since build-release is effectively cargo build.
    # However, to be consistent with "recipes calling recipes", let's use nu script invocation.
    
    nu .config/scripts/build-release.nu
    
    log info $"Installing ($env.MAIN_PACKAGE) binary..."
    cargo install --bin $env.MAIN_PACKAGE
}

#!/usr/bin/env nu

use std/log

# Sync Cargo dependencies into the Buck2 third-party build files.
#
# Run this after adding or changing crate versions in build/third-party/Cargo.toml:
#   1. reindeer vendor  — downloads all crates into build/third-party/vendor/
#   2. reindeer buckify — generates build/third-party/BUCK from the vendored sources
#   3. Appends build/third-party/git-fragment.bzl to the generated BUCK so that
#      git-sourced crates (pyrefly, oxc, snix, …) and compat aliases are visible.
#
# The generated BUCK replaces registry.bzl + defs.bzl as the source of truth for
# third-party crate targets.  Only BUCK and Cargo.lock are committed; vendor/ is
# gitignored.
def main [] {
    log info "Vendoring crates with reindeer..."
    reindeer --third-party-dir build/third-party vendor

    log info "Removing vendor BUCK files (Meta-internal sub-packages that block our package)..."
    glob build/third-party/vendor/**/BUCK | each { |f| rm $f }

    log info "Generating BUCK with reindeer buckify..."
    reindeer --third-party-dir build/third-party buckify

    log info "Appending git-fragment.bzl to generated BUCK..."
    "\n" | save --append build/third-party/BUCK
    open build/third-party/git-fragment.bzl | save --append build/third-party/BUCK

    log info "Done. Commit build/third-party/BUCK and build/third-party/Cargo.lock."
}

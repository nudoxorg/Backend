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

    log info "Removing vendor/registry BUCK files (Meta-internal sub-packages that block our package)..."
    # Crates that ship Meta-internal BUCK files (e.g. strong_hash) break analysis
    # with missing //tools/build_defs/rust_library.bzl. Strip from both vendor/
    # and the cargo registry cache (.cargo is gitignored but still on disk).
    glob build/third-party/vendor/**/BUCK | each { |f| rm $f }
    glob build/third-party/.cargo/registry/**/BUCK | each { |f| rm $f }

    # arborium language crates: env!("CARGO_MANIFEST_DIR") → std::env::var so
    # grammar/** is found under Buck's runtime package materialization. See
    # build/third-party/patches/arborium/fix-build-rs-manifest-dir.sh.
    log info "Applying arborium build.rs CARGO_MANIFEST_DIR patch..."
    ^bash build/third-party/patches/arborium/fix-build-rs-manifest-dir.sh

    log info "Generating BUCK with reindeer buckify..."
    reindeer --third-party-dir build/third-party buckify

    log info "Appending git-fragment.bzl to generated BUCK..."
    "\n" | save --append build/third-party/BUCK
    open build/third-party/git-fragment.bzl | save --append build/third-party/BUCK

    log info "Done. Commit build/third-party/BUCK and build/third-party/Cargo.lock."
}

#!/usr/bin/env nu

# Build and run the `index` crate under `--features server`.
#
# # Why this exists as its own check
#
# `workspace-tests` runs `cargo nextest -P default --workspace`, which compiles
# `index` with its DEFAULT features. The `server` feature is not default, so
# nothing in `nix flake check` ever built it — and the serving plane drifted far
# enough to stop compiling entirely without a single check going red
# (docs/LIMITATIONS.md, `index-server-zstd-clash`). Roughly 140 tests across 19
# `#![cfg(feature = "server")]` files under `workspace/index/tests/` built and
# ran *never*, while a default `cargo test -p index` stayed fully green.
#
# That is the failure this check exists to make impossible: it is a compile
# gate first and a test run second.
#
# # Why `--run-ignored` is NOT passed
#
# The ignored tests in this suite are the live tier: they need a real qdrant and
# a real embedding endpoint (`SERVER_TEST_BACKENDS=1`, see
# `.config/scripts/local-backends.nu`). A Nix check cannot stand those up
# hermetically, and this suite treats a missing opt-in as a hard failure rather
# than a silent pass — correctly. So the ignored set stays ignored here and the
# check covers exactly what it can honestly cover: that everything COMPILES
# under the feature, and that every backend-free test passes.
#
# Anything stronger belongs in the Live tier, which is a human-run command
# against real services, not a sandboxed derivation pretending to have them.

# The check materializes several sources as local paths (SmolVM, pyroscope,
# doltlite, zstd-seekable), so their package identities differ from the
# checkout lockfile. Re-resolve only this temporary build tree, then keep the
# run locked so nextest cannot change dependencies while it runs — the same
# two-step `workspace-tests/check.nu` uses and for the same reason.
^cargo generate-lockfile

# `RUSTC_BOOTSTRAP=1` because `nudox-ir` still uses unstable macro
# declarations; the rest of the repo's tiers set it for the same reason.
^env RUSTC_BOOTSTRAP=1 cargo nextest run --locked -p index --features server

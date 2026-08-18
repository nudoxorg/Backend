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
# It is a compile gate first and a test run second, and the two steps below are
# deliberately separate.

# ── Step 1: compile everything the feature gates ─────────────────────────────
#
# This is the step that would have caught the regression above, and it is worth
# more than the test run: it builds every server-gated target — including the
# nine that cannot execute here (below) — so a type error, a private re-export,
# or a missing symbol behind `--features server` fails the check even though
# those tests never run.
#
# `--no-run` rather than a filtered run because it cannot go stale: it covers
# whatever the feature gates today, with no list to maintain.
#
# The check materializes several sources as local paths (SmolVM, pyroscope,
# doltlite, zstd-seekable), so their package identities differ from the checkout
# lockfile. Re-resolve only this temporary build tree, then keep every
# invocation locked so nextest cannot change dependencies while it runs — the
# same two-step `workspace-tests/check.nu` uses, for the same reason.
^cargo generate-lockfile

# `RUSTC_BOOTSTRAP=1` because `nudox-ir` still uses unstable macro
# declarations; the repo's other tiers set it for the same reason.
^env RUSTC_BOOTSTRAP=1 cargo nextest run --locked -p index --features server --no-run

# ── Step 2: run what can honestly run here ───────────────────────────────────
#
# `--lib` only, and that restriction is load-bearing rather than lazy.
#
# Nine integration targets — `api_add_package`, `authz_cap_routing`,
# `client_surfaces`, `compiled_lookup`, `indexing_flow`, `initialization_flow`,
# `pipeline_end_to_end`, `semantic_search_gating`, `symbol_store` — route
# through `tests/server_common::required_assembled_server`, which **panics** when
# `SERVER_TEST_BACKENDS` is unset rather than skipping. That is the right
# behaviour for that helper: a test whose assertions are only meaningful against
# a real catalog, qdrant and object store must not report success without them.
# Measured 2026-08-16: the full suite is 1168 passed / 29 failed on a machine
# with no backends, and all 29 are that panic.
#
# A sandboxed Nix derivation cannot stand those services up, so this check does
# not pretend to. Their coverage is the Live tier
# (`.config/scripts/local-backends.nu up` then
# `cargo nextest run -p index --features server --run-ignored all`), which is a
# human-run command against real services.
#
# The lib target has no such dependency: 945 unit tests covering the catalog,
# queue, scratch, pack and codec layers — the indexing spine — all of which run
# against temporary directories they create themselves.
^env RUSTC_BOOTSTRAP=1 cargo nextest run --locked -p index --features server --lib

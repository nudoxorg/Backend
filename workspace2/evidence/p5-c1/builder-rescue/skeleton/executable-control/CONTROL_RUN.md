# C1 builder rescue executable-control run

R19 snapshot: the formatted control source and tests at the SHA-256 values recorded in the current
canonical card were executed before their custody commit. This receipt is control custody only.

Commands:

- `CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-r19-control RUSTC_WRAPPER= cargo fmt --manifest-path workspace2/evidence/p5-c1/builder-rescue/skeleton/executable-control/Cargo.toml --all`
- `CARGO_TARGET_DIR=/private/tmp/p5-c1-builder-rescue-r19-control RUSTC_WRAPPER= cargo test --locked --manifest-path workspace2/evidence/p5-c1/builder-rescue/skeleton/executable-control/Cargo.toml --all-targets`

Status: 0 for formatting and tests. The test process reported 2 library provenance tests, 4 fragment
tests, and 1 paired-consumer test passed. This is feasibility-only control evidence; it is not production
actual-rlib, mutant, returned-view, release-codegen, or clean-gate evidence.

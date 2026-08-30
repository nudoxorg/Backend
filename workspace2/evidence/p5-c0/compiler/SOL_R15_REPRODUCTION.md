# P5 C0-COMPILER independent Sol reproduction

## Custody

```text
candidate commit: 1a3675e473399113fc597481679cedd1c8b9cced
manager card: 0741bebef3479f1a5e701e5d2246a85f10c82931
manager card SHA-256: 933b72bcd4bce8ccc2bbfb48817816225e9fbdfecff7464efad94652cb8b6dd3
rustc: 1.97.1 (8bab26f4f 2026-07-14)
cargo: 1.97.1 (c980f4866 2026-06-30)
host: aarch64-apple-darwin
isolated target: /private/tmp/p5-c0-r15-sol.0lnQby
captured terminal: /private/tmp/p5-c0-r15-sol-log.VqPZ2L
```

The target and terminal paths are ephemeral reproduction custody, not repository inputs.

## Independent gate

From a new isolated target, Sol ran the locked compiler workspace with all targets and `--nocapture`,
then formatting, warnings-denied Clippy, diff validation, rlib cardinality, and source/artifact hashing.

```text
cargo test --locked --workspace --all-targets -- --nocapture: PASS
dispatch tests: 4 passed; 0 failed
actual-rlib subset test: 1 passed; 0 failed
cargo fmt --all -- --check: PASS
cargo clippy --locked --workspace --all-targets -- -D warnings: PASS
git diff --check: PASS
registry rlib cardinality: 1
compile-vocab rlib cardinality: 1
captured terminal NUL bytes: 0
```

The subset terminal was ordinary text only. This independently falsifies the superseded R9 closure's
metadata-to-terminal defect: the repaired child compiler process keeps `--emit=metadata=-` but binds
stdout to `Stdio::null()`, while stderr remains available for the exact diagnostic predicate.

## Per-run identities

```text
registry source  bbe0152ce639ee7f92a9b72e26dd6b040133a28f3700f04aee302abd1c4fe867
dispatch tests   588ee2b4c5ec3ad7847970ff316640a7ca05343ab66e75aea890b8880acdd9ab
subset test      05bc6f455335106bac3812f4cde299d97890592fddd1c07a6ec1043213c77b8a
release consumer b86ecd9c1c15f855e8523883250b89ab96f042708b54e4ccd21f01edfd4a76c8
registry rlib    b5dcea263838fc542a7bb8effda1120e25a82958e87772a58b02ebd8e669e5f7
vocab rlib       f69a8a7431caf7963186e13c89830db7cd846a9882486e7d63cce750e0679228
```

The rlib hashes bind only the exact artifacts used in this reproduction. They are not asserted equal to
other fresh builds. Reproducible custody is the pinned source/toolchain/command, exact one-artifact
cardinality, and the path/hash pair actually passed by that run.

## Decision

C0-COMPILER is independently reproduced as a narrow prototype baseline. Pointer/length forwarding,
the exact typed rejection, actual-export member absence, and clean compiler-process containment are
verified. Stable per-callable text size, optimized input erasure, dispatch erasure, zero-cost behavior,
and cross-platform codegen remain unverified and unclaimed.

**PROMOTE FOR FUTURE INTEGRATION REVIEW**

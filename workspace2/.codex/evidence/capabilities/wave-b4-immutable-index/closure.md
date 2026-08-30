# Wave B4 immutable-index Phase 0 closure receipt

## Status

Phase 0 is not a production candidate. This receipt completes only after calibration readers,
misreader, and source-isolated registered Terra review are attached. No matrix row is proved by this
evidence-only increment; B4-01 through B4-13 remain red.

## Exact planned Nix commands

```text
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" fmt --manifest-path workspace2/planes/index/Cargo.toml --all -- --check'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" test --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --all-targets --no-fail-fast'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" test --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --doc --no-fail-fast'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" clippy --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --all-targets -- -D warnings'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" doc --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --no-deps'
```

These are read-only checks for existing I0 vocabulary. Future B4 code needs focused commands in a
frozen worker card and cannot cite them as segment/query proof.

## Toolchain failure and recovery receipt

Ambient `cargo` under `nix develop` is inadmissible: Sol's raw checkout receipt observed a nightly
`rustc` on `PATH`, `NUDOX_STABLE_TOOLCHAIN=1.97.1`, a shared `RUSTC_WRAPPER` sccache setting, and
`E0514` even with a fresh target. The manager's bounded ambient retry emitted only Nix's initial
`fetching git input` line before the 30-second command window expired, so it is recorded as an
inconclusive alternate attempt and not as a claimed reproduction. The commands above explicitly pick
the pinned stable `cargo`/`rustc` and clear the wrapper; their raw successful receipt is required
before Phase 0 closes. `workspace2/tools/quality.sh` remains an equivalent full-workspace fallback
because it registers pinned rustup toolchains.

## Raw checks and reviewer custody

Pending after the frozen artifact commit. The final receipt retains raw command output, exit/status,
sidecar/reviewer task receipts, effective sandbox event, sole disposable writable root, excluded
implicit temp roots, source digest before/after, packet digest, raw JSON, findings, and disposition.

## Strongest pre-edit counterexample

A stale rendezvous route sends declared E2 to a lost node after S2 is pinned. An implementation that
reads latest head, trusts assignment, or makes the fault empty success returns wrong facts or empty
`Complete`. The seam requires exact E2 absence then equivalent retry, or `Partial { missing: [E2] }`.

## Remaining red work

No manifest/segment format, sealed-delta builder, exact/prefix/lexical query, publication/head, range
lease, placement adapter, compactor, allocation/work control, compiler/probe integration, or bounded
Tantivy differential adapter exists. Deterministic owner parity cannot claim file/NVMe/object-store
durability/outage proof. Sol must make the public seam red; a later Terra freezes one narrow
production card and recalibrates when semantics change.

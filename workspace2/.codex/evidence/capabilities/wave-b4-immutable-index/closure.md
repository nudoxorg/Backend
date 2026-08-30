# Wave B4 immutable-index Phase 0 closure receipt

## Status

Phase 0 is not a production candidate. This receipt completes only after calibration readers,
misreader, and source-isolated registered Terra review are attached. No matrix row is proved by this
evidence-only increment; B4-01 through B4-13 remain red.

## Exact planned Nix commands

```text
nix develop ./workspace2 --command cargo test --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --all-targets --no-fail-fast
nix develop ./workspace2 --command cargo test --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --doc --no-fail-fast
nix develop ./workspace2 --command cargo clippy --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --all-targets -- -D warnings
nix develop ./workspace2 --command cargo doc --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --no-deps
nix develop ./workspace2 --command cargo fmt --manifest-path workspace2/planes/index/Cargo.toml --all -- --check
```

These are read-only checks for existing I0 vocabulary. Future B4 code needs focused commands in a
frozen worker card and cannot cite them as segment/query proof.

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

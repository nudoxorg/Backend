# `backend-performance-tests`

This package is a structural performance and complexity suite. It compares
visible state with separate BTreeMap and weighted support models before it
checks work counters. Assertions use deterministic sizes, seeds, fanout, and
resource budgets; they do not use wall-clock thresholds.

The integration suite covers:

- one-key path-copy updates at three map sizes, subtree reuse, exact delta
  apply/inversion, and a full rebuild oracle;
- uniform and clustered random edits, deletes/re-adds, and repeated hot-key
  updates;
- weighted arrangement updates, cancellation/no-op suppression, and a
  three-term incremental join with measured hot-key fanout;
- sparse replication checkpoints, wire resume admission, idempotent replay,
  and exact accepted bytes; and
- scheduler coalescing, reusable outputs, hedge winner selection, loser
  cancellation, and both route reservations; and
- owner-level dispatcher planning and publication: a cold local route ends
  before remote admission, while a warm version reuses one immutable output
  owner without growing lookup state; and
- durable tree-CAS point reads, cold reopen path work, one-row publication
  node/byte deltas, and filesystem-level reuse accounting.

Run the structural suite with:

```console
cargo test -p backend-performance-tests --offline
```

Run the executable harness with:

```console
cargo run -p backend-performance-tests --bin structural --offline
```

The harness prints p50/p95/p99 nanoseconds as descriptive measurements. Each
line also reports the relevant structural counters: store visits/copies/reuse
and encoded bytes, flow input/output/no-op/compaction/seek/join rows,
replication chunks and retained/resumed bytes, and scheduler coalesced and
hedged reservations. Timing values are never used as pass/fail criteria.

The evidence and remaining hidden work seams are recorded in
[`docs/research/performance-oracles.md`](../../docs/research/performance-oracles.md).

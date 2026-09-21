# Integrated benchmark runner

`src/bin/integrated.rs` drives the production Tantivy, Turso, CAS, library,
CLI, MCP, and optional desktop GUI contracts over real source files. It emits
`nudox.integrated-benchmark.v1` JSON; timings are descriptive measurements and
correctness assertions are recorded beside every result.

Run the checked-in smoke profile with the pinned shell:

```console
nix shell .#backend --command cargo run --locked --offline \
  -p backend-performance-tests --bin integrated -- \
  --profile smoke --output tests/performance/results/integrated-smoke.json
```

To include a live desktop harness, pass its executable explicitly. The GUI
slice requests the package route at the exact `1440x1000@1` viewport and only
reports success when the harness writes a verified seven-frame package
manifest:

```console
nix shell .#backend --command cargo run --locked --offline \
  -p backend-performance-tests --bin integrated -- \
  --profile smoke --output tests/performance/results/integrated-smoke.json \
  --gui-bin /absolute/path/to/backend-desktop-gui-harness
```

Use `--profile full` for 15 measured repetitions instead of the three-sample
CI smoke profile. Compilation and dependency setup happen before the runner's
measured phases. Each JSON `wall` object reports nanoseconds at p50/p95/p99;
`cold_durable`, `cold`, and `cold_restart` are cold phases, while
`warm_in_memory`, `warm`, and `warm_overlay` are warm phases. Ingest and delta
records carry source bytes, durable output bytes, CAS reuse, rebuilt rows, and
immutable roots. Search records cover exact, prefix, and full-text queries at
1/8/32 concurrent readers and retain cursor/root assertions. Turso records
database and pack sizes, reopen/no-op reuse, and one-package publication.

When Nix corpus variables (`NUDOX_*_CORPUS_DIR`) are set, the runner uses those
real multilingual trees. Otherwise it uses this workspace's checked-in source
tree, filters files above the canonical 32 KiB relation-row bound, and caps the
large class at 256 files or 4 MiB for a deterministic smoke duration. The
artifact names that source kind explicitly; the fallback is a real source-tree
corpus, not a mock.

Network singleflight, registry/advisory traversal, and GUI subjourneys are
reported as typed `unavailable` records when their production transport or live
workspace is not configured. `null` allocation fields mean the current public
Rust boundary does not expose an allocation counter. The runner samples its
own macOS/Linux process RSS and CPU time with `ps`; GUI child-process resources
are not folded into those host totals.

# backend_1 comparison — 2026-09-21

This is a read-only comparison of the current backend against
`/Users/mileswirht/Downloads/backend_1`. The old checkout was not modified.
The comparison worktree is `codex/old-backend-comparison-v5`, based on current
HEAD `b30ff3741`.

## Result

There is no defensible end-to-end old/new latency comparison on this host yet.
The current product has a measured v3 smoke artifact; `backend_1` has neither a
runnable product artifact nor a buildable Cargo root at its current tracked
HEAD. Its checked-in root manifest stops Cargo before dependency resolution.

Reporting an old ingest/search/restart number would therefore be fabricated.
The old source still documents the equivalent surfaces and contains substantial
storage/index tests, so it remains valuable as a behavioral reference. A true
A/B run needs either a repaired, read-only old snapshot or a prebuilt old binary
with a pinned corpus.

## Reproduction and host limits

Commands run from the comparison worktree:

```text
df -h /Users/mileswirht/Downloads
du -sh /Users/mileswirht/Downloads/backend_1
cargo metadata --offline --no-deps --format-version 1
find /Users/mileswirht/Downloads/backend_1 -type f \
  \( -path '*/target/*' -o -path '*/buck-out/*' -o -path '*/buck2-out/*' \) \
  -perm -111 -print
```

At measurement time the volume had 14 GiB free. The comparison task therefore
did not start a cold build. This is below the 20 GiB reserve required by the
agent build policy, and the old graph includes vendored DoltLite, GPUI, iroh,
and semantic producer dependencies that would make a cold build an unsafe
experiment under this reserve.

The old checkout is also dirty independently of this task: 32,835 tracked
changes (mostly deleted `.ci-cache` files) and 10 untracked paths. The source
and manifest claims below use tracked `HEAD` where possible and call out
working-tree observations separately.

## Exact old-tree blocker

`backend_1` is at `1db1d688331972509e872da792552d48a0c3a8df`.
`cargo metadata --offline --no-deps --format-version 1` fails with:

```text
failed to load manifest for workspace member
.../backend_1/workspace/index
failed to load manifest for dependency `nudox-ir`
failed to read .../backend_1/workspace/ir/model/Cargo.toml
No such file or directory
```

The root manifest names these members:

```text
workspace/compiler/sandbox
workspace/ir/vcs
workspace/transport
workspace/ir/model
workspace/compiler/languages
workspace/nudox-engine
```

but the tracked tree stores the IR crates at
`workspace/compiler/ir/{model,vcs}` and transport at
`workspace/heart/transport`. The same stale paths occur in
`workspace/index/Cargo.toml` dependencies. This is a pre-existing old-tree
integrity failure, not a comparison-worktree edit.

The old checkout has no product executable or Cargo target to reuse. Its
`.local-backends/serve.pid` and `embeddings.pid` contain PIDs `43292` and
`21032`, respectively, but neither process is alive. The old `.local-backends`
directory contains runtime evidence, not a runnable binary: a 780 KiB
`catalog/catalog.dolt`, several 76–144 KiB scratch SQLite directories, 60 MiB
of screenshots, and a 1.4 GiB `serve.log`.

## Temporary replay attempt

After the read-only inspection, a temporary shared-object clone was created at
`/Users/mileswirht/Downloads/nudox-qa-lanes/backend1-replay-v5` from the old
tracked commit. Only that clone received path repairs for the stale locations
listed above; `backend_1` itself still has the same 32,845-file dirty status.
The repaired clone is 566 MiB and `cargo metadata --offline --no-deps` now
passes, enumerating nine packages and the expected `ingest` and `nudox-serve`
binaries.

The first lean product check was:

```text
nix shell 'git+file:///Users/mileswirht/Downloads/backend#luna-tools' \
  --command cargo check --offline --locked -p index \
  --no-default-features --features test-engine --lib
```

It stopped before compilation because the old lockfile pins `grit-lib` to Git
revision `dfb079967b9cbc99e533c21e65f674bb3f5e8b07`, which is not present as a
Cargo checkout in the offline cache. A locked online `cargo fetch --locked`
was then attempted in the temporary clone. It began updating the crates.io
index and Grit repository but remained blocked on Cargo's global package-cache
path while other workspace agents were compiling. It was terminated before
compilation to avoid serializing those builds; no old benchmark result was
recorded from that partial attempt. At that point free space remained above
30 GiB.

This is a stronger result than a generic "old code unavailable": the old graph
is metadata-valid after isolated path repair, but its actual product dependency
closure is not locally reproducible without fetching the pinned Grit source and
then building a large DoltLite/transport graph. The eventual A/B runner should
reuse this temporary replay shape after the shared package cache is idle, or
use a prebuilt old binary, while preserving the same corpus and process metrics.

## Current measured baseline

The latest available checked-in v3 smoke artifact is
`/tmp/nudox-index-benchmark-v3-final3.json` and its Markdown rendering is
`/tmp/nudox-index-benchmark-v3-final3.md`. It was produced at commit
`22196f14b3ecf8adb345d88bbfe492b7513b898a`, before the latest integration
commits on this comparison branch, so it is a baseline rather than a claim
about the exact current HEAD.

Host: Apple M3 Pro, 11 CPUs, 36 GiB RAM, macOS/aarch64, Rust/Cargo 1.97.1.
Peak process RSS was 324,550,656 bytes and peak file descriptors were 11.
The large corpus was 256 real source files, 1,997,397 bytes, across seven
languages.

| Surface | Current v3 smoke measurement | Correctness/evidence |
| --- | ---: | --- |
| Tantivy durable ingest + reopen | 4,203,400,542 ns p50 | cold source build and durable reopen passed |
| Tantivy warm in-memory build | 3,282,112,666 ns p50 | warm builds passed |
| Tantivy exact search | 8,055,625 ns p50 | 256 results |
| Tantivy prefix search | 12,273,041 ns p50 | 256 results |
| Tantivy full text | 14,528,291 ns p50 | 256 results |
| Turso cold open/synchronize | 164,474,083 ns p50 | 257 rows |
| Turso warm no-op synchronize | 113,375 ns p50 | zero-op reuse |
| Turso restart recovery | 9,942,458 ns p50 | no-op recovery |
| Turso one-package delta | 3,301,292 ns p50 | one delta, 28,840 bytes added |
| Turso catalog read | 1,355,458 ns p50 | 16 rows |
| Dependency projection rebuild/reuse | 1,237,958 ns p50 | one projection row |
| Fresh process lifecycle | 22,281,291 ns p50 | successful exit |
| Graceful restart | 9,691,166 ns p50 | successful exit |
| SIGKILL lifecycle | 13,166,500 ns p50 | signaled process recovered |
| Offline restart after SIGKILL | 2,825,084 ns p50 | reused two rows |

The same artifact records one real failure: concurrent catalog readers/writer
returned `database is locked`. Registry and advisory lanes were unavailable in
that run, and GUI was not included. Those are open validation issues, not
silently successful comparisons.

## What the old product actually exposes

The old source gives us a useful capability map even though it cannot be
executed here:

| Surface | Old implementation evidence | Executable here? |
| --- | --- | --- |
| Registry/catalog ingest | `workspace/index/ingest/{driver,enumerate,git,homebrew}.rs`; `workspace/index/tests/ingest_pipeline_e2e.rs` | No; root manifest fails |
| Versioned catalog | `workspace/index/Cargo.toml` defaults to `dolt-engine`; `workspace/index/store`, `schema`, `migrations` | No; no old binary/target |
| Text search | `workspace/index/runtime/text` and `workspace/index/search` | No; no old binary/target |
| Graph/vector | `workspace/registry/{graph,vector}` and index server search tests | No; external services/binary absent |
| Local-first engine | `workspace/nudox-engine/src/{store,graph,search,mcp,versions}` | No; root path failure |
| GUI | `workspace/gui/src/{app,bridge,motion,stores,views}` and GPUI screenshot tests | No; no GUI executable |
| MCP/CLI | old server and GUI manifests document remote/local surfaces, but no built launcher remains | No |

The old tree has 902 Rust files under `workspace` (excluding vendored code and
embedded worktrees). The principal old surfaces contain approximately 77,954
Rust lines in `index`, 90,054 in `nudox-engine`, 49,140 in `gui`, and 17,954
in `registry`. The current tree has a deliberately broader split: 1,402 Rust
files under `crates` and `apps`; the main engine/semantic/application surfaces
contain approximately 263,621 Rust lines. These counts are architectural
context, not quality or speed scores.

## Old storage observations (not an A/B result)

The old runtime directory can be inspected without changing it:

```text
.local-backends/catalog/catalog.dolt             780 KiB
.local-backends/nudox-data/scratch.sqlite        13 pages × 4096 = 53,248 B
.local-backends/serve.log                        1.4 GiB
```

The scratch database contains only ephemeral `claims`, `jobs`, `sessions`, and
`wanted` tables. It is not the old catalog and cannot be compared to the current
257-row Turso catalog. The Dolt file is a directory-backed versioned catalog,
but the old engine that opens it is not available as a binary and its content
was not generated from the current benchmark corpus. These values are retained
to prevent accidental future claims of comparability.

The old server logs do provide an operational warning: the preserved verification
logs repeatedly report failed Pyroscope uploads and the large historical log
contains the old telemetry failure history. Those logs are not latency samples,
but they are relevant to production readiness and should be included in a later
old-binary replay rubric.

## Historical old performance evidence

`backend_1/workspace2/PERFORMANCE_DEEP_DIVE.md` contains older, explicitly
superseded microbenchmarks, including a 100,000-row root construction result of
13.6 MB peak / 6.4 MB retained for the owned path and 6.4 MB / 6.4 MB for the
streaming path, with medians of 18.25 ms and 19.04 ms. It also records SIMD
locality and object-pack experiments. The document explicitly says these are
not an end-to-end 10× system result and that the executable authority is the
layout lab, not the historical prose. They should remain design evidence only;
they are not substituted into the current ingest/search table.

## Required follow-up for a real A/B run

1. Preserve the old checkout and make a temporary read-only source snapshot at
   a known commit, repairing only the temporary manifest paths listed above.
2. Use the same Rust 1.97.1 shell, one pinned corpus (256 files / 1,997,397 B
   and the seven-language matrix), and identical process metric collection.
3. Run old `ingest`/`nudox-serve` plus the old GUI/MCP paths from that temporary
   snapshot, recording cold start, warm query, durable reopen, restart, RSS,
   FDs, catalog bytes, text-index bytes, and request counts.
4. Keep engine comparisons separate: old DoltLite versus current Turso is a
   storage-engine comparison, while old Tantivy versus current Tantivy can be a
   search comparison only after schema and corpus parity are proven.
5. Repeat the current v3 benchmark at the final integration HEAD after the
   concurrent-writer lock issue and unavailable registry/advisory/GUI lanes are
   resolved. Only then publish a ratio table.

This report intentionally marks unavailable values as unavailable and records
the old-tree failure instead of smoothing it into a false performance claim.

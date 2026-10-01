# Index tentpole measurements

These are observed values, not targets or extrapolations. Measurements run in
the separate `codex/index-compiler-tentpole` worktree with `nix shell
.#luna-tools`, Rust 1.97.1, and `CARGO_BUILD_JOBS=2` on an Apple M3 Pro with
36 GiB of RAM. The machine was not otherwise quiescent, so repeat runs and a
base-revision comparison are required before claiming a speedup.

## Live Turso ingest and physical cold restore, 2026-09-29

`tests/journeys/run-live-turso-backup.sh` acquired exact public pins
`serde@1.0.228` and `serde_json@1.0.145`, indexed their real source trees,
then used Turso's `VACUUM INTO` for both the projection and index-authority
databases. The restored daemon opened those physical main files without WAL
sidecars. The harness checked the exact schema-8 projection root
`858507FDF732B91130ABC9864C87A3C966AA8D7F55B88C2B11D59A0B06BE3709`,
row digest, graph witness, edge-row digest, and authority rows across the
backup/restart boundary. It found 4,355 projection rows and 17 dependency
edges; warm and cold package search, registry search, dependencies, and
dependents returned the pinned packages. The cold Tantivy rebuild also
exercised distinct Go module coordinates differing only by case, which a
folded sort key had previously collapsed.

The cold-restored daemon stayed warm during 20 measured calls per path. Each
call launched a fresh CLI process, so these timings include CLI startup and
IPC; they are not in-process index latency. Three calls warmed each path.
The measuring machine was also running other builds and tests.

| Query path | p50 / p95 |
| --- | ---: |
| Tantivy package search, 20 rows | 52.296 / 56.592 ms |
| Registry/discovery index search, 20 rows | 355.983 / 493.616 ms |
| Forward dependencies, 15 rows | 30.050 / 35.437 ms |
| Reverse dependents, 1 row | 29.460 / 34.575 ms |
| Direct Turso graph forward, 2 rows | 12.156 / 13.326 ms |
| Direct Turso graph reverse, 2 rows | 10.173 / 12.586 ms |

The registry/discovery path is materially slower than package search in this
run and remains an optimization target. These results establish a real cold
baseline, not a before/after speedup or a large-corpus throughput claim.
The full local evidence is under
`.local/live-turso/20260929T025610Z-62613/artifacts/restore/`.

An isolated copy of that restored owner was sampled on 2026-09-29 without a
new build. The first search rebuilt in-memory discovery and local-declaration
Tantivy indexes; the cold stack spent substantial time adding declaration
documents and committing the writer. A separate warm search sample spent most
owner samples in ranked release-facet queries repeated for selected lineages.
These are sampling observations, not a controlled latency attribution or a
speedup measurement. One cold attempt while other builds ran returned an IPC
`operation would block` fault; a later cloned-owner run completed. The raw
profiles and fault are under `.local/search-profile-20260929T032223Z-4079/`
and `.local/search-profile-20260929T032040Z-2692/`. Next compare cold-open,
owner-only warm search, and new-CLI-process latency on the same saved owner,
with exact result and cursor parity, before claiming an optimization.

## Package graph projection, 2026-09-28

Command: `cargo bench -p backend-extension-turso --bench package_graph --offline`.
The release-mode fixture has 512 dependency edges, four warmups, and 32 timed
samples for each path. Fact construction and the cold publication are outside
the timed region. The source worktree starts from `92f974b37`. The current
path prepares an immutable, checked fact snapshot outside the timer, then
reuses its cached witness for exact reuse, root-only moves, and changed-fact
detection.

| Path | Before checked snapshot median / p95 | Checked snapshot median / p95 |
| --- | ---: | ---: |
| Exact root and facts reuse | 0.716 / 1.226 ms | 0.021 / 0.082 ms |
| New root, identical facts | 0.861 / 1.255 ms | 0.088 / 0.188 ms |
| Same root, one changed edge | 1.578 / 2.117 ms | 0.590 / 1.264 ms |

The unchanged base revision (`92f974b37`) was measured separately in
`/Users/mileswirht/Downloads/backend` with the same release command and
fixture. It reported a root-only move median/p95 of **0.284/0.724 ms** and a
one-edge edit median/p95 of **1.506/3.027 ms**. The base edit fixture changed
the root on every sample, while the checked-snapshot fixture holds the root
fixed; those edit timings are **not** a like-for-like speed comparison. The
initial witness path fixed a correctness bug—base code reused by root alone
and missed changed facts under the same root—but regressed root-only moves by
hashing all 512 facts on every call. The checked snapshot removes that
repeated work. Its comparable root-only move median is 0.088 ms against the
base's 0.284 ms. These are one machine's separate runs, not a controlled
throughput or concurrency result. The snapshot is constructed outside each
timed call, so these numbers represent repeated syncs of an already admitted
fact set; snapshot construction cost needs its own measurement when the ingest
pipeline can supply new facts.

## Persistent compiler input tree, 2026-09-28

The release benchmark built a deterministic metadata-only fixture with 100,000
files and 1,801 directories (101,801 Merkle pages). It ran 101 timed samples,
two warmups, and one/four/sixteen concurrent read lanes on the same M3 Pro.
The independent range-minimum reference reconstructed both complete roots and
the exact changed-page set; three deliberately wrong deltas were rejected.
This measures page-tree construction and delta calculation, **not** scanning,
rehashing source bytes, CAS writes, compiler execution, or network transfer.

| Operation | p50 / p95 / p99 | Median measured allocations | Median allocated bytes |
| --- | ---: | ---: | ---: |
| Full rebuild after one file edit | 266.012 / 378.625 / 413.942 ms | 1,571,857 | 151,381,111 |
| Immutable same-key path-copy update | 0.0264 / 0.0296 / 0.0309 ms | 198 | 31,238 |
| Delta from immediate base | 0.000417 / 0.002916 / 0.011458 ms | 8 | 2,384 |

The one edit changed 24 of 101,801 pages and required 3,710 encoded page bytes;
101,777 pages retained identical identity and shared the prior `Arc` subtree.
The measured construction median is about 10,070× lower for path copying than
for a full tree rebuild, but this is **not** an end-to-end ingest speedup: the
current capture path still reads and hashes all source files, and the production
retained-capture caller is being integrated separately. The benchmark's
current-process RSS snapshots were 230.2 MiB after building the base tree and
321.5 MiB while retaining base, a full rebuilt tree, and the persistent update.
`/usr/bin/time -l` reported 339.9 MiB maximum resident set size for the direct
benchmark process. Allocation counts cover only the measured current-thread
closure; they exclude fixture setup and concurrent reader allocations.
Raw output: `/tmp/nudox-compiler-tree-bench-run-1.log` on the measuring host.

## Immutable S3 pack loopback, 2026-09-28

The `backend-store-s3` library test built a deterministic pack of 1,024
independently addressed roughly 8 KiB objects. It asserted one conditional
PUT of **8,925,314 bytes**, followed after a cold route reopen by three range
GETs: the 410,754-byte canonical manifest and two 8,315-byte complete object
envelopes. The four requests transferred **9,352,698 application bytes** in
total. The reader checked the manifest identity, per-object Merkle inclusion,
exact HTTP 206 ranges, and typed object admission; it did not trust ETag as a
hash. The test also asserted resident pack metadata below 4 MiB and passed
with both one and two Rust test threads. These are loopback protocol and byte
counts, not AWS latency, billed-byte, compression, or peak-RSS measurements.

## Retrieval amplification and latency, 2026-09-28

Command: `cargo test -p backend-store-s3 --lib retrieval_bench::retrieval_benchmark_reports_latency_bytes_and_amplification --offline -- --ignored --nocapture`.
The deterministic run completed 21 samples per lane in 234.86 seconds. The
S3-compatible service was an in-memory TCP loopback server; the OS page cache
was not flushed. Cold local numbers include a new process and verified object
reopen, while cold S3 numbers include pack-route reopen. These are not AWS
latencies or controlled side-by-side throughput results. Raw output is in
`/tmp/nudox-s3-retrieval-bench-current-1.log` on the measuring host.

| Payload / query | Local cold p50 | Local warm p50 | S3 loopback cold p50 | S3 loopback warm p50 | Cold S3 read amplification |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB object, complete | 5.01 ms | 0.020 ms | 24.26 ms | 12.59 ms | 1.326× |
| 64 KiB object, complete | 8.29 ms | 0.895 ms | 27.96 ms | 13.47 ms | 1.005× |
| 1 MiB object, complete | 64.94 ms | 14.29 ms | 186.81 ms | 146.50 ms | 1.000× |
| 16 MiB object, complete | 954.27 ms | 227.97 ms | 2,696.75 ms | 2,197.43 ms | 1.000× |
| 1,000-object pack, 32-byte point | 5.61 ms | 0.057 ms | 150.79 ms | 57.21 ms | **12,571×** |
| 1,000-object pack, eight 32-byte records | 37.65 ms complete | 0.488 ms complete | 548.93 ms complete | 455.49 ms complete | **1,603×** |

The 1,000-object pack's cold point query read **402,277 bytes** to return
32 bytes; its warm query read **1,147 bytes**. The paged layout reduced the
directory read, but a point lookup then required serial root, page, and object
requests. That selective policy's 21-sample run measured cold/warm point p50
of **157.48/101.47 ms** and cold/warm eight-record range p50 of
**906.33/508.39 ms**. It read **8,979 bytes** for a cold point and
**34,948 bytes** for a cold range. Its final concurrency lane failed in the
loopback fixture, so those point and range observations are valid only for the
lanes that completed before that failure. The raw selective output is
`/tmp/nudox-s3-retrieval-bench-paged-1.log` on the measuring host.

### Paged layout with cached pages and explicit coalesced reads

Command: `cargo test --offline -p backend-store-s3 --lib retrieval_benchmark_reports_latency_bytes_and_amplification -- --ignored --nocapture`.
This 21-sample run completed in 232.22 seconds. The S3-compatible service was
an in-memory TCP loopback server and the OS page cache was not flushed. Raw
output is `/tmp/nudox-s3-retrieval-bench-paged-coalesced-1.log` on the
measuring host. The 1,000-object pack used the explicit, opt-in coalesced
directory policy: one bounded root-plus-page-prefix GET (87,688 bytes), then
one complete-envelope GET for a point query or eight parallel complete-envelope
GETs for the document range. The selective policy remains the route default.
The per-pack page cache holds only proof-verified pages, and the batch reader
caps active range requests at 16.

| 1,000-object query | Cold first-byte p50 / p95 | Cold complete p50 / p95 | Warm first-byte p50 / p95 | Warm complete p50 / p95 | Cold / warm response bytes | Cold / warm GETs per query |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 32-byte point | 95.07 / 98.54 ms | 95.19 / 98.77 ms | 47.47 / 51.03 ms | 47.69 / 51.14 ms | 88,835 / 1,147 B | 2 / 1 |
| Eight 32-byte records | 344.21 / 353.70 ms | 345.03 / 354.33 ms | 296.40 / 301.07 ms | 297.01 / 301.66 ms | 96,864 / 9,176 B | 9 / 8 |

Compared with the paged selective run, this loopback run lowered cold/warm
point p50 by about 40%/53% and cold/warm range p50 by about 62%/42%. The
coalesced cold point transferred 9.9 times as many bytes as a selective page
lookup, and the cold range transferred 2.8 times as many; it still transferred
4.5 times fewer cold point bytes than the original full-manifest route. Warm
point bytes were unchanged, while warm range bytes fell by about 3.8 times
because each checked page is cached and the batch reads only the eight complete
envelopes. This is a measured latency-versus-bytes tradeoff on one machine and
one loopback server, not an AWS or universal speedup claim. The earlier
benchmark reports peak RSS and allocator counts as unavailable; this rerun
does too, so it provides no runtime memory-performance conclusion.

# Index-fabric benchmark rubric

This folder contains release run wrappers, a hand-authored pinned-package
smoke QREL, and a scorer. No new benchmark result has been collected. The V2
Merkle benchmark source is checked in at
`crates/engine/benches/compiler_input_tree_v2.rs`, but has not been compiled or
run. Every wrapper refuses Cargo until the root grants a build slot, uses an
explicit existing `CARGO_TARGET_DIR`, serializes lanes, and caps Cargo
compilation jobs at four. Do not make another target directory while the
shared disk is low.

## Existing measurements and their limits

`backend-store-s3`'s ignored `retrieval_bench` test already covers deterministic
FileStore and S3-pack workloads. It measures FileStore cold child-process reopen
and warm readers, S3 cold route reopen and warm packs, point/range retrieval,
and synchronized 1/4/16-reader batches. The test reports nearest-rank p50/p95/p99,
application bytes per read, storage amplification, request counts, PUT and GET
body bytes, and one peak RSS sample for the test process. Its byte oracle varies
with offset, so returning the beginning of an object or seeking one byte off
does not satisfy the check. It never flushes the operating-system page cache.
The S3 backend is an in-memory server over real loopback TCP: its request and
response body counts exclude HTTP/TLS/wire overhead and do not predict remote
AWS latency, billed bytes, or service behavior.

The Iroh/Bao two-process test measures 1 MiB and 16 MiB objects, cold full
transfer, interruption and resume, per-range latency, application payload
bytes, Bao stream bytes, outboard construction, and retained storage bytes.
The client uses real independent processes and direct loopback QUIC. It reports
median/p95 for per-range samples, but not p99, process RSS, or QUIC/IP wire
bytes. `ServeMetrics::bao_stream_bytes` explicitly excludes control frames and
transport headers. Preserve those as unavailable; do not estimate them from
payload bytes.

The current `search_corpus` bench times admission and reuse of a synthetic
2,048-package corpus, and source-page reuse. It has no relevance judgments,
100k-file update, or release-freshness oracle. The seven-language pinned Nix
corpus in `.config/nix/corpus-pins.json` is a suitable source pool, but indexing
that corpus alone does not produce independent search judgments. The files in
this folder establish a smoke QREL using exact, immutable coordinates from the
pinned manifest. It proves that all seven lanes can be mapped and scored; it is
not a promotion-grade relevance set. Most queries have one positive judgment,
so they do not meaningfully compare ranking quality, and the version-order
judgments are explicitly only comparisons within a pinned pair. The qrels are
not generated from search results. Query adapters must map returned package
identities back to those coordinates without consulting the current rank.

`search-qrels.tsv` includes two-version comparisons for attrs, Go x/sync,
Jakarta Servlet, and the NuGet ESP.NET source package. These are immutable
archive-version checks, not registry freshness or yank observations. A
freshness promotion run still needs a frozen registry observation containing
`observed_at`, release publication time, yank state, and source authority for
every judged version. A live query or current package page cannot serve as a
reproducible historical oracle.

## Run after a build slot is granted

Use a result directory that already exists outside the source files (for
example, an existing ignored `.local` evidence directory), and one of the
existing slot target directories. The script hashes every tracked and
nonignored workspace file before and after each lane, records the Git status,
commit, host, Rust/Cargo versions, target, and sample count, and rejects a run
if a source file changes mid-run.

```sh
BENCH_BUILD_SLOT_GRANTED=1 \
CARGO_TARGET_DIR=/absolute/path/to/an/existing/slot-target \
BENCH_CARGO_BUILD_JOBS=1 \
BACKEND_STORE_RETRIEVAL_SAMPLES=101 \
docs/benchmarks/run-existing-release.sh /absolute/path/to/an/existing/results-directory
```

That wrapper executes these release-mode Cargo commands serially inside
`nix shell '.#luna-tools'`:

```sh
cargo test --locked --offline --release -p backend-store-s3 --lib retrieval_bench -- --ignored --nocapture
cargo bench --locked --offline --release -p backend-local-service --bench search_corpus
cargo test --locked --offline --release -p backend-cluster-transport --test two_process reports_two_process_cold_and_interrupted_resume_costs_for_one_and_sixteen_mib -- --nocapture
```

The standalone V2 Merkle lane measures deterministic baseline and one-edit
full tree construction at 100,000 files, warm no-op and one-edit deltas, and
concurrent immutable delta readers at 1/4/16. It reports nearest-rank
p50/p95/p99, allocation counts/bytes, encoded page bytes, page/delta/visited
counts, declared source lengths, and process RSS snapshots. An asymmetric small
reference tree uses a separate encoder/treap/hash implementation to calculate
expected changed page IDs; the harness also checks that skipping the edit gives
the wrong root. The large fixture is metadata-only: it does not read 100,000
physical files or exercise capture, FileStore, or network transfer. The current
API exposes concurrent immutable reads, so no concurrent capture/write claim
is made.

```sh
BENCH_BUILD_SLOT_GRANTED=1 \
CARGO_TARGET_DIR=/absolute/path/to/an/existing/slot-target \
BENCH_CARGO_BUILD_JOBS=1 \
BENCH_TREE_FILES=100000 \
BENCH_TREE_SAMPLES=101 \
BENCH_TREE_WARMUPS=2 \
docs/benchmarks/run-v2-tree-release.sh /absolute/path/to/an/existing/results-directory
```

The wrapper records the source fingerprint, Git state, CPU/memory/OS, Rust and
Cargo versions, profile, target, sample count, and exact command. It rejects
results if tracked or nonignored workspace source changes during the run. Its
RSS values are post-stage resident-set snapshots, not a process high-water
mark; report allocator peak-live bytes separately from any external RSS peak
and name the measurement scope.

For the larger production integration lane, the checked-in command is:

```sh
CARGO_TARGET_DIR=/absolute/path/to/an/existing/slot-target \
nix shell '.#luna-tools' --command cargo run --locked --offline --release \
  -p backend-performance-tests --bin integrated -- \
  --profile full --require-complete --output /absolute/path/to/an/existing/results-directory/integrated-full.json
```

The integration runner's fallback is the checked-in workspace tree and is
explicitly incomplete for promotion. Set all seven pinned `NUDOX_*_CORPUS_DIR`
variables from the same Nix corpus generation before claiming the real-corpus
lane. It currently caps the large source class at 256 files / 4 MiB and is not
a physical 100k-file update benchmark.

The scheduler placement lane is **not yet implemented and cannot be run**. No
benchmark target or public production-counter snapshot exists. Once the target
and counters land, the planned command is:

```sh
CARGO_TARGET_DIR=/absolute/path/to/an/existing/slot-target \
nix shell '.#luna-tools' --command cargo bench --locked --offline --release \
  -p backend-execution --bench compiler_cluster_placement -- \
  --workers 1,2,4,8 --files 100000 --edits 0,1 --session warm,cold
```

This is a proposal only; do not run it until the target exists and a build slot
is granted. Instrumentation still needed includes active/local/remote slots,
per-peer CPU/memory/transfer-credit use, queue wait samples and maximum age by
demand, selected route/cost, transferred and reused bytes/page IDs,
queue-to-start and end-to-end latency, retry/backoff and peer-incarnation
resets, hedge duplicate CPU/results, stale-completion rejections, fairness
maximum wait, and process RSS. Report p50/p95 only after collecting samples.

## Freeze and comparison rules

Freeze one candidate source snapshot before timing. Record the Git commit plus
the wrapper's full-file source fingerprint, `Cargo.lock`, `.config/nix` corpus
pin hashes, benchmark source hashes, qrels hash, Rust version, profile, target
triple, CPU model/count, RAM, OS, and any unrelated load. Do not edit the source,
QREL, corpus manifest, or lockfile between repetitions. Use the same seed,
query order, and sample count in every comparison. Report failures and missing
lanes beside successful measurements.

The user-named baseline is `/Users/mileswirht/Downloads/backend_1` at
`1db1d688331972509e872da792552d48a0c3a8df`; the working tree currently has
many deleted `.ci-cache/cargo/registry` files, so do not repair or build from
that checkout in place. The unrelated `/Users/mileswirht/Downloads/backend`
checkout is active GUI work and is not the baseline. Once storage and target
slots are available, obtain an immutable baseline source snapshot at the named
commit and use the same already-existing target cache where Cargo permits.
Record whether the two lockfiles match; if they do not, pin and report their
dependency graph separately rather than treating them as one Cargo build.

For the public lib.rs Rust-package ranking comparison, pin the mirror source to
[`4642a01664e14f4ae30a3804a55556b0770119d9`](https://github.com/Protryon/lib-rs-mirror/tree/4642a01664e14f4ae30a3804a55556b0770119d9).
The reference query uses conjunction by default, searches crate name,
keywords, description, and README with field boosts, overfetches before
reranking, and applies an exact-name bonus before truncation. Its package
quality score also sees release history, Cargo metadata, README structure,
code size, owners, and traffic. Compare both rankers on the exact same pinned
Cargo package/release documents and QREL. Run a common-field lane using only
fields that both systems receive; run the fuller lib.rs score as a separately
labeled system with its additional inputs listed. Never count those additional
fields as a cross-ecosystem algorithm advantage. Report recall@1/5/10, MRR,
nDCG@1/5/10, and missing/unknown metadata rates. Score only the Cargo subset
for lib.rs; report cross-ecosystem results separately because that baseline
does not cover the other six source lanes.

Use `score-qrels.py` on a ranked TSV with header
`query_id<TAB>rank<TAB>document_id`:

```sh
python3 docs/benchmarks/score-qrels.py /path/to/results.tsv --k 1,5,10
```

The scorer computes per-query recall, reciprocal rank, and nDCG from the
committed static judgments. Keep raw ranks as well as summary metrics. Review
the judgments from source/package facts independently of both result lists;
new aliases, typos, descriptions, or semantic questions need source-backed
relevance grades and a recorded second review before the benchmark run.

The pinned public lib.rs source is a reference contract, not a runnable
benchmark adapter. No same-corpus document exporter or query runner is checked
in, and the release wrapper does not build that historical ranker. Before
comparing it, implement an adapter that consumes the exact same frozen Cargo
package-version documents and queries, emits the ranked TSV above, and has
independent fixed-order tests for conjunction, field boosts, exact-name bonus,
and keyword diversity. Run a common-fields mode and a separately labeled
full-metadata mode; the latter must identify README/code/owner/traffic inputs
that discovery metadata does not contain.

## Required measurements not yet measured or fully implemented

The following cells remain open; lower-level test timings are not substitutes.

| Lane | Required result | Independent oracle / falsifying mutation |
| --- | --- | --- |
| 100k-file one-edit | The source-only `compiler_input_tree_v2` bench covers a deterministic 100,000-file metadata fixture, baseline/no-op/one-edit tree builds and deltas, 1/4/16 immutable readers, latency quantiles, allocator counts/bytes, page work, encoded bytes, declared bytes, and post-stage RSS snapshots. It is not yet compiled or run and does not read physical files, exercise capture/CAS, or measure a true process RSS high-water mark. The production lane still needs pinned physical inputs, cold/warm filesystem measurements, changed durable bytes, files visited, full-rebuild comparison, and end-to-end peak RSS. | The small asymmetric reference tree independently computes page preimages, BLAKE3 path priorities, typed page IDs, and expected delta page-ID order without reading the implementation tree. A BTreeMap oracle confirms the 100k fixture changes exactly one path. The harness verifies skip-edit, empty-delta, and full-tree-transfer mutations fail against the fixed expected root/page IDs/bytes. |
| Cross-ecosystem search | Frozen per-version corpus across Rust, TypeScript, Python, Go, Java, C#, and Clang. Extend `search-qrels.tsv` with exact-name, alias, typo, description, multi-result, local/forge-only, and adverse/yanked-version questions. Score recall@k, MRR, nDCG@k, freshness/yank errors, cold/warm p50/p95/p99, index bytes, and incremental update cost. | Human/source adjudication precedes ranking runs. Include asymmetric query/document names and one plausible irrelevant near-match. Mutations remove a metadata field, hide a lane, or float the irrelevant result above a relevant one and must worsen a metric or fail an asserted top result. |
| Multi-version freshness | Publish at least two versions for each chosen package, then advance one version, rename/remove a symbol, mark an older version yanked, and refresh only the registry observation. Measure query visibility before/after cold reopen and observation-only refresh. | A frozen registry observation with exact `observed_at`, source, version, yank state, and release time. Mutations keep the previous projection head or ignore a yank; the test must observe a stale result or freshness error. |
| Scheduler placement | 1/2/4/8 workers; 100k-file unchanged and one-edit jobs; warm/cold workers; queue-to-start and end-to-end p50/p95; transfer/page reuse; peak RSS; fairness max wait; retries, hedge cost, and stale-completion rejection. | Fixed seeded jobs with independently expected assignments, costs, roots, bytes, and deadlines. Mutate worker availability, reorder queue admission, replay a stale completion, and force one retry; the oracle checks fairness and the selected root. |
| Public lib.rs ranker | Same Cargo corpus, same queries, same judged documents, and same candidate limit. Measure ranker latency and relevance; keep common-field and richer-metadata runs distinct. | Pin the exact upstream source commit and reproduce expected ordering on hand-ranked cases before comparing it with current search. Mutate conjunction behavior, exact-name bonus, or keyword diversity and require the fixed oracle to catch it. |

No performance claim should combine loopback S3 body bytes with remote object
store bills, Bao payload bytes with wire bytes, or a synthetic search-corpus
admission time with user-visible query latency.

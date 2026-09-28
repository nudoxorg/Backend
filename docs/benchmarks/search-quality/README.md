# Search quality and cost benchmark

This benchmark compares a search adapter against a frozen package corpus and a hand-authored relevance file. The QREL judgments were written from package identity, pinned archive, manifest, advisory, registry-index, and explicit fixture facts before any search result file was scored. No expected order is generated from either the candidate or `lib.rs` ranking implementation. Grade 0 marks an explicit distractor or stale release; grades 1–3 express decreasing relevance. A missing document is treated as grade 0.

The primary lane has 25 real package/source identities from the repository's pinned Nix corpus: Cargo, npm, PyPI, Go, Maven, NuGet, and two Clang forge-only packages. Each pin retains its exact archive URL and declared archive hash; archives were not fetched for this benchmark. The forge pins also retain a repository/ref coordinate derived from the pinned GitHub archive URL, but no commit, manifest, or forge API metadata is inferred. The lane includes four historical Cargo release rows from captured crates.io sparse-index records. `source-snapshots.json` pins the Nix manifest, an immutable captured copy of the workspace lockfile, local RustSec advisory, local fixture sources, and the three raw crates.io index files by SHA-256. The source capture time is `2026-09-28T06:58:07Z`; captured index bytes remain in `snapshots/`. Other ecosystems use exact package/archive pins from the Nix corpus rather than claims about current registry metadata. Only the captured Cargo index records have observed yanked state; unsupported fields remain explicitly `unknown`.

The 25 queries and 39 QREL rows cover exact identities in each ecosystem, within-candidate version freshness, Cargo dependency and advisory lookup, captured yanked releases, aliases, one-character typo, descriptions, fixture dependency/advisory/yank facts, and an unknown-metadata expected-empty query. Judgments are hand-authored from the cited frozen evidence and explicit asymmetric fixtures before any candidate run; a candidate's own ranking is never used to create or sort QREL rows. The six synthetic adversarial records live only in `adversarial-corpus.jsonl`; they are not mixed into the real primary lane. Queries about cross-ecosystem capabilities and the primary/adversarial split are documented separately in `ecosystem-capabilities.tsv`.

The dependency-field judgment uses `snapshots/workspace-cargo-lock-2026-09-28.lock`, a content-addressed copy captured at `2026-09-28T08:51:28Z` with SHA-256 `d90d48b58d9f339fbc6c4bb3666127194e12c5727d0e30d334bab782e9038fe6`. This immutable file is the QREL evidence; the captured `thiserror@2.0.20` package lists `thiserror-impl@2.0.20` as a dependency. A machine's current root `Cargo.lock` is excluded from source freezing so unrelated workspace dependency updates cannot rewrite the corpus or invalidate judgments; quality and scale run manifests record that live lock's hash and size as execution context only, before and after each run.

Freshness means ordering inside this frozen candidate set. The `attrs`, Go, Maven, and NuGet pairs only assert which of two pinned versions is newer; they do not assert that either is globally current. The serde freshness query additionally uses captured crates.io yank bits and treats the highest unyanked candidate as fresh. Security labels are package-level for the frozen RustSec fixture; this corpus does not claim version-specific exploitability. No download-count field or fabricated popularity value is present.

## Freeze and validate

From the repository root, run these commands without network access:

```sh
python3 docs/benchmarks/search-quality/freeze_corpus.py --check
python3 docs/benchmarks/search-quality/search_benchmark.py validate
```

`--check` verifies that the generated primary corpus and source manifest still match the pinned inputs. The manifest pins the production search core, feature bridge, adapter, relevant coordinate/facet parsers, manifests, frozen records, and fixture sources by SHA-256. The captured root Cargo.lock file is immutable evidence; the live root lock is only reported in each run manifest. To deliberately create a new freeze after changing source inputs, inspect the changes and run `python3 docs/benchmarks/search-quality/freeze_corpus.py --write`, then update and review any QREL provenance that refers to a changed source hash. The validator checks document/query identity, corpus separation, source references, qrel scope, version-pair direction, and mutation targets. It does not build an index or produce retrieval/performance claims.

## Adapter protocol and evidence

A real adapter command must implement these subcommands:

```text
ADAPTER build --corpus CORPUS.jsonl --index INDEX_DIR
ADAPTER serve --index INDEX_DIR --limit K
ADAPTER search-cold --index INDEX_DIR --query-json REQUEST.json --limit K
```

`build` consumes the combined primary and adversarial JSONL corpus. `search-cold` reads one JSON request. `serve` reads newline-delimited JSON on stdin and writes one JSON response per line on stdout. Search requests contain `op`, `query_id`, `query`, `category`, `corpus`, `scope`, `exclude_yanked`, and `limit`; successful replies contain `document_ids` in rank order. An update request contains `op: "update"` and one or more `{document_id, fields}` changes; reply with an update object. The harness checks that cold and warm rankings agree, that updates change the alias-only query as requested, and that three rebuild mutations remove results whose supporting alias, description, or forge lane was removed.

The in-repository Rust adapter is `adapter/`. Its feature-gated bridge calls the production `DiscoverySearchIndex` and uses its Tantivy query and incremental-sync paths; it does not contain a parallel ranker. Registry records are committed to the real discovery journal, then ranked source-coordinate keys are mapped back to canonical benchmark document IDs. The two forge pins enter through the index's explicit `open_with_forge` route as source-only facts: only the pinned URL, derived repository/ref coordinate, and declared SRI digest are indexed as generic terms; omitted forge metadata stays unknown. This exercises the actual search ranker while keeping forge admission separate from fabricated registry facts. Workspace manifest fixtures remain unsupported by this discovery index and are reported as skipped. The adapter's `index_bytes` is the logical size of Tantivy's in-memory managed files; `persistent_index_directory_bytes` separately counts the copied corpus, source journal, and adapter manifest on disk. There are no recorded candidate rankings or benchmark numbers yet.

The adapter integration tests exercise canonical-ID ranking, positive Tantivy index-byte reporting, alias removal through the production incremental sync path, and direct retrieval of a pinned forge source identity. Because `adapter/` is a standalone Cargo workspace, its first Cargo invocation creates `adapter/Cargo.lock`. After that first resolution, regenerate the source manifest so the lock is pinned, then use `--locked` for later builds and measurements. The exact commands are:

```sh
python3 docs/benchmarks/search-quality/freeze_corpus.py --check
python3 docs/benchmarks/search-quality/search_benchmark.py validate
cargo test --manifest-path docs/benchmarks/search-quality/adapter/Cargo.toml --target-dir /Users/mileswirht/.cache/nudox/cargo-1.97/build/slot-2
python3 docs/benchmarks/search-quality/freeze_corpus.py --write
python3 docs/benchmarks/search-quality/freeze_corpus.py --check
python3 docs/benchmarks/search-quality/search_benchmark.py validate
cargo test --locked --manifest-path docs/benchmarks/search-quality/adapter/Cargo.toml --target-dir /Users/mileswirht/.cache/nudox/cargo-1.97/build/slot-2
cargo build --locked --release --manifest-path docs/benchmarks/search-quality/adapter/Cargo.toml --target-dir /Users/mileswirht/.cache/nudox/cargo-1.97/build/slot-2
```

Only the first two commands are source-only. Run the Cargo commands after the repository owner grants the build slot. The first `cargo test` resolves and writes the standalone adapter lockfile; the subsequent freeze records its exact hash before the locked test and release build. The adapter binary produced by the release command is `/Users/mileswirht/.cache/nudox/cargo-1.97/build/slot-2/release/search-quality-tantivy-adapter`. The adapter's result can reveal unsupported QREL fields (for example, registry dependency facts are retained in the input but are not currently a search field); do not reinterpret such a miss as a successful capability.

Create the evidence directory first, and ensure it is git-ignored if it is inside the checkout. Once the repository owner grants the adapter build slot, the exact harness command is:

```sh
BENCH_BUILD_SLOT_GRANTED=1 python3 docs/benchmarks/search-quality/search_benchmark.py run \
  --adapter-command '/absolute/path/to/adapter' \
  --result-dir /absolute/path/to/ignored/evidence \
  --run-name search-quality-v1 \
  --samples 101 --seed 20260928 --limit 50
```

The gate is deliberate: `run` invokes adapter build/search commands, so this command must not be run until the build slot is granted. It records raw latencies, frozen input hashes, and the adapter executable path/hash, then writes ranked results, relevance metrics, mutation checks, host/runtime details, index bytes, and build/update timings. Cold latency includes a new adapter process, index open, and one search but does not flush the OS page cache. Warm latency includes JSONL pipe round-trip overhead. Incremental update measures alternating one-document, one-field alias removal/restoration. p50/p95/p99 use nearest-rank quantiles over at least 101 samples. No timing or quality result is checked into this folder.

## Synthetic scale lane

The scale lane is separate from the primary/adversarial QREL oracle. It deterministically derives 100k- and 1m-row indexes from the frozen record field schema, but every generated name, alias, description, keyword, and record is labeled synthetic; it carries no real package claims, hashes, archive URLs, downloads, or relevance judgments. It is for load and cost measurements only, and its probe queries check only that deterministic synthetic targets remain searchable. Generate inputs outside the checkout:

```sh
python3 docs/benchmarks/search-quality/generate_scale_corpus.py \
  --records 100000,1000000 --seed 20260928 \
  --output-dir /tmp/search-quality-scale-inputs
```

The generator refuses to replace existing `records-*` directories; choose a fresh output directory for another seed or generation run. Create the evidence directory before running the gated measurements:

```sh
mkdir -p /tmp/search-quality-scale-evidence
```

After the adapter has been built in the authorized build slot and `/tmp/search-quality-scale-evidence` exists, run:

```sh
BENCH_BUILD_SLOT_GRANTED=1 python3 docs/benchmarks/search-quality/scale_benchmark.py run \
  --corpus-dir /tmp/search-quality-scale-inputs \
  --adapter-command /Users/mileswirht/.cache/nudox/cargo-1.97/build/slot-2/release/search-quality-tantivy-adapter \
  --result-dir /tmp/search-quality-scale-evidence \
  --samples 101 --seed 20260928 --parallelism 1,4,16
```

This writes source hashes, the source-manifest hash, the adapter executable hash, Tantivy-managed and persistent directory bytes, build time, incremental one-field update p50/p95/p99, new-process/open/search p50/p95/p99, warm reader request p50/p95/p99 and throughput for the selected reader counts, plus sampled process RSS. Process-open timing reconstructs the production Tantivy projection from its local source journal; it does not flush the OS page cache. Warm request latency includes JSONL pipes and queueing, and RSS is sampled every 50 ms. Result evidence marks `quality_qrel_scored: false`. These scale runs are not results for real-package retrieval quality.

## `lib.rs` common-field comparison

`lib-rs-common-contract.json` pins the upstream mirror revision and the source-file hashes used for the contract. `lib_rs_adapter.py` is an offline input exporter for the two Cargo name queries marked `lib_rs_common`; it projects Cargo documents to package name, keywords, and description, preserves unknown/absent status, and emits the matching QREL subset plus an input manifest. Invoke it with a new or matching output directory:

```sh
python3 docs/benchmarks/search-quality/lib_rs_adapter.py \
  --output-dir /tmp/lib-rs-search-quality-input
```

The captured real Cargo package rows currently have known package names but unknown keywords and descriptions, so those fields are not fabricated. The projection intentionally omits README, dependency, advisory, yank, release-history, traffic, and other upstream ranking inputs. `run_common_lane.py` is the actual comparison harness: it verifies the pinned upstream git revision and both source hashes, uses the real `search_index::CrateSearchIndex` implementation and the repository's production `DiscoverySearchIndex` adapter, records ranked canonical IDs, and scores both result sets against the same four independent QREL rows. It records warm and fresh-process latency samples, index bytes, input IDs and hashes, source hashes, and unknown-field coverage. The upstream harness shims only the Crates.io `Origin` type and reads an empty synonym file; ranking uses the pinned Tantivy implementation. Its `monthly_downloads=0` argument is required by upstream's API but ignored by query-relevance sorting; the harness records the value as unknown rather than ranking evidence.

The common lane has seven Cargo release rows, three crate origins, two exact-name queries, and four QREL judgments. The pinned upstream indexer retains only the highest SemVer release for each origin, while Nudox keeps all seven rows. All seven rows have unknown keywords and descriptions, and no README text is available, so the lane is effectively package-name-only. It cannot support general quality or speed superiority claims. Warm latency boundaries differ (Nudox JSONL request/response versus lib.rs in-process search), cold production latency rebuilds its projection from a source journal while upstream opens its Tantivy index, and each index byte count uses its implementation's own storage boundary. The report preserves these differences.

After a slot is explicitly granted, build the production adapter and run the common comparison from the existing Nix shell. The result directory must be new and empty, and the lib.rs checkout must match the pinned revision. These commands use the allocated slot-2 target and two Cargo build jobs:

```sh
source /Users/mileswirht/Downloads/backend/.local/devenv/development.sh
export CARGO_TARGET_DIR=/Users/mileswirht/.cache/nudox/cargo-1.97/build/slot-2
export CARGO_BUILD_BUILD_DIR=/Users/mileswirht/.cache/nudox/cargo-1.97/build/slot-2
export CARGO_BUILD_JOBS=2
cargo build --locked --release \
  --manifest-path docs/benchmarks/search-quality/adapter/Cargo.toml \
  --target-dir "$CARGO_TARGET_DIR"
mkdir -p /tmp/search-quality-common-evidence
BENCH_BUILD_SLOT_GRANTED=1 python3 docs/benchmarks/search-quality/run_common_lane.py \
  --adapter "$CARGO_TARGET_DIR/release/search-quality-tantivy-adapter" \
  --lib-rs-mirror /tmp/codex-lib-rs-mirror \
  --target-dir "$CARGO_TARGET_DIR" \
  --result-dir /tmp/search-quality-common-evidence \
  --samples 1001 --cold-samples 101 --limit 150 --seed 20260928
```

The common harness builds lib.rs with `--locked --offline`; if the pinned dependencies are unavailable in Cargo's local cache, report that build limitation rather than silently enabling network access. To run the broader production-only lane after the same adapter build, create another empty evidence directory and run:

```sh
mkdir -p /tmp/search-quality-primary-evidence
BENCH_BUILD_SLOT_GRANTED=1 python3 docs/benchmarks/search-quality/search_benchmark.py run \
  --adapter-command "$CARGO_TARGET_DIR/release/search-quality-tantivy-adapter" \
  --result-dir /tmp/search-quality-primary-evidence \
  --run-name primary-25q-39qrel \
  --samples 101 --seed 20260928 --limit 50
```

That lane covers the frozen 25 queries and 39 QRELs across the supported primary and adversarial fixtures. lib.rs has no corresponding cross-ecosystem, forge, or adversarial field coverage, so only the two-query common projection can be compared directly. The input-export command above remains source-only and needs no network.

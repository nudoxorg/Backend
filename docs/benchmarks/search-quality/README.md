# Search quality and cost benchmark

This benchmark compares a search adapter against a frozen package corpus and a hand-authored relevance file. The QREL judgments were written from package identity, pinned archive, manifest, advisory, registry-index, and explicit fixture facts before any search result file was scored. No expected order is generated from either the candidate or `lib.rs` ranking implementation. Grade 0 marks an explicit distractor or stale release; grades 1–3 express decreasing relevance. A missing document is treated as grade 0.

The primary lane has 25 real package/source identities from the repository's pinned Nix corpus: Cargo, npm, PyPI, Go, Maven, NuGet, and two Clang forge-only packages. Each pin retains its exact archive URL and declared archive hash; archives were not fetched for this benchmark. The forge pins also retain a repository/ref coordinate derived from the pinned GitHub archive URL, but no commit, manifest, or forge API metadata is inferred. The lane includes four historical Cargo release rows from captured crates.io sparse-index records. Read-only crates.io API snapshots add current package descriptions and keywords only to `serde@1.0.229` and `bincode@3.0.0`, where the API's `max_version` matches the pinned release; the metadata values, capture time, endpoint, and SHA-256 are frozen in `snapshots/`. `source-snapshots.json` also pins the Nix manifest, an immutable captured copy of the workspace lockfile, local RustSec advisory, local fixture sources, and three raw crates.io index files. Nix and sparse-index capture time is `2026-09-28T06:58:07Z`; the API metadata capture time is `2026-09-29T16:43:42Z`. Other ecosystems use exact package/archive pins from the Nix corpus rather than claims about current registry metadata. Only the captured Cargo index records have observed yanked state; unsupported fields remain explicitly `unknown`.

The 33 queries and 60 QREL rows cover exact identities in each ecosystem, within-candidate version freshness, Cargo dependency and advisory lookup, captured yanked releases, prefix and typo lookup against real Cargo names, description and keyword retrieval from captured crates.io API metadata, a scoped Cargo/forge ambiguity query, aliases, fixture dependency/advisory/yank facts, and an unknown-metadata expected-empty query. Judgments are hand-authored from the cited frozen evidence and explicit asymmetric fixtures before any candidate run; a candidate's own ranking is never used to create or sort QREL rows. The six synthetic adversarial records live only in `adversarial-corpus.jsonl`; they are not mixed into the real primary lane. Queries about cross-ecosystem capabilities and the primary/adversarial split are documented separately in `ecosystem-capabilities.tsv`.

The new real-source QRELs judge `ser` as a package-name prefix and `sered` as a one-edit transposition of `serde`; each gives `serde@1.0.229` grade 3 and its captured older releases grade 1. The description query `binary serialization` gives `bincode@3.0.0` grade 3 and `serde@1.0.229` grade 0, while `generic serialization framework` reverses those judgments. The keyword query `binary` gives bincode grade 3 and serde grade 0; `serialization` gives serde grade 3 and bincode grade 2 because the latter phrase occurs in its description. The scoped `json` query judges the real pinned forge package `json-c` relevant and records real `yaml-cpp`, serde, and bincode distractors, making Cargo/forge ecosystem filtering observable. These judgments cite the raw [serde API snapshot](https://crates.io/api/v1/crates/serde) and [bincode API snapshot](https://crates.io/api/v1/crates/bincode).

The user's live workspace registry journals were empty at inspection, and the local Turso projection contained no package-state or dependency-edge rows. The primary lane therefore reports a frozen real-source corpus built from pinned Nix package/archive identities and captured public crates.io records; the adversarial lane remains explicitly synthetic.

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

The in-repository Rust adapter is `adapter/`. Its feature-gated bridge calls the production `DiscoverySearchIndex` and uses its Tantivy query and incremental-sync paths; it does not contain a parallel ranker. Registry records are committed to the real discovery journal, then ranked source-coordinate keys are mapped back to canonical benchmark document IDs. The two forge pins enter through the index's explicit `open_with_forge` route as source-only facts: only the pinned URL, derived repository/ref coordinate, and declared SRI digest are indexed as generic terms; omitted forge metadata stays unknown. This exercises the actual search ranker while keeping forge admission separate from fabricated registry facts. Workspace manifest fixtures remain unsupported by this discovery index and are reported as skipped. The adapter's `index_bytes` is the logical size of Tantivy's in-memory managed files; `persistent_index_directory_bytes` separately counts the copied corpus, source journal, and adapter manifest on disk. Measurement output goes into user-selected evidence directories and does not alter the frozen inputs.

The adapter integration tests exercise canonical-ID ranking, positive Tantivy index-byte reporting, alias removal through the production incremental sync path, cold-reopened yanked overlay/cursor recovery, and direct retrieval of a pinned forge source identity. `adapter/Cargo.lock` is checked in and included in the source freeze. Because `adapter/` is a standalone Cargo workspace, its manifest mirrors the root path patches for `ra_ap_project_model` and `ra_ap_toolchain`; both remain version `0.0.341`, but the vendored project model adds the `CargoConfig::isolate_env` field used by Rust authority setup. The root GPUI patches are absent from this adapter's dependency graph. When a manifest change requires a lock update, regenerate it offline and refresh the freeze before later commands use `--locked`.

```sh
python3 docs/benchmarks/search-quality/freeze_corpus.py --check
python3 docs/benchmarks/search-quality/search_benchmark.py validate
```

Run tests and builds from Bash in the repository's existing Nix environment after a build slot is assigned. Use the stable QA build and target directories supplied for that slot; do not select or clean a transient cache path. Keep one Cargo build job:

```sh
source /Users/mileswirht/Downloads/backend/.local/devenv/development.sh
# Set these to the existing QA paths supplied for the allocated slot.
export CARGO_TARGET_DIR=/absolute/path/to/granted-qa-target
export CARGO_BUILD_BUILD_DIR=/absolute/path/to/granted-qa-build
export CARGO_BUILD_JOBS=1
cargo test --locked --manifest-path docs/benchmarks/search-quality/adapter/Cargo.toml --target-dir "$CARGO_TARGET_DIR"
cargo build --locked --release --manifest-path docs/benchmarks/search-quality/adapter/Cargo.toml --target-dir "$CARGO_TARGET_DIR"
```

The adapter binary produced by the release command is `$CARGO_TARGET_DIR/release/search-quality-tantivy-adapter`. The adapter's result can reveal unsupported QREL fields (for example, registry dependency facts are retained in the input but are not currently a search field); do not reinterpret such a miss as a successful capability.

Create the evidence directory first, and ensure it is git-ignored if it is inside the checkout. Once the repository owner grants the adapter build slot, the exact harness command is:

```sh
BENCH_BUILD_SLOT_GRANTED=1 python3 docs/benchmarks/search-quality/search_benchmark.py run \
  --adapter-command '/absolute/path/to/adapter' \
  --result-dir /absolute/path/to/ignored/evidence \
  --run-name search-quality-v1 \
  --samples 101 --seed 20260928 --limit 50
```

The gate is deliberate: `run` invokes adapter build/search commands, so this command must not be run until the build slot is granted. It records raw latencies, frozen input hashes, and the adapter executable path/hash, then writes ranked results, relevance metrics, mutation checks, host/runtime details, index bytes, and build/update timings. Quality output includes MRR, NDCG, precision and recall at 1/5/10, the closed-corpus false-positive rate, explicit-negative and unjudged hit counts, and QREL coverage. Missing documents receive grade 0 by this benchmark's closed-corpus rule; the separate unjudged count and QREL-coverage metrics expose the limits of the hand-judged subset. Cold latency includes a new adapter process, index open, and one search but does not flush the OS page cache. Warm latency includes JSONL pipe round-trip overhead. Incremental update measures alternating one-document, one-field alias removal/restoration. p50/p95/p99 use nearest-rank quantiles over at least 101 samples. No timing or quality result is checked into this folder.

## Grouped release freshness

The grouped lane measures the release order returned within production package-lineage search groups for the six frozen freshness queries. It uses the same real primary corpus and independent version-pair QRELs as the regular quality run, and reports both quality and freshness-order accuracy. Production now keeps evidence-tier priority and orders comparable releases newest-first within the bounded release facet; if any displayed version fails the ecosystem parser, it preserves the index's stable order. The facet still selects its existing bounded candidate window before presentation ordering, so histories larger than that window need a separate index-level recency selection strategy. Warm JSONL latency and adapter-measured search time are separate; cold latency includes a new process plus journal-to-Tantivy projection reconstruction. Use new empty result directories to compare a baseline binary and a revised binary against the same frozen inputs:

```sh
mkdir -p /tmp/search-quality-group-baseline
BENCH_BUILD_SLOT_GRANTED=1 CARGO_BUILD_JOBS=1 python3 docs/benchmarks/search-quality/run_grouped_release_lane.py \
  --adapter /absolute/path/to/baseline-adapter \
  --result-dir /tmp/search-quality-group-baseline \
  --samples 101 --cold-samples 101 --limit 50
```

The output records per-query rankings, pairwise freshness checks, index bytes, and warm/cold p50/p95/p99 values. It marks a yanked older version filtered by `exclude_yanked` as correctly excluded from a freshness query. The script verifies the frozen corpus and adapter response consistency before writing its result.

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
  --adapter-command "$CARGO_TARGET_DIR/release/search-quality-tantivy-adapter" \
  --result-dir /tmp/search-quality-scale-evidence \
  --samples 101 --seed 20260928 --parallelism 1,4,16
```

This writes source hashes, the source-manifest hash, the adapter executable hash, Tantivy-managed and persistent directory bytes, build time, incremental one-field update p50/p95/p99, new-process/open/search p50/p95/p99, warm reader request p50/p95/p99 and throughput for the selected reader counts, plus sampled process RSS. Process-open timing reconstructs the production Tantivy projection from its local source journal; it does not flush the OS page cache. Warm request latency includes JSONL pipes and queueing, and RSS is sampled every 50 ms. Result evidence marks `quality_qrel_scored: false`. These scale runs are not results for real-package retrieval quality.

## `lib.rs` common-field comparison

`lib-rs-common-contract.json` pins the upstream mirror revision and the source-file hashes used for the contract. `lib_rs_adapter.py` is an offline input exporter for the Cargo exact-name, prefix, typo, description, and keyword queries marked `lib_rs_common`; it projects Cargo documents to package name, keywords, and description, preserves unknown/absent status, and emits the matching QREL subset plus an input manifest. Invoke it with a new or matching output directory:

The upstream query parser and field weighting come from the [pinned public search-index source](https://github.com/Protryon/lib-rs-mirror/blob/4642a01664e14f4ae30a3804a55556b0770119d9/search_index/src/lib_search_index.rs). The standalone comparison executes `search_index::CrateSearchIndex.search(sort_by_query_relevance=true)` in common-field mode, with a neutral crate score and no README input. The separate [ranking source](https://github.com/Protryon/lib-rs-mirror/blob/4642a01664e14f4ae30a3804a55556b0770119d9/ranking/src/lib_ranking.rs) is pinned and hash-checked for context; it is not linked or executed by this adapter. The harness requires the exact mirror revision and a clean checkout, including untracked files, so its `categories` path dependency is fixed by the same revision.

```sh
python3 docs/benchmarks/search-quality/lib_rs_adapter.py \
  --output-dir /tmp/lib-rs-search-quality-input
```

The frozen Cargo set has known package names for all seven release rows; only the current serde and bincode rows have captured descriptions and keywords, and every README remains unknown. The projection omits README, dependency, advisory, yank, release-history, traffic, and other upstream ranking inputs. `run_common_lane.py` uses the real `search_index::CrateSearchIndex` search implementation and the repository's production `DiscoverySearchIndex` adapter, then scores returned canonical IDs against the independent QRELs. It records per-query samples atomically and supports `--resume` only when the prior run manifest matches every input hash, source, executable, and sample setting. A small reporter can score saved upstream-only partial data without starting either engine:

```sh
python3 docs/benchmarks/search-quality/report_common_partial.py \
  --raw /path/to/lib-rs-results.json \
  --output /path/to/partial-report.json
```

The upstream shim adapts only the Crates.io `Origin` type and reads an empty synonym file; search uses the pinned Tantivy implementation. Its `monthly_downloads=0` argument is required by upstream's API but ignored by query-relevance sorting; the harness records the field as unknown, not as ranking evidence. Cargo compilation wall time, upstream Tantivy index-construction time, and child-process index-plus-warm-query time are separate. The report keeps Nudox's logical in-memory Tantivy bytes apart from persistent corpus/journal/manifest bytes, and upstream's on-disk Tantivy directory apart from the full harness data directory.

The common lane has seven release rows, three crate origins, nine queries, and 21 QREL rows. The pinned upstream indexer retains only the highest SemVer release per Crates.io origin, while Nudox keeps all seven rows. The report therefore includes both a full-candidate Nudox capability score and paired ranker scores restricted to the exact upstream-indexed IDs. If the configured limit is below the seven-row corpus size, an untimed Nudox request retrieves the full ordering for paired scoring; the score then filters to the upstream IDs and applies the requested cutoff. Latency samples still use the configured limit. Precision divides by the number actually returned within top-k; an empty top-k is undefined. Undefined per-query metrics remain null, are omitted from that metric's macro mean, and have a defined-query count beside the mean. Both engines receive the same query strings in the same seeded order and the same result limit, while Nudox applies Cargo/primary scope to its Cargo-only index. Warm latency boundaries differ (Nudox JSONL request/response versus upstream in-process search); cold measurements include fresh process startup, index open, and one search without flushing the operating-system page cache. Storage bytes are labeled at each implementation's own boundary. This small fixed fixture supports regression evidence only, not general quality or speed superiority claims.

After a slot is explicitly granted, build the production adapter and run the common comparison from the existing Nix shell. The result directory must be new and empty for a first run, and the lib.rs checkout must match the pinned revision and be clean. If interrupted, continue in the same evidence directory with `--resume`; the durable manifest rejects changed source, input, executable, or sample settings. These commands use the stable QA paths supplied for that slot and one Cargo build job:

```sh
source /Users/mileswirht/Downloads/backend/.local/devenv/development.sh
# Set these to the existing QA paths supplied for the allocated slot.
export CARGO_TARGET_DIR=/absolute/path/to/granted-qa-target
export CARGO_BUILD_BUILD_DIR=/absolute/path/to/granted-qa-build
export CARGO_BUILD_JOBS=1
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
  --run-name primary-33q-60qrel \
  --samples 101 --seed 20260928 --limit 50
```

That lane covers the frozen 33 queries and 60 QRELs across the supported primary and adversarial fixtures. lib.rs has no corresponding cross-ecosystem, forge, or adversarial field coverage, so only the nine-query common-field Cargo projection can be compared directly. The input-export command above remains source-only and needs no network.

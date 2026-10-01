# Search quality adversarial holdout

This holdout is separate from `search-quality/` and its frozen 25-document primary corpus. Every generated row is synthetic and uses the explicit source label `benchmark-holdout:search-quality:v1`; no registry or package claims are made.

The cases target gaps the small primary corpus cannot expose: ecosystem filtering before the bounded Tantivy page, equal names across registries, release grouping by source-scoped package lineage, and package-level NPM retraction across multiple versions. The overflow fixture intentionally supplies 257 matching NPM documents ahead of one scoped PyPI document. The current benchmark bridge requests an unfiltered 256-result page and filters afterward, so that case should fail until the bridge passes ecosystem scope into the production pre-pagination filter. The production `DiscoverySearchIndex` already supports this filter.

The group and retraction checks use hooks compiled only with the existing `search-bench` feature. Group search reports canonical document IDs per source-scoped lineage. The retraction hook sends a real `DiscoveryPackageRetraction` through `DiscoveryStore`; the next search returns standing from the store overlay so a missed release update remains visible to the assertion.

First run the source-only fixture and falsification checks:

```sh
python3 docs/benchmarks/search-quality-holdout/holdout_benchmark.py validate
```

After the repository owner grants the build slot and the adapter has been built, run the candidate holdout against an existing empty evidence directory:

```sh
BENCH_BUILD_SLOT_GRANTED=1 python3 docs/benchmarks/search-quality-holdout/holdout_benchmark.py run \
  --adapter-command /tmp/search-quality-adapter-target/release/search-quality-tantivy-adapter \
  --result-dir /tmp/search-quality-holdout-evidence
```

The runner builds only the synthetic holdout index and exits nonzero on a failed assertion. It records fixture/source hashes and observed IDs/statuses, with no latency or quality scores. It does not alter the frozen corpus or source manifest. The benchmark bridge source is already among the frozen benchmark inputs, so its hash will need to be refreshed after the production source tree is frozen; do not treat the current freeze-check failure from concurrent source drift as a holdout result.

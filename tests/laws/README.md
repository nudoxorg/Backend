# `backend-laws`

This package is the executable contract suite for the public v2 version,
store, and flow APIs. It computes expected values from independent bounded
models rather than calling production test helpers or reading private fields.

The suite includes:

* a literal canonical V2NODE encoder and delta-ID oracle, including anchored
  cuts, insertion-order independence, large trees, re-keying, deletes,
  re-adds, value-only edits, grouping, and exact version/store
  apply/inverse/compose fences;
* weighted batch consolidation, map/filter/group/reduce, all three weighted
  join cross terms, retained distinct support, top-k boundary changes, and
  arrangement work counters;
* fresh transitive closure and SCC partition checks for merge/split/delete,
  typed producer scope admission failures, typed demand lifecycle/no-op behavior,
  arrangement frontier pin/retention laws, and root/sequence subscriber reset
  and gap fences;
* checked map fixtures backed by producer-admitted scope observations, plus
  negative laws for raw scopes, mismatched producer metadata, closed relation
  publication, and raw untrusted map constructors;
* red mutation cases showing that dropped retractions, dropped simultaneous
  join cross terms, skipped coverage admission, stale signed-batch content,
  and ignored subscriber fences are observable.
* the cutover regression target, which compares full recomputation with delta
  output, randomizes multi-leaf order, admits payload sized budgets, checks
  one-row delivery over a 100,000-row view, bounds a disjoint 100,000-row
  join, and holds compaction behind a stalled observer before releasing it.

`TestContract` metadata is exported through `backend_laws::test_contracts()`.
Each record names the law, input grammar, bug ingredients, independent oracle,
reducer, coverage, fault sites, and bounded budget.

Run the package from the workspace root:

```text
cargo fmt --manifest-path tests/laws/Cargo.toml -- --check
cargo test -p backend-laws
cargo test -p backend-laws --test cutover_regressions -- --nocapture
cargo clippy -p backend-laws --all-targets -- -D warnings
cargo doc -p backend-laws --no-deps
```

# Structural performance audit

This note records the performance claims that can be checked without a clock,
and the remaining seams where the current implementation still performs work
that is invisible in its public counters.  The tests in
`tests/performance/tests` deliberately use a separate model or an external
filesystem/`Arc` observation.  They should remain independent of the
implementation's own work counters: a counter that reports zero after a full
scan is not evidence of zero work.

## Evidence already available

* `store_one_key_path_copy_is_logarithmic_and_matches_full_rebuild` compares a
  one-key update with an independently rebuilt `BTreeMap` and checks the
  `UpdateStats` visit/copy/reuse envelope.
* `durable_point_reads_reopen_without_full_tree_materialization` reopens a
  selected tree and checks authenticated root-to-leaf node and byte work at
  four sizes.  `durable_one_row_update_writes_only_a_path_and_reuses_payload_bytes`
  counts immutable node files and physical bytes before and after one edit.
* `replication_resume_reuses_checkpoint_bytes_and_accepts_exact_object` checks
  sparse ranges and exact completion.  The replication crate's shared
  checkpoint tests additionally check `Arc::ptr_eq` across checkpoint/resume.
* `scheduler_deduplicates_work_and_accounts_both_hedge_reservations` captures
  admission, retained bytes, and canonical-byte ownership before a warm reuse.
  A warm lookup must leave all three unchanged and return the same `Arc`.

These are structural contracts.  They must not be converted into timing
thresholds.  Timing benchmarks are useful for diagnosis, but cannot prove
that a fast full scan stayed fast on another machine.

## Seams that still hide work

The following functions require a stronger persistent-root boundary before a
one-node or warm-path claim is complete.

* `crates/store/src/durable/nodes.rs`, `write_node_recursive`: every
  publication recursively visits every in-memory descendant and calls
  `metadata` for each one.  CAS writes are path-local, but publication work is
  still proportional to the whole tree.  The durable object should carry a
  checked subtree residence/closure reference so a known persisted child can
  be skipped without visiting its descendants.  `TreeWriteStats.bytes_written`
  currently counts canonical payload bytes and omits the node envelope, so its
  name/semantics need to be made explicit.

* `crates/flow/src/arrangement/checkpoint.rs`, `persist_node` and
  `collect_node_objects`: both recurse through the complete visible tree on
  every checkpoint.  A cache hit avoids encoding but does not avoid the walk
  or the filesystem existence probe.  A root-indexed closure object should
  make the retained transitive closure reusable as one immutable reference.

* The same file's `write_checkpoint` walks every level and retained history
  record and rebuilds the reference index.  The limits bound the amount of
  memory, but do not make a one-row checkpoint delta-local.  A versioned
  manifest should path-copy level/history indexes and append only the changed
  descriptor cells.

* `hydrate_checkpoint` first materializes the complete visible map and then
  calls `canonical_for_visible` once for the visible root and twice for every
  retained history record while walking history backwards.  Each call builds
  a new `BTreeSet`/persistent tree from the whole visible map.  This is
  potentially `O(history * visible_rows)` and the rebuild is not charged to
  `WorkScope`.  Persisted before/after roots or a path-copy inverse over the
  checked root should replace this reconstruction.  Add a test-only
  allocation/rebuild counter or a poisoned full-rebuild hook before claiming
  bounded reopen work.

* `crates/flow/src/materialized.rs`, `MaterializedIndex::from_state`: it
  computes `state.iter().count()` even though a checked persistent root already
  has a cardinality summary.  Carry the summary through `RelationState` and
  make the wrapper construction constant work.

* `crates/engine/src/workspace/catalog.rs`, catalog append/find paths: an
  append decodes, sorts, trims, and re-encodes the complete bounded catalog;
  lookup decodes the complete index and rebuilds a map.  The bounds prevent
  unbounded growth, but each version still pays `O(catalog_entries)` and
  copies every retained object.  A persistent ordered catalog/index root with
  delta cells and proof-directed lookup is needed for a genuinely local
  append.

* `crates/execution/src/scheduler/publication.rs`: `complete` performs receipt
  checks, attempt validation, and hedge claiming and then delegates to
  `complete_ephemeral`, which repeats those checks.  This is bounded duplicate
  work and creates an avoidable multi-lock/claim path; the API should have one
  preflight token consumed by one acceptance transition.

* `crates/store/src/durable/nodes.rs`, `open_tree_publication`: the root node
  is decoded twice in the current implementation.  The second decode should
  be eliminated and the checked root admission should be retained once.

* `crates/replication/src/transfer/assembly.rs`: the legacy `checkpoint` and
  `checkpoint_wire` APIs clone each chunk body.  Reconnect code must use
  `checkpoint_shared`/`resume_shared`; if the legacy APIs remain, keep them
  visibly off the local hot path and add byte/allocation accounting around
  their boundary.

## Oracle work to add at the durable boundary

The durable store currently exposes logical node/byte counters, but not the
number of subtree probes, node decodes, or allocations.  Before optimizing
around those counters, add an injectable read/write probe to the test support
layer (a trait or a `cfg(test)` callback, never a global timing hook).  Then
add these independent checks:

1. Re-publishing the same root after warming its closure must perform zero
   node encodes and zero child probes.  A test store that panics on a child
   probe makes a hidden full-tree walk observable.
2. A one-row edit must add at most one leaf plus one ancestor per height, and
   its closure descriptor must reference unchanged subtrees by identity.
3. Reopen of a point-read view must not call a materializer; a poisoned
   materialization path should still allow `get_with_stats`.
4. A history append must add one delta object and a logarithmic number of
   index/manifest nodes.  Reopen work must remain proportional to the retained
   delta payload and root paths, with every byte/row/probe charged.
5. Repeated catalog/tombstone/journal churn must keep object count, retained
   bytes, and index cardinality below their configured caps.  Test the cap
   after `cap + 1`, `2 * cap`, and a mixed insert/evict sequence so a stale
   side table cannot grow behind the bounded primary map.

The test support should compute expected roots and range coverage from a small
independent model.  It should never read the production `WorkCounters` to
decide what the expected work was; those counters are assertions about the
implementation, not an oracle.

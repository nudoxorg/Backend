# Test obligation source inventory

`extract_source_obligations.py` is the reproducible source extractor used for
the Rust/Python/JavaScript/TypeScript declaration rows in
[`../test-obligations.json`](../test-obligations.json). It is inventory
automation, not a test runner. It scans first-party `apps/`, `crates/`,
`frontends/`, `.config/`, and `tests/` sources while excluding vendored,
generated, fixture, and snapshot trees. Rust comment/string masking, Python
AST discovery, and the JS/TS test-call scan are implemented in the script.
The `not_explicitly_ignored` scope deliberately makes no claim about `cfg`,
feature, or runtime-filter reachability.

To reproduce the source comparison recorded in the ledger, check out the
baseline revision and run the extractor on both source trees. From the current
repository checkout:

```sh
audit_dir=$(mktemp -d /tmp/nudox-obligations.XXXXXX)
rmdir "$audit_dir"
git worktree add --detach "$audit_dir/baseline" cb92bd655bfc6c6a80f11a1d80dca2d7e2fd4e15
git worktree add --detach "$audit_dir/current" f54c6430cea702d1865795900a98091294f99509
python3 tests/keel/tools/extract_source_obligations.py \
  --root "$audit_dir/baseline" \
  --revision cb92bd655bfc6c6a80f11a1d80dca2d7e2fd4e15 \
  --output "$audit_dir/baseline.json"
python3 tests/keel/tools/extract_source_obligations.py \
  --root "$audit_dir/current" \
  --revision f54c6430cea702d1865795900a98091294f99509 \
  --output "$audit_dir/current.json"
git worktree remove "$audit_dir/baseline"
git worktree remove "$audit_dir/current"
```

The generated JSON inventories remain in `$audit_dir` for comparison. Remove
that directory after reviewing or retaining those outputs.

Each output contains the recorded revision, source rows, path/name, source
hash, line span, scope, runtime-kind observation, and extraction uncertainty.
Rows include stable IDs based on language, path, name, and same-name occurrence
within the file; source hashes distinguish unchanged, modified, and removed or
added obligations. The extractor preserves duplicate names instead of
collapsing them.

The committed ledger also contains manually reviewed material that this
source pass cannot infer: source-subject labels, parameterized/macro families,
Nushell assertion blocks, fuzz targets and corpus inputs, CSS/SQL fixtures,
registered Keel cases, retirement mappings, and report receipts. Reconcile
those fields when updating the ledger. A Keel receipt is a current-definition
replacement only when its passing report's `inventory.definition_sha256`
matches the hash of that case in the strict evaluated manifest; same-ID
historical passes and reports without per-case hashes remain separately
qualified as the ledger records them.

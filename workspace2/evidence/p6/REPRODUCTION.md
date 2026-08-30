# P6 raw reproduction inventory

## Frozen source inventory

Command:

```text
rg -n 'FullRegistry::dispatch|\\.dispatch\\(' planes/compiler crates domains adapters --glob '*.rs' --glob '!**/tests/**' --glob '!**/src/**test*.rs'
```

Output: no matches.

Command:

```text
rg -n 'SegmentFamily::try_from|SegmentFamily::(Exact|Lexical|Relation|Usage|Vector)' planes/index crates domains adapters --glob '*.rs' --glob '!**/tests/**' --glob '!**/src/**test*.rs'
```

Output: no matches.

Command:

```text
rg -n 'SchemaId::try_from\\(schema_wire\\)|fn parse_header' crates/nudox-view/src --glob '*.rs'
```

Output:

```text
crates/nudox-view/src/validate/header.rs:19:pub(crate) fn parse_header(
crates/nudox-view/src/validate/header.rs:46:    match SchemaId::try_from(schema_wire) {
```

## Mutation diffs

Foundation mutation (run and reverted before recording other mutations):

```diff
-    match SchemaId::try_from(schema_wire) {
+    match SchemaId::try_from(u32::from(SchemaId::Frame)) {
```

Compiler mutation:

```diff
-            Language::RustSubset => drive::<RustFrontend>(stage, source),
-            Language::TypeScriptSubset => drive::<TypeScriptFrontend>(stage, source),
+            Language::RustSubset => drive::<RustFrontend>(stage, b""),
+            Language::TypeScriptSubset => drive::<TypeScriptFrontend>(stage, b""),
```

Index mutation:

```diff
-        match code {
+        match 1 {
```

## Raw test-result summaries

The detached mutation worktree was at the stated frozen source commit before changes. The literal
commands and their terminal summaries were:

```text
$ cargo test -p nudox-view --lib
running 34 tests
test result: FAILED. 31 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out
failures: validate::raw_property::every_byte_value_has_exact_structural_provenance
          validate::tests::header_mutations_report_exact_records
          validate::raw_property::bolero_combines_structural_byte_mutations
error: test failed, to rerun pass `-p nudox-view --lib`

$ cargo test --workspace --all-targets  # planes/compiler
running 0 tests
test result: ok. 0 passed; 0 failed
running 1 test
test full_rows_lend_the_source_and_retain_exact_rejection ... FAILED
error: test failed, to rerun pass `-p nudox-compile-registry --test dispatch`

$ cargo test --workspace --all-targets  # planes/index
running 0 tests
test result: ok. 0 passed; 0 failed
running 4 tests
test unknown_segment_family_retains_the_raw_value ... FAILED
test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
error: test failed, to rerun pass `--test vocabulary`
```

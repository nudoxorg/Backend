# P6 input-removal mutant results

## Fixture custody

| field | value |
| --- | --- |
| source candidate | `44c22154fd5238e4769562590420371979306050` |
| detached mutation worktree | `/private/tmp/p6-mutants.B2CThI/workspace2` |
| production prototype worktree changed by mutant | no |
| mutation commit | none; all changes were uncommitted and isolated |

## Commands and outcomes

### Foundation schema input removal

Mutation: `SchemaId::try_from(schema_wire)` became
`SchemaId::try_from(u32::from(SchemaId::Frame))` in
`crates/nudox-view/src/validate/header.rs`.

Command: `cargo test -p nudox-view --lib`.

Result: exit 101; 31 passed and 3 failed. The failures were
`validate::raw_property::every_byte_value_has_exact_structural_provenance`,
`validate::tests::header_mutations_report_exact_records`, and
`validate::raw_property::bolero_combines_structural_byte_mutations`. Each expected
`Err(UnsupportedSchema { wire: ... })` but received a validated frame. This proves the supplied
wire schema is consumed by the real foundation validation path.

### Compiler dispatcher source removal

Mutation: both `FullRegistry::dispatch` language arms passed `b""` rather than `source`.

Command: `cargo test --workspace --all-targets` in `planes/compiler`.

Result: exit 101. The library had zero unit tests; the one failing assertion was the fixture
`crates/nudox-compile-registry/tests/dispatch.rs:full_rows_lend_the_source_and_retain_exact_rejection`.
No non-test call site of `FullRegistry::dispatch` exists in the frozen tree.

### Index family-code removal

Mutation: `SegmentFamily::try_from` used `match 1` instead of `match code`.

Command: `cargo test --workspace --all-targets` in `planes/index`.

Result: exit 101. Three tests passed; the fixture
`tests/vocabulary.rs:unknown_segment_family_retains_the_raw_value` failed because
`Ok(Exact)` replaced the expected `Ok(Lexical)`. No non-test call site of
`SegmentFamily::try_from` exists in the frozen tree.

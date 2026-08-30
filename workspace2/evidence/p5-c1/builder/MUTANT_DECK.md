# C1 builder causal mutant deck

After the builder commits its exact candidate, the manager records its source SHA-256, copies only
`nudox-ir-format` to `/private/tmp/p5-c1-builder-mutant-<name>`, applies the named patch there, and
runs the named focused integration/unit test with a fresh `CARGO_TARGET_DIR`. Each retained row records
the patch SHA-256, command, nonzero status, failing test name, and pristine candidate control status.

| mutant | one required mutation | focused falsifier | expected result |
| --- | --- | --- | --- |
| `mutants/constant-body.patch` | replace every emitted entity/type coordinate with one named constant while preserving header/count geometry | `prepared_writer_matches_fixed_goldens_and_existing_validator` | nonzero test status from unequal golden/cursor values |
| `mutants/partial-write.patch` | move one header byte write before the full-prefix length check | `shorter_outputs_remain_byte_identical` | nonzero test status from exact error plus changed sentinel |

The copied target is evidence-only and is removed after the recorded run. No patch is applied to the
manager candidate. A missing patch, missing source hash, green mutant, missing pristine control, or
mutation outside the listed behavior makes the direct-body row unproven.

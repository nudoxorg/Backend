# P1 C1a12C2 final lab-closure repair

Base: `057e7cb0` on the isolated repair branch. Independent closure review passed all C1a source,
behavior/UI, strict clippy, C0 preservation, LOC, and replay facts, but blocked two final
lab-evidence defects. This card permits exactly the existing lab source and raw TSV.

1. Run `cargo fmt --all` from `workspace2/layout-lab` (the lab's independent workspace), then
   verify `cargo fmt --all -- --check` from that same directory. The C1a12C1 worker's format
   command did not format this workspace.
2. Extend the TSV schema and source output with one explicit metadata field naming the frozen
   validation setup categories: `schema_provenance,typed_slice,hierarchy`. This is evidence
   metadata required by the C1a master card, not a measured work counter. Do not add any
   N-derived counter, checksum, copy claim, or behavior change.

Rerun strict lab release clippy and the exact release command after these corrections to
regenerate all 24 rows. Preserve all workloads, scopes, allocator facts, UNVERIFIED OOM field,
and source/output limits. Final Terra closure review must run the lab-workspace format command
and replay again before integration.

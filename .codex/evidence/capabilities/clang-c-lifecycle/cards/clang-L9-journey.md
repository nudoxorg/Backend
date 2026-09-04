# Card clang-L9-journey — close R10/R12: changed-content gen-2 lifecycle with typed terminals at every phase

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `codex/fidelity-clang` AFTER the L8 replay-heal commit lands (Terra will point
  you at the exact sha at dispatch; do not start before it).
- Owned path: `compiler/driver/tests/clang_lifecycle.rs` (only this file).
- FORBIDDEN: all product code, `build_drive.rs`, `corpus_harness.rs`, publication/index/journal
  crates, other tests. If a product terminal you need does not exist, STOP and report — do not add
  product surface from a test card.

## Laws under test (proof-matrix R10 + R12)

R10: PURL → closure → per-TU authority → fragments → publish → reopen → index, old fragments keep
validating; the gen-1→gen-2 chain carries CHANGED translation-unit content (not only an added
file). R12: capacity and cancellation are exact typed errors at EVERY phase of the lifecycle —
never truncation, never a panic, never a silent skip.

## Public terminal

### T1 — changed-content whole-TU journey (strengthen `clang_database_whole_tu_generation_journey`)

1. Between gen-1 and gen-2, CHANGE `src/main.c`'s bytes (add a declaration, e.g.
   `int changed_value;` plus a distinct struct) AND add `src/extra.c` (keep `src/util.c`
   unchanged). Assert the gen-2 `main.c` fragment bytes differ from the gen-1 `main.c` fragment
   bytes (SHA-256), while `src/util.c`'s fragment bytes are IDENTICAL across generations —
   unchanged content produces unchanged fragments, changed content does not.
2. Build and seal the compilation index for GEN-1's opened package before publishing gen-2, then
   build and seal GEN-2's index after reopening — BOTH index snapshots seal (current test only
   seals gen-2).
3. After the chained publish: `journal.shutdown()`, `DurablePublisher::reopen` (now legal after
   L8), `open_published` returns sequence 1, all three fragments validate, and the gen-1 `main.c`
   fragment still opens from `ImmutableArtifactStore` and validates (old bytes decode).

### T2 — publish-phase cancellation terminal

With the cancel flag set BEFORE `publish_compiled`, assert the exact typed terminal
`PublishCompiledError::Uncommitted(UncommittedPublication::CancelledBeforeStorage)` (verify the
variant against `compiler/publication/publication/types.rs`; if admission order proves a
different deterministic variant, pin the observed one and say why in the test comment). No
partial generation may open afterwards: `open_published` must still return gen-1 (or None on a
fresh journal) — the cancelled publication released nothing.

### T3 — publish-phase capacity terminal

Assert `PublishCompiledError::BindingOutputLength { observed, .. }` for a 8-byte
`binding_output` scratch (exact `COMPILATION_BINDING_BYTES` required) — an exact typed capacity
error naming observed/required, with the journal left usable for a subsequent correct publish.

### T4 — reopen/open-phase capacity terminal

`open_published` with an undersized `fragment_output` scratch (e.g. 1 KiB for two 4 MiB-class
fragments) fails with its exact typed `OpenPublishedError` variant (find the fragment-output
length variant in `compiler/publication/publication/types.rs`; pin it by name). The same call
with a sufficient scratch then still succeeds — the failure consumed nothing.

### T5 — index-phase capacity terminal

`server_index_build::build` with an undersized projections scratch fails with its exact typed
error variant (read `server/index/build` sources for the variant; pin by name). Same for
`seal_compilation_index` with a short `exact` id list if it has a typed length terminal — pin
whichever exist; if one of the two has NO typed short-scratch terminal, report that fact as your
smallest remaining red instead of weakening an assertion.

### T6 — drive-phase and compile-phase terminals are already pinned

`build_drive.rs` proves drive `Cancelled`/`ToolAbsent`; `clang_lifecycle.rs` proves compile
`Cancelled { input }` before loading and the `Authority(ScratchCapacity)` terminal with the
untouched-output law. Do not duplicate them; T2–T5 complete the phase map. Your report must
include the complete phase→terminal table:

drive | compile | publish | reopen/open | index — typed terminal name per phase.

## Constraints

- Test-only card. Deny set (`expect_used`, `panic`, `unwrap_used`) stays. No new dependency.
- Reuse the file's existing helpers (`fresh`, `write_database`, `compile_one`, `publish`) — do
  not fork a parallel harness; extend coherently.
- `cargo fmt` on the owned file.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lifecycle` green twice (journey + all
   phase-terminal tests).
2. Mutation spot-check: temporarily break ONE assertion of T1 (e.g. accept equal main-fragment
   bytes) and show the test fails, then restore. Report the observed failure line.
3. One commit: `test(clang): close the chained whole-TU lifecycle with changed content and per-phase typed terminals`.
   Report: commit sha, phase→terminal table, exact variant names pinned for T2–T5, command tails,
   smallest remaining red.

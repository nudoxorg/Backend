# Card clang-W3r-journey — R10 composition repair: chain generations through one publisher

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `ace71dbd`. Your predecessor left the journey
  UNCOMMITTED in `compiler/driver/tests/clang_lifecycle.rs` (+477 lines): two phase tests are
  green (`database_capacity_terminal_names_lane_and_preserves_tail`,
  `cancellation_between_translation_units_preserves_generation_one`); the journey test fails.
  Continue from that working tree; do not revert it.
- Owned path: `compiler/driver/tests/clang_lifecycle.rs` ONLY. Test-only, zero production edits.
  Leave `build_drive*` files alone (another worker owns them).

## Finding being repaired (root-caused by Terra — fix the composition, not the journal)

The journey publishes gen-2 through a REOPENED publisher after `shutdown()`. On current trunk
that sequence has no green precedent and fails with
`Journal(Reduction(StageKeyMismatch { .. }))` from `server/journal/publication/owner.rs`. The two
sanctioned precedents on this exact tree:

- `compiler/driver/tests/rust_purl_lifecycle.rs` (`rust_workspace_member_lifecycle_chains_two_generations`,
  green, 95s): gen-1 and gen-2 both publish through the SAME `DurablePublisher`; generations chain
  with parent linkage (`pinned_root` advances); `shutdown()` happens only at the end.
- `compiler/driver/tests/python_purl_lifecycle.rs`: `DurablePublisher::reopen` is used for
  `open_published` reads after a shutdown — a reopened publisher READS; it does not publish a new
  generation.

## Public terminal

1. Rewrite the journey to: create journal → publish gen-1 (2 TUs) → `open_published` verify →
   add `src/extra.c` + db entry → compile all 3 TUs → publish gen-2 through the SAME publisher →
   generation advanced + `pinned_root` changed → `journal.shutdown()` → `DurablePublisher::reopen`
   → `open_published` on the REOPENED publisher → all three fragments validate, index
   build/plan/encode/seal → generation-1 fragment from `ImmutableArtifactStore` hash-identical +
   validating → corrupted-copy falsifier returns the exact typed validation fault.
2. Keep both green phase tests unchanged (cancel between TUs; capacity terminal).
3. The TestError enum currently contains copy-pasted rust-lane variants
   (`RustAuthorityError`, `MetadataIo`, `MetadataJson(serde_json::Error)`, ...) — replace them
   with exactly the causes this test can produce (typed, thiserror-derived, no serde_json).
4. Add one focused test: after `shutdown()`, a REOPENED publisher used for `try_publish` of a
   changed compilation fails — record the exact observed terminal (today
   `Journal(Reduction(StageKeyMismatch { .. }))`) in a comment citing the two precedent files, and
   assert THAT terminal exactly. This pins today's trunk semantics as evidence for the
   shared-surface owners without weakening anything. If a future trunk makes reopened publishing
   legal, this test failing is the intended signal.

## Constraints

No new dependency, no unsafe, deny set stays, deterministic across three consecutive runs, every
expected failure asserts its exact variant, unique temp dirs, bounded retry cleanup for the
libclang handle race.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lifecycle` green three runs.
2. `cargo fmt --check`. One commit: `test(clang): chain generations through one publisher and pin reopen semantics`.
   Report: commit sha, journey assertion list, generation values observed, command tails,
   smallest remaining red.

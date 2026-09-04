# Card clang-W3r2-journey — final R10 composition with the replay defect pinned

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `a1d30131e` (your predecessor's uncommitted journey is
  in the working tree; keep editing it — do not revert).
- Owned path: `compiler/driver/tests/clang_lifecycle.rs` ONLY. Test-only, zero production edits.
  Leave `build_drive*` files alone.

## Terra-diagnosed facts you build on (verified on this exact tree)

1. Same-publisher generation chaining WORKS: publish gen-1 → publish gen-2 through the SAME
   `DurablePublisher` → generation advances, `pinned_root` advances (the live append path chains
   new-key Requested events; see `server/journal/journal.rs` `append_group_using` fallback).
2. REOPENING a chained journal FAILS TODAY: `DurablePublisher::reopen` replays frames through
   `server_workflow::replay_stream` (`server/workflow/durable.rs`, plain `reduce`, no chaining
   fallback) → `PublicationOpenError::Journal(JournalError::Reduction(ReductionError::
   StageKeyMismatch { .. }))`. This is a trunk-shared-surface defect; your test PINS it as
   evidence, it does not work around it silently.
3. The rust precedent (`rust_purl_lifecycle.rs`) never reopens after gen-2; the python precedent
   reopens a ONE-generation journal. The reopened-publisher publish test from your predecessor is
   hence MISDIAGNOSED: the failure it saw happens at reopen, not at publish. Rewrite it to pin
   exactly that.

## Public terminal

1. **Journey test** (`clang_database_whole_tu_generation_journey`): as in your predecessor's
   composition — real on-disk codebase (`include/base.h`, sysroot-only header, `src/main.c`,
   `src/util.c`, relative-path `compile_commands.json` with `-I include --sysroot sysroot`) →
   discover TUs → compile each via `compile_database_translation_unit` → publish gen-1 →
   `open_published` verify on the live publisher → add `src/extra.c` + db entry → compile 3 TUs →
   publish gen-2 through the SAME publisher → generation advanced + `pinned_root` changed →
   WITHOUT shutting down: `open_published` → all three fragments validate → index
   build/plan/encode/seal → `ImmutableArtifactStore::open` of gen-1's recorded fragment facts →
   bytes hash-identical + still validating → corrupted-copy falsifier returns the exact typed
   validation fault. Then `shutdown()`.
2. **Reopen defect pin** (`reopening_a_chained_journal_is_red_until_trunk_heals_replay`): after
   the journey's shutdown, `DurablePublisher::reopen` must fail with EXACTLY
   `PublicationOpenError::Journal(JournalError::Reduction(ReductionError::StageKeyMismatch { .. }))`
   — assert the fully-qualified variant shape. Header comment: cites
   `server/journal/journal.rs::append_group_using` (live chaining) vs
   `server/workflow/durable.rs::replay_stream` (replay without chaining), the two precedent files,
   and states that flipping this test to a successful reopen is the intended signal once trunk
   heals replay. Cross-check with the artifact-store reads (which must KEEP WORKING) so the pin
   cannot hide a total breakage.
3. **One-generation reopen control**: a second journal with only gen-1 (two TUs) shuts down,
   reopens successfully, and `open_published` works on the reopened publisher — proving the
   defect is specific to CHAINED generations, not to reopen generally.
4. Keep both phase-terminal tests unchanged (capacity; cancel between TUs). Fix the TestError
   enum's leftover rust-lane copy-paste variants (`RustAuthorityError`, `MetadataIo`,
   `MetadataJson`, `serde_json`) — only causes this file can produce.

## Constraints

No new dependency, no unsafe, deny set stays, deterministic across three consecutive runs, every
expected failure asserts its exact variant, unique temp dirs, bounded retry cleanup for the
libclang handle race.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lifecycle` green three consecutive runs
   (the reopen-defect pin counts as green when it asserts the exact terminal).
2. `cargo fmt --check` on the owned file. One commit:
   `test(clang): prove chained-generation publication and pin the replay reopen defect`.
   Report: commit sha, assertion list, generation values observed, command tails, smallest
   remaining red.

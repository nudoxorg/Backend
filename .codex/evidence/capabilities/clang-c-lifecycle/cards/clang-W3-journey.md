# Card clang-W3-journey — R10: whole-TU lifecycle gen-1 → gen-2 chained publication

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `621b7d67c`. Trunk's journal generation chaining
  (parent linkage) is now merged; the C1d blocker is gone.
- Owned path: `compiler/driver/tests/clang_lifecycle.rs` — NOTHING else. Test-only card; zero
  production edits. If production must change, STOP and report the exact need.
- The two existing tests (`cancelled_database_entry_does_not_open_or_publish`,
  `absent_database_is_retained_as_typed_terminal`) stay green unchanged.

## Public terminal

The full C1d journey, as one test, real on-disk, mirroring the composition of
`compiler/driver/tests/rust_purl_lifecycle.rs` (publish_compiled → DurablePublisher::reopen →
open_published → build/plan/encode/seal_compilation_index):

1. Write a real directory: `include/base.h`, `src/main.c` and `src/util.c` (both include
   `base.h`), and a `compile_commands.json` with relative paths, `-I include`, and one
   `--sysroot <dir>` cell that the parse provably consumes (a second header only resolvable under
   the sysroot dir, included by a TU — the TU compiles only because the sysroot flag survived
   verbatim).
2. Discover TUs through `compiler_languages_clang::CompilationDatabase::from_directory`, compile
   each via `compile_database_translation_unit`.
3. Publish generation 1 (both fragments). Record `published.publication.generation`.
4. Add `src/extra.c` + its database entry. Reopen the journal with `DurablePublisher::reopen`,
   compile all three TUs, publish generation 2 THROUGH the reopened journal with
   `PublishControl::Continue`. Assert generation advanced and `pinned_root` changed (parent
   linkage).
5. Shutdown, reopen, `open_published` the newest publication: all THREE fragments validate
   (`FragmentView::validate`), the index pack plans/encodes/seals.
6. Old-generation survival: open generation 1's fragment bytes from the immutable artifact store
   (`ImmutableArtifactStore::open` with its recorded facts) — bytes identical to what was
   published (hash compare) and still validating.
7. Falsifier: corrupt one committed semantic byte of the stored generation-1 artifact copy and
   assert the exact typed validation fault — the store really validates.

## Capacity and cancel at every phase (mandate)

Extend the file with focused tests:

- Cancel between TUs: cancelled flag set after TU 1 completes → TU 2 returns the exact
  `Cancelled` terminal; the journal's latest publication is still generation 1 (nothing partial
  published).
- Capacity: a TU beyond the lane's fact capacity (a generated C file with thousands of distinct
  declarations) returns the exact typed capacity terminal through
  `compile_database_translation_unit` — naming the lane and required count, not truncation. Match
  the authority's existing capacity shape (see `clang_lane::capacity_terminal_…` for the cause
  surface) and assert the fragment output buffer is unchanged (0xa5 tail law).
- Publish-phase cancel: inspect the publication API; if `publish_compiled` exposes no cancellation
  handle, do NOT invent one — record the evidenced exclusion in a comment naming the API you read.

## Constraints

No new dependency, no unsafe, `#![forbid(unsafe_code)]` + deny set stays. Deterministic across
three consecutive runs (unique temp dirs, bounded retry cleanup for the libclang handle race like
clang_lane's). Every expected failure asserts its exact variant.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lifecycle` → all green, three runs.
2. `cargo test -p compiler-driver --offline --test clang_lane` → 16/16 or 13/16+the three known
   wire migrations (another worker owns those; do not edit that file).
3. `cargo fmt --check`. Report: commit sha, journey assertion list, generations observed, command
   tails, smallest remaining red.

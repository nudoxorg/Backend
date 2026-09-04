# Card clang-C1-lifecycle — PURL grammar, compilation-database TUs, and the whole-TU publish→reopen→index journey

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ current HEAD (R7 landed at `deebd3342`). Verify with
  `git log --oneline -3`.

## Owned paths

1. `compiler/languages/clang/**` (input.rs, ffi.rs, lib.rs, new purl.rs, tests/)
2. `compiler/driver/tests/clang_lifecycle.rs` (new integration test)
3. `compiler/driver/tests/clang_lane.rs` ONLY if a shared helper must move; prefer copying.

## Forbidden surface

`compiler/driver/lower.rs`, `compiler/driver/lower/**`, `compiler/driver/types/**`,
`compiler/ir/**`, `compiler/publication/**`, `server/**`, other lanes' files. The lifecycle test
COMPOSES the existing publication/index crates from the test; it never edits them. Explicit
paths only, never `git add -A`.

## Public terminal

1. PURL (R8): a closed typed grammar in the clang crate (new `purl.rs`) parsing the generic
   form `pkg:generic/<name>@<version>` with exact typed fields (type, name, version) and exact
   typed rejection variants: foreign ecosystem (`pkg:npm/…`), missing version, empty name, bad
   percent-encoding, non-lowercase scheme/type, and over-length cells. Table tests cover every
   acceptance and rejection class. No stringly identity escapes the type.
2. Compilation-database translation units (R9, libclang half): the authority's ffi loads the
   ungated `clang_CompilationDatabase_fromDirectory`/`getAllCompileCommands`/`dispose` symbols
   through the existing runtime-loaded `RequiredApi` mechanism, and `ClangInput` gains a
   database-derived variant carrying the file path and a BOUNDED argument array (exact typed
   capacity fault when a database entry exceeds the bound — never truncation). The sysroot and
   all flags travel verbatim from the database into the parse call; the lane never constructs a
   flag it did not read.
3. Whole-TU lifecycle (R10): a driver integration test `clang_lifecycle.rs` composes, from
   public APIs only, over a REAL on-disk directory the test writes (two `.c` files, one shared
   `base.h`, a `compile_commands.json` whose entries carry `-I<dir>` and one `--sysroot` cell
   echoed verbatim): discover the TUs through the database authority → compile each TU →
   publish generation 1 → add a third TU file and its database entry → publish generation 2 →
   reopen the store → BOTH generations' fragments validate → both index snapshots seal.
4. Capacity and cancellation (R12): a directory whose TU exceeds the lane bound yields the
   exact typed capacity terminal naming the lane and required count (never truncation); a
   cancelled flag set mid-lifecycle stops before the next TU boundary with the typed cancelled
   terminal, and no fragment from the cancelled generation is published.
5. Interim scope record: when no `compile_commands.json` exists, discovery is the exact typed
   `database absent` terminal. Driving CMake/Make/Meson/BUCK to GENERATE the database is the
   next packet (C2); the terminal makes that extension point explicit, not silent.

## Constraints

- The authority's laws hold verbatim: runtime-loaded symbols only (`clang_3_6` surface; the
  compilation-database symbols are ungated), no scanner fallback, no allocation arena, exact
  typed capacity/cancel terminals, cancellation observed before native loading and at every TU
  boundary.
- Database args flow into the existing parse path without breaking the two current
  `ClangInput` consumers (driver collect + crate tests); update both consumers in this card if
  the input enum's shape requires it — both are in owned paths.
- The integration test cleans up its directory with a bounded retry and treats only NotFound
  as success (follow `clang_lane.rs`'s current pattern).
- No new dependency; no unsafe beyond the reviewed ffi.rs boundary; typed diagnostics only.

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-languages-clang --offline --features native-test` → all pass
   including the PURL table tests and the database-input tests.
2. `cargo test -p compiler-driver --offline --test clang_lifecycle` → all pass.
3. `cargo test -p compiler-driver --offline --test clang_lane` → 16/16 unchanged.
4. `cargo fmt --check` on owned paths; zero new warnings in owned files.

## Checkpoint

One commit, explicit paths, message `feat(clang): compile whole codebases from a compilation
database and prove the generation lifecycle`. Report: commit sha, each falsifier's assertion
summary, the four command tails, the bounded argument-array capacity, smallest remaining red.

## Plan closure

Next decision after return: the build-system drive adapters (CMake/Make/Meson/BUCK, C2 packet),
then the real-world corpus packet (R11).

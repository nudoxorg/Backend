# Card clang-C1b-lifecycle — additive per-TU compile entry, then the whole-TU publish→reopen→index journey

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `308b0c264` (C1 landed: PURL grammar, database-derived
  `ClangInput` variant with bounded 64-arg capacity, typed absent-database and capacity
  terminals).

## Owned paths (v2: the seam file is lane-owned and included)

1. `compiler/driver/lower/clang.rs` — the collect seam may thread the database-derived input
   variant through to the existing pipeline (this file is the clang lane's own surface; the
   v1 stop was a card-scoping error)
2. `compiler/driver/lib.rs` (additive public entry only)
3. a new `compiler/driver/database.rs` (or similarly named module) for the additive entry
4. `compiler/driver/tests/clang_lifecycle.rs` (the integration test)
5. `compiler/driver/tests/clang_lane.rs` (helpers only if shared)

## Forbidden surface

`compiler/driver/types/**` — `CompileRequest` and every existing public type stay UNTOUCHED
(other lanes construct them; any churn there is a stop decision). `compiler/driver/lower.rs`
and every other lowering module (`lower/{rust,go,python,typescript,csharp,java}.rs`) stay
untouched; `lower/clang.rs` changes must keep its 16/16 harness and zero warnings. `compiler/ir/**`, `compiler/languages/**`
except read-only use of the C1 authority surface; `compiler/publication/**`, `server/**`
compose-from-test only. Explicit paths only, never `git add -A`.

## Public terminal

1. ADDITIVE ENTRY: one new public driver function (name it precisely, e.g.
   `compile_database_translation_unit`) that takes the database directory, the TU path, the
   bounded argument array capacity semantics from the authority's input variant, the identity/
   recipe inputs the existing path uses, the cancel flag, and the fragment output. It reuses
   the SAME collect/admit/extension pipeline as `compile()` — no second lowering
   implementation; it is the database-input seam the C1 authority variant was built for.
   Existing public types gain no fields; existing functions change no signatures.
2. WHOLE-TU JOURNEY (R10): the `clang_lifecycle.rs` integration test, over a real on-disk
   directory (two `.c` TUs sharing `base.h`, a `compile_commands.json` with `-I` and a sysroot
   cell echoed verbatim): discover TUs → compile each through the new entry → publish
   generation 1 → add a third TU + database entry → publish generation 2 → reopen the store →
   both generations' fragments validate → both index snapshots seal. Publication/index crates
   are composed from the test exactly as the python lane's driver tests compose them.
3. CAPACITY + CANCELLATION (R12): a TU beyond the lane bound yields the exact typed capacity
   terminal naming lane and required count; a cancelled flag set mid-lifecycle stops at the
   next TU boundary with the typed cancelled terminal and publishes nothing from that
   generation.
4. INTERIM RECORD: absent database → the C1 typed absent-database terminal (build-system drive
   adapters are the C2 packet).

## Constraints

- No second lowering implementation: the additive entry delegates to the existing
  collect/admit pipeline (`compiler/driver/lower` internals may be reached through their
  existing public module surface; if `admit` is sealed deeper than public modules expose, reuse
  the same composition the existing public `compile` uses — study `compiler/driver/lib.rs` and
  route through the narrowest existing seam; if that requires editing `lower.rs` beyond a
  visibility marker, STOP and report).
- Typed diagnostics only; no scanner fallback; sysroot and flags verbatim; bounded lanes with
  exact capacity faults; cancellation observed before native loading and at every TU boundary.
- Test directory cleanup: bounded retry, NotFound-only tolerance (copy `clang_lane.rs`'s
  pattern).

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lifecycle` → all pass.
2. `cargo test -p compiler-driver --offline --test clang_lane` → 16/16 unchanged.
3. `cargo test -p compiler-languages-clang --offline --features native-test` → all pass.
4. `cargo fmt --check` on owned paths; zero new warnings in owned files.

## Checkpoint

One commit, explicit paths, message `feat(clang): drive whole-TU compilation from the
compilation database through one public entry`. Report: commit sha, each falsifier's assertion
summary, the four command tails, smallest remaining red.

## Plan closure

Next decision after return: the C2 build-system drive adapters (CMake/Make/Meson/BUCK), then
the real-world corpus packet (R11).

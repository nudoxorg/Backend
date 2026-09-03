# Card clang-C1c-repair — real compilation-database TUs parse, and the generation journey is proved

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `2fd221a07`.
- Owned paths: the four C1b paths (`compiler/driver/lower/clang.rs`, `compiler/driver/lib.rs`,
  the database module, `compiler/driver/tests/clang_lifecycle.rs`) plus
  `compiler/languages/clang/{ffi.rs, input.rs, lib.rs, tests/}` — the C1 authority surface.
  Forbidden: everything else (types/**, lower.rs, other lanes, ir, publication, server).

## Finding being repaired

A real `compile_commands.json` TU lowered through the new database entry fails with
`Authority(Parse { failure: AstRead })` (libclang `CXError_ASTReadError`). C1's falsifiers
proved capacity and absence terminals but never an actual parse through the database path, so
the seam shipped unproven end-to-end. The C1b worker wrote and then deleted the real journey
test after this failure; restore it (or its equivalent) and make it pass.

## Public terminal

1. The full C1b journey test passes: real on-disk directory (two `.c` TUs sharing `base.h`,
   `compile_commands.json` with `-I` and a sysroot cell echoed verbatim) → discover TUs →
   compile each through the additive entry → publish generation 1 → add a third TU → publish
   generation 2 → reopen → both generations validate → both snapshots seal.
2. Root cause: diagnose WHY the database parse raises ASTReadError on this libclang (candidate
   classes, to verify — not to assume: relative vs absolute file path resolution against the
   database's directory, the working directory handed to the parse call, argument array
   construction, or include-path resolution from a real file entry). Fix the seam lawfully:
   flags and paths travel verbatim from the database; no guessed flags; no scanner fallback.
   Add an authority-level live falsifier for the exact failing shape so the seam can never
   again ship without a real parse through it.
3. The typed capacity/absence/cancelled terminals from C1/C1b stay green; `clang_lane` 16/16;
   PURL tests stay green.

## Constraints

Same as C1/C1b: runtime-loaded ungated symbols only; flags verbatim; bounded lanes; typed
diagnostics; explicit paths only; the custodial rust commit and other lanes' uncommitted work
stay untouched.

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lifecycle` → all pass.
2. `cargo test -p compiler-languages-clang --offline --features native-test` → all pass
   including the new real-parse falsifier.
3. `cargo test -p compiler-driver --offline --test clang_lane` → 16/16.
4. `cargo fmt --check` on owned paths; zero new warnings.

## Checkpoint

One commit, explicit paths, message `fix(clang): lower real compilation-database translation
units and prove the generation journey`. Report: commit sha, the diagnosed root cause (one
paragraph), the journey test's assertion list, the four command tails, smallest remaining red.

## Plan closure

Next decisions after return: the C2 build-system drive adapters, then the real-world corpus
packet (R11).

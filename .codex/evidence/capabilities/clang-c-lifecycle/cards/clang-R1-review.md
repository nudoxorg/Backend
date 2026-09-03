# Card clang-R1-review — independent-review repairs M1–M5 + m7 (drive transport honesty + lane faithfulness)

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `codex/fidelity-clang` @ HEAD at dispatch (Terra names the sha; expected
  `5696f8cd9` or later). You are the ONLY worker. Do NOT run a workspace-wide `cargo fmt` —
  scope formatting to your owned files.
- Owned paths: `compiler/driver/build_drive.rs`, `compiler/driver/lower/clang.rs`,
  `compiler/driver/database.rs`, `compiler/driver/tests/build_drive.rs`,
  `compiler/driver/tests/clang_lane.rs`.
- FORBIDDEN: everything else (in particular `compiler/driver/lower.rs` — do not change
  `AdmissionFault` or shared-lane constants; `compiler/driver/types/**` except reading them;
  `server/**`; `corpus_harness.rs`).
- Findings are from the independent reviewer (Terra-reproduced by inspection and accepted).

## M1 — separated redirection keeps its target as a phantom compiler input

`is_pure_redirection` (build_drive.rs ~512) strips only the operator word. For
`clang -c one.c > build.log` the argv keeps `build.log`, which `compile_build_command` then hands
to libclang as an input file. Fix: strip the redirection target together with its operator (one
word of lookahead in the same filter pass). FALSIFIER (tests/build_drive.rs): a Makefile recipe
`clang -c one.c > build.log` must drive `one.c` with arguments containing neither `>` nor
`build.log`; also pin `2> /dev/null` and `< input.txt` shapes. Keep the existing attached forms
(`1>&2`) green.

## M2 — compdb JSON string decode is lossy for non-ASCII bytes

`JsonReader::string` (build_drive.rs ~694) pushes raw bytes with `byte as char` (Latin-1 mojibake
for any multi-byte UTF-8 path) while rejecting `\uXXXX` as `Json`. Fix: accumulate the raw string
bytes and convert once with `String::from_utf8`, returning the existing typed
`CompdbError::Json { offset }` (at the string's start offset) for invalid UTF-8; `\uXXXX` stays a
typed rejection — document both in the module header. FALSIFIER: a hand-authored compdb whose
`directory` is `…/café` in raw UTF-8 drives a TU whose directory bytes equal the source bytes; and
a `\u00e9` escape yields the typed `CompdbError::Json` (not mojibake).

## M3 — argv capacity aborts the drive on non-compile commands

`parse_make` checks `argv.len() > MAX_DATABASE_ARGUMENTS` for EVERY shell command BEFORE the
`-c`+source classification, so a 257-word `ar`/link line kills the drive; the same shape exists in
`read_compdb` (capacity checked while parsing `arguments`, before admission). The module law says
non-compile commands are adapter selection. Fix: classify FIRST (`-c` + source suffix + compiler
cell for make; for compdb entries the same `-c`+suffix law), and apply the exact
`CommandCapacity`/`CompdbError::Capacity` bound only to ADMITTED translation units. The compdb
arguments-array parse must not carry the 256 cap mid-parse (the database file itself is already
bounded by the command-stream bound; an admitted TU over 256 arguments still fails exactly).
FALSIFIER: a Makefile whose link rule carries 300 object words before its compile rule drives the
compile TU; a compile command with 301 arguments still fails with exact `CommandCapacity
{ required: 301, capacity: 256 }`.

## M4 — function signatures beyond 16 children are silently min()'d

`compiler/driver/lower/clang.rs` (~1100): `let kept = signature_children.len().min(MAX_FACT_CHILDREN)`
publishes a 16-child signature for a longer function — silent truncation, contradicting "never
truncation" while the type-row path at ~1472 rejects overflow exactly. Fix: reject exactly —
`ClangCollectError::Rejected(FactRejection { fact: facts.len(), name_len: <the executable fact's
name length>, cause: FactFault::ChildCapacity })` — mirroring the lane_terminal preservation (the
helper from commit 899b6c224). FALSIFIER (tests/clang_lane.rs): a C fixture with a 17-parameter
function (and a 16-parameter control that compiles) — the 17-param compile fails with the exact
`Rejected { cause: ChildCapacity }` terminal and an untouched output buffer, and dies under the
old `min()` behavior.

## M5 — the database compile path still erases AdmissionFault

`lower_database` (lower/clang.rs ~265) maps `super::admit`'s `AdmissionFault` with
`.map_err(|_| …NoSupportedDeclaration)`. The profile path (types/compile.rs ~62-90) maps every
variant exactly. Fix: add `Admission(super::AdmissionFault)` to `ClangCollectError` (the type is
crate-visible; the enum's module-header criticism note is updated to say admission faults are now
preserved), map `lower_database`'s admit errors into it, and extend `database.rs`'s exhaustive
match with exact public terminals carrying `source_identity` + `recipe` —
`Prepare { cause: PrepareError }`, `Write { cause: WriteError }`, `Canonical { cause }`,
`ExtensionAtomUnbound { row, provisional, atom_count }` — mirroring the profile path's operands.
FALSIFIER (tests/clang_lifecycle.rs is FORBIDDEN — put it in tests/clang_lane.rs): call
`compile_build_command` with a deliberately tiny output buffer (e.g. 64 bytes) on a compiling
fixture; assert the exact new `Write`/`Prepare`-class terminal with operands (today:
`Lowering(NoSupportedDeclaration)`); the old code fails this test.

## m7 — `Leaving directory` resets to the invocation root instead of the enclosing directory

`parse_make`'s directory tracking (build_drive.rs ~468) resets to `root` on any `Leaving
directory`; ≥3-level recursive makes then misattribute the middle level's later commands. Fix: a
`Vec<PathBuf>` directory stack (push on Entering, pop on Leaving, root as base). FALSIFIER: a
root → `src` → `src/inner` nested make where a compile is echoed at `src` AFTER the inner make
returns; the unit's `directory` must be `…/src`. (Nest real `$(MAKE) -C` invocations so `make -n`
prints the transcript itself; do not fabricate the transcript in the test.)

## Constraints

Typed terminals only; no flag guessing; no new dependency; no unsafe; deny sets stay; scoped fmt
(`rustfmt --edition 2024` on the owned files only). Keep every existing test green — where M1/M3
change classification, adjust ONLY tests that pinned the old dishonest behavior, and say so in the
report.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test build_drive --test clang_lane
   --test clang_lifecycle` green twice with the new falsifiers.
2. `NUDOX_CORPUS_DIR=/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus
   cargo test -p compiler-driver --offline --test build_drive -- --ignored` (redis probe) green.
3. One commit: `fix(clang): preserve exact drive and admission causes end to end`.
   Report: commit sha, M1–M5+m7 falsifier test names + pass counts (twice), any test adjusted for
   reclassified behavior with its reason, smallest remaining red.

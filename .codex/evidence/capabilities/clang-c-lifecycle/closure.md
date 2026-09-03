# Closure receipt — clang-c-lifecycle

## Candidate identity

- Branch `codex/fidelity-clang`, candidate commit `0534cc968` (parent chain includes the L8 heal
  `db845d5c9`, L9 `dd2166cbb`, L10 `28b385f92`, L10c `180ec274`, Terra packaging `6f78997ea`,
  drive fixes `3298db496`, cause preservation `899b6c224`, corpus `0534cc968`).
- Environment: shared lane worktree `/private/tmp/nudox-fidelity-clang`,
  `CARGO_TARGET_DIR=$PWD/.local/target`, live libclang at
  `/Library/Developer/CommandLineTools/usr/lib/libclang.dylib` (runtime-loaded clang-sys),
  corpus at `.local/worktrees/clang-lifecycle/.local/corpus`, tools make/cmake/meson/ninja/buck2
  all provisioned.

## Worker custody (luna commits this round)

| card | worker | commit | Terra ingest |
|---|---|---|---|
| L8 replay heal | luna ses_f973406ebffe | `db845d5c9` | gates reproduced twice; diff re-read; falsifiers F1-F3 verified |
| L9 journey+terminals | luna ses_f972ab84affey | `dd2166cbb` | 11/11 twice; diff re-read; mutation spot-check reported at line 355 |
| L10 drive (T1-T3) | luna ses_f9717c658ffe | `28b385f92` | 15/15 twice; redis probe 234 TUs reproduced; T4 not delivered (honest report) |
| L10b packaging | luna ses_f970fdbe6ffe | none (honest stop) | root-caused Terra: compdb junk-edge admission |
| L10c compdb law | luna ses_f96fe11afffe | `180ec274` (product) | admission law verified; worker again deferred tests; a workspace-wide `cargo fmt` left uncommitted on non-owned surfaces was restored by Terra (custody incident, formatting-only, verified) |
| packaging journeys + Entering-directory/empty-selection fixes + corpus finish | Terra (test-authoring lane + mechanical product fixes smaller than a worker turn) | `6f78997ea`, `3298db496`, `899b6c224`, `0534cc968` | all gates reproduced twice per commit |

Terra mechanical fixes this round: parallel temp-dir nonce collisions (`74c3a41fb`), failed-drive
capture retaining stdout+stderr, recursive-make Entering-directory transport, exportless cmake
configure → `NoTranslationUnits`, pooled-lane rejection cause preservation (`899b6c224`) with the
exact falsifier `include_list_over_the_pooled_bound_names_the_exact_cause`.

## Public terminal

PURL → build-system drive (CMake/Meson/Make/BUCK, verbatim flags) → per-TU libclang authority →
canonical fragments → durable publish gen-1 → shutdown + reopen → open + validate → index build +
seal → gen-2 with changed content → publish → reopen → both generations validate → old fragment
reopened from the immutable store. Every capacity and cancellation boundary is a typed terminal:
drive (`ToolAbsent`/`Cancelled`), compile (`Cancelled{input}`/`Authority(ScratchCapacity)`),
publish (`CancelledBeforeStorage`, `BindingOutputLength`), reopen (`FragmentOutputTooSmall`),
index (`OutputTooSmall{region}`, `ExactScratch`).

## Exact gates (all green, run twice)

- `cargo test -p server-workflow --offline` (12), `cargo test -p server-journal --offline` (42)
- `cargo test -p compiler-driver --offline --test clang_lane --test clang_lifecycle
  --test build_drive` (19 / 11 / 19+1 ignored)
- `cargo test -p compiler-languages-clang --offline` (9)
- `cargo test -p compiler-ir --offline` (37 across suites)
- corpus: twice with `NUDOX_CORPUS_DIR` (peak RSS 253,689,864 / 270,352,384 bytes via one
  `/usr/bin/time -l` per run) + no-op without the marker

## Strongest surviving counterexample

A translation unit with seventeen `#include`s could not be represented: the shared emission lane's
pooled reference-list row is 16 elements wide, and the clang lane funneled the TU's entire include
spelling set through one list — every real-world C file (json_object.c: 34 includes) died at a
cause-erased `NoSupportedDeclaration` that misreported a capacity wall as an unsupported
declaration. The erasure is fixed (exact `FactRejection{ordinal, name_len, cause}` now survives);
the 16-element pooled row itself is a shared-lane geometry/wire decision recorded for Sol, and
affected corpus rows fall back to public-header slices with the wall verbatim in their cells.

## Residual named defects (not silently accepted)

1. yaml-cpp: index seal rejects its 36-fragment generation (`compiler-derived segment identities
   did not form an index snapshot`) — smallest repro is the corpus row.
2. Vulkan-Headers: header-only project → `NoTranslationUnits` (honest terminal; no edges exist).
3. buck2-with-prelude: the real cell's own uquery fails (incomplete shallow checkout) — honest
   `DriveFailed` with captured evidence; positive Buck drive proven by fixture.
4. Full-set geometry walls: sqlite declarations 1025>1024, zlib/pugixml references 4097>4096,
   nng emission facts 1024, occurrence-lane capacity, include-list `RefListElements` — all exact
   typed terminals at the declared shared-lane geometry; raising that geometry is a Sol decision.
5. FragmentView lacks decoded include/diagnostic accessors (recorded gap; no source-text scan
   substituted).

## Environment adaptation

The registered `nudox_terra_reviewer` runs as the read-only `reviewer` opencode subagent
(edit:deny by configuration) rather than a separate sidecar codex parent; Terra reproduces every
accepted finding, and worker cards carry exact owned paths + digests recorded in index.toml.

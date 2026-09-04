# Card clang-L10b-packaging — the missing half of L10: per-system packaging journeys + tool-absent twins (R16)

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `codex/fidelity-clang` @ the commit carrying L10's T1–T3 (Terra names it at
  dispatch; expected `28b385f92` or its record commit). You are the ONLY worker on the worktree.
- Owned paths: `compiler/driver/tests/build_drive.rs` ONLY. (If — and only if — a journey needs a
  product fix, STOP and report instead of editing `build_drive.rs`.)
- FORBIDDEN: everything else. This card completes EXACTLY the missing T4 of card clang-L10-drive;
  T1–T3 are landed and reproduced (redis probe: 234 TUs, no `UnrecognizedCompileCommand`;
  `buck2_compilation_database_target_is_driven` green twice; `cmake_and_meson_redrive_are_idempotent`
  green twice).

## Public terminal (R16: packaging per build system, end-to-end)

### J1 — make packaging journey, completed

Extend `make_drive_compiles_and_publishes_two_generations` with the halves it still lacks, in
place (do not fork a second make journey):
after the gen-1 publish: `journal.shutdown()` → `DurablePublisher::reopen` → `open_published`
validates all gen-1 fragments → `server_index_build::build` + `seal_compilation_index` on the
reopened package. Then (it already drives gen-2 and opens it live) also validate through a SECOND
shutdown+reopen that gen-2 is the selected publication and gen-1's first fragment still opens
from `ImmutableArtifactStore` and validates.

### J2 — cmake packaging journey (new)

`cmake_packaging_journey_drives_publishes_reopens_and_indexes`: a small real CMake project
(CMakeLists.txt with `cmake_minimum_required`, a library/include dir, TWO .c files with distinct
records; second file added only in gen-2 together with its `target_sources`/list append), gated on
cmake availability the same way `absent_cmake_is_not_silently_skipped` detects it. Full chain:
drive → compile every driven TU with `compile_build_command` (assert the driven `-I` flag reaches
a live parse that only resolves a header under the include dir) → publish gen-1 → shutdown+reopen
→ open+validate → index build+seal → mutate (change one .c's bytes; add the third file) → drive
again → publish gen-2 → shutdown+reopen → open: all fragments validate, the changed fragment's
bytes differ from gen-1's, gen-1's first fragment still validates from `ImmutableArtifactStore`.

### J3 — meson packaging journey (new)

`meson_packaging_journey_drives_publishes_reopens_and_indexes`: same shape as J2 with a real
`meson.build` (`project()`, `executable()`), gated on meson AND ninja availability; relies on the
landed `--wipe` re-drive for the second `discover_and_drive` over the same scratch.

### J4 — tool-absent twins (new)

`absent_meson_and_make_are_not_silently_skipped`: in the style of
`absent_cmake_is_not_silently_skipped`, run `discover_and_drive` for a meson-marked root and a
Makefile-marked root with a PATH that contains NEITHER tool (build the child PATH from the
surviving dirs only — exclude /opt/homebrew/bin and /Users/mileswirht/.local/bin entries that
carry the binary, exactly as the cmake twin does); assert the exact `BuildDriveFailure::ToolAbsent`
terminal naming `meson` (and `ninja` when the project needs the compdb step — assert whichever
fires first and document the order in a comment) and `make`. Never an empty TU list.

## Constraints

- Reuse the existing helpers in the file (`directory`, `toolchain`, `compile_unit`, `limits`,
  the in-test `publish` closure of the make journey); do not fork a parallel harness.
- Tool gating must keep the suite green on machines without the tools (skip-with-evidence via
  the existing early-return style is acceptable ONLY for the real-tool journeys; the absent-tool
  twins must always run).
- Deny set stays; no new dependency; no product edits; `cargo fmt` on the owned file.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test build_drive` green twice with J1–J4 present.
2. Report each journey's pass count and the exact reopen/index calls used.
3. One commit: `test(clang): codify packaging per build system with reopened chained generations`.
   Report: commit sha, journey names + pass counts (twice), smallest remaining red.

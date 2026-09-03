# Card clang-L10c-compdb — compdb entry admission mirrors the established non-compile-selection law (repairs L10's ninja regression; unblocks the meson packaging journey)

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `codex/fidelity-clang` @ `b142fb136` plus the L10b worker's local state if any
  (Terra will say; expected: clean at b142fb136). You are the ONLY worker.
- Owned paths: `compiler/driver/build_drive.rs`, `compiler/driver/tests/build_drive.rs`.
- FORBIDDEN: everything else (in particular `compiler/languages/clang/**`, `database.rs`,
  `corpus_harness.rs`, `clang_lifecycle.rs`, `server/**`).

## Defect (Terra-diagnosed; reproduce first)

L10 changed `run_ninja` from `-t compdb c cxx` to bare `-t compdb`. Without a rule filter, ninja
emits a compdb entry for EVERY edge: link edges (`file: "probe.p/main.c.o"`, no `-c`), phony
edges (`file: "meson-internal__test"`, empty command), and tool-runner edges
(`file: "all"`, command `meson test ...`). `read_compdb` admits every entry as a translation
unit, so the meson path now yields junk TUs and `compile_build_command` dies with
`DatabaseCompileFailure::Authority` ("libclang rejected the database translation unit") when it
tries to parse an object file or a phony name as C source. Terra reproduced the exact junk-entry
shape with a real meson+ninja project under
`/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/meson-probe2`.

`build_drive.rs`'s module header already declares the law: "Non-compile lines (`mkdir`, `ar`,
`echo`, and link commands) are adapter selection, not translation units." The JSON compdb path
must follow the SAME law instead of admitting every entry.

## Public terminal

1. In `read_compdb` (or the shared entry-admission step used by the cmake, meson, and buck
   paths): an entry becomes a translation unit iff its argv contains `-c` AND its `file` names a
   C/C++ source suffix (the existing `is_source`). Entries without `-c` are adapter selection
   and are skipped (document this in the module header next to the make law). An entry WITH `-c`
   and a source-suffixed file whose COMPILER cell is unrecognized raises the exact existing
   `UnrecognizedCompileCommand`-class failure only where the make path raises it — for JSON
   entries keep today's behavior (no compiler check) unless a test demands symmetry; record your
   choice in the report. Empty `command` strings parse to zero argv and are skipped by the same
   law.
2. `database_directory` for the meson path stays the build directory (compdb entries carry
   absolute `directory` cells; the source file cell `../main.c` resolves relative to it).
3. Falsifiers (new tests in `tests/build_drive.rs`):
   - F1: a hand-authored compdb containing one real compile entry, one link entry
     (`file: "out.o"`, no `-c`), one phony entry (empty command), and one tool-runner entry
     (`command: "meson test ..."`) yields EXACTLY the real compile TU.
   - F2: the existing cmake journey shape still passes (real cmake compdb entries all carry
     `-c`).
   - F3: `meson_packaging_journey_drives_publishes_reopens_and_indexes` from card clang-L10b
     (J3) — implement it now; it must pass end-to-end with the real meson (/opt/homebrew/bin/meson)
     and ninja (/etc/profiles/per-user/mileswirht/bin/ninja): drive (twice over the same scratch,
     proving `--wipe` re-drive) → compile all TUs → publish gen-1 → shutdown+reopen → open+validate
     → index build+seal → mutate → drive again → publish gen-2 → shutdown+reopen → both
     generations validate, changed fragment bytes differ, gen-1's fragment still validates from
     `ImmutableArtifactStore`. Model every helper on the existing make journey.
   - F4: the absent-tool twin `absent_meson_and_make_are_not_silently_skipped` from L10b (J4).
4. Re-run the redis probe (`redis_make_compound_probe_has_no_unrecognized_compile_command`,
   NUDOX_CORPUS_DIR-gated) to prove no regression in the make path.

## Constraints

- No new dependency, no unsafe, deny set stays, typed terminals only, `cargo fmt` on owned files.
- This card EXISTS to restore an honesty law, not to widen one: no per-entry guessing, no
  heuristic beyond the documented `-c`+source-suffix selection.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test build_drive` green twice with F1–F4.
2. Redis probe green (NUDOX_CORPUS_DIR=/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus).
3. One commit: `fix(clang): admit only compile commands from generated compilation databases`.
   Report: commit sha, F1–F4 pass counts (twice), the compiler-check symmetry choice, smallest
   remaining red.

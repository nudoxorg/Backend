# Card clang-L10-drive — finish C2: real drive for Make compound lines and Buck compdb targets; per-system packaging journeys (R13/R16)

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `codex/fidelity-clang` AFTER the L8 replay-heal commit lands (Terra points you
  at the exact sha; the packaging journeys need chained reopen).
- Owned paths: `compiler/driver/build_drive.rs`, `compiler/driver/tests/build_drive.rs`.
- FORBIDDEN: `compiler/languages/clang/**`, `compiler/driver/database.rs`, `lower/**`,
  `clang_lifecycle.rs`, `corpus_harness.rs`, `server/**`, goldens.

## Verified mechanism facts (Terra pre-verified on this machine, 2026-09-03)

1. `make -n` recipe lines may be COMPOUND shell lines: redis prints
   `printf '    %b %b\n' "\033[34m"CC"\033[0m" ... 1>&2;clang -pedantic ... -c threads_mngr.c`.
   Today `parse_make` classifies the whole line by `argv[0]` and rejects it as
   `UnrecognizedCompileCommand` — the corpus redis row dies here. The honest transport is
   top-level shell command splitting, NOT flag interpretation.
2. Meson re-drive: `meson setup build` on an existing configured build dir FAILS
   ("run meson setup --wipe"). A second generation drive over the same scratch directory dies
   with `DriveFailed` today.
3. Buck2 (binary at /Users/mileswirht/.local/bin/buck2): `compilation_database` is NOT a rule in
   the bundled/default prelude ("Variable `compilation_database` not found"). A repo that wants a
   compdb defines its OWN rule; Terra verified this exact flow works with real buck2:
   - `buck2 uquery 'kind("compilation_database", //pkg/...)'` → target list (empty ⇒ genuinely
     unavailable through the build system);
   - `buck2 build <targets> --show-output` → lines `<target> <cell-relative output path>` (e.g.
     `root//:compdb buck-out/v2/art/root/<hash>/__compdb__/compile_commands.json`);
   - read that file → existing `read_compdb`.
   The current adapter returns `ToolPresentUndrivable` UNCONDITIONALLY after the query, even when
   targets exist — that is an adapter dodge, not a typed absent terminal.

## Public terminal

### T1 — Make compound-line transport (R13, redis class)

Split each `make -n` line into top-level shell commands on `;`, `&&`, `||`, `|` (quote-aware, at
the same nesting depth as `shell_words`), classify each command independently with the existing
`-c`+source+compiler recognition, and strip pure redirection tokens (`1>&2`, `2>&1`, `>x`, `>>x`,
`2>x`, `<x`) from a command's argv before classification. Unknown non-compile commands
(`printf`, `echo`, `mkdir`, `ar`) stay silently non-TUs; a compile-shaped command (`-c` + source
suffix) with an unrecognized compiler cell REMAINS the exact `UnrecognizedCompileCommand`
terminal — no new silent skips. Falsifier: a Makefile whose recipe is
`@printf '  %b\n' "CC" "x.o" 1>&2;clang -Iinclude -D'VERBOSE=1' -c one.c -o one.o`
drives `one.c` with its flags verbatim; and a compound line whose compile half names an unknown
compiler still fails typed.

### T2 — Buck positive path (R13)

In `discover_and_drive`'s Buck arm: run the existing scoped `uquery`; if it yields targets,
drive them: `buck2 build <targets...> --show-output` from the cell root (PATH-augmented like the
other children, cancellation observed before spawn), parse each `--show-output` line's
cell-relative output path, read those bytes, and parse them with the existing `read_compdb` into
translation units (empty database rows ⇒ existing `NoTranslationUnits`). Query failure or build
failure ⇒ exact `DriveFailed { tool: "buck2", captured }`. Zero query targets ⇒ keep the exact
`ToolPresentUndrivable { tool, evidence }` with the captured query output. Falsifier: a test
gated on buck2 availability (same style as `buck2_nested_marker_returns_query_terminal_at_cell_root`)
authors a scratch cell — `.buckconfig` (`[cells] root = .`), `compdb.bzl` custom rule and a BUCK
target with Terra's verified shape (copy the mechanism, assert real compile commands for a real
`main.c` with an `-I` include land in the parsed units) — and `discover_and_drive` returns C-sourced
translation units with `build_system == Buck` through the REAL buck2 binary. The existing
no-targets terminal test stays green against the real buck2-examples checkout.

### T3 — Meson re-drive (R13)

`run_one` for Meson must survive a re-drive over an occupied build directory: pass `--wipe` when
the target dir already exists (still a single `meson setup` invocation), keeping
`ToolAbsent`/`Cancelled`/`DriveFailed` semantics. Falsifier: two successive
`discover_and_drive` calls on the same meson project + scratch directory both succeed with
identical translation-unit lists. (CMake re-configure over an existing build dir is already
idempotent — assert it too in the same test shape.)

### T4 — packaging journeys per build system (R16)

Generalize `make_drive_compiles_and_publishes_two_generations`'s shape into one journey per
system, each gated on real tool availability and each proving
drive → per-TU authority → fragment → publish gen-1 → **journal.shutdown + DurablePublisher::reopen**
→ open → validate ALL fragments → index build + seal → mutate the project (change one .c's bytes;
add a file to the build description) → drive again → publish gen-2 → reopen → validate both
generations (gen-1 fragment still validates from `ImmutableArtifactStore`):
- `cmake_packaging_journey_drives_publishes_reopens_and_indexes` (real cmake; keep
  `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`; assert the driven flags — `-I`/`--sysroot` if present —
  travel byte-identical into a live parse);
- `meson_packaging_journey_drives_publishes_reopens_and_indexes` (real meson + ninja via
  `-t compdb c cxx`);
- `make_packaging_journey_...` — extend the existing make journey with the reopen + both-index
  halves it lacks;
- buck2's journey stays the typed-terminal one (`ToolPresentUndrivable` with evidence on the real
  checkout) PLUS the T2 positive-drive test — Buck packaging is proven where the build system
  actually exposes a compdb.
Tool-absent twins: extend the existing `absent_cmake_is_not_silently_skipped` shape with
`ToolAbsent` journeys for meson+ninja and make when the binary is missing (PATH without the
tool), asserting typed terminals, never empty-TU success.

## Constraints

- Typed terminals only; no flag guessing: every argument a TU carries must come from build-system
  output. `shell_words`/splitting is transport classification, documented in the module header.
- No new dependency, no unsafe, deny set stays, `#![forbid(unsafe_code)]` in the test file stays.
- Keep the PATH-augmentation child-process discipline and cancellation checks before every spawn.
- `cargo fmt` on owned files.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test build_drive` green twice with all new
   falsifiers.
2. Corpus spot-rows (read-only consumption; `NUDOX_CORPUS_DIR=/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus`):
   a tiny Rust-free check that redis's make now classifies its compile commands — drive redis via
   a one-off `discover_and_drive` probe test under `--ignored` or document the manual probe:
   report the observed redis TU count and that no `UnrecognizedCompileCommand` fires.
3. One commit: `feat(clang): drive make compound lines and buck compdb targets with per-system packaging journeys`.
   Report: commit sha, per-system journey command tails, redis probe result, exact terminal names
   pinned, smallest remaining red.

# Card clang-W2r3-drive — compdb identity fix + real cmake/meson drives + honest buck terminal

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `39d8da5df` + the uncommitted working tree state of
  `compiler/driver/tests/build_drive.rs` (+324 lines, your predecessor's red e2e — keep editing
  it, do not revert).
- Owned paths:
  - `compiler/driver/build_drive.rs` (+ optional `compiler/driver/build_drive/` module dir)
  - `compiler/driver/tests/build_drive.rs`
  - `compiler/driver/database.rs` (additive public entry only)
  - `compiler/driver/lib.rs` (exports)
  - `compiler/languages/clang/ffi.rs` (the `parse_arguments` strip rule ONLY — see finding 1)
  - `compiler/languages/clang/input.rs` (only if the strip fix needs a typed cell there)
- FORBIDDEN: `compiler/driver/tests/clang_lifecycle.rs` (committed, stable, another lane's pin
  lives there), `compiler/languages/clang/collect.rs`, everything else. The tree also has
  unrelated fmt-only drift in `server/journal/**`, `compiler/ir/**`, `rust_traits.rs` — NEVER
  stage those; commit only your owned paths.

## Terra-verified root-cause facts (reproduced on this tree, 2026-09-03)

1. **libclang's JSON compilation-database reader misreports the input file when the arguments
   contain `-o <output>`**: an entry with `"file":"one.c", arguments:[…, "-c","one.c","-o","one.o"]`
   returns `CompileCommand::file_name() == "one.o"` (empirically verified through
   `CompilationDatabase::from_directory`). CMake- and Meson-generated compdbs ALWAYS carry `-o`,
   so every real build system hits this.
2. **`parse_arguments` (`compiler/languages/clang/ffi.rs`)** strips argv[0] and the LAST argument,
   assuming the source file is last. Real build commands end with `-o <out>` or place the source
   anywhere, so the parse receives a wrong/dangling input cell — the observed effect was a
   "successful" parse of an EMPTY translation unit (zero entities, three identical empty exact
   segments, then `DuplicateExactSegment` at seal).

## Repairs

1. **`parse_arguments` strip rule**: strip argv[0] (the compiler cell) and every argument cell
   that EXACTLY equals the input `file_name` bytes — not "the last cell". Keep everything else
   verbatim and keep the capacity fault. The `-o` cells remain and libclang ignores `-o` when
   parsing (verify with a falsifier that the parsed fragment has entities). Note in a comment:
   for database-loaded entries the caller must supply the TRUE source name (see repair 3).
2. **Lane-owned compdb reader** in the driver (`build_drive/compdb.rs` or equivalent): a minimal
   recursive-descent reader for the documented compilation-database JSON grammar — top-level
   array of objects with string cells `directory`, `file`, optional `arguments` (array of
   strings) or `command` (single string, shell-split with the same quoting discipline the make
   parser uses). Typed errors for every malformed shape (bad JSON, missing `file`, missing both
   argument forms, over-capacity argv), retaining the offending byte offset or cell. No new
   dependency. The `file` cell is the input-file identity — never overridden by libclang's
   quirked reader.
3. **New public compile entry** in `compiler/driver/database.rs`:
   `compile_build_command(command, source, profile, stage, toolchain, cancelled, output)` taking
   the adapter's own verbatim command (file + arguments + working directory) — the identity
   authority is the compdb `file` cell or the make parse, never libclang's mangled view. Reuse
   `lower::clang::lower_database`'s shape (it is pub(crate); mirror its construction). Keep the
   existing `compile_database_translation_unit` untouched (its libclang-reader quirks apply only
   to callers that opt in; note this in its doc comment).
4. **Adapters switch to the honest path**: the Make adapter stops synthesizing a
   compile_commands.json (delete that round-trip) and returns its parsed commands directly; the
   CMake adapter drives `cmake -S -B -DCMAKE_EXPORT_COMPILE_COMMANDS=ON` and then reads
   `<build>/compile_commands.json` with the lane-owned reader; the Meson adapter drives
   `meson setup` + `ninja -C <build> -t compdb c cxx` and reads the stdout JSON with the same
   reader.
5. **Buck2 (REAL binary now installed at `$HOME/.local/bin/buck2`, version 2026-09-03)**: the
   drive is `buck2 uquery 'kind("compilation_database", //…)'` scoped to the detected cell; when
   the repo exposes compilation_database targets, build one and read its compdb artifact with the
   lane reader. Terra verified TODAY on buck2's own with_prelude examples: the bundled prelude
   does NOT export a `compilation_database` rule (`Variable compilation_database not found`), so
   when the query finds none, return the exact typed `ToolPresentUndrivable { tool: "buck2",
   evidence }` terminal with the captured query/build evidence — that is the sanctioned
   absent-adapter terminal (present tool, genuinely unavailable extraction), not a skip. Falsifier:
   a test driving the prelude example shape (or a stub buck2 on PATH that prints the same query
   result) asserting exactly that terminal with non-empty evidence.

## Integration falsifiers (tests/build_drive.rs — finish the owed ones with the new entry)

- **Make e2e with publishing** (the red one): drive → per-command `compile_build_command` →
  assert the fragments have NON-ZERO entities (this is the regression that was missed) → publish
  gen-1 → add TU → drive → publish gen-2 through the SAME publisher → generation + pinned_root
  advance → `open_published` on the live publisher → all fragments validate → index
  build/plan/encode/seal (must now pass: distinct non-empty exact segments) → gen-1 fragment from
  `ImmutableArtifactStore` hash-identical + validating. Do NOT reopen a chained journal (trunk
  replay defect, pinned in clang_lifecycle.rs).
- **CMake real drive** (cmake IS installed at /opt/homebrew/bin — augment PATH in the test via
  `std::env::set_var` under a static mutex, or accept the augmented ambient PATH; be
  deterministic): real CMakeLists project → drive → every command through `compile_build_command`
  → non-zero entities → verbatim `-I` transport proven by a header only resolvable via the
  CMake-supplied include dir.
- **Meson real drive** (meson IS installed at /opt/homebrew/bin; ninja present): same shape,
  include dir supplied via `include_directories()`.
- **Absent terminals**: cmake/meson absent via PATH-restricted env under the same mutex; buck2
  absent vs present-undrivable (stub binary printing empty query output).
- Keep: no-marker, drive-failure, cancel-before-spawn, cancel-leaves-no-database, ccache-verbatim,
  unrecognized-compile-command, >64 KiB command stream, large-stream terminal.

## Constraints

Typed terminals only, no silent skips, no new dependency, no unsafe, no network in tests, scratch
dirs under caller paths, `#![forbid(unsafe_code)]` + deny set. Flags/sysroot/includes travel
verbatim; dropping the `-o` cell pair from the PARSE call is forbidden — only the input-file cell
is removed, matched by exact name.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test build_drive` green three consecutive runs.
2. `cargo test -p compiler-driver --offline --test clang_lane` stays 16/16 (you touched ffi.rs —
   prove no authority regression).
3. `cargo test -p compiler-driver --offline --test clang_lifecycle` stays 7/7.
4. `cargo fmt --check` on owned files only. One commit:
   `fix(clang): make build-system command identity honest and drive cmake, meson, and buck for real`.
   Report: commit sha, finding-by-finding resolution, the compdb reader's grammar coverage, exact
   command tails, smallest remaining red.

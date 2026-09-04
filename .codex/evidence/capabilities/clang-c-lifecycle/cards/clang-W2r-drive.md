# Card clang-W2r-drive — build-drive repair: honest terminals, real drives, full e2e

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `ace71dbd` (your predecessor's accepted-partial base).
  Keep the module layout; repair it.
- Owned paths (unchanged): `compiler/driver/build_drive.rs`, `compiler/driver/lib.rs` (exports),
  `compiler/driver/tests/build_drive.rs`. Everything else is forbidden. Leave
  `clang_lifecycle.rs` alone (another worker owns it — it currently has uncommitted changes).

## Findings being repaired (each needs a falsifier or an exact fix)

1. **Buck lies.** `discover_and_drive` returns `ToolAbsent` for Buck WITHOUT checking the PATH.
   If `buck2` is absent → `ToolAbsent` is correct. If present → you must not fake absence: STOP
   and report the exact `buck2 --version` + query-surface evidence for Terra adjudication instead
   of inventing a drive. Falsifier: a test that runs discovery under a PATH that CONTAINS a stub
   `buck2` executable must NOT return `ToolAbsent` (it should return the present-but-undrivable
   path you implement or stop before shipping).
2. **Meson runs in the wrong CWD.** `Command::new("meson").arg("setup").arg(build)` executes in
   the test process's CWD. Set `.current_dir(root)` (or pass the source dir explicitly the way
   CMake does with `-S`).
3. **The 64 KiB cap hits the product, not just diagnostics.** For Make, `make -n` stdout IS the
   command stream; real projects exceed 64 KiB immediately, so the current cap turns large repos
   into `DriveFailed` or silently truncated discovery. Restructure: read child stdout fully up to
   a generous, documented product bound (e.g. 16 MiB) with a typed overflow terminal
   (`CommandStreamTooLarge { bytes }`), and keep the 64 KiB retained-diagnostic cap for stderr
   only. Falsifier: a Makefile whose dry-run output exceeds 64 KiB still drives successfully.
4. **Silent skips in `parse_make`.** A line containing `-c` and a known source suffix whose
   compiler is not recognized (ccache/sccache wrappers are common in real projects) is currently
   dropped silently. That breaks the no-silent-skip law. Add a typed terminal
   (`UnrecognizedCompileCommand { line: String }` or an ordinal-cell shape you justify) for such
   lines, and handle the ccache-prefixed form by transporting argv verbatim (the wrapper IS the
   command). Non-compile lines (`mkdir`, `ar`, `echo`, linker invocations without `-c`) are
   legitimately not TUs; skipping them is adapter selection, not a skip — say so in the module
   header.
5. **Environment change: cmake and meson are NOW INSTALLED** at `/opt/homebrew/bin/` (not on the
   default test PATH). Two consequences:
   - The absent-terminal tests for cmake/meson must scope PATH explicitly (e.g.
     `std::env::set_var("PATH", scratch_dir)` guarded by a static test mutex so parallel tests
     don't race), or the discovery entry must accept the ambient PATH — pick one mechanism,
     document it, and make the absent-terminal tests deterministic.
   - You must now prove REAL drives: a CMake project (`cmake_minimum_required` +
     `project` + `add_library(main main.c)` + `target_include_directories`) and a Meson project
     (`project()`, `files()`, `include_directories`) each drive end-to-end with the augmented
     PATH: `discover_and_drive` → for every command `compile_database_translation_unit` →
     `FragmentView::validate`, with one header only resolvable because of a flag the build file
     supplied (verbatim transport proof for cmake `-I` and meson `include_directories`).

## Completion of the original card (still owed — the checkpoint was accepted as partial)

6. Make e2e with PUBLISHING: drive a real Make repo (two `.c` TUs + included header + one
   `--sysroot <dir>` style flag that only works verbatim) → compile every command through
   `compile_database_translation_unit` → publish gen-1 (`publish_compiled`,
   `PublishControl::Continue`) → add a TU + Makefile rule → drive again → publish gen-2 through
   the SAME publisher (the rust precedent in `tests/rust_purl_lifecycle.rs` lines ~339-376; do
   NOT reopen between generations) → assert generation advanced and `pinned_root` changed →
   `open_published` → all fragments validate → index build/plan/encode/seal.
7. Drive-failure falsifier: a Makefile that fails (`make -n` on a missing rule) → exact
   `DriveFailed` with retained capture.
8. Cancel-before-spawn and cancel-between-phases tests (at least one cancelled spawn observable
   via a marker file the rule would have touched).

## Constraints

Unchanged from W2: typed terminals only, no silent skips, no new dependency, no unsafe, no
network, scratch dirs under caller-provided paths, `#![forbid(unsafe_code)]` + deny set.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test build_drive` green three consecutive runs.
2. `cargo test -p compiler-driver --offline --test clang_lifecycle` unchanged-state (do not edit).
3. `cargo fmt --check`. One commit: `fix(clang): make build drives honest, complete, and real`.
   Report: commit sha, finding-by-finding resolution, the terminal enum's final shape, exact
   command tails, smallest remaining red.

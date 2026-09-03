# Card clang-W2-drive — C2 build-system drive adapters: CMake, Make, Meson, BUCK

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `621b7d67c`.
- Owned paths (exactly these, all new except lib.rs):
  - `compiler/driver/build_drive.rs` and/or `compiler/driver/build_drive/` (new module)
  - `compiler/driver/lib.rs` (module registration + public export only)
  - `compiler/driver/tests/build_drive.rs` (new integration test file)
- Forbidden: `compiler/driver/database.rs`, `compiler/driver/lower/**`, `compiler/languages/**`,
  everything outside the owned paths. If you believe `database.rs` must change, STOP and report.

## Capability

`discover_and_drive(root, cancelled, …) -> Result<DrivenCompilation, BuildDriveFailure>`: from a
local checkout root, detect the codebase's own build system and drive it to obtain the exact
compile commands (TU list + argv + working directory) that feed the existing per-TU authority
entry `compile_database_translation_unit` (`compiler/driver/database.rs`, read as a consumer, do
not edit). Marker detection selects the adapter — it never supplies flags:

| marker (first match wins) | system | drive |
|---|---|---|
| `CMakeLists.txt` | CMake | `cmake -S <root> -B <build> -DCMAKE_EXPORT_COMPILE_COMMANDS=ON`, then open `<build>/compile_commands.json` through `compiler_languages_clang::CompilationDatabase::from_directory` |
| `meson.build` | Meson | `meson setup <build>`, then `ninja -C <build> -t compdb c cxx` (ninja IS installed on this host) producing compdb in the build dir, same database path |
| `Makefile` / `makefile` / `GNUmakefile` | Make | `make -n` (dry run, `-C <root>`); parse echoed compile lines (compiler + `-c` + `.c`/`.cc/.cpp` source); retain argv verbatim, then serialize a compile_commands.json equivalent into a caller-owned scratch dir so ONE downstream database path is reused |
| `BUCK` (or `BUCK2`) | Buck | if `buck2` binary is absent → exact typed `ToolAbsent` terminal (it is absent on this host). If you find a binary: drive its real command-extraction path; if none exists, STOP and report the exact CLI evidence for Terra adjudication instead of inventing one |
| none of the above | — | typed `NoBuildSystemDetected { root }` |

## Non-negotiable laws

- Flags/sysroot/include paths travel verbatim from build-system output. Parsing `make -n` output
  to SPLIT an already-echoed command line into argv is transport, not guessing — but you must not
  normalize, reorder, drop, or add flags. Strip only argv[0] (the compiler) the same way the
  database entry already does.
- Typed terminals, never silent skips: `ToolAbsent { tool }` (binary not found in PATH),
  `DriveFailed { tool, captured }` (the build system ran and failed; retain bounded captured
  stderr), `NoTranslationUnits { tool }`, `CommandCapacity { required, capacity }` (reuse the
  64-argument bound), `NoBuildSystemDetected { root }`, `Cancelled`. No `.unwrap`, no `let _ =`.
- Cancellation is observed before every process spawn and between TUs.
- Bounded capture: child stdout/stderr capped (typed overflow terminal or exact-truncated with a
  typed `captured_truncated: bool` cell — pick one and document it in the module header).
- Reuse existing machinery where it fits: read `compiler/driver/native/child.rs` and
  `native/work.rs` first; reuse their spawn/capture/work-dir discipline instead of inventing a
  second one. If they truly do not fit, say why in the module header.
- Build/scratch dirs go under the caller-provided scratch path (tests use a fresh temp dir),
  never the repo checkout.
- No new crate dependency. No unsafe. No network.

## Host facts

macOS; PATH has `make` 4.4.1, `clang`/`clang++` (Xcode CLT), `ninja`; `cmake`, `meson`, `buck2`
are ABSENT. libclang runtime-loads from the CLT. Tests must not require network.

## Integration falsifiers (mandate: repo → TU → IR per build system)

In `tests/build_drive.rs`, all real, no mocks:

1. **Make drive e2e (tool present):** write a real repo with `Makefile` (two `.c` TUs + one
   included header, one `--sysroot`-style flag echoed verbatim), `discover_and_drive` it, then for
   each command compile through `compile_database_translation_unit` and validate the fragments;
   assert the exact argv reached libclang (a header only resolvable via a flag from the Makefile
   proves verbatim transport). Then publish gen-1 → reopen → publish gen-2 after adding a TU —
   mirroring the composition of `tests/rust_purl_lifecycle.rs` — and index both generations.
2. **CMake absent terminal:** marker present, binary absent → exact `ToolAbsent` variant, and the
   test asserts NOTHING was spawned for the drive (observable via the terminal variant alone).
3. **Meson absent terminal:** same shape (binary absent on this host; ninja presence does not
   authorize running Meson's setup — the system is Meson or nothing).
4. **Buck absent terminal:** same shape.
5. **No-marker terminal:** empty dir → `NoBuildSystemDetected`.
6. **Make drive-failure terminal:** a Makefile that fails (e.g. missing include of a rule) →
   `DriveFailed` with retained bounded capture.
7. **Cancellation:** cancelled flag set after detection → no spawn, `Cancelled`.

## Checkpoint

One commit `feat(clang): drive codebase build systems to whole-TU compile commands`. Report:
commit sha, module layout, the exact terminal enum, which existing child machinery you reused and
why, command tails (`cargo test -p compiler-driver --offline --test build_drive` etc.),
`cargo fmt --check`, smallest remaining red. Run the test file three times for determinism.

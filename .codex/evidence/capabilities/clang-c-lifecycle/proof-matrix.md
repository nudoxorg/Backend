# Proof matrix — clang-c-lifecycle

States: RED | PROVED BY WORKER | REPRODUCED BY TERRA | FALSIFIED | UNVERIFIED.
Only Terra moves a row to REPRODUCED BY TERRA.

| # | law | weakened implementation it kills | falsifier | required evidence | state | owner |
|---|---|---|---|---|---|---|
| R1 | clang lane test module compiles; clang assertions execute | shipping with a broken test module hidden by cross-lane reds | `cargo test -p compiler-driver --offline --lib clang 2>&1` shows zero diagnostics naming `lower/clang.rs` | exact command output | RED | L-repair |
| R2 | C++ virtual override edges are authority facts, not source guesses | scanner or spelling inference for overrides | C++ fixture with `virtual`/`override` across a header base; decoded `OverrideFact` rows must equal libclang's `clang_getOverriddenCursors` identities; mutating the derived method's source spelling must not change facts | live authority test rows + mutation test | RED | L-overrides |
| R3 | Override edges project to occurrences at code 7, Oracle confidence, owner-relative spans | override edge silently dropped, or fabricated as MethodCall | decode the fragment; assert occurrence kind byte == 7 with local base ordinal and foreign c-universe target when the base lives in an include; assert absence when authority had no override rows | fragment decode test | RED | L-overrides |
| R4 | `ReferenceKind::Overrides` is additive: every old fragment decodes identically; duplicate codes rejected | vocabulary bump that breaks old bytes | run `compiler-ir` + `ir-vocabulary` + `compiler-driver` gates; bytes of a pre-change fragment fixture decode unchanged; ir-vocabulary duplicate-code mutation test still fails | gate outputs + golden decode | RED | L-overrides |
| R5 | Extension section schema 2 carries the clang owner cell; schema-1 sections still validate | silent row-width change (old fragments unopenable) | dual-schema decode test: hand-preserved schema-1 section (golden bytes) validates after the change; a schema-2 fragment round-trips owner ordinals | compiler-ir tests (escalated card) | RED | L-render-ir |
| R6 | Records/namespaces/enums carry member structure into the Ir | flat item list; struct renders without body | build_ir from a C fixture yields members on the record item; `render` emits the struct body | compiler-ir render golden + driver-level decode test | RED | L-render-ir / L-render-driver |
| R7 | Golden struct rendering is byte-stable and C-honest | rendering invents Rust syntax or drops C qualifiers | golden render tests: `struct Node { const struct Node *next; }` renders exactly; pointer/const/array/fn-ptr spellings render exactly; golden files committed | golden test files | RED | L-render-ir |
| R8 | PURL is a closed typed grammar, rejecting foreign ecosystems and malformed forms | stringly package identity | table test: valid generic form accepted with exact fields; `pkg:npm/...`, missing version, bad encoding rejected with exact variants | clang crate unit tests | RED | L-purl |
| R9 | Compile-argument binding comes only from the located database/build system; sysroot travels verbatim | guessed `-I`/`-std`/`--sysroot` | database entry with `-std=c11 -I inc --sysroot S` yields exactly those args to libclang (asserted through a live parse that only resolves a header under `inc/`); absent build system → typed terminal, no fallback | live integration test | RED | L-buildsys |
| R10 | Whole-TU lifecycle: PURL → closure → per-TU authority → fragments → publish → reopen → index, old fragments still validating | per-file pipeline that invalidates or drops old generations | integration test: publish gen1, add a file, publish gen2, reopen store, validate BOTH fragments decode and both index snapshots seal | top-level integration test | RED | L-pipeline |
| R11 | Real codebases: single-header library (stb) and one small real project produce complete decoded facts | fixture-only test-suite that dies on real code | vendored stb single-header + small real C project; decoded-output analysis asserts named record/function/macro facts, include closure, occurrence resolution; no capacity errors | integration test outputs | RED | L-realworld |
| R12 | Capacity and cancellation remain exact typed errors in lifecycle mode | silent truncation of large TUs; unkillable long runs | oversized TU → exact capacity terminal naming lane/required; cancelled flag set mid-lifecycle stops before next TU boundary with `Cancelled` | integration test | RED | L-pipeline |

## Out of contract (evidenced exclusions)

- Bit-field widths, typedef underlying spellings, default arguments, inline asm bodies: the
  consumed clang_3_6 surface cannot prove them; they stay absent (module header law, already
  reviewed in the baseline lane).
- Other language lanes' test-module repair (rust/go/typescript reacting to python's
  `OccurrenceTarget::Stable`): external concurrent ownership; tracked as an external red, not a
  matrix row.
- ThreadLocal storage remains unreachable through the enabled symbol surface (baseline law,
  unchanged).

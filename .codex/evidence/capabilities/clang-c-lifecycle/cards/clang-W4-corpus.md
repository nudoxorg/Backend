# Card clang-W4-corpus — R11: twenty real online projects, decoded-IR review vs source truth, perf profiles

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `430f9a30c`.
- Owned paths:
  - `.local/corpus/` (checkouts; gitignored — never committed)
  - `compiler/driver/tests/corpus_harness.rs` (NEW harness test, env-gated: runs only when
    `NUDOX_CORPUS_DIR` names the corpus root; when absent it prints one line naming the env and
    returns Ok — evidence generation harness, not a shipping assertion)
  - `.codex/evidence/capabilities/clang-c-lifecycle/corpus.md` (NEW committed table)
  - `compiler/driver/build_drive.rs` / `database.rs` / `lib.rs` ONLY if the harness forces a
    minimal public-surface gap — name the forced change in the report.
- FORBIDDEN: everything else, especially `clang_lifecycle.rs`, `clang_lane.rs`, `render_goldens.rs`
  (a parallel worker owns rendering), `server/**`, `compiler/ir/**`.

## Tools available (verified by Terra on this host)

`clang`/`clang++` (Xcode CLT), `make` 4.4.1, `ninja`, `cmake` at /opt/homebrew/bin,
`meson` at /opt/homebrew/bin, `buck2` at ~/.local/bin/buck2 (2026-09-03). Network available for
shallow clones. Augment PATH deterministically for child processes where needed.

## Harness contract

`corpus_harness.rs` is one test-driven driver: for every project row (a table in the file:
name, URL, ref, expected build system, layout notes, source-truth commands) it

1. ensures a shallow checkout under `$NUDOX_CORPUS_DIR/<name>` (clone if absent; never modify the
   upstream tree — fixture preparation such as `./configure` for zlib or `meson setup` is part of
   the DRIVE and must go through the adapter, not manual file generation);
2. drives it through `discover_and_drive` (expected build system asserted);
3. compiles every command through `compile_build_command` with per-TU wall time recorded
   (std::time) and peak RSS recorded externally per run (`/usr/bin/time -l` around the whole test
   is acceptable: capture the max once and report it);
4. decodes every fragment and records: entity count by kind (record/enum/alias/function),
   type-fact row count, occurrence count, include count, diagnostic count;
5. compares against SOURCE TRUTH computed independently: `grep`-equivalent counts over the
   sources (e.g. `rg -c '^\s*(?:typedef\s+)?struct\s+\w+\s*{'` style counts recorded as commands
   in the table) — the table records BOTH numbers and the delta with a one-line analysis
   (headers included multiple times, macros generating decls, extern blocks are EXPECTED
   differences; record them as analyzed deltas, not failures);
6. writes one Markdown row per project into the committed `corpus.md` — name, build system,
   TU count, fact counts vs truth counts (with delta notes), slowest TU with its time, peak RSS,
   and any lane defect observed (exact decoded-vs-source mismatch).

The committed table is the evidence; the harness makes it reproducible.

## The twenty projects

Well-known: 1 `nothings/stb` (single-header, no build system — assert the honest
`NoBuildSystemDetected` terminal, then drive a caller-authored compdb over its sources as the
"stb single-header slice" row), 2 `sqlite` amalgamation (one giant TU via a caller-authored
compdb naming the amalgamation — this IS sqlite's own build description shape), 3 `redis/redis`
(make), 4 `lua/lua` (make, 5.4 branch), 5 `json-c/json-c` (cmake), 6 `jbeder/yaml-cpp` (cmake,
C++), 7 `antirez/kilo` (NO build file — single file; assert `NoBuildSystemDetected`, then
caller-authored compdb row), 8 `madler/zlib` (configure once in fixture prep as documented
preparation, then make drive), 9 `KhronosGroup/Vulkan-Headers` (cmake, header-only — many TUs?
expect few; record), 10 `facebook/buck2` `examples/with_prelude` (BUCK row: drive through the
buck2 adapter, assert the exact `ToolPresentUndrivable` terminal with evidence, AND separately
record `buck2 build //cpp/hello_world:main` succeeding as build-level evidence in the table).

Niche: 11 `attractivechaos/klib` (single-header C data structures), 12 `mackron/miniaudio`
(single-header, C++-compileable, huge), 13 `vurtun/lib` (single-header suite), 14 `rxi/monocle`?
— substitute `rxi/map.c` or `rxi/vec.c` (single-header), 15 `jnz/q3vm` (cmake), 16
`tylov/STC` (smart C containers; non-standard layout: the "stc" header dir + misc), 17
`CamDavidsonPilon/...` no — `esneider/c11q`? use `jd4no/...` — pick `juan-memory/...`: instead
use `zeux/pugixml` (meson AND cmake both present — assert cmake marker priority), 18
`DaveGamble/cJSON` (cmake, small well-known), 19 `orlp/edlib`? use `nanomsg/nng` (cmake+make,
real), 20 `ThrowTheSwitch/Unity` (cmake, C test framework, non-standard src layout).

Substitutions are allowed when a repo is gone or unbuildable: replace with an equivalent
niche project (single-header or make-based), record the substitution and reason in the table.
The twenty rows must include: stb, sqlite amalgamation, kilo, >=1 BUCK-based, >=3 non-standard
layouts, >=5 niche single-header projects.

## Edge-case feedback loop (mandate: fixes via cards)

Every defect the corpus exposes (wrong spans, missing facts vs source, capacity faults, parse
terminal mismatches) must be recorded in `corpus.md` with: repo, file:line-ish location, decoded
vs expected, and the smallest reproduction. Do NOT fix lane defects yourself — STOP when the
first lane defect blocks further corpus progress, report it, and continue with the rows that do
not need it. Terra owns defect adjudication and repair cards.

## Constraints

No new dependency, no unsafe, no network in the harness itself (clones happen through
`std::process::Command` git or are pre-cloned by you into `.local/corpus/` before the harness
run — prefer pre-cloning and keeping the harness offline). `#![forbid(unsafe_code)]` + deny set.
Deterministic output ordering.

## Evidence

1. `NUDOX_CORPUS_DIR=$PWD/.local/corpus CARGO_TARGET_DIR=$PWD/.local/target cargo test -p
   compiler-driver --offline --test corpus_harness -- --nocapture` → all rows processed.
2. `corpus.md` committed with the full twenty-row table and defect list.
3. `cargo fmt --check` on owned files. One commit:
   `test(clang): codify the twenty-project corpus review with decoded facts and perf profiles`.
   Report: commit sha, the table's summary lines, every lane defect found (this is the most
   important part of your report), smallest remaining red.

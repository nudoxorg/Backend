# Card clang-L11-corpus — close R11/R14: twenty real projects end-to-end repo→TUs→authority→publish→reopen→index→gen-2 with decoded-IR review and honest perf

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `codex/fidelity-clang` AFTER the L8 replay-heal and L10 drive commits land
  (Terra points you at the exact sha; the corpus journeys need chained reopen and the redis/
  meson/buck drive fixes).
- Owned paths: `compiler/driver/tests/corpus_harness.rs`,
  `.codex/evidence/capabilities/clang-c-lifecycle/corpus.md`.
  You MAY write scratch/driver files under each checkout's `.nudox-corpus-scratch/` directory and
  under `/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode` — never into upstream tracked trees.
- FORBIDDEN: all product code (if a row exposes a genuine lane defect, record it as a named
  defect with its smallest reproduction — the harness never fixes lanes).
- Corpus root (read-only consumption of the checkouts):
  `/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus` — run with
  `NUDOX_CORPUS_DIR` pointed there. All 20 checkouts exist. Tools on this machine:
  make, cmake (/opt/homebrew/bin), meson (/opt/homebrew/bin), ninja (/etc/profiles/per-user/mileswirht/bin),
  buck2 (/Users/mileswirht/.local/bin/buck2). The test process PATH may lack these — reuse
  `compiler_driver`'s child PATH augmentation behavior via the product adapters (they augment
  internally); for the buck2 build-evidence probe keep the absolute binary path.

## Law under test (proof-matrix R11 + R14)

Every project lowers end-to-end: repo → (drive or authored compdb) → TUs → per-TU libclang
authority → fragments → publish gen-1 → shutdown+reopen → open+validate → index seal → gen-2
(changed content) → publish → reopen → BOTH generations' fragments validate. Decoded IR is
reviewed against source truth; per-TU wall time and honest peak RSS are recorded. Every edge case
found becomes a named defect or a committed harness fix — never a silent skip.

## Public terminal

### T1 — end-to-end per row

Replace `run_row`'s compile-only loop with the full lifecycle (reuse the shapes proven in
`build_drive.rs`'s `make_drive_compiles_and_publishes_two_generations` and `clang_lifecycle.rs`):
1. discovery exactly as today (drive or authored compdb);
2. compile EVERY translation unit through `compile_build_command`;
3. publish all fragments as gen-1 on a fresh per-row journal (`PublicationLimits::new` as the
   existing journeys do; size scratches from the fragment count);
4. `journal.shutdown()` + `DurablePublisher::reopen`; `open_published` validates every gen-1
   fragment; build+seal the gen-1 compilation index;
5. gen-2 with CHANGED CONTENT: author a scratch driver TU per row (all rows, uniformly:
   `.nudox-corpus-scratch/<name>-driver.c` including the project's primary public header with the
   project's include path — for rows that already have driver TUs reuse theirs); gen-1 publishes
   driver-v1, gen-2 REPLACES its bytes (changed declarations) and republishes the driven set plus
   driver-v2; assert the driver fragment bytes differ between generations and the unchanged
   project fragments are byte-identical;
6. reopen once more, validate gen-2's fragments, build+seal the gen-2 index, and open gen-1's
   driver fragment from `ImmutableArtifactStore` to prove old bytes still decode.
A row that fails any step is a DEFECT ROW in corpus.md with its exact terminal — do not weaken
the journey to make a row pass. Buck2's row is the typed-terminal exception proven by L10
(`ToolPresentUndrivable` + real `buck2 build` evidence) — keep that row honest, not "0 TUs ok".

### T2 — decoded-IR review vs source truth (deep, asserted)

For every row:
- count entities/occurrences/type facts by DECODING each fragment (`FragmentView`), as today;
- replace the `windows(9) == b"#include "` text scan with the fragment's DECODED include atoms
  (find the include/extension accessor on `FragmentView` — clang include facts ride the
  extension section; if the fragment genuinely cannot express a decoded include count, report it
  as a defect and fall back to asserting the lane's `ClangFacts` include count is unreachable —
  never keep a source-text scan in a decoded-review table);
- count decoded diagnostics from the fragment (today the harness hardcodes `diagnostics=0` —
  delete that lie; assert the real decoded count, and if it is nonzero for a row, that row's
  diagnostics column shows the number);
- assert named spot entities per project, derived from the analyzed MAIN files (whole-TU law:
  the lane's fact model covers each analyzed TU's main file; headers ride the include-closure
  and driver-TU rows — record this law in corpus.md): sqlite3.c → `sqlite3_open` +
  `sqlite3_exec` exist as decoded function entities; kilo.c → `main` and `editorOpen`;
  map.c → `map_new` + `map_get`; stb/tests/stb.c → decode and name its own defined functions;
  yaml-cpp → name the C++ entities its driven .cpp files define (pick two, verify present);
  json-c → `json_object_get`; lua → one core function the driven TUs define (e.g. `lua_pushstring`
  from lapi.c — verify against the actual file list before asserting); zlib → `deflate` + `inflate` (from deflate.c /
  inflate.c); pugixml → `pugi::xml_document`-defined methods in pugixml.cpp (pick one);
  cJSON → `cJSON_Parse` + `cJSON_Delete`; nng → nng.c-defined symbol (pick one); Unity →
  `UnityBegin`/`UnityEnd`; q3vm → `VM_Create`-class (pick from vm.c); miniaudio/vurtun/STC/klib →
  the driver TU's `nudox_driver` plus the entity kinds the header's declarations produce in the
  driver TU's facts. Decode entity NAMES via the fragment's atom/name access (the clang_lane
  tests show the decoding pattern). Spot entities are ASSERTED, not printed.
- lower-bound law per row: decoded record/function counts for the analyzed main files must be
  nonzero for projects whose main files define records/functions (all rows except the buck
  terminal row) — zero is a defect row, never an accepted line.

### T3 — honest perf

- per-TU wall time (keep, in µs) + per-row total and TU count;
- peak RSS: ONE external `/usr/bin/time -l` around the complete run, recorded ONCE in
  corpus.md's preparation section with the exact command — DELETE the fabricated per-row
  `1,062,912,000 bytes` literal (it is a stale copy of one old measurement);
- the regenerated corpus.md table keeps columns: project | build system | TUs | decoded facts |
  type facts | slowest TU | row wall time | source truth / delta — with `source truth` keeping
  the documented line-count truth function and the delta explanation naming the lane's
  main-file fact law.

### T4 — harness honesty fixes

- `sources()` silently skips unreadable directories (`let Ok(entries) = ... else { continue }`);
  make it error the row instead.
- `truth()`'s unused `marker` if/else arms produce the same string — collapse to the one truth
  function and document its definition (it is a line-count approximation; the ASSERTED review
  lives in T2).
- The buck2 build-evidence probe hardcodes `/Users/mileswirht/.local/bin/buck2` and the
  `//cpp/hello_world:main` target: keep the absolute binary (recorded environment adaptation)
  but derive nothing else dishonestly — assert `ToolPresentUndrivable` evidence nonempty and the
  build note from the real probe.

## Constraints

- Test/evidence-only card. No product edits. Deny set stays. No new dependency.
- The corpus run is opt-in (`NUDOX_CORPUS_DIR`); without it the harness stays a no-op so normal
  gates stay fast.
- `cargo fmt` on the owned file. One commit (after the run):
  `test(clang): run the twenty-project corpus end-to-end with chained generations and decoded-IR review`.

## Evidence

1. `NUDOX_CORPUS_DIR=/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus CARGO_TARGET_DIR=$PWD/.local/target cargo test -p compiler-driver --offline --test corpus_harness -- --nocapture` — all 20 rows end-to-end green (buck row = typed terminal + build evidence), run twice; keep the raw table output.
2. Without `NUDOX_CORPUS_DIR`: `cargo test -p compiler-driver --offline --test corpus_harness` green no-op.
3. Report: commit sha, the regenerated per-row table, defects found (exact terminals), spot-entity assertion list, per-row wall times + the measured peak RSS with its exact command, smallest remaining red.

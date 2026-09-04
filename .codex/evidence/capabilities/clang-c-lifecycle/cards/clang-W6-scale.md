# Card clang-W6-scale — raise the clang authority lanes to trunk-class geometry

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `954213c91`.
- Owned paths: `compiler/languages/clang/{facts.rs, scratch.rs, input.rs, collect.rs,
  tests/boundary.rs, tests/live_authority.rs}`, `compiler/driver/lower/clang.rs`,
  `compiler/driver/database.rs` (the borrowed argument array only),
  `compiler/driver/tests/clang_lane.rs` (only capacity assertions that change because of this).
- FORBIDDEN: `build_drive*`, `corpus_harness.rs`, `clang_lifecycle.rs`, `render_goldens.rs`,
  goldens, `compiler/ir/**`, `server/**`.

## Finding (Terra-adjudicated from the corpus run)

The shared emission lane was raised to 1024 facts (trunk c8a24c743) with scaled lanes boxed, but
the clang authority's own fact lanes kept their original bounds — declarations cap at 128 and
references at 512 (`Corpus`: stb's tests/stb.c fails at `129 > 128`; json-c/yaml-cpp/zlib/pugixml/
cJSON/Unity fail at `513 > 512`; redis's single command needs `157 > 64` argument cells). The
pipeline can never express a TU the emission lane could handle. The lane law stays: a capacity
boundary is an EXACT typed terminal naming the lane and required count, never truncation — the
fix raises the bound, it does not add fallbacks.

## Public terminal

1. ClangFacts lane bounds align with the emission lane's 1024-class geometry (declarations 1024
   matching `MAX_EMISSION_FACTS`; references 1024 matching `MAX_EMISSION_OCCURRENCES`; every
   other ClangFacts lane gets an explicit documented bound in facts.rs with a one-line
   justification referencing the trunk lane it feeds — do not invent magic numbers). State each
   lane's retained byte bound in facts.rs as a `const` with a `size_of`-check assertion where
   meaningful.
2. All scaled arrays follow trunk's boxing discipline; heap-build any array whose element is not
   const-promotable (`vec![elem; n].into_boxed_slice()` — the 4 MiB stack-temporary SIGILL class).
   State the retained/live byte accounting for one filled lane in the module header.
3. `MAX_DATABASE_ARGUMENTS` 64 → 256; the borrowed array in `compile_database_translation_unit`
   stays stack-safe at the new bound (verify: it is `[&CStr; N]`, pointer-sized cells — fine;
   assert the size in a test).
4. Capacity terminals stay exact: a generated TU with 1025 declarations fails with the exact
   terminal naming the lane and required 1025 — update the existing capacity tests to the new
   bounds and keep the buffer-tail law.
5. All lane gates stay green: clang_lane, clang_lifecycle, build_drive, corpus_harness
   (with `NUDOX_CORPUS_DIR` set — stb's tests/stb.c and the 513-reference rows must now pass
   capacity, though other defects may remain; record which rows still fail and why).

## Constraints

No new dependency, no unsafe beyond the reviewed ffi surface, no silent fallbacks, deny sets
stay. `size_of::<ClangFacts>()` must not regress for the SMALL-TU path: the scratch lanes are
caller-provided; construction of the scratch for an empty TU must not grow (state the before/after
scratch byte cost in the module header or report).

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lane --test clang_lifecycle --test
   build_drive` all green, twice.
2. The corpus harness re-run: `NUDOX_CORPUS_DIR=$PWD/.local/corpus cargo test -p compiler-driver
   --offline --test corpus_harness -- --nocapture` — stb, json-c, yaml-cpp, zlib, pugixml, cJSON,
   Unity, redis rows now PASS capacity (other failures are other workers' surface; record them).
3. One commit: `feat(clang): raise the authority lanes to trunk geometry with exact bounds`.
   Report: commit sha, the new lane table (lane | bound | bytes), scratch cost before/after,
   command tails, smallest remaining red.

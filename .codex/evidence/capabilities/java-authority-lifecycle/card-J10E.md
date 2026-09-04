# Card J10-E — corpus completion: whole-artifact lang3, per-row publication,
# deep IR-vs-image review, rendering check

registered role: nudox_luna_implementer
baseline: commit a6b1e8110 (branch codex/fidelity-java) — create your own detached
worktree from it; do not write anywhere else.
expected config: max effort; house style (`deliver-reviewed-rust-slice`) applies.

## Context (Terra-scouted 2026-09-03)

`compiler/driver/tests/java_corpus.rs` + `java_corpus/mod.rs` freeze a
twenty-row Maven corpus (well-known + niche, multi-module Maven/Gradle
projects included). Terra closed the lane defects that blocked real whole-
artifact lowering: package-info rows (J10-A), pooled list width (J10-C), and
— landing in parallel with this card, do not touch production code — the
documentation lane (J10-D). Terra re-measured the whole commons-lang3 3.14.0
sources jar (246 `.java` entries): all but 2 files already lower at your
baseline; the remaining 2 are the doc-lane bound owned by J10-D. Your corpus
leg must therefore expect zero typed failures on all 246 entries ONCE J10-D
integrates — pin the law, not the current count.

This card turns the frozen corpus from "selected entries" into the brief's
end-to-end law: every artifact lowers fully, publishes, reopens, indexes, and
chains a second generation; the reopened IR is deeply cross-checked against
the javac authority image (source truth); profiles are recorded.

## Owned paths (no overlapping writer; nothing else may change)

- `compiler/driver/tests/java_corpus.rs` and `compiler/driver/tests/java_corpus/mod.rs`
- `.codex/evidence/capabilities/java-authority-lifecycle/corpus-table.md`
  (new evidence doc referenced by mod.rs's header comment but missing; the
  frozen rows must never be substituted — document purl, frozen entries,
  dependency coordinates, scout verdict, and measured profile per row)

Forbidden: every production file (`compiler/`, `server/`, `heart/`,
`interface/`); `java_lifecycle.rs` (its T1–T3 stay untouched); render
goldens in `java_render.rs`.

## Law 1 — commons-lang3 FULL leg (P28, new)

Row 0 (`maven:org.apache.commons:commons-lang3@3.14.0`) must lower the WHOLE
artifact: fetch the sources jar from the canonical Central host, extract
every `*.java` entry (no frozen-entry subset for this row), image each file
against the binary jar + JDK, `Stage::LowerIr` compile each file through the
public `compile` seam, validate every fragment. Zero typed lowering failures
on all entries (246 at scout time — assert "every entry lowered", not a
hard-coded count). Package-info entries must assert their package row
(`EntityKind::Module` entity carrying the qualified package name); type-
bearing entries assert the existing qualified-name entity law. Per entry,
record a profile line
`CORPUS|maven:...commons-lang3@3.14.0|<path>|<image bytes>|<fragment bytes>|<millis>`.
The five previously-frozen entries keep their existing laws.

## Law 2 — every row publishes, reopens, indexes, and chains a generation
(P29, new)

Generalize the existing commons-csv publication leg to EVERY corpus row:
publish all of the row's compiled fragments, `shutdown`, `reopen`,
`open_published`, validate every reopened fragment, build + seal + plan +
encode the index (assert the row's primary qualified name appears in the
built rows), then chain a generation-2 leg: append a method to the row's
LAST frozen entry's source, recompile, `publish_compiled` with
`PublishControl::Continue`, assert the generation advanced, reopen and assert
the added entity exists, assert the FIRST generation's fragments still reopen
with identical SHA-256 digests through the immutable stores. Scratch buffers
must size dynamically (no frozen entry-count constants like
`PUBLICATION_ENTRY_COUNT = 2`).

## Law 3 — deep decoded-IR review against javac truth (P30, new)

For EVERY row and EVERY frozen entry: after reopening (or immediately after
compiling for the lang3-full leg), decode the fragment and cross-check it
against its authority image — the javac image is the source-truth oracle:
1. every image type/module declaration yields exactly one admitted entity
   with equal qualified name and entity kind (Package rows → Module-kind
   entities with the qualified package name);
2. the fragment's occurrence count equals the image reference count, and
   every occurrence's span lies within the bound source's byte length;
3. the Java extension payload decodes, and per declared executable the
   throws/annotations planes are present when the image carries any, with
   counts equal to the image's extension entries for that declaration;
4. rendered display (`compiler_ir` render path) of the primary type entity's
   signature contains the entity's simple name.
Any mismatch fails the journey naming purl, path, and the exact
image-vs-fragment divergence. Rows whose entries are annotation-type
declarations keep their typed occurrence-absence law.

## Law 4 — profiles (P31, new)

Keep and extend the per-row `CORPUS|...` profile lines: rows now print
entries lowered, total image bytes, total fragment bytes, wall millis, entity
count, occurrence count. Print one final `CORPUS|TOTAL|<millis>|<entries>`
line. No budgets are frozen yet beyond the existing 16 MiB image / 4 MiB
fragment caps; the recorded numbers become the baseline for future
regression judgment.

## Environment

- `CARGO_TARGET_DIR=<your-worktree>/target-we` (dedicated; never share).
- `NUDOX_JDK=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/jdk/jdk-21.0.12.1+1/Contents/Home`
  (live javac; network is available; fetch only from the canonical Central
  host `https://repo.maven.apache.org/maven2/`).

## Exact commands (all must be green before commit)

1. `cargo test -p compiler-driver --test java_corpus -- --test-threads=1 --nocapture`
   (full journey, single-threaded; expect several minutes — record the raw
   profile lines in the return)
2. `cargo test -p compiler-driver --test java_lifecycle -- --test-threads=1`
3. `cargo check -p compiler-driver --tests`
4. `cargo fmt -p compiler-driver && git diff --check`

Known pre-existing reds outside your custody (do not fix): the shared
`lower` suite's
`lower::tests::bounded_fact_and_child_lanes_reject_overflow_and_admit_the_exact_bound`,
6 stale `lower::rust` cfg(test) expectations, 4 stale oxc direct-frontend
tests in `native_compile`.

## Budgets and house laws

- Test-code delta <= 900 LOC across the two test files; corpus-table.md is
  documentation. Honest variance reporting.
- The corpus table stays frozen: you may not swap rows/entries/deps. If a
  row proves unfetchable at run time, do NOT substitute it — stop and report
  the exact typed failure.
- No ignored tests, no skipping stages, no best-effort catches: every stage
  failure fails the journey with purl + path + exact typed cause.
- Test plumbing must not restate the wire grammar; reuse the support module.

## Checkpoint and return

Commit once, coherent, on a branch `j10e-corpus` in your worktree. Return
exactly: commit hash + branch; files changed with net LOC; per-law outputs
(test names + pass lines + the raw CORPUS profile lines); the corpus-table.md
row verdicts; smallest remaining red or "none"; any deviation with one-line
justification.

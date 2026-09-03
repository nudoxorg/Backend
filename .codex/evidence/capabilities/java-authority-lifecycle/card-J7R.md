# Card J7R — real-jar deflate + unnamed-package lowering repair (P14, P17-unblock)

## Registered role

`nudox_luna_implementer`. One frozen repair card with two independent rows. Do
not widen paths or change any other behavior.

## Baseline and workspace

- Worktree: `/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/java-lane`.
  FIRST command: `git -C <worktree> checkout --detach <BASELINE>` where BASELINE
  is named in your task prompt (the card text may be newer than the baseline;
  the baseline pins all production content). Verify `git rev-parse HEAD` equals
  BASELINE; if checkout fails or differs, STOP and report. Never stage anything
  under `.codex/` or foreign quarantine files.

## Evidence (Terra-diagnosed 2026-09-03; do not re-derive, but DO reproduce)

Row A — jar deflate: the REAL Maven Central artifact
`org.apache.commons:commons-lang3:3.14.0` sources jar fails
`Jar::parse`+`entries`+`data` with
`JarError::Deflate("deflate failed at offset 680846: deflate decompression
error")`. Root cause: `compiler/languages/java/jar.rs` line ~327 uses
`Decompress::new(true)` (zlib wrapper mode). ZIP deflate streams are RAW
deflate; the correct constructor is `Decompress::new(false)`. The existing
tests never exercised deflate at all, so the mutant survived. Terra verified
with an independent decoder: the first deflated entry
(`META-INF/MANIFEST.MF`, 251 deflated entries in the artifact) decompresses
with raw wbits (-15) and FAILS with zlib wbits (+15).

Row B — unnamed package: `compiler/driver/lower/java.rs` `push_root` pushes
every Module/Package declaration unconditionally. A source file with NO
`package` statement makes javac report a Package element whose qualified name
is the EMPTY string; `FactSet::push` rejects `EmptyName`; `push_fact` erases
the cause into `NoSupportedDeclaration`, surfacing as the misleading
"unsupported LowerIr declaration recipe". Terra reproduced with a real javac
image of two default-package sources (14 declarations, binding digest
matching, zero facts admitted). Language law: annotations and javadoc cannot
be declared for the unnamed package (package-info.java requires a named
package), so an unnamed Package row carries no retainable facts.

## Owned paths (exclusive write custody)

1. `compiler/languages/java/jar.rs` (Row A fix)
2. `compiler/languages/java/tests/jar.rs` (Row A tests)
3. `compiler/driver/lower/java.rs` (Row B fix)
4. `compiler/driver/lower/java.rs` test module (Row B tests; same file)

Nothing else.

## Row A — required behavior and falsifiers

- `JarEntry::data` decodes method-8 entries with RAW deflate
  (`Decompress::new(false)`), writing exactly `unpacked` bytes into `output`.
- New tests in `tests/jar.rs` (reuse the existing local zip-writer fixture
  style; if the file has no zip writer, add a minimal one producing correct
  LFH/CDH/EOCD with CRC-32):
  1. roundtrip: one stored + one RAW-deflated entry (flate2
     `Compress::new(Compression::default(), false)`, then `finish`/`FlushCompress::Finish`
     into a Vec — verify total_out == compressed length) parse, name-filter,
     and decode to the original bytes;
  2. hostile: a zlib-WRAPPED stream (flate2 `Compress::new(.., true)`) placed
     in a method-8 entry must produce a typed `JarError::Deflate`, not a
     panic, not silent garbage;
  3. truncated deflate stream (cut the compressed bytes in half) → typed
     `JarError::Deflate` or a size/validation error naming the entry offset —
     exact variant asserted.
- Falsifier killed: the real commons-lang3 sources jar decodes entry data
  end-to-end. Terra will re-run the corpus probe after acceptance; your gate
  is the unit trio above plus the existing jar tests.

## Row B — required behavior and falsifiers

- In `collect` pass one, a `DeclarationKind::Package` (and only Package;
  Module keeps existing behavior) whose `declared.name.bytes` is empty is
  skipped with ordinal `0`, exactly like member kinds, with a comment stating
  the language law: the unnamed package cannot carry package-info annotations
  or documentation, so the row carries no retainable facts. Named packages
  push exactly as today.
- New tests in the java.rs test module (Rust-constructed Fixture images, in
  the established style):
  1. a v2 image whose declarations are `Package(name="")`, `Class(name="C")`
     admits exactly the class facts (class entity present, no entity named
     "") — kills the mutant that reverts the guard (without it the compile
     fails with the erased NoSupportedDeclaration);
  2. a named `Package(name="demo")` with one annotation still commits the
     package fact with that annotation atom (guards the J3F gap).
- Falsifier killed: `cargo test -p compiler-driver --lib lower::java` green
  including both new tests; and the T1 lifecycle shape (default-package
  sources → compile succeeds) — Terra verifies the lifecycle end-to-end
  separately; your gate is the lib tests.

## Semantic bounds

- No error-erasure additions. Do NOT touch `push_fact` (shared lane, Sol
  escalation handled separately). Do not change `JarError` variants.
- No new dependencies; no production API changes; comments state the language
  law, not the implementation history.
- Row A ≤ 30 changed production LOC; Row B ≤ 25 changed production LOC; tests
  ≤ 220 LOC total.

## Exact gates (all green at your checkpoint)

1. `cargo test -p compiler-languages-java` (jar tests live here)
2. `cargo test -p compiler-driver --lib lower::java`
3. `cargo test -p compiler-driver --test java_image` and `--test java_render`
4. `cargo check -p compiler-driver -p compiler-languages-java --tests`
5. `cargo fmt --check` on your owned files only (rustfmt --check <files>)
6. `cargo test -p compiler-languages-java --test javac_authority` (live, NUDOX_JDK
   set in your task prompt)

## Stop decisions

- If raw-deflate decoding of the synthetic roundtrip still fails after the
  one-flag fix, STOP and report the exact flate2 error — do not add fallbacks,
  retries, or alternative decoders.
- If a named-package behavior differs from the J3F law, STOP and report.
- `git rev-parse HEAD` ≠ BASELINE → STOP.

## Commit and return

One commit on top of BASELINE, message:
`fix(java): decode raw zip deflate and skip the unnamed package row`.
Stage only your four owned paths. Return: commit hash; one line per gate; LOC
delta per file; smallest remaining red; any stop decision hit.

Plan closure: next manager decision is Terra reproduction plus the J9
lifecycle resume with a raised test budget.

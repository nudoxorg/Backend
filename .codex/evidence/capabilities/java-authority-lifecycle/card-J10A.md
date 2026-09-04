# Card J10-A — doclet package-info declarations

registered role: nudox_luna_implementer
baseline: commit ea0950773 (branch codex/fidelity-java) — create your own detached
worktree from it; do not write anywhere else.
expected config: max effort; house style (`deliver-reviewed-rust-slice`) applies.

## Baseline facts (Terra-scouted, 2026-09-03)

`compiler/languages/java/doclet/AuthorityImage.java` `collect()` scans each
compilation unit with a `TreePathScanner` that only overrides `visitClass`.
A `package-info.java` unit (named package, often with package annotations and
package Javadoc) produces ZERO declaration rows, because `emitPackage` only
fires as a side effect of `emitType` via `elements.getPackageOf(type)`. The
shared compile seam then rejects the source with
`LoweringUnsupported::NoSupportedDeclaration` (`facts.len() == 0`).

Empirical proof at the baseline: the image of
`org/apache/commons/lang3/tuple/package-info.java` (single source, binary jar
on classpath, Temurin 21.0.12.1) contains zero declarations and zero
references; 18 of 27 whole-artifact commons-lang3 3.14.0 lowering failures are
exactly this class.

The Rust side already lowers Package rows: `compiler/driver/lower/java.rs`
pass one maps `DeclarationKind::Package` with a non-empty name through
`push_root` (entity kind `Module`), carrying annotation extension inputs and
documentation. No Rust-side change is expected.

## Public terminal

A compilation unit that declares a named package and no type — and generally
every unit's package declaration — emits its declaration row (image kind 2,
"Package") exactly once per package, carrying:
- the qualified package name,
- package-level annotations (the `declaration(...)` helper already interns
  `element.getAnnotationMirrors()` for any `Element` — `PackageElement`
  included),
- the package Javadoc, with the real `TreePath` so the documentation of a
  package-info unit is retained.

Anonymous/unnamed packages are skipped in every emission path (mirror
`emitModule`'s unnamed check). Emission stays deduplicated across units and
across the `emitType -> emitPackage` side effect; `visitPackage` fires before
class visits (source order), so the package row precedes its type rows and the
`documentation(pkg, path)` variant wins the dedup race.

## Owned paths (no overlapping writer; nothing else may change)

- `compiler/languages/java/doclet/AuthorityImage.java`
- `compiler/languages/java/tests/javac_authority.rs` (new live tests)
- `compiler/languages/java/tests/` new fixture files if needed
- `compiler/driver/tests/java_lifecycle.rs` ONLY if a pinned expectation
  breaks because default-package images lose their empty-named package row
  (see below); keep any such edit to the exact re-pin with a why-comment.

Forbidden: `CompilerExtractor.java` argument grammar; image binary layout,
versions, or section count; every Rust file; other doclet behaviors.

## Design boundary (recorded, do not silently deviate)

Skipping unnamed packages at the doclet removes the empty-named kind-2 row
that default-package class sources emit today; the Rust `DeclarationKind::Package => 0`
arm (J7R row B law) then simply never fires for doclet images while staying
authoritative for hand-built fixture images. Both paths keep compiling.

## Proof matrix rows bound to this card

- P20 (new, Terra will record): "A named-package package-info source lowers
  end-to-end through the public compile seam" — weakened implementation:
  zero-declaration package images (baseline). Falsifier: live javac image of
  an annotated package-info fixture decodes >=1 declaration row of kind
  Package with the qualified name and the annotation atom; the same source
  compiles through `compile(...)` with `SemanticAuthorityInput::Java` and
  `FragmentView::validate` succeeds with the package entity present.
- P21 (new): "The whole commons-lang3 package-info set (18 files) images and
  lowers with zero typed failures" — falsifier: per-file image + compile of
  every `**/package-info.java` entry in the 3.14.0 sources jar (fetched live
  from the canonical Central host `https://repo.maven.apache.org/maven2/`),
  binary jar `commons-lang3-3.14.0.jar` on the classpath; every file must
  produce a validated fragment. This is a live, JDK-gated test in
  `javac_authority.rs` (or a sibling `#[ignore]`-free test in the same
  target); it replaces the scratch probe Terra used to find the defect.

## Environment

- `NUDOX_JDK=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/jdk/jdk-21.0.12.1+1/Contents/Home`
  (Temurin 21.0.12.1; `javac -version` must print 21.0.12.1).
- `CARGO_TARGET_DIR=<your-worktree>/target-wa` (dedicated; never share).
- Network is available; fetch jars only from the canonical Central host.

## Exact commands (all must be green before commit)

1. `cargo test -p compiler-languages-java` (live javac suite incl. your new tests)
2. `cargo test -p compiler-driver --test java_lifecycle -- --test-threads=1`
3. `cargo test -p compiler-driver --test java_image --test java_render`
4. `cargo fmt -p compiler-languages-java && git diff --check`

## Budgets and house laws

- Production delta <= 80 Java LOC; test delta <= 220 LOC. Honest variance in
  the report if exceeded; never pad.
- No `unwrap`/`expect`/panics in test helpers; typed errors with exact causes.
- No ignored tests, no silent fallbacks, no dedup-key changes beyond the
  unnamed-package skip.
- Java doclet style: tabs are the existing file style; keep it.

## Checkpoint and return

Commit once, coherent, on a branch `j10a-package-info` in your worktree.
Return exactly:
- commit hash + branch,
- files changed with net LOC,
- the P20/P21 falsifier outputs (test names + pass lines),
- smallest remaining red (or "none"),
- any deviation from this card with one-line justification.

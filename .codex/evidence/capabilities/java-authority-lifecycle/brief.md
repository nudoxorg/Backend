# Capability brief — java-authority-lifecycle

## Public terminal

End-to-end Java capability on the four-boundary workspace:

1. Every javac authority-image type plane lowers through the shared canonical fact
   lane with full structural fidelity: compound/wildcard/array/generic rows via
   anonymous type rows; throws, annotations, overloads, and record components fed
   into the `JavaFacts` extension plane; UTF-16→byte occurrence spans with an
   honest, fast provenance basis.
2. Struct rendering of Java items through `compiler/ir/render.rs` proven by golden
   tests.
3. Full lifecycle from PURL: `maven:group:artifact@ver` → artifact locate (local
   repository) / fetch (central) → sources extraction → javac doclet authority
   image production → classpath assembly → `compile` → fragments →
   `publish_compiled` → `open_published` → index build. Previously published
   fragments keep validating.

## Baseline

- Commit `40608fc1c95dd8d41fd1a78a51d7ed92307df60a`, tree
  `748ff3104076c9e4171ebdc865c5d8bf0cf2b969` (java lane clean at freeze).
- Per-file baseline (LOC, sha256-16): lower/java.rs 2404 d212d3329566841a;
  languages/java/image.rs 1388 dc6ab35958201a2d; bound.rs 153 3704aac43795768a;
  lib.rs 16 c186970cb3af27e6; Cargo.toml 17 ac90d4c3f2a8fe75; tests/image.rs 306
  b637cd374e5b5c9e; tests/javac_authority.rs 317 7619bef33f729c17;
  tests/occurrence_authority.rs 76 f43d9b523b5bf202; driver/tests/java_image.rs
  163 ecc17e0c9329a899; doclet/AuthorityImage.java 391; doclet/CompilerExtractor.java 120.

## Owned paths (exclusive write custody)

- `compiler/languages/java/**` (image, bound, doclet, new lifecycle modules, crate tests)
- `compiler/driver/lower/java.rs`
- Java-owned integration tests: `compiler/driver/tests/java_image.rs`,
  `compiler/driver/tests/java_render.rs` (new), `compiler/driver/tests/java_lifecycle.rs` (new),
  private fixture support under `compiler/driver/tests/java_support/` (new)
- `.codex/evidence/capabilities/java-authority-lifecycle/**`

## Adjacent surface explicitly NOT owned

- `compiler/ir/**` (escalation only; render.rs fixes go to the Sol chief)
- `compiler/driver/lower.rs` shared lane (FactSet, admit, terminals are frozen; java.rs
  consumes `intern_anonymous_type_row`, `anonymous_type_child`, `attach_extension`,
  `intern_{atom,type,entity}_list` exactly as committed at HEAD)
- other language lanes and their tests; shared publication/server crates.

## Non-negotiable laws

- Backwards compatibility: existing green tests stay green unless the test itself
  asserted a v1-only image detail that v2 replaces (then the assertion migrates with
  the format bump, never the law).
- No source-text or regex recovery of Java facts anywhere; authority flows only from
  the validated javac image (tests/occurrence_authority.rs guards the doclet).
- Modifiers never fold into visibility or kind: image `Modifiers` bits stay honest
  image facts; projection never fabricates visibility.
- Every fold to `Unknown` retains the image spelling and a typed reason; structural
  shapes the lane can carry must not fold.
- All error paths preserve source, operands, and exact typed variant. The
  `terminal()` collapse in java.rs is a known recorded lane criticism (shared driver
  owns terminal arms) — do not widen it silently.
- No new dependency outside the already-approved workspace set
  (`flate2`, `ureq` are workspace dependencies; `zip`/`xml`/`serde` crates are forbidden).
- Every checkpoint keeps `cargo check -p compiler-driver -p compiler-languages-java`,
  `cargo test -p compiler-languages-java`, `cargo test -p compiler-driver --test java_image`,
  and `cargo fmt --check` green on touched files.

## Resource bounds

- Anonymous rows ≤ `MAX_ANONYMOUS_TYPE_ROWS` (256) per fragment; facts ≤ 128;
  projection depth ≤ 64 (existing documented producer budget).
- UTF-16→byte basis: O(source) preparation once per collect, O(log n) or O(1) per
  reference lookup; ASCII sources take an identity fast path. No per-reference
  whole-source rescans.
- No retained allocation in lowering (borrowed image atoms only); lifecycle adapters
  may allocate fetched/extracted bytes with documented bounds (artifact jar ≤ 512 MiB
  guarded, sources entries written streaming per entry).

## Environment constraints (recorded honestly)

- No JVM installed on this host (`/usr/libexec/java_home` fails; NUDOX_JDK unset).
  A pinned Temurin 21 JDK is being provisioned under the manager temp root for gate
  runs; live-javac matrix rows are gated on its availability and stay UNVERIFIED if
  provisioning fails.
- TESTING.md does not exist at the repository root (no such file in any parent); its
  digest is recorded as absent and its intended clauses are bound directly to proof
  matrix rows instead.

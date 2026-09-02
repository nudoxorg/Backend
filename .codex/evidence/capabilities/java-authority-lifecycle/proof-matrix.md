# Proof matrix — java-authority-lifecycle

States: RED | PROVED BY WORKER | REPRODUCED BY TERRA | FALSIFIED | UNVERIFIED.
Only Terra moves a row to REPRODUCED BY TERRA.

| # | law | weakened implementation it kills | falsifier | required evidence | state | owner |
|---|-----|----------------------------------|-----------|-------------------|-------|-------|
| P1 | Compound generic applications lower structurally via anonymous rows (`List<List<String>>`, nested args) | Fold to `Unknown` when any argument is not a pure nominal | Fixture image with nested application; reopened fragment shows Apply row over anonymous child rows, not Unknown | driver java.rs test asserting exact type-lane rows + admission remap | RED | J1 |
| P2 | Arrays over arbitrary components lower via anonymous rows (`String[]` as generic argument, deep arity) | Fold unless component is a nominal | Fixture `List<String[]>` + `int[][]` | same as P1 | RED | J1 |
| P3 | Wildcards with compound bounds lower via anonymous rows (`? extends List<String>`) with variance cell kept | Fold unless bound is a nominal | Fixture wildcard-extends-application | same as P1 | RED | J1 |
| P4 | Intersections/unions with compound members lower member-wise | Fold whole row to Unknown on first non-nominal member | Fixture intersection with array member | same as P1 | RED | J1 |
| P5 | Pure-nominal fast path stays byte-identical (backwards compat) | Rewriter changes existing admitted bytes | All existing java.rs/java_image.rs tests green unchanged | cargo test gates | RED | J1 |
| P6 | Anonymous-row capacity (256) and depth (64) fold to typed faults retaining operands | Silent truncation or panic | Fixture with 257 compound rows → typed `TypeRowCapacity` fold; 65-deep type → Depth | driver test | RED | J1 |
| P7 | Image v2 carries throws/annotations/record-components planes with fixed-width rows + pooled ranges; v1 images still validate | v2 fields read unsafely / v1 rejected | Offline byte-level attacks: truncation at every plane, bad range, checksum mutation (image.rs tests) | languages/java tests/image.rs | RED | J2 |
| P8 | Doclet emits throws (executable + constructor checked exceptions), annotations (declaration + member), record components, in javac order, without lexical fallback | Doclet scans source text or misses planes | Live javac run (JDK-gated): record + throws + annotated fixture image decoded plane-by-plane | languages/java tests/javac_authority.rs (UNVERIFIED until JDK) | RED | J2 |
| P9 | Lowering feeds `JavaFacts.throws` (carrier facts, javac order), `.annotations` (interned atoms), `.record_components` (component field ordinals) attached via `attach_extension` | Planes stay empty lists | Fixture v2 image → reopened extension section shows non-empty lists with exact coordinates | driver java.rs tests | RED | J3 |
| P10 | UTF-16→byte span basis: one shared preparation; ASCII identity fast path; surrogate-pair correctness; typed `Utf16` fault on out-of-range units | Per-reference whole-source rescan; wrong offset after astral chars | Test: source with 😀 before refs; measured single-pass basis; boundary units 0/len/len+1 | driver java.rs tests + perf note | RED | J4 |
| P11 | Golden rendering: Java class/interface/record/method/field render through `compiler/ir/render.rs` (`Ir::signature`, `display_type`, `display_docs`, `embedding_text`) with exact expected strings | Renderer emits wrong/mangled Java signatures unnoticed | Golden render test file with literal expectations | driver/tests/java_render.rs | RED | J5 |
| P12 | `maven:group:artifact@ver` parses into borrowed coordinates with typed rejection retaining input | Stringly split, allocation per parse, or swallowed rejection | purl.rs unit tests: canonical, `pkg:maven/g/a@v` variant, empty group, bad version chars, trailing junk | languages/java tests | RED | J6 |
| P13 | Local-repo locate resolves `{repo}/{g}/{a}/{v}/{a}-{v}.jar` and `-sources.jar`; absent artifacts return typed NotFound with attempted paths | Panic/Option-erasure; wrong layout | repo.rs tests with temp-dir repo layouts (present/absent/sources-missing) | languages/java tests | RED | J6 |
| P14 | Jar reader walks ZIP central directory (stored + deflate), rejects truncated/hostile entries with typed errors, bounded extraction | Inflate-everything or unzip-crate dependency | jar.rs tests: local tmp jar fixtures, truncation at each header, bogus sizes | languages/java tests | RED | J7 |
| P15 | Central fetch retrieves release artifacts over HTTPS with typed failures; base URL injectable for offline tests | Silent retry storms or erased network errors | central.rs tests against local fixtures where possible; live central UNVERIFIED if no network | languages/java tests | RED | J7 |
| P16 | Doclet harness: production API produces a bound `JavaAuthorityImage` for source files + classpath (JDK root param, NUDOX_JDK fallback), reusing compiled doclet classes | Test-only javac plumbing; undeleted scratch | Live javac run on fixture (JDK-gated); scratch dirs removed on success/failure | lifecycle tests (UNVERIFIED until JDK) | RED | J8 |
| P17 | Full lifecycle: PURL → locate/fetch → extract → image → `compile` → `publish_compiled` → `open_published` → index build; real artifacts small + commons-lang3 slice | Composed stages drift from public APIs | java_lifecycle.rs end-to-end (JDK + artifacts); small fixture offline via local-repo | driver/tests/java_lifecycle.rs | RED | J9 |
| P18 | Old fragments keep validating after all changes | Format/regression break | Existing publication + java gates stay green; `open_published` round-trip in lifecycle test | full gates | RED | J9 |
| P19 | Mutual recursion + self-nominals + signatures + Javadoc links + maven foreign keys + modifiers honesty hold across the new projection | Regressions from the compound rewriter | Existing javac_authority + java.rs tests green; new fixture with two mutually recursive types + astral-char docs link | driver tests | RED | J1/J3 |

## TESTING.md binding

No TESTING.md exists at the repository root (verified 2026-09-02; no such file at any
parent). Every clause the capability would bind (unit law naming, boundary cases,
allocation assertions, hostile mutation, restart) is bound directly to rows P1–P19.

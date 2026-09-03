# Capability brief: rust-analyzer-semantic-lane

## Public terminal

The complete Rust semantic journey, backwards-compatible with the existing
fragment wire:

1. `cargo:<name>@<version>` PURL resolution — workspace crate location
   (multi-crate workspace member resolution by name+version) or offline
   registry-cache locate under `CARGO_HOME`; feature-unification controls
   (`all_features`, `no_default_features`, named features) reach the
   rust-analyzer `CargoConfig`.
2. rust-analyzer authority at its fullest: HIR type walking (ADTs → local or
   foreign-qualified nominals; refs, pointers, tuples, slices, arrays,
   fn-ptrs, impl Trait, dyn Trait, Apply, TypeVar; exact widths), recursive
   self-nominals through `Option<Box<T>>`, signatures with self-ownership,
   trait-impl edges, macro facts (including facts written inside macro
   expansions, projected to real-file spans), lifetimes and bound predicates
   (inline and `where`), docs with `[`x`]` intra-doc links, occurrences at
   Oracle confidence.
3. Canonical fragment admission through the shared driver lane
   (anonymous type rows, pooled refs, `EmissionExtension::Rust`), decoded and
   rendered through `compiler/ir` `render.rs` with frozen golden render
   tests.
4. Publish → reopen → index seal through `compiler-publication` and
   `server-index-publish`; previously published fragments keep validating
   under the frozen `FRAGMENT_SCHEMA`.
5. Integration tests on real crates: this workspace itself, a serde slice,
   and a niche registry-cache crate, with decoded-output analysis.

## Non-negotiable laws

- The lane borrows source bytes only: no rust-analyzer-rendered path is
  manufactured into any cell; unprovable positions fold to typed
  `Unknown` reasons, never invented shapes.
- Every emission is a fact rust-analyzer proved; `#[cfg]`-gated-out items
  never become facts.
- Occurrence spans are owner-relative; oracle-resolved targets are `Local`
  or foreign at Oracle confidence; unresolved positions stay Syntactic.
- Strictly backward type-graph references; diagonal self-nominals; leaf-only
  anonymous rows; carrier facts for nested compounds.
- Exact typed rejections at lane bounds (capacity, empty, budget), never
  truncated emissions.
- No new dependency, no network fetch, no unsafe, no SIMD, no `compiler/ir`
  edits (escalation lane) — all remain parent authority.

## Explicit negative space (this capability does not)

- Network registry fetch (new dependency authority — recorded as the
  remaining fork; offline workspace/cache locate satisfies the brief's
  "locate/fetch **or** workspace crate location" disjunction).
- `compiler/ir` wire or render changes; rendering gaps are escalated, not
  patched.
- Other languages' lanes: clang, csharp, go, python, typescript files are
  concurrent owners' territory.

## Baseline and concurrent ownership

Frozen at the working tree on top of commit `815b957793926065cf95e1217338e3bf54c4f333`
(see `index.toml` for per-file LOC + sha256). Concurrent lanes (go, python,
csharp, typescript, clang) share `compiler/driver/lower.rs` and sibling
lower modules; this capability owns exactly:

- `compiler/languages/rust/**`
- `compiler/driver/lower/rust.rs`
- `compiler/driver/tests/rust_semantic_lane.rs`, `rust_hir.rs`,
  `authority_terminal.rs`, and new `rust_*` integration test files
- `.codex/evidence/capabilities/rust-analyzer-semantic-lane/**`

A mechanical cross-lane stabilization (clang lane's uncommitted mid-refactor
breakage: `EmissionExtension::Clang` wrapping, `try_from_cell`, slice-array
literal, edge-borrow collect) was applied by the manager to unblock
`compiler-driver` compilation; it remains uncommitted property of the clang
lane.

## TESTING.md digest

`TESTING.md` does not exist in this four-boundary repository (evidenced
exclusion; verified by file search 2026-09-02). Test-craft clauses are bound
from `.opencode/skills/deliver-reviewed-rust-slice/SKILL.md` directly:
boundary cases (zero, one, limit, limit+1, truncation, hostile mutation,
duplicate/reorder), typed exact diagnostics, allocation assertions, golden
raw artifacts, compile-fail tests for illegal states.

## Applicable clause → matrix row mapping

- Exact typed rejection at bounds → R12
- Boundary cases zero/one/limit → R2, R3, R4, R5, R6
- Hostile mutation / truncation → R10, R1
- Golden evidence → R8
- Real public journey → R9, R5–R7

## Wave-2 addendum (mandate 2026-09-02): full fidelity, rendering, lifecycle, corpus

The chief mandate expands this capability to: (1) fullest HIR fidelity, (2)
rendering perfection through the fixed `compiler/ir` render.rs with golden
tests, (3) full PURL lifecycle publish→reopen→index, (4) a 20-crate real
corpus proof.

Adjudicated facts this wave (Terra, from the live tree):

- The compact fragment already carries the complete recursive type lattice,
  docs lane, occurrences, and Rust extension rows. The deficit is the shared
  driver's `FactSet::build_ir` (compiler/driver/lower.rs): it maps only ≤3
  primitive builtins, hardcodes `Visibility::Unknown`, and drops docs.
  Deepening build_ir is additive driver-lane work (precedent: d0f2a4935
  touched lower.rs for a Rust lane need); it is NOT a compiler/ir change and
  the fragment wire stays untouched.
- Visibility as a *fragment* fact does not exist on the wire (all four
  extension cells are owned). Live-Ir visibility rides the driver-internal
  FactSet; fragment-level visibility remains a parent wire decision and is
  recorded as the remaining fork.
- The frozen render goldens were aspirational: under the fixed render.rs
  grammar they were unachievable (docs.rs `self` elision does not exist in
  the renderer's parameter grammar; `/* visibility unknown */` was rendered
  for every item). Goldens must be re-frozen to exact achievable output.
  The renderer's Rust self grammar (`self: &Self`) is itself a candidate
  parent fork if docs.rs-style elision is required.
- `builtin_type`'s width mapping (only Bool/I32/str on the wire entity cell)
  is wire-side and stays; the live-Ir lattice maps the full width set from
  the type-fact records. `usize`/`isize`/`f128` have no BuiltinType in the
  Ir lattice and fold to `?unsupported` while the fragment keeps exact
  TypeWidth::Arch cells — recorded as a render-lattice gap for the parent.
- `purl.rs` hardcodes `RustEdition::Rust2024` for located packages (R14 red).

Corpus pinned from the local registry cache (hermetic, no network):
workspace members compiler-ir-vocabulary@0.1.0, compiler-ir@0.1.0 via PURL;
serde@1.0.229, serde_json@1.0.151, thiserror@2.0.20, anyhow@1.0.104;
libc@0.2.189, hashbrown@0.16.1, smallvec@1.15.2, tinyvec@1.12.0,
compact_str@0.10.0, either@1.18.0, memchr@2.8.3, nom@7.1.3, winnow@0.7.15,
toml_edit@0.22.27, http@1.5.0, log@0.4.34, getopts@0.2.24 (edition 2015),
typed-arena@2.0.2.

## Wave-3 addendum (mandate 2026-09-03): trunk consumption, computed fork, shortcut hunt

Adjudicated trunk facts (Terra, live tree 2026-09-03):

- Computed-type segment: LANDED. `compiler_ir::ComputedType` + `intern_computed`
  exist in the shared Ir; `TypeScriptFacts` (wire WIDTH 12) carries the only
  computed cell. Wire `RustFacts::WIDTH` is the compile-time const 16 with all
  four cells owned; widths are not fragment-self-describing, so adding a Rust
  computed cell changes decode of every previously published fragment.
  → R15 is returned as AUTHORITY_FORK: parent must choose the wire law.
  Until resolved, Rust inferred types stay on the proven anonymous-row/carrier
  mechanism; no silent live-only minting.
- Raised geometry (1024 facts): NOT landed. `MAX_EMISSION_FACTS` remains 128;
  the python lane's ignored six.py test records "trunk capacity decision
  pending". This lane does not raise shared constants. R17 is trunk-watch.
- Journal multi-generation: `server-journal` `DurablePublisher`
  (create/reopen/try_publish, `PublicationFacts.generation`) supports chained
  generations; CARD-PURL-LIFECYCLE consumes it for R10's second generation.
- Exact terminals: `compiler_driver::CompileFailure`/`NativeWorkPrimary` and
  `compiler/application/terminal` are the typed terminals the lane tests
  assert; no string fallbacks admitted in new lane tests.

Shortcut-hunt inventory (Terra, 2026-09-03, to be consumed by cards):
O(n·m) doc-line `span_of_text` windows scan (emit_docs, per line);
O(n²) method-call dedup (`method_calls` projected_span contains check);
O(rows × occurrences) `owner_of` linear scan; `foreign_rows` dedup cap 64
(memory-only, correctness preserved); tuples >8 children fold to OracleGap
(shared MAX_TYPE_CHILDREN bound); type walk depth 16 fold. Each retained
bound must name its folded reason in closure (R16).


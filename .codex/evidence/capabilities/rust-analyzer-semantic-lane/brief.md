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

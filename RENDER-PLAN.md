# IR → Source Renderer: Plan to Perfection

Status of the foundation (done): a Wadler–Lindig document algebra
(`render/doc.rs`), a `Backend` trait + shared combinators (`render/backend.rs`),
and five backends (`render/emit/{rust,go,java,typescript,python}.rs`). Every
backend renders records, sum types, functions, and traits, with width-driven
line breaking, doc comments, and inferred sum-type generics. Verified by
compiling the real files against a minimal `ir` mock and rendering a complex
sample into all five languages.

**Definition of "perfect" (the bar this plan targets):** for every supported
IR construct, each backend emits output that (1) **parses and compiles** under
that language's real toolchain, (2) is **idiomatic** (a fluent engineer would
write it that way), (3) is **lossless** where the language can express the
concept and **honestly, consistently degraded** where it cannot, and (4) is
**stable** (deterministic, gofmt/rustfmt/prettier/black-clean, byte-for-byte
reproducible). Enforced by an automated compile-check test matrix.

---

## Phase 0 — Document-algebra completeness (`render/doc.rs`)

The algebra is the layout substrate; a few missing primitives currently force
awkward workarounds in the backends.

- [ ] **`flat_alt(flat, broken)`** — Wadler's alternative: render `flat` when the
  enclosing group is flat, `broken` when broken. This is the elegant, correct
  way to express **trailing commas** (`flat_alt(nil, text(","))`), `flat_alt(" ",
  hardline)`, and language-specific flat/expanded spellings. Replaces the current
  `trailing_comma_when_broken()` stub (which returns `nil`, so no trailing commas
  are ever emitted — a rustfmt/prettier deviation).
- [ ] **`align(d)` / `column` / `nesting`** — set indent to the current column so
  wrapped items line up under an opening delimiter (call-argument alignment,
  `where`-clause alignment) instead of a fixed nest. Needed for rustfmt-faithful
  output.
- [ ] **`fill(sep, items)`** — greedy fill (pack as many per line as fit) for
  long `+`-bound lists, doc-comment prose reflow, derive lists.
- [ ] **Unicode width** — replace `chars().count()` with East-Asian width so wide
  glyphs/emoji in identifiers, string literals, and doc comments do not
  mis-measure and overflow the column budget.
- [ ] **`group` continuation tuning** — verify (with tests) the `fits`
  continuation logic against nested groups so arg lists prefer to break *before*
  generic lists (see the TS `transform<\n  T\n>(...)` artifact). Add a
  `group_with_id` / union mechanism if we need to couple break decisions.
- [ ] **Property tests** — idempotence (`render(render_doc)` stable), width
  monotonicity, "no line exceeds width unless it contains an unbreakable token,"
  and no trailing whitespace on blank breaks.

---

## Phase 1 — Cross-cutting engines (shared, language-parameterised)

These are the levers that move quality in *all five* languages at once. Build
them once in `backend.rs` / a new `render/lower.rs`, parameterised per language.

### 1a. Standard-library type mapping (highest leverage)

Today `Vec<T>` renders as `Vec<T>` in Go/Java/Python. A perfect renderer maps
well-known IR type references to each language's native equivalent:

| IR reference | Rust | Go | Java | TypeScript | Python |
|---|---|---|---|---|---|
| `Vec<T>` / list | `Vec<T>` | `[]T` | `List<T>` | `T[]` | `list[T]` |
| `Option<T>` | `Option<T>` | `*T` | `Optional<T>` | `T \| null` | `T \| None` |
| `HashMap<K,V>` | `HashMap<K,V>` | `map[K]V` | `Map<K,V>` | `Record<K,V>` / `Map<K,V>` | `dict[K,V]` |
| `Result<T,E>` | `Result<T,E>` | `(T, error)` | `T` (throws) | `T` | `T` (raises) |
| `Box/Rc/Arc<T>` | keep | `*T` / `T` | `T` | `T` | `T` |
| set | `HashSet<T>` | `map[T]struct{}` | `Set<T>` | `Set<T>` | `set[T]` |
| tuple | `(A,B)` | `struct{...}` | `record`/`Pair` | `[A,B]` | `tuple[A,B]` |

- [ ] A `KnownType` recogniser keyed on the *shortened* identifier + arity, with
  a per-language lowering table. Extensible, data-driven.
- [ ] Track which mappings imply an **import** (Java `java.util.List`, Python
  `from __future__ import annotations` / `datetime`, TS none) and feed the import
  collector (1d).

### 1b. Generics engine (replace the current simple-bound heuristic)

`analyze_generics` currently keeps only single-trait, no-arg bounds inline and
drops everything else. Perfection needs the whole `Generics` vocabulary:

- [ ] Lifetimes, const params (with real const type, not hard-coded `usize`),
  variance, default type params.
- [ ] Bounds with arguments (`T: Iterator<Item = u8>`), associated-type bounds,
  HRTB (`for<'a>`), const-expr bounds, lifetime bounds.
- [ ] **Where-clause policy** (port from legacy `rust.rs`): simple bounds inline,
  everything else to a `where` block, one constraint per line, aligned.
- [ ] Per-language projection: Rust `where`, Java `extends A & B`, TS `extends
  A & B`, Go `[T Constraint]` (interface constraints, union elements), Python PEP
  695 `[T: Bound]` + `TypeVar` fallback for older targets.
- [ ] **Recover sum-type bounds**, not just names — infer `Shape<T: Clone>` by
  unifying inferred params with any bounds discoverable from usage; today only
  the bare name is recovered.

### 1c. Identifier casing / naming conventions

- [ ] A casing utility (`snake`, `camel`, `Pascal`, `SCREAMING`) and per-language
  policy: Go exports PascalCase (already partial), Java fields/methods camelCase,
  TS camelCase, Python snake_case, Rust as-is. Apply to fields, methods, params,
  enum variants, and generated names (`field0`, marker methods) consistently.
- [ ] Keyword-collision escaping per language (`type`, `class`, `match`, `in`,
  `interface`, reserved words) → raw-ident / suffix strategy.

### 1d. Import / qualified-name collection

- [ ] A render pass that accumulates required imports (from 1a and from qualified
  paths) and can emit a header block (`use`, `import`, `from … import`) — needed
  once we render more than a single item, and to make Java/Python output
  compile.
- [ ] Finalise the `qualified_paths` option semantics per language.

### 1e. Documentation engine

- [ ] Language-faithful doc comments: Rust `///` + `//!`, Go `// Name ...`
  convention (comment must start with the identifier), Javadoc with `@param` /
  `@return`, TSDoc `/** */` with `@param`, Python **docstrings** (triple-quoted,
  inside the body) rather than `#` line comments for classes/functions.
- [ ] Thread **parameter descriptions** (`LiteralParameter::description`) and
  field/variant docs into the right surface (Javadoc `@param`, Python docstring
  Args:, TSDoc `@param`).
- [ ] Prose reflow via `fill` (Phase 0) to the column budget; preserve code
  spans / blank lines / lists.

### 1f. Annotation channel (make it earn its place)

- [ ] A highlighting `Sink` (ANSI / HTML) that consumes `Annotation` spans —
  demonstrates the algebra's separation of structure from styling.
- [ ] Optional: use annotations to drive language-server-style output or diffing.

---

## Phase 2 — Full `Type` algebra coverage

Every `Type` variant must have a defined rendering (or a documented, principled
fallback) in every language. Current explicit coverage is the common core; the
following need real handling or an intentional degradation with a comment.

Coverage matrix to complete (✔ = idiomatic, ≈ = best-effort, — = documented
fallback):

| Variant | Rust | Go | Java | TS | Python |
|---|---|---|---|---|---|
| `FunctionPointer` (with HRTB, attrs) | ✔ | `func(...)` | `Function<>`/functional iface | `(a:T)=>R` | `Callable[...]` |
| `DynTrait` | `dyn A + B` | iface | interface | union/iface | Protocol |
| `QualifiedPath` (`<T as Tr>::X`) | ✔ | ≈ | ≈ | ≈ | ≈ |
| `ImplTrait` | `impl Tr` | constraint | `? extends` | inline type | bound |
| `Sum` in type pos | inline `enum` | ≈ | ≈ | union | union |
| `RecordLiteral` (anon) | `{ a: T }` | anon struct | ≈ record | object type | TypedDict |
| `Union` / `Intersection` | `\|` / `+` | — | `\|`? sealed | `\|` / `&` | `\|` / — |
| `Array{n}` vs `Slice` | `[T; N]`/`[T]` | `[N]T`/`[]T` | `T[]` | `T[]`/tuple | `list`/`tuple` |
| `Variadic` | `...T` | `...T` | `T...` | `...T[]` | `*T` |
| TS-origin: `Mapped` | — | — | — | ✔ `{[K in …]}` | — |
| TS-origin: `Conditional` | — | — | — | ✔ `A extends B ? … : …` | — |
| TS-origin: `TypeOperator` | — | — | — | ✔ `keyof T` | — |
| TS-origin: `Predicate` | — | — | — | ✔ `x is T` | — |

- [ ] Fill each cell; TS gets first-class treatment of the TS-origin constructs
  (it is their native home). Non-TS languages render TS-origin types as a
  documented `/* … */` degradation, never silently wrong.
- [ ] **Slice-in-field-position fix (Rust):** a bare `[T]` field is unsized;
  render owned (`Vec<T>`) or borrowed (`&[T]`) per context, not `[T]`.

---

## Phase 3 — Per-language perfection checklists

Each item ends in a compile/format check (Phase 4).

### Rust
- [ ] Reconcile `String` vs `str` primitive policy with the legacy renderer's
  documented canonical choices; unify (owned in field/return position).
- [ ] Full generics + `where` clauses; lifetimes; const generics; HRTB.
- [ ] Records: tuple structs, unit structs, `#[derive]`s if carried, field
  defaults (`field: T = expr`), visibility per field.
- [ ] Traits: `super_traits` (`: A + B`), associated types (with bounds/defaults),
  associated consts, provided-method bodies (`{ … }` vs `;`), trait attributes
  (`#[marker]`, `auto`, `unsafe`).
- [ ] Impl blocks (`impl Trait for Type`) — `TraitImpl` is in the IR and has no
  renderer yet.
- [ ] Receiver kinds complete; function attributes (`async`/`const`/`unsafe`).
- [ ] Retire the legacy string renderer once parity + its golden tests pass on
  the new path (Phase 5).

### Go
- [ ] `Vec/Option/Map` → slice/pointer/map (1a); drop `Vec[T]`.
- [ ] Sum encoding: marker-method **receivers carry type params**
  (`func (ShapeCircle[T]) isShape()`); consider `const`+`iota` for dataless
  enums; unexported marker method name.
- [ ] Interface constraints for generics (`[T any]`, union elements `~int | ~string`).
- [ ] Exported-name policy configurable; struct tags passthrough if carried.
- [ ] Multiple return values (already partial) incl. idiomatic `(T, error)` for
  `Result`.
- [ ] gofmt-clean (tabs, alignment). Consider emitting tabs and letting gofmt
  normalise, or match gofmt exactly.

### Java
- [ ] Variant records declare their own type params: `record Circle<T>(…)
  implements Shape<T>`.
- [ ] Standalone functions → `static` methods inside a holder class (Java has no
  free functions); or document the interface-method degradation.
- [ ] Real `enum` for dataless sums; sealed-interface + records for data-carrying
  (already good) — pick per shape.
- [ ] Generics bounds (`extends A & B`), wildcards (`? extends`), `Optional<T>`
  imports, checked-exception policy for `Result`.
- [ ] Field/method casing camelCase; visibility keywords complete; annotations
  (`@Override`, `@Nullable`) where meaningful.
- [ ] javac-clean.

### TypeScript
- [ ] Tune generic-vs-arg break order (Phase 0) to avoid lone-param generic
  explosions.
- [ ] TS-origin type constructs first-class (Phase 2).
- [ ] `readonly`, optional `?`, index signatures (`[k: string]: V`), union
  discriminants (already good), `export`/`declare` policy.
- [ ] Method vs arrow-property in interfaces; overloads.
- [ ] prettier-clean (2-space? configurable), trailing commas via `flat_alt`.

### Python
- [ ] **Field ordering:** defaulted/optional fields must come last in a dataclass
  (current output is invalid). Reorder, or use `field(default=…)` /
  `kw_only=True`.
- [ ] `Vec/Map` → `list`/`dict` (1a); docstrings instead of `#` comments (1e).
- [ ] Protocol generics (`class Drawable[T](Protocol)`), `@runtime_checkable`
  where useful; `...` bodies (good).
- [ ] Import surface: `from dataclasses import dataclass`, `from typing import
  Protocol, Optional`, `datetime`, `TypeAlias`; `from __future__ import
  annotations` for forward refs.
- [ ] Configurable target: PEP 695 (3.12+) `[T]` vs `TypeVar` for ≤3.11.
- [ ] black/ruff-clean.

---

## Phase 4 — Verification: the compile-check test matrix

This is what makes "perfect" objective rather than aspirational.

- [ ] **Golden tests** per (language × construct), byte-exact, checked in.
- [ ] **Compile/parse gate** — for a corpus of IR samples, render each language
  and run the real toolchain, asserting success:
  - Rust → `rustc --emit=metadata` (or `cargo check` snippet)
  - Go → `gofmt -e` + `go vet` on a wrapped file
  - Java → `javac` on a wrapped compilation unit
  - TypeScript → `tsc --noEmit`
  - Python → `python -m py_compile` + `ruff check`
  Skip gracefully when a toolchain is absent (report which ran).
- [ ] **Formatter-idempotence** — piping our output through
  rustfmt/gofmt/prettier/black must be a no-op (proves idiomatic layout).
- [ ] **Round-trip** — feed real producer output (source → IR via the Go/Java/
  Python/TS/Rust producers) back through the renderer and compile it; diff
  against the original where semantics allow.
- [ ] **Property tests** from Phase 0.
- [ ] A single `render_matrix` demo/bin (extend the current harness) that prints
  every construct in every language for eyeballing + docs.

---

## Phase 5 — Legacy retirement & integration

- [ ] Port the 14 golden `assert_eq!` tests from legacy `render/rust.rs` onto the
  new `Backend` path; achieve parity on the documented canonical choices.
- [ ] Delete legacy `render/rust.rs`; collapse `render::rust` re-exports.
- [ ] Wire `render_entry` into whatever consumes rendering (emit/linked_data,
  graph, or a CLI subcommand) and document the public API.
- [ ] Update module docs to drop the "legacy" section.

---

## Sequencing (suggested)

1. **Phase 0** (algebra: `flat_alt`, `align`, unicode width) — unblocks trailing
   commas and alignment everywhere. Small, high value.
2. **Phase 1a + 1b + 1c** (type mapping, generics engine, casing) — the biggest
   idiomaticness jump across all languages.
3. **Phase 4 harness** early — stand up the compile-check gate so every
   subsequent change is proven, not eyeballed.
4. **Phase 2** (type algebra) and **Phase 3** per-language, iterating against the
   gate.
5. **Phase 1d/1e/1f** (imports, docs, annotations) to reach compile-cleanliness.
6. **Phase 5** retire legacy.

## Definition of done

- Every `Type` / `Entry` variant has defined output in all five languages.
- The compile-check matrix is green on the sample corpus for every language whose
  toolchain is available in CI.
- Output is formatter-idempotent for all five.
- Legacy `rust.rs` deleted; golden parity retained.
- One command renders and compiles the full construct matrix.

---

## Appendix — build infrastructure (out of renderer scope)

The in-tree `buck2` build of `//build:compiler` is gated by pre-existing
`registry.bzl` defects unrelated to the renderer, surfaced only on a full local
rebuild (previously served from cache). Fixed so far:

- **Windows gating** (`build/third-party/defs.bzl`): `cfg(windows)`-only crates
  (`windows*`, `winapi`, `uv_windows`) are now wrapped in
  `select({prelude//os/constraints:windows: […], DEFAULT: []})`, matching Cargo.
- **`uv-macros`**: corrected `proc_macro: False` → `True`.
- **`rustix-1`**: added the missing `termios` feature (needed by `terminal_size`).

Remaining (not attempted): a `terminusdb-schema-derive` git-vendored genrule
fails because `terminusdb-rs-repo/crates/schema-derive/` is missing from the
fetched archive — a submodule/checkout issue, plus likely further
under-specified crate features. **Recommendation:** rather than hand-patch
further, regenerate `registry.bzl` from a resolved `Cargo.lock` via
`build/third-party/tools/gen-registry.py` (run while cargo is available), which
does full feature unification and correct `proc_macro` flags in one pass. Keep
the Windows-gating change in `defs.bzl` regardless, since the generator emits the
same flattened deps.

# RA parity lowering (P1) — implementer brief

Pin: `ra_ap_* =0.0.341`. Full plan: repo root `RUST-ANALYZER-PLAN.md`. Sources: `/tmp/ra-src/*-0.0.341`.

---

## Public contract (do not change)

```rust
// compile/rust/mod.rs
pub fn generate_ir(
    root: &Path,
    name: &str,              // cargo package name (e.g. "odd-duck", "calculator")
    version: &Version,
    document_private: bool,  // → RustPackage.direct_repo
) -> Result<(Index, HashMap<String, String>), Package>
```

Also keep:
- `RustPackage { name, direct_repo }` + `generate_ir` / `generate_ir_with_sources`
- Callers: `generate/surface.rs` (discards source map today), tests, `producer_worker`

**Output**
- `Index { root_ids, entries_by_path }` via `Ir::from_entries(…).index().into_index()`
- Root rule: `NudoxPath::Local` with **one** path component, no `::` (crate name only)
- Source map: `fq_name → function source text` (key like `calculator::add`)

**`document_private` / `direct_repo`**
| Flag | Effect (today) |
|---|---|
| `true` | Include private items; if root has **no lib**, BFS workspace path-deps that **have lib** and lower them too |
| `false` | Public-only gate; root package only |

`surface.rs` sets `direct_repo` only for `RegistryOrigin::Custom`; tests always pass `true`.

**Orchestration change (RA)**
```
load_workspace_at(root) once
→ for each documented local package: find hir::Crate → walk → merge entries + source map
```
No per-package `cargo rustdoc`. Keep offline (`CARGO_NET_OFFLINE` / `extra_args: ["--offline"]`).

---

## File ownership (parallel implementers)

| File (new layout) | Owns | Replaces |
|---|---|---|
| `mod.rs` | `generate_ir` signature, re-exports | same |
| `load.rs` | `ExtractConfig`, `load_workspace_at`, prime caches, proc-macro hold | most of `package.rs` cargo/rustdoc |
| `ctx.rs` | `LowerCtx`, path/alias caches, `ImplIndex`, `stable_id`, visibility gate | `context.rs` |
| `walk.rs` | module stack, `include()`, lenient per-item | `RustdocParser::parse` |
| `item.rs` | ModuleDef / ADT / Trait / Impl → `Entry` | `item.rs` |
| `function.rs` | Function / TraitMethod / receiver / attrs / type_links | `function.rs` |
| `ty.rs` | **AST-first** type lowering + sema path resolution | `types.rs` type_ |
| `generics.rs` | AST generic params + where clause | `generics.rs` |
| `docs.rs` | `hir_docs` (parity); links later | scattered |
| `source.rs` | HasSource + LineIndex → source map | `source_map_from_crate` |
| `error.rs` | trim rustdoc variants; add Cancelled/ProcMacroDegraded | `error.rs` |
| `traversal.rs` | **KEEP AS-IS** (git version walk) | — |
| `package.rs` | thin: document set + call load/walk, or delete into load+mod | shell orchestration |

---

## IR Entry → rustdoc today → RA API

| `Entry` | rustdoc path | RA (P1) |
|---|---|---|
| `Module` | Module + child paths as members | `Module::children/declarations`; members = child `NudoxPath`s |
| `RecordType` | Struct → fields; attach inherent methods + `implemented_protocols` from impls | `Adt::Struct`; fields via `fields()`/`kind()`; methods from inherent `Impl`; protocols from trait impls (+ blanket/auto probe) |
| `SumType` | Enum → variants | `Adt::Enum` + `EnumVariant::{fields,kind}` |
| `UnionType` | Union fields as `Vec<Type>` | `Adt::Union::fields` |
| `Function` | free Function | `ModuleDef::Function` |
| `TraitDef` | Trait items; required vs provided by `has_body` | `Trait::{items,is_auto,is_unsafe,direct_supertraits}`; supertrait **args from AST** |
| `TraitImpl` | only `trait_` Some; inherent → error dropped | `Impl` with `trait_().is_some()`; inherent → methods only, **no Entry** |
| `TypeAlias` | alias type | `TypeAlias` + AST rhs |
| `Constant` / `Variable` | Const / Static payload `()` | same |
| `Macro` | Macro + ProcMacro → Macro `()` | `ModuleDef::Macro` |
| `PrimitiveType` | Primitive scan | `BuiltinType` / lang path map |
| skip | StructField, Variant, Assoc*, Use, TraitAlias, ExternCrate | EnumVariant at scope, TraitAlias (not in ModuleDef) |

**Symbol shell (every entry):** `name`, `path: Local("crate::…")`, `aliases`, `visibility`, `documentation`, `inner`.

**Record key fields:** `fields`, `methods`, `implemented_protocols`, `generics`.  
**Function key fields:** `input_parameters`, `output_parameters`, `receiver`, `attributes`, `generics`, `implemented`, `type_links`.  
**TraitDef:** `required_methods` / `provided_methods`, `associated_types`, `required_constants`, `super_traits`, `attributes` (Auto/Unsafe).  
**TraitImpl:** `tr`, `for_type`, `methods`, `associated_*`, `is_negative`, `is_blanket`, `is_unsafe`.

---

## Visibility mapping

| rustdoc | IR | RA |
|---|---|---|
| `Public` | `Public` | `Visibility::Public` |
| `Default` | `Private` | non-public default |
| `Crate` | `Internal` | `Module(m,_)` where `m` is crate root (`pub(crate)`) |
| `Restricted {..}` | `Package` | other `Module(..)` (`pub(super)` / `pub(in …)`) |

`document_private=false` → gate at walk (`Public` only). Tests accept `Internal | Private` for private items.

---

## Type lowering dual-path (mandatory)

1. **Shape from AST** (`ast::Type` via `HasSource` / `Semantics`) — lifetimes, mut, arg order.
2. **Identity from** `Semantics::resolve_path` / `resolve_type` → canonical `TypeReference.identifier`.
3. **`hir::Type` only** when AST missing (macro junk) or cross-check / probe (`impls_trait`, layout).

**Why:** `hir::Type::as_reference` returns `(Type, Mutability)` — **no lifetime**. `type_arguments` skips lifetimes. HIR-only is a **fidelity regression** vs rustdoc (`BorrowedRef.lifetime`, `DynTrait.lifetime`).

Generics/where: public hir drops where-preds and trait-bound args → lower `ast::GenericParamList` + `ast::WhereClause`. Skip implicit Self (`TypeParam::is_implicit` ≈ rustdoc `is_synthetic`).

Primitive widths: keep `types.rs` map (`i32`/`isize`→`Int(Arch)`, `u32`/`usize`→`UInt(Arch)`, etc.).

---

## Receiver / function quirks

| Case | IR `receiver` |
|---|---|
| no self | `None` (not `Some(Static)` on free/associated methods list) |
| `self` by value | `Some(Owned)` |
| `&self` | `Some(SharedRef)` |
| `&mut self` | `Some(MutRef)` |
| typed self (`self: Pin<&mut Self>`) | `Some(Arbitrary)` |

**RA:** `Function::self_param` → `SelfParam::access` → `Access::{Shared,Exclusive,Owned}`; typed self via `ast::SelfParam::ty()` → Arbitrary.

Attrs: `is_const`→Const, `is_async`→Async, `is_unsafe`→Unsafe.  
`implemented = has_body`. Self param stripped from `input_parameters`.  
`type_links`: hash **canonical path** (`stable_id`), not rustdoc `Id` — same wire type, better stability.

Trait methods: `has_body` → provided vs required; keep `TraitMethod.receiver` (includes Static).

---

## Paths / externals / package set

- Canonical path: `Module::path_to_root` + names, `::`-joined, crate segment = **rustc name** (`odd-duck` package → `odd_duck`).
- `NudoxPath::Local` vs `External { dependency, path }`: by `CrateOrigin::{Local, Library, Lang, Rustc}`.
- External dep name = crate display/canonical name (`helper`), relative path without leading crate segment.
- Aliases: `Module::scope` (improves globs; rustdoc Use-only, skips globs).
- Package match: `Crate::all` + `CrateOrigin::Local` + name/version vs `PackageData`.
- Binary workspace: when documenting `app` with `direct_repo`, also lower local lib deps (`corelib`).

---

## Impl / protocols (fixture-critical)

- Inherent impls → `Record.methods` only (no `TraitImpl` entry).
- Trait impls → `Entry::TraitImpl` + push trait path onto ADT `implemented_protocols`.
- Blankets: **do not** use `Impl::all_for_type` (excludes blankets). Use `Impl::all_in_crate` + `ty.impls_trait` for std-tier blankets (`BlanketView` must appear on `Counter`).
- `is_blanket` = self_ty is bare generic param.
- Negative: skip for protocols list (match today) or set `is_negative` on TraitImpl.

---

## Hard test requirements

### `tests/parse_rust_to_ir.rs` (all `document_private=true`)

| Test | Must hold |
|---|---|
| `regular_crate_lowers_public_items` | `calculator::add` Function; `calculator::Counter` RecordType; root `calculator` |
| `private_items_retain_private_visibility` | `calculator::internals` Module non-Public; `HiddenCounter` Internal\|Private |
| `record_methods_and_blanket_impls_are_attached` | Counter.methods **len > 3**; protocols contain Local ending `BlanketView` |
| `workspace_resolves_hyphenated_member_crates` | root `odd_duck`; Widget methods ≥2 with **both** `receiver.is_none()` and `is_some()`; `Behavior` TraitDef |
| `binary_workspace_includes_local_library_deps` | `corelib::CoreCounter` RecordType; `app` present |
| `external_references_are_external_paths` | protocols have External dep `"helper"`; **never** Local containing `Marker` |
| `source_map_is_produced_for_each_function` | key `calculator::add` contains `"left + right"` |

### `rust_compiler_e2e.rs` / `generate_blob.rs`
Surface non-empty; same path kinds for add/Counter/root; archive has Cargo.toml + src, **no `target/`**; linked-data emit Package+Symbol; snapshot hash reproducible.

### Fixtures (sources only under git; tests write Cargo.toml)

| Fixture | Packages | Notable symbols |
|---|---|---|
| `regular/` | calculator + path `helper` | add, Counter, BlanketView blanket, helper::Marker |
| `workspace/` | odd-duck 0.3.1 | Widget\<T\>, Behavior, Mode |
| `binary_workspace/` | app bin + corelib | CoreCounter via dep BFS |

---

## Pitfalls

1. **Lifetime erasure** — always AST-first for types/signatures.
2. **Where clauses** — not on public `ra_ap_hir`; AST only.
3. **`Impl::all_for_type`** — no blankets; wrong for protocols.
4. **Hyphen crate names** — package `odd-duck` vs path `odd_duck`.
5. **Dep rename** — `PackageDependency.name` may differ from package name; External dependency string must match what rustdoc used for path deps (`helper`).
6. **Salsa `Cancelled`** — unwind; `Cancelled::catch` / `catch_unwind` per item; Cancelled aborts package (retryable), other panics drop item (lenient, like today).
7. **Hold `ProcMacroClient`** for load lifetime; drop kills expansion.
8. **`LoadCargoConfig` (0.0.341)** has **no Default** — set all five: `load_out_dirs_from_check`, `with_proc_macro_server`, `prefill_caches`, `num_worker_threads`, `proc_macro_processes`.
9. **`set_test: false`** — exclude `cfg(test)` (match rustdoc).
10. **Sysroot + rust-src** required; missing → unknown std types → parity fail.
11. **Lenient parse** — item lower failure drops item, not whole crate.
12. **Stable IDs** — path-hash `type_links`; do not assert old rustdoc Id hashes.
13. **Receiver Static** — free/associated: `None` on `Function.receiver`; TraitMethod may use Static.
14. **RAM** — raise sandbox mem to ~6 GiB; one load per workspace.
15. **Offline** — no network in load; fixtures path-only deps.

---

## Surprises vs plan / API drift (0.0.341 verified under `/tmp/ra-src`)

| Plan says | Actual 0.0.341 |
|---|---|
| `LoadCargoConfig { …, ..Default::default() }` | **No Default**; required: `num_worker_threads`, `proc_macro_processes` |
| `as_reference → Option<(Type, Mutability)>` no lifetime | Confirmed: lifetime discarded via `skip_binder` / `Region::error` |
| `all_for_type` excludes blankets | Confirmed in doc comment on `Impl::all_for_type` |
| `CrateOrigin::{Lang,Rustc,Library,Local}` | Confirmed in `ra_ap_base_db` |
| `SelfParam::access → Access::{Shared,Exclusive,Owned}` | Confirmed |
| `resolve_doc_path_on` | In `ra_ap_hir::attrs` (exceed phase) |
| Plan `named_deps` | Cargo side: `PackageDependency.name` (rename); Buck named_deps is third-party tooling only |

**Not surprises (still true):** one load whole graph; AST for where/lifetimes; inherent impls no TraitImpl entry; source map keys by fq path; P1 IR shape frozen.

**P1 exit:** ✅ `parse_rust_to_ir` + e2e + generate_blob green on RA (default); temp fallback `NUDOX_RUST_PRODUCER=rustdoc` until P3.

# RUST-ANALYZER-PLAN — Replace the rustdoc pipeline with an in-process rust-analyzer producer

**Status: in progress (2026-07-09). P0 complete; P1 exit met — RA is the default producer; `NUDOX_RUST_PRODUCER=rustdoc` is the temporary fallback until P3 removal. Supersedes the rustdoc in-process driver plan (Phase A/B librustdoc vendoring) — that scaffolding gets deleted, not activated.**

Goal: the Rust producer stops shelling out to `cargo rustdoc --output-format json` (and stops carrying the vendored-librustdoc driver scaffolding) and instead loads the workspace once with the `ra_ap_*` rust-analyzer crates, walks HIR in-process, and lowers directly to `ir::Index`. The new pipeline must first reach **parity** with the IR the rustdoc path produces today, then **exceed** it using channels rustdoc either drops or cannot provide. The rustdoc path is removed entirely at the end — no fallback.

Everything here is verified against `ra_ap_*` **0.0.341** (2026-07-06) on docs.rs and the current repo state.

---

## 0. Why this works now (decision record)

- **Trait-solver parity**: rust-analyzer removed chalk and now shares the `rustc_type_ir` "next solver" with rustc (PR rust-analyzer#20329, Aug 2025; complete as of changelog #299). The historical "RA types diverge from rustc" objection is largely gone.
- **One load, whole graph**: rustdoc JSON is one crate per subprocess run (we currently spawn `cargo rustdoc` *per package*). RA loads the workspace + all dependencies + sysroot once; `hir::Crate::all(db)` exposes every crate. `direct_repo` multi-package repos go from N compiler runs to one load.
- **Strictly more information**: resolved re-export graph (incl. globs we currently skip), typed cfg/attr access, spans for macro-generated items (rustdoc: `span: None`), trait solving (`Type::impls_trait`), method resolution, layout, dyn-compatibility — none of which rustdoc JSON has.
- **No nightly hostage**: drops `RUSTC_BOOTSTRAP=1`, the rustdoc wrapper script, the JSON tempfile round-trip, and the entire librustdoc vendor/patch/genrule apparatus. Our flake's fenix nightly already satisfies RA's MSRV (1.95, edition 2024) and ships `rust-src`.
- **Known costs** (mitigated below): lockstep `=0.0.x` pinning across ~15 crates; proc-macro server as a separate failure domain; HIR erases lifetimes (we lower signatures from the AST instead); RAM footprint 1–4 GiB per loaded workspace.

Prior art: cargo-modules consumes `ra_ap_*` exactly this way (exact-pin, Renovate-grouped bumps); GitHub's CodeQL Rust extractor is rust-analyzer-based; RA's own `StaticIndex` (SCIP/LSIF) is the in-tree template for "walk everything, emit records".

---

## 1. Prerequisites

### 1.1 Toolchain (flake.nix)

- fenix `complete` (nightly) already includes `rustc`, `cargo`, `rust-src` ✅ (`rust-src` is required so RA can analyze `std`/`core`).
- **Add the `rust-analyzer` component** to `fenix-pkg.complete.withComponents` so the sysroot ships `libexec/rust-analyzer-proc-macro-srv` → `ProcMacroServerChoice::Sysroot` just works. Fallback (if the component's server ABI drifts from our pinned nightly): vendor `ra_ap_proc_macro_srv_cli` (feature `sysroot-abi`), build it with the same toolchain as a Buck binary, and use `ProcMacroServerChoice::Explicit(path)`. The ABI contract is "server built by the same rustc that compiled the proc-macro dylibs" — since cargo-check runs under our pinned nightly, either option satisfies it; sysroot component is less maintenance.
- **Cleanup**: `rustc-dev` was added to the flake *solely* for the librustdoc vendoring plan (see the comment at flake.nix:151–158). Drop it with the removal phase — it inflates the sysroot considerably.

### 1.2 Crate pinning policy

All `ra_ap_*` crates are published in lockstep with **no semver**; internal salsa keys are incompatible across versions. Policy:

- Pin every crate to the **same exact version**: `=0.0.341`.
- Record the pin once as `RA_AP_VERSION = "0.0.341"` in `build/third-party/registry.bzl` provenance comments; bump all entries together (one `nudox update` run), never individually.
- Budget one bump per quarter unless a fix is needed. Each bump is a mechanical PR: regenerate registry, fix compile errors in `compile/rust/` (API churn is real but shallow — cargo-modules absorbs it weekly).

### 1.3 Vendoring (~50–70 crates into Buck)

Mechanism: `build/third-party/tools/nudox` (our custom crates.io→`registry.bzl` generator, entrypoints `add-crate.py` / `gen-registry.py`). `ra_ap_*` are plain registry crates — no git entries, no patches expected.

1. Scratch manifest with the direct set (below), `cargo metadata` to enumerate the closure.
2. `python3 build/third-party/tools/add-crate.py` per new root; `gen-registry.py` to unify features.
3. Already vendored and reusable (verify versions): `rowan`, `smol_str`, `text-size`, `triomphe`, `rustc-hash`, `either`, `indexmap`, `itertools`.
4. New heavy transitive families to expect: `ra_ap_salsa` (RA's salsa fork), the hyphenated `ra-ap-rustc_*` mirrors (`rustc_abi`, `rustc_index`, `rustc_lexer`, `rustc_parse_format`, `rustc_pattern_analysis`, `rustc_type_ir`…), `la-arena`, `dashmap`, `crossbeam-*`, `jod-thread`, `camino`, `cargo_metadata`, `semver`, `dot`-free (no graphviz needed).
5. Build-script flags: several `ra_ap_*` crates have trivial `build.rs` (version strings) — `"build_script": True` in their entries; no network, sandbox-safe.

Direct dependency set for the producer:

```toml
ra_ap_load_cargo     = "=0.0.341"  # load_workspace_at → (RootDatabase, Vfs, ProcMacroClient)
ra_ap_project_model  = "=0.0.341"  # CargoConfig, ProjectWorkspace, package/target metadata
ra_ap_hir            = "=0.0.341"  # the entire traversal + semantics surface
ra_ap_ide            = "=0.0.341"  # Analysis, parallel_prime_caches
ra_ap_ide_db         = "=0.0.341"  # RootDatabase, LineIndex, FileRange
ra_ap_base_db        = "=0.0.341"  # Cancelled, CrateOrigin, FileId
ra_ap_vfs            = "=0.0.341"  # Vfs, VfsPath
ra_ap_syntax         = "=0.0.341"  # ast::* nodes for the syntactic lowering path
ra_ap_paths          = "=0.0.341"  # AbsPathBuf
ra_ap_cfg            = "=0.0.341"  # CfgExpr (exceed-phase)
ra_ap_proc_macro_api = "=0.0.341"  # ProcMacroClient (held alive for the load's lifetime)
```

No genrule, no `RUSTC_BOOTSTRAP`, no `local_only` — plain `rust_library` deps in `workspace/compiler/BUCK`, replacing `crate("rustdoc_types")`.

---

## 2. Architecture

### 2.1 Process model

The analysis itself is in-process and only *parses* untrusted source. Arbitrary-code execution moves to two well-defined subprocess boundaries, both already covered by our isolation machinery:

1. **`cargo metadata` + build scripts** — `load_out_dirs_from_check: true` runs a `cargo check`-shaped build (build.rs, proc-macro compilation). This is the same trust boundary the current `cargo rustdoc` run has. Run the *entire producer* (load + walk) inside the sandboxed worker path (`ProducerProfile::Rust`), same as today's subprocess but now the parent is our own extraction binary. This also contains RA's memory (1–4 GiB typical; cap below) and any hir panics away from the indexer.
2. **proc-macro server** — a child process RA spawns; inherits the sandbox of its parent.

Profile changes (`workspace/util/sandbox/profiles.rs`): keep `ProducerProfile::Rust`, update the doc comment (`cargo rustdoc` → `rust-analyzer load: cargo metadata + build scripts + proc-macro srv`), and raise `mem` 3 GiB → **6 GiB** (RA resident sets on medium workspaces regularly exceed 3). `max_stdout` no longer matters (no JSON on stdout) but 16 MiB is harmless.

### 2.2 One load serves all packages

Today `generate_ir_for_package` runs `cargo rustdoc` per package. New flow:

```
generate_ir_with_sources(root, version)
  └─ load_workspace_at(root)                 # once
       └─ for pkg in documented_local_packages(ProjectWorkspace):
            find hir::Crate for pkg           # Crate::all + CrateOrigin::Local + name/version match
            walk + lower → merge into Index   # shared LowerCtx caches across packages
```

`documented_local_packages` re-derives from `ProjectWorkspace`/`CargoWorkspace` (members, lib targets, dep edges) — the separate `cargo metadata` subprocess in `package.rs:321` disappears.

### 2.3 File layout (replaces `workspace/compiler/compile/rust/*`)

```
compile/rust/
  mod.rs        — public API (unchanged signature), error re-exports
  load.rs       — ExtractConfig → LoadedWorkspace (ra_ap_load_cargo wiring)
  ctx.rs        — LowerCtx: caches, path derivation, impl index, stable IDs
  walk.rs       — crate/module traversal → Entry stream
  item.rs       — ModuleDef → Entry lowering (structs/enums/traits/impls/…)
  function.rs   — Function/TraitMethod lowering, receivers
  ty.rs         — ast::Type + hir::Type → ir::Type (dual-path, §4)
  generics.rs   — generic params + where clauses (AST-driven)
  docs.rs       — documentation, intra-doc links, deprecation/attrs
  source.rs     — HasSource → FileRange/LineIndex, source map
  error.rs      — error taxonomy (trimmed of rustdoc variants)
  traversal.rs  — KEEP AS-IS (git Cargo.toml version resolution; not rustdoc-specific)
```

---

## 3. The typed API surface

### 3.1 Public entry point — unchanged contract

```rust
// mod.rs — signature identical to today (mod.rs:44); callers in generate/surface.rs untouched
pub fn generate_ir(
    root: &Path,
    name: &str,
    version: &Version,
    document_private: bool,
) -> Result<(Index, HashMap<String, String>), Package>
```

### 3.2 Loading

```rust
// load.rs
pub(crate) struct ExtractConfig {
    pub document_private: bool,
    pub offline: bool,                      // → extra_env CARGO_NET_OFFLINE=true, extra_args ["--offline"]
    pub run_build_scripts: bool,            // load_out_dirs_from_check; default true
    pub proc_macros: ProcMacroPolicy,       // Sysroot | Explicit(AbsPathBuf) | Disabled
    pub probe: ProbeTier,                   // §6.2: Off | Std | Graph — auto/blanket-impl synthesis depth
    pub num_threads: usize,                 // prime-caches workers; default = min(8, cores)
}

pub(crate) struct LoadedWorkspace {
    pub db: RootDatabase,
    pub vfs: Vfs,
    pub ws: ProjectWorkspace,               // kept for package/target metadata
    _proc_macro: Option<ProcMacroClient>,   // held alive; dropping kills expansion
}

pub(crate) fn load(root: &AbsPath, cfg: &ExtractConfig) -> Result<LoadedWorkspace, RustError> {
    let cargo_config = CargoConfig {
        sysroot: Some(RustLibSource::Discover),      // fenix devshell sysroot; needs rust-src
        all_targets: false,
        set_test: false,                             // exclude #[cfg(test)] — matches rustdoc
        no_deps: false,                              // deps required for cross-crate resolution
        extra_env: offline_env(cfg),
        ..Default::default()
    };
    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: cfg.run_build_scripts,
        with_proc_macro_server: cfg.proc_macros.into(),   // ProcMacroServerChoice
        prefill_caches: false,                            // we prime explicitly, with progress + cancellation
        ..Default::default()
    };
    let (db, vfs, pm) = load_workspace_at(root.as_ref(), &cargo_config, &load_config, &|_| {})?;
    // prime types in parallel before walking; all Cancelled → RustError::Cancelled
    AnalysisHost::with_database(db) … analysis().parallel_prime_caches(cfg.num_threads, |_| {})?;
    …
}
```

Degradation ladder (recorded on the produced `Index` as producer diagnostics, not silent):
1. proc-macro server unavailable → items from proc macros absent; emit `RustError::ProcMacroDegraded` warning, continue.
2. build scripts disabled/failed → `OUT_DIR`-generated items absent; warn, continue.
3. sysroot without `rust-src` → std types unresolved (`Type::is_unknown`); warn loudly — parity tests fail in this state by design.

### 3.3 Lowering context (replaces `ParseContext`/`ParseState`)

```rust
// ctx.rs
pub(crate) struct LowerCtx<'db> {
    db: &'db RootDatabase,
    sema: Semantics<'db, RootDatabase>,       // path/type resolution on AST nodes
    krate: hir::Crate,                        // the package currently being lowered
    display: DisplayTarget,                   // DisplayTarget::from_crate(db, krate)
    edition: Edition,

    /// ModuleDef → canonical NudoxPath. Canonical = defining-module chain
    /// (Module::path_to_root + names), matching today's `crate::a::b::C` shape.
    path_cache: FxHashMap<PathKey, NudoxPath>,
    /// All public paths per def (re-export aliases, incl. through globs), from the scope scan.
    alias_cache: FxHashMap<PathKey, HashSet<Vec<String>>>,
    /// Trait- and self-ty-bucketed impls for the crate graph slice we probe (§6.2).
    impls: ImplIndex,
    /// FileId → LineIndex, lazily built for span/source-map extraction.
    lines: FxHashMap<FileId, Arc<LineIndex>>,

    visiting: FxHashSet<PathKey>,             // cycle guard (kept from today)
    cache: FxHashMap<PathKey, Entry>,         // memoization (kept from today)
}

/// Version-stable identity for defs. hir ids are salsa-interned and NOT stable
/// across runs, so identity/hashing is by canonical path string.
pub(crate) type PathKey = SmolStr;            // canonical path, e.g. "serde::de::Deserialize"
```

**Path derivation** (replaces `build_path_map` BFS):
- Canonical: walk `Module::path_to_root(db)`, render names with `Name::as_str()`, join with `::`, classify by `CrateOrigin` → `NudoxPath::Local` (`Local{..}`) vs `NudoxPath::External { dependency, path }` (`Library`/`Lang`). This is exactly rustdoc's `paths` semantics, but it works for *every* crate in the graph, always.
- Aliases: one pass over each local module's `Module::scope(db, None)`; every `(Name, ScopeDef)` whose def resolves elsewhere is an alias path. **This resolves glob re-exports** (today: skipped with a TODO) and renamed re-exports for free, and populates `Symbol.aliases` strictly better than the current non-glob `Use`-following.

**Stable IDs for `Function.type_links`** (today: `DefaultHasher` over rustdoc's opaque `Id.0` string — meaningless across runs): hash the canonical path instead.

```rust
pub(crate) fn stable_id(path: &PathKey) -> i64 { fxhash64(path.as_bytes()) as i64 }
```

Same wire type (`HashMap<String, i64>`), now deterministic across runs and machines — an improvement the `linked_data` `takes`/`returns` edges inherit silently.

### 3.4 Traversal (replaces id-map iteration)

```rust
// walk.rs
pub(crate) fn lower_crate(ctx: &mut LowerCtx<'_>) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut stack = vec![ctx.krate.root_module(ctx.db)];
    while let Some(module) = stack.pop() {
        stack.extend(module.children(ctx.db));
        out.push(item::module_entry(ctx, module));                 // Entry::Module + members
        for def in module.declarations(ctx.db) {
            if !ctx.include(def) { continue; }                     // visibility gate (§3.5)
            if let Some(e) = item::lower(ctx, def) { out.push(e); }
        }
        for imp in module.impl_defs(ctx.db) {
            item::lower_impl(ctx, &mut out, imp);                  // TraitImpl entries + Record method attach
        }
    }
    out
}
```

Every lowering call sits inside `catch_unwind`: `Cancelled::catch(|| …)` — a panic from `hir_ty` on one item drops that item (today's lenient per-item policy, `parse()` at context.rs:64) instead of the whole package; a `Cancelled` unwind aborts the package as retryable.

### 3.5 Visibility & inclusion

```rust
// kind-for-kind with today's mapping (types.rs:11)
fn visibility(ctx: &LowerCtx, def: impl HasVisibility) -> ir::Visibility {
    match def.visibility(ctx.db) {
        hir::Visibility::Public                  => ir::Visibility::Public,
        hir::Visibility::Module(m, _) if m.is_crate_root() ... => ir::Visibility::Internal, // pub(crate)
        hir::Visibility::Module(..)              => ir::Visibility::Package,  // pub(in …)/pub(super)
        _                                        => ir::Visibility::Private,
    }
}
```

`document_private == false` filters to `Public` at the `include()` gate; `true` keeps everything (today's behavior — the fixtures assert private items appear with `Private`/`Internal`).

### 3.6 Item lowering map (parity column = what must not change on the wire)

| RA source | IR target (unchanged) | Notes |
|---|---|---|
| `Module` | `Entry::Module(Symbol<Module{members}>)` | members = declarations' `NudoxPath`s |
| `Adt::Struct` — `fields()`, `kind()` (Record/Tuple/Unit) | `Entry::RecordType(Record)` | `FieldKey::Ident/Index`; field `ty` via §4; field vis/docs via `HasVisibility`/`hir_docs` |
| `Adt::Enum` — `variants()`, per-variant `fields()+kind()` | `Entry::SumType(Vec<SumVariant>)` | `SumField::Tuple/StructLike` |
| `Adt::Union` — `fields()` | `Entry::UnionType(Vec<Type>)` | |
| `Function` | `Entry::Function(Function)` | §3.7 |
| `Trait` — `items()`, `is_auto()`, `is_unsafe()`, `direct_supertraits()` | `Entry::TraitDef(TraitDef)` | required vs provided via `Function::has_body`; `AssocItem::TypeAlias` → `AssociatedType`; `AssocItem::Const` → `TraitConstant` (default via `HasSource` expr text → `ConstExpr::Var`, matching today) |
| `Impl` with `trait_() == Some` | `Entry::TraitImpl(TraitImpl)` | `trait_ref()` for args; `self_ty()`; `is_negative()`, `is_unsafe()`; `is_blanket` = self_ty is a bare generic param |
| `Impl` inherent | *no entry*; methods attach to owning `Record.methods` | matches `InherentImplNotSupported` policy today |
| `TypeAlias` — `ty()` + AST rhs | `Entry::TypeAlias(Type)` | |
| `Const` / `Static` | `Entry::Constant(())` / `Entry::Variable(())` | payload-free today; stays |
| `Macro` (all `MacroKind`s) | `Entry::Macro(())` | |
| `ModuleDef::BuiltinType` | `Entry::PrimitiveType(())` + primitive path map | replaces `scan_primitives` |
| `ModuleDef::EnumVariant` at module scope (`use Enum::*`) | skip (consumed inline) | matches today |
| `TraitAlias` | skip | not in `ModuleDef`; today's path also drops it (`UnsupportedItemType`) — revisit in exceed phase |

Supertraits: `direct_supertraits()` returns `Vec<Trait>` without generic args — for `TraitRef.args` parity (e.g. `PartialOrd<Rhs>`), read the supertrait bound list from the AST (`ast::Trait::type_bound_list()`) through the same §4 type lowerer. This is the general pattern (next section) whenever hir's convenience surface loses arguments.

### 3.7 Functions and receivers

```rust
// function.rs
fn lower_function(ctx: &mut LowerCtx, f: hir::Function) -> ir::Function {
    // inputs: name from Param::name(db), type via §4 (AST-first) with hir::Type fallback
    // output: f.ret_type(db) — but AST ast::RetType for the syntactic shape
    // attributes: f.is_async → Async, is_const → Const, is_unsafe → Unsafe, is_varargs → Variadic
    // implemented: f.has_body(db)
    // receiver:
    //   f.self_param(db) → SelfParam::access(db):
    //     Access::Shared    → ReceiverKind::SharedRef
    //     Access::Exclusive → ReceiverKind::MutRef
    //     Access::Owned     → ReceiverKind::Owned
    //   typed self (self: Pin<&mut Self> …, detected on ast::SelfParam::ty()) → ReceiverKind::Arbitrary
    //   no self → None
}
```

This *fixes* a latent bug in the rustdoc path: receiver detection today string-matches the first param named `"self"` and misses `Pin<&mut Self>` shapes (function.rs:36) — `SelfParam::access` is the compiler's answer. (Python producer note: `ReceiverKind::Self_` naming collision from the toolchain memory does not apply here; IR variants are unchanged.)

### 3.8 Docs, links, deprecation

```rust
// docs.rs
fn documentation(ctx: &LowerCtx, def: impl HasAttrs + Copy) -> Option<String> {
    def.hir_docs(ctx.db).map(|d| d.into_docs())     // == rustdoc's Item.docs concatenation
}
// exceed-phase (§6.1): resolve intra-doc links — rustdoc path never consumed Item.links
fn doc_links(ctx: &LowerCtx, def: impl HasAttrs + Copy, docs: &str) -> Vec<(String, NudoxPath)> {
    extract_markdown_link_targets(docs)
        .filter_map(|link| hir::resolve_doc_path_on(ctx.db, def, &link, None, IsInnerDoc::No)
            .map(|d| (link, ctx.path_of_doclink(d))))
        .collect()
}
```

`AttrsWithOwner` also gives `is_deprecated()`, `is_doc_hidden()`, `cfgs()`, `is_unstable()`, `is_non_exhaustive()` — all exceed-phase inputs (§6.1); the parity phase only needs `hir_docs`.

### 3.9 Spans and the source map

```rust
// source.rs — replaces source_map_from_crate (package.rs:373)
fn fn_source(ctx: &mut LowerCtx, f: hir::Function) -> Option<(PathKey, String)> {
    let src: InFile<ast::Fn> = f.source(ctx.db)?;
    let range: FileRange = ctx.sema.original_range_opt(src.value.syntax())?; // macro-aware
    let text = ctx.file_text(range.file_id)?;             // via Vfs, not a raw fs read
    Some((ctx.canonical(f), text[range.range].to_string()))
}
```

Same output shape (`HashMap<String, String>` keyed by fq name). Improvements inherited: byte-precise ranges instead of line slicing; **macro-generated functions get call-site sources** (rustdoc: no span at all); no re-reading files from disk out from under the VFS.

---

## 4. Type lowering — the core design decision

**Two-path lowering: syntactic shape from the AST, resolution from Semantics, semantics from `hir::Type` only where it adds information.**

Rationale (all verified):
- `hir::Type` **erases lifetimes** (`as_reference()` returns no lifetime; `type_arguments()` skips lifetime args; display code `skip_binder()`s them). Our IR carries `BorrowedRef { lifetime: Option<String>, … }` and `DynTrait { lifetime }` — HIR-only lowering is a fidelity *regression* vs rustdoc.
- Where clauses and inline param bounds are not reachable through public `ra_ap_hir` (`GenericParams::where_predicates` is hir_def-internal; `TypeParam::trait_bounds` drops args and where-clauses).
- rustdoc's own `Type` is semi-syntactic (it lowers type *as written*); matching its output is naturally an AST walk.
- The AST is always available — `HasSource` works for macro-expanded items too (expansion files).

So:

```rust
// ty.rs
/// Lower a written type. `ast` is the source of shape (lifetimes, mutability,
/// arg order, parens); `sema` resolves every path segment to a def for
/// TypeReference identifiers and NudoxPath linking.
pub(crate) fn lower_ast_type(ctx: &mut LowerCtx, node: &ast::Type) -> ir::Type {
    match node {
        ast::Type::PathType(p)     => lower_path_type(ctx, p),        // → TypeReference | SelfType | GenericParam | Primitive | QualifiedPath
        ast::Type::RefType(r)      => ir::Type::BorrowedRef { lifetime: r.lifetime().map(text), is_mutable: r.mut_token().is_some(), r#type: box lower(r.ty()) },
        ast::Type::PtrType(p)      => ir::Type::RawPointer { … },
        ast::Type::SliceType(s)    => ir::Type::Slice(box …),
        ast::Type::ArrayType(a)    => ir::Type::Array { r#type: box …, length: eval_len(ctx, a) },  // const-eval via sema, fallback to expr text
        ast::Type::TupleType(t)    => ir::Type::Tuple(…),
        ast::Type::FnPtrType(f)    => ir::Type::FunctionPointer(…),
        ast::Type::DynTraitType(d) => ir::Type::DynTrait(lower_bounds_as_polytraits(ctx, d)),
        ast::Type::ImplTraitType(i)=> ir::Type::ImplTrait(lower_generic_bounds(ctx, i.type_bound_list())),
        ast::Type::NeverType(_)    => ir::Type::Never,
        ast::Type::InferType(_)    => ir::Type::Infer,
        ast::Type::ParenType(p)    => lower_ast_type(ctx, &p.ty()…),
        ast::Type::ForType(f)      => lower_with_hrtb(ctx, f),        // for<'a> fn(...) — PolyTrait.lifetimes
        ast::Type::MacroType(m)    => sema_fallback(ctx, node),       // expand via Semantics, or Infer
    }
}

/// Path resolution: Semantics::resolve_path on each ast::Path
///   PathResolution::Def(ModuleDef)     → TypeReference { identifier: ctx.canonical(def), generic_args }
///   PathResolution::TypeParam/SelfType → GenericParam / SelfType
///   builtin primitives                 → ir::Type::Primitive (Width map identical to today)
///   unresolvable                       → TypeReference with the written path (today's last-resort string split)
```

`hir::Type` is used where the AST cannot answer (all verified public API):
- `Semantics::resolve_type(ast::Type) -> hir::Type` when a semantic check is wanted on a written type;
- `Field::ty`, `Function::ret_type`, `TypeAlias::ty` as **cross-checks** in debug builds (`debug_assert` display-equality) and as the *only* source for expansion-generated positions with no useful AST;
- everything in §6 (impls_trait probing, normalization, layout).

**Generics** (`generics.rs`): lower `ast::GenericParamList` + `ast::WhereClause` directly:
- `ast::TypeParam` → `Parameter::Type(TypeParam)` (skip implicit `Self` — `hir::TypeParam::is_implicit` is the check that replaces rustdoc's `is_synthetic` skip);
- `ast::ConstParam` → `Parameter::Const(ConstParam)` (ty + default expr text → `ConstExpr::Var`, today's encoding);
- `ast::LifetimeParam` → `Parameter::Lifetime(LifetimeParam)`;
- `ast::WherePred` → `Constraint::TraitBound` / `Constraint::LifetimeBound` / assoc-type bindings inside bound args → `Constraint::AssociatedTypeBound` — the exact trio the rustdoc path emits today.

---

## 5. Impls: inherent methods, implemented_protocols, blanket & auto traits

Parity requirements from the fixtures: `Record.methods` populated from inherent impls; `Record.implemented_protocols` includes trait impls **including blanket impls** (`parse_rust_to_ir.rs` asserts this); external traits resolve to `NudoxPath::External`.

```rust
// ctx.rs
pub(crate) struct ImplIndex {
    by_self_ty: FxHashMap<PathKey, Vec<hir::Impl>>,   // ADT-keyed, local crate: Impl::all_in_crate
    blanket: Vec<(hir::Trait, hir::Impl)>,            // impls whose self_ty is a bare generic param
    auto_traits: Vec<hir::Trait>,                     // Send, Sync, Unpin, UnwindSafe, RefUnwindSafe, Sized
}
```

Construction, tiered by `ProbeTier` (config §3.2):
- **Local (always)**: `Impl::all_in_crate(db, krate)` — partition trait/inherent, bucket by self-ty ADT. Inherent methods attach to `Record.methods`; local trait impls become `Entry::TraitImpl` and feed `implemented_protocols`. (`Impl::all_for_type` is explicitly documented as excluding blanket impls — do not rely on it for this.)
- **Std tier (default)**: scan `Impl::all_in_crate` over `core`/`alloc`/`std` + direct deps for blanket impls (`impl<T: Bound> Trait for T`); for each local ADT `ty = adt.ty(db)`, `ty.impls_trait(db, trait, &args)` decides membership → synthesize `implemented_protocols` entries with `is_blanket: true`, exactly the rustdoc synthesized set (`From`/`Into`/`TryFrom`/`Any`/`Borrow`/`ToOwned`/`ToString`…).
- **Auto traits**: for each local ADT, `ty.impls_trait(db, send, &[])` etc. over the curated auto-trait list (resolved once via `Trait::lang`/well-known paths) → synthesize marker `TraitImpl`-shaped protocol memberships mirroring rustdoc's `is_synthetic` impls. Negative results for auto traits are representable (`is_negative: true`) and *better-founded* than rustdoc's (which ICEs on some deeply-generic cases — rust#135363).
- **Graph tier (opt-in)**: extend the blanket scan to the whole dependency graph. Cost is real (RA method-candidate iteration on impl-heavy graphs is slow — rust-analyzer#17068); keep behind config.

Trait-solver caching lives in salsa; repeated `impls_trait` probes on the same (ty, trait) are cheap after the first.

---

## 6. Exceeding rustdoc fidelity

### 6.1 Channels the rustdoc path *drops today* (immediate wins, IR mostly ready)

These are fields the current lowering reads from nothing — rustdoc JSON has them, we discard them; RA gives them to us typed:

| Channel | RA API | IR landing spot |
|---|---|---|
| Deprecation | `attrs.is_deprecated()` (+ note/since via raw attr) | new `Symbol` field (schema change, see below) |
| `#[doc(hidden)]` | `attrs.is_doc_hidden()` | `Symbol` field / filter policy |
| Intra-doc links, resolved | `hir::resolve_doc_path_on` (§3.8) | new `Symbol.doc_links: Map<String, NudoxPath>` → feeds the `mentions` edge in `linked_data` `EDGE_FIELDS` — an edge that exists in the TDB schema and is currently starved |
| cfg gating | `attrs.cfgs() -> Option<&CfgExpr>` (typed!) rendered to string | `Symbol` field; rustdoc JSON only has this as an unversioned debug string |
| `#[non_exhaustive]`, `#[must_use]`, repr | `attrs.is_non_exhaustive()`, `by_key` lookups, `Struct::repr()` | `Record`/enum attrs |
| Trait: dyn-compatibility | `Trait::dyn_compatibility(db)` | `TraitAttribute::ObjectSafe` — **already in the IR, never populated** |
| Sealed-trait detection | private supertrait / private-module bound pattern over resolved defs | `TraitAttribute::Sealed` — already in the IR, never populated |
| Auto-trait *non*-membership (`!Send`) | negative `impls_trait` probe | `TraitImpl.is_negative` synthesized entries |
| Glob re-export aliases | `Module::scope` scan (§3.3) | `Symbol.aliases` (existing field, richer content) |
| Macro-generated item sources | `original_range_opt` call-site mapping | source map (existing), spans where rustdoc has none |
| Inline type-param bounds | AST `type_bound_list` (rustdoc puts these in `GenericParamDefKind::Type{bounds}` which we drop) | `Constraint::TraitBound` (existing) |

### 6.2 Channels rustdoc JSON *cannot provide at all*

- **Whole-graph external entries**: today `NudoxPath::External` is a name-only pointer. RA can lower *full entries* for direct-dependency public items in the same load (docs, signatures, everything) — one load replaces N per-crate rustdoc runs for cross-crate surface. Gate behind a producer option; this is the docs.rs/registry-scale unlock.
- **Layout**: `Type::layout(db)` → size/align/field offsets/niches → future `Record` metadata for FFI/perf-oriented facets.
- **Normalization**: `Type::normalize_trait_assoc_type` — render `<T as Iterator>::Item` resolved where rustdoc shows the projection.
- **Method reachability**: `Type::iterate_method_candidates` — "all methods callable on `Foo` incl. Deref chain and trait methods" as a search facet no doc tool has.
- **Const evaluation**: `Const::eval` / `EnumVariant::eval` → actual values (rustdoc: numeric-only, stringly).
- **Body-level analysis**: `Function.body` today comes from a separate tree-sitter pass (`syntax::ParsedBody`); RA has the real HIR bodies — callers/callees, unsafe-block usage → future `mentions`/`resolves_to` edges. (Keep tree-sitter for now; fold in later.)

### 6.3 Schema discipline

Every new IR field goes through the LinkML source of truth (`schema.yaml` → fork's rustgen/terminusdbgen → Rust + TDB `schema.json`, via `regen-schema.sh`) — additive fields, `Option`-al on the wire, no schema-version break. The parity phase changes **zero** IR shape; the exceed phase is additive-only. Coordinate with the `origin/clang` in-flight IR branch (additive `Type::Void`/`Width::W80`) — no structural conflicts expected.

---

## 7. Performance plan

Targets: match or beat today's per-package wall time on warm caches; ≤ 6 GiB RSS; the sandbox 15-min wall cap holds with margin.

1. **One load per workspace** (§2.2). Today: per-package `cargo rustdoc` = per-package full dep compilation. New: one `cargo metadata`, one build-script check run, one analysis. Multi-member repos get the biggest win; single-crate packages roughly break even cold (RA still type-checks deps' public surface, but skips codegen/metadata emission entirely).
2. **Prime, then walk**: `parallel_prime_caches(num_threads)` does the parallel type-checking; the subsequent walk is mostly cache hits. Walk itself: start single-threaded (it's read-only queries over primed caches — cheap); if profiling disagrees, parallelize per-module over salsa snapshots (`Analysis`-style), every worker in `catch_unwind` handling `Cancelled`.
3. **Skip what we don't lower**: never touch bodies during parity walk (no `Const::eval`, no diagnostics — RA's expensive paths are body inference and diagnostics, not signatures).
4. **`ProbeTier::Std` bounded**: blanket/auto probing is the only trait-solver load we add; it's per-ADT × curated-trait-set, salsa-cached.
5. **Drop redundant subprocesses**: the standalone `cargo metadata` (package.rs:321) and the `rustc --print sysroot` probe (package.rs:297) both fold into project_model's load. Set `extra_env` to keep everything offline.
6. **Memory ceiling**: profile cap 6 GiB (§2.1); the producer is one package-workspace at a time in the worker, memory released on process exit — no long-lived RA daemons in phase 1. (If the daemon plan later wants a resident analyzer with salsa incrementality across requests, `AnalysisHost::apply_change` supports it; RA has no on-disk salsa persistence, so resident is the only reuse shape. Explicit non-goal here.)
7. **Measure**: `analysis-stats`-style timing counters (load / build-scripts / prime / walk / probe) logged per package; fixture-repo benchmarks in CI comparing against recorded rustdoc-path baselines before it's deleted.

---

## 8. Testing & parity acceptance

1. **Golden parity harness (temporary, pre-removal)**: while both paths exist in the tree, a test target runs *both* producers over `tests/fixtures/rust/{regular, workspace, binary_workspace}` and diffs the serialized `Index` (normalized: entry order, alias-set order, `type_links` values excluded — the ID scheme legitimately changes). Acceptance: zero diffs on entry set, names, paths, visibilities, docs, signatures, generics, trait defs/impls, `implemented_protocols` (incl. the blanket-impl assertion), `Record.methods`, source-map keys.
2. **Existing tests keep passing unmodified in spirit**: `parse_rust_to_ir.rs`, `rust_compiler_e2e.rs`, `generate_blob.rs` — assertions unchanged except: (a) no more "requires nightly rustdoc on PATH" precondition — new precondition is `rust-src` + proc-macro server in the devshell sysroot; (b) `type_links` assertions re-golden to path-hash IDs.
3. **New fixture crates** for the fidelity edges: proc-macro-derived items (serde derive), `build.rs`-generated module (`include!(concat!(env!("OUT_DIR"), …))`), glob re-exports, `Pin<&mut Self>` receivers, HRTB bounds, `#[deprecated]`/`#[doc(hidden)]`/`#[non_exhaustive]`, blanket + auto + negative impls.
4. **Differential corpus run** (pre-removal gate): the top-N crates the registry pipeline already ingests, old vs new, scripted diff report; triage every regression as bug-or-accepted-delta before phase 5.
5. **Degradation tests**: proc-macro server disabled → warning surfaced, derive items absent, run completes; offline enforced.

---

## 9. Removal — the rustdoc path dies completely

After parity acceptance (and only then), one PR removes:

**Files/dirs**
- `workspace/rustdoc-driver/` (driver Cargo workspace + binary)
- `workspace/rust-lowering/` (thin wrapper crate)
- `scripts/vendor-librustdoc.sh`
- `build/third-party/patches/librustdoc/expose-json-crate.patch`
- `build/third-party/vendor/librustdoc/`, `build/third-party/vendor/librustdoc-json-types/` (if materialized)
- Old lowering files replaced wholesale by §2.3: `compile/rust/{package,context,item,function,generics,types}.rs` (git history keeps them; `traversal.rs` and the error taxonomy survive trimmed)

**Build config**
- `build/third-party/git.bzl`: `LIBRUSTDOC_COMMIT`, `RUSTDOC_TYPES_REV`, the `rustdoc-types-repo` GIT entry, `librustdoc-repo` entry + sha256 placeholder
- `workspace/compiler/BUCK`: `crate("rustdoc_types")` dep; the `local_only` rustdoc-driver genrule (`RUSTC_BOOTSTRAP=1 cargo build`)
- `build/third-party/defs.bzl`: the `env`/`rustc_flags`/`crate_root_suffix` `_git_crate` extensions stay (harmless, generic) — audit for other users first
- flake.nix: drop `rustc-dev` component + its librustdoc justification comment (~lines 151–158); add `rust-analyzer` component (§1.1)

**Code references**
- All `NUDOX_RUSTDOC_DRIVER` / `NUDOX_RUSTDOC_SYSTEM` / `NUDOX_RUSTDOC_OUT` / `RUSTDOC=` env plumbing (dies with package.rs)
- `profiles.rs:15` comment + `max_stdout` comment (§2.1)
- Docs/README mentions of the rustdoc JSON path

**Memory**: update `rustdoc-inprocess-plan` memory as superseded-and-removed; this plan file is the live reference.

---

## 10. Phases

| Phase | Deliverable | Exit criterion |
|---|---|---|
| **P0 — vendor & spike** | `ra_ap_*` closure in `registry.bzl`; flake `rust-analyzer` component; a `bin/` spike that loads fixture `regular/`, prints every `ModuleDef` with canonical path, docs, and one lowered signature | `buck2 build` green; spike output eyeballed against rustdoc JSON for the same fixture |
| **P1 — parity producer** | §2.3 modules complete; RA is default; `NUDOX_RUST_PRODUCER=rustdoc` keeps the temporary fallback (removed at P3) | ✅ existing 3 test files pass on RA path (default) |
| **P2 — parity proof** | golden diff harness + new fixtures + corpus differential | zero unaccepted diffs; perf counters ≤ rustdoc baseline ×1.25 cold, ≤ ×0.5 for multi-member workspaces |
| **P3 — removal** | §9 executed; flag removed; RA is the only path | tree contains no `rustdoc` reference in the Rust producer; CI green |
| **P4 — exceed** | §6.1 additive channels (deprecation, doc_links→`mentions`, cfg, ObjectSafe/Sealed, glob aliases) with LinkML schema additions | new fields emitted + landed in TDB; renderer consumes deprecation/doc_links |
| **P5 — exceed, graph tier** | §6.2: full external-dependency entries option, layout, normalization facets | behind config; registry-scale evaluation |

P1/P2 keep both paths alive — that's the only window the flag exists; it does not survive P3.

---

## 11. Risks & mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| `ra_ap` weekly churn breaks builds on bump | Med | exact `=` lockstep pin; quarterly scheduled bumps; cargo-modules is the canary (they absorb the same churn weekly) |
| proc-macro server missing/ABI drift → silent item loss | High | explicit degradation diagnostics (§3.2); devshell ships matched server; parity fixtures include a derive crate so CI catches it |
| HIR lifetime erasure | High (fidelity) | AST-first signature lowering (§4) — decided, not open |
| where-clauses not public in `ra_ap_hir` | Med | AST `ast::WhereClause` lowering (§4); if RA later exposes predicates, swap for less code |
| hir_ty panics on pathological items | Med | per-item `catch_unwind` (today's lenient drop policy); `Cancelled` → package retry |
| RAM on big workspaces (1–4 GiB, spikes beyond) | Med | worker-process isolation + 6 GiB cap; one workspace per process; no bodies/diagnostics in walk |
| blanket/auto probe cost on impl-heavy graphs (RA#17068) | Med | `ProbeTier` bounds it; salsa caches repeats; Graph tier opt-in |
| single-cfg analysis (one target/feature set per load) | Low | identical limitation to today's single `cargo rustdoc` run; per-target loads later if needed |
| build scripts must run (`OUT_DIR` codegen) | Low | same trust boundary as today, inside `ProducerProfile::Rust` sandbox; degradation diagnostic when disabled |
| `TraitAlias` unreachable via `ModuleDef` | Nil | today's path drops it too; parity unaffected; track upstream |
| `type_links` ID scheme change | Low | values were run-local hashes already; path-hash is strictly more stable; re-golden tests |

---

## Appendix A — current-state pointers (for the implementer)

- Entry: `workspace/compiler/compile/rust/mod.rs:44`; orchestration `package.rs:88–429` (cargo rustdoc, wrapper script, source map); lowering `context.rs` (`RustdocParser`, path BFS), `item.rs`, `function.rs:36` (receiver string-match), `generics.rs`, `types.rs:11` (visibility map) + `types.rs:83` (`id_to_number` hash).
- Caller: `generate/surface.rs:34–46` (note: the returned source map is currently *discarded* there — `(collected, _sources)`; keep producing it, but know downstream tree-sitter re-reads files independently).
- IR contract: `intermediate-representation/{entry,kind,ty,generics,function,parameter,record,protocols,module,primitives}.rs` — serde adjacent-tagged (`kind`/`value`, `type`/`value`); `Entry` variants and the Rust-produced `Type` subset must not change in P1–P3.
- Tests: `tests/parse_rust_to_ir.rs`, `tests/rust_compiler_e2e.rs`, `tests/generate_blob.rs`; fixtures `tests/fixtures/rust/{regular,workspace,binary_workspace}`.
- Sandbox: `workspace/util/sandbox/profiles.rs:15` (`ProducerProfile::Rust`, 3 GiB/900 s/15 min/512 pids/16 MiB stdout).
- Third-party: `build/third-party/registry.bzl` (generated — `gen-registry.py` → `nudox.commands.update`), `defs.bzl` (`_registry_crate`/`_git_crate`), `git.bzl` (librustdoc/rustdoc-types entries to delete).

## Appendix B — verified ra_ap API quick-reference (0.0.341)

- Load: `ra_ap_load_cargo::load_workspace_at(&Path, &CargoConfig, &LoadCargoConfig, &progress) -> Result<(RootDatabase, Vfs, Option<ProcMacroClient>)>`; `LoadCargoConfig { load_out_dirs_from_check, with_proc_macro_server: ProcMacroServerChoice::{Sysroot, Explicit(AbsPathBuf), None}, prefill_caches, .. }`; `CargoConfig { features, target, sysroot: Option<RustLibSource>, extra_env, set_test, no_deps, .. }`.
- Crates: `Crate::all(db)`, `::origin(db) -> CrateOrigin::{Lang, Rustc, Library, Local}`, `::root_module`, `::modules`, `::edition`, `::version`, `::cfg`, `::dependencies`, `::to_display_target`.
- Modules: `Module::declarations`, `::children`, `::impl_defs`, `::scope(db, Option<Module>)`, `::path_to_root`.
- Defs: `ModuleDef::{Module, Function, Adt(Struct|Union|Enum), EnumVariant, Const, Static, Trait, TypeAlias, BuiltinType, Macro}` (no TraitAlias).
- Function: `assoc_fn_params`, `params_without_self`, `self_param -> Option<SelfParam>` (`access -> Access::{Shared, Exclusive, Owned}`), `ret_type`, `async_ret_type`, `is_async/const/unsafe/varargs`, `has_body`.
- ADTs: `Struct::{fields, kind: StructKind::{Record,Tuple,Unit}, repr, ty}`, `Enum::{variants, repr}`, `EnumVariant::{fields, kind, eval}`, `Union::fields`, `Field::{name, ty, index}` (+ `HasVisibility`, `HasAttrs`).
- Trait: `items`, `is_auto`, `is_unsafe`, `direct_supertraits`/`all_supertraits` (no args — use AST for bound args), `dyn_compatibility`, `Trait::lang(db, krate, LangItem)`; `AssocItem::{Function, Const, TypeAlias}`.
- Impl: `all_in_crate`, `all_in_module`, `all_for_type` (blanket-excluding approximation!), `all_for_trait`; `trait_`, `trait_ref -> Option<TraitRef>` (`get_type_argument(idx)`), `self_ty`, `items`, `is_negative`, `is_unsafe`.
- Type (semantic): `as_adt(_with_args)`, `as_reference -> Option<(Type, Mutability)>` (no lifetime!), `as_raw_ptr`, `as_slice`, `as_array -> Option<(Type, usize)>`, `tuple_fields`, `as_callable -> Option<Callable>` (`params`, `return_type`), `as_dyn_trait`, `as_impl_traits`, `type_arguments`, `walk`, `impls_trait(db, &Trait, &[Type])`, `has_any_impl`, `normalize_trait_assoc_type`, `layout`, `autoderef`, `could_unify_with`.
- Generics: `GenericDef::{params, lifetime_params, type_or_const_params}`; `TypeParam::{name, default, trait_bounds /*inline only*/, is_implicit}`; `ConstParam::{ty, default(db, DisplayTarget) -> Option<String>}`; where-predicates **not public** → AST.
- Docs/attrs: `HasAttrs::{attrs -> AttrsWithOwner, hir_docs -> Option<&Docs>}`; `AttrsWithOwner::{is_deprecated, is_doc_hidden, cfgs -> Option<&CfgExpr>, is_unstable, is_non_exhaustive, doc_aliases}`; `Docs::into_docs -> String`; `hir::resolve_doc_path_on(db, def, link, Option<Namespace>, IsInnerDoc) -> Option<DocLinkDef::{ModuleDef, Field, SelfType}>`.
- Source: `HasSource::source -> Option<InFile<ast::X>>`; `Semantics::{original_range_opt -> Option<FileRange>, resolve_path, resolve_type}`; `Vfs::file_path(FileId) -> VfsPath`; `ide_db LineIndex` for line/col.
- Execution: `AnalysisHost::{with_database, analysis, apply_change}`; `Analysis::parallel_prime_caches(threads, cb) -> Cancellable<()>`; `Cancellable<T> = Result<T, Cancelled>`; cancellation = panic-unwind → `catch_unwind` mandatory around HIR walks.
- Display: `HirDisplay::display(db, DisplayTarget::from_crate(db, krate))`.

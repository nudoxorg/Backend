# OXC-PLAN — TypeScript producer: deno_doc → direct OXC

**Status: PROPOSED** (2026-07-11)

Replace the deno_doc/deno_graph/swc pipeline behind the TypeScript producer with
a hand-written extractor on the OXC crates, at **parity or better** on every
behavior the current lowering depends on, then **exceed** deno_doc's resolution
with a checker oracle deno_doc never had. All API facts below were verified
against docs.rs / crates.io / upstream source on 2026-07-11 by parallel research
agents; versions are pinned in §2.

---

## 0. Why

The current producer (`workspace/compiler/compile/typescript/`, ~2,750 lines)
lowers deno_doc's patched `Document { symbols: Vec<Symbol> }` model into
`ir::Entry`. It works, but:

1. **We carry a patched fork.** deno_doc 0.202.0 is built from a source tarball
   with `patches/deno-doc/deno_doc-0.202.0.patch` to expose the `params` module.
   Every bump means re-rolling the patch.
2. **The dependency tree is enormous for what we use.** deno_doc drags in
   deno_ast 0.53 (12 swc crates + string_cache/ast_node/sourcemap externals),
   deno_graph 0.110 (9 deno_* crates + futures/chrono/sha2/…), comrak,
   handlebars, wasm-bindgen — we use none of the HTML/markdown machinery.
   Removal: ~60 vendored crates. Addition (OXC set): ~40. Net shrink.
3. **deno_doc is a ceiling, not a floor.** It is purely syntactic (verified:
   zero checker/LSP usage in 0.202.0). Its inference is a fixed expression
   table; its type-ref resolution is name-based. Several of our lowerings are
   lossy *because* deno_doc's model is lossy (template-literal types → flat
   `string`, `TypeQuery` → bare name string, literal values dropped, indexed
   access stringified via `format!("{:?}")`).
4. **The IR direction is arena + `&'a`** (see [[ir-provenance-and-lifecycle]]).
   OXC is arena-native (`oxc_allocator`); swc is `Arc`-heavy. OXC is the
   natural substrate for the owned-string → arena migration.
5. **Strategic alignment.** oxc_resolver's `resolve_dts()` (11.19.0+) replaces
   our ~600 lines of hand-rolled entry-point/`.d.ts` discovery with the actual
   `ts.resolveModuleName` algorithm. And the checker story changed on
   2026-07-08: **TypeScript 7.0 GA shipped tsgo**, a dependency-free static Go
   binary containing the one true checker — the "exceed" tier is now a
   vendorable sidecar, matching our existing oracle precedent (Pyrefly for
   Python, go/types for Go, doclet for Java).

Non-goals: HTML rendering, markdown processing, remote (http) module loading —
we never used them. The surface is always a materialized local tree.

---

## 1. What deno_doc actually gives us (parity contract)

Audit of deno_doc 0.202.0 source. Everything in this section must be
reimplemented or consciously dropped; it is the acceptance checklist for
Phase 4.

### 1.1 Symbol graph & re-exports (src/parser.rs, src/visibility.rs)

- **`export * from 'mod'`** → recursively parses the target, wraps its symbols
  in a synthesized `NamespaceDef`, each element a `Reference` declaration
  (`ReferenceDef { target: Location }`).
- **`export { x } from 'mod'`** → resolved via `module_info.exports()` →
  `go_to_definitions()`; produces `Reference` declarations pointing at the
  origin. `resolve_dangling_reference()` follows chains segment-by-segment
  across module boundaries (including through `export *`) until a
  non-reference definition is reached.
- **`export default`** → `Symbol.is_default = true`; expressions and
  declarations both handled.
- **Visibility BFS** (`SymbolVisibility::build`): phase 1 collects root
  exports; phase 2 BFS-follows type dependencies of exported symbols. Types
  referenced by the public API but not exported land in
  `non_exported_public_ids` and ARE emitted even without `private: true`.
  (We run with `private: true`, which additionally includes non-exported and
  ambient symbols, tagged `DeclarationKind::Private`/`Declare`.)
- **Declaration merging** is purely name-grouping: multiple DocNodes with one
  name become one `Symbol` with several `Declaration`s. No semantic merge —
  our `pick_primary_declaration` + overload grouping already handles this
  shape.
- **`@ignore`** suppresses a symbol entirely (checked at `js_doc_for_range`);
  `@internal` is metadata only.

### 1.2 Syntactic type inference (src/ts_type.rs `infer_ts_type_from_expr`)

| Expression | Inferred |
|---|---|
| number/string/bool/bigint literal | keyword type; literal type when `is_const` |
| `null` literal | `null` |
| regex literal | `TypeRef("RegExp")` |
| template literal | `string`; template-literal type when `is_const` |
| array literal | union of element types; `any[]` fallback |
| arrow/function expr | full `FnOrConstructor` signature |
| `new X(...)` | `TypeRef("X")` (+ type args) |
| object literal | `TypeLiteral` with props/methods |
| `x as T` / `<T>x` / `satisfies T` | `T` |
| `x as const` | re-infer with `is_const = true` |
| ternary | union of branch inferences |
| `x!` | inner minus null/undefined |
| binary ops | comparisons→`boolean`, arithmetic→`number`, string `+`→`string` |
| `x++` | `number` |
| `await e` | unwrap `Promise<T>` of inner inference |
| calls, idents, member exprs | none |

Plus: **return-type fallback** — if a function has no annotation and no
`return <expr>` anywhere in its body (recursive statement walk), infer `void`
(sync) / `Promise<void>` (async). **Parameter defaults** (`AssignPat`) set
`optional: true` and backfill the type from the RHS inference. **Variable
docs** resolve type as: explicit annotation → referenced symbol's annotation →
initializer inference. **Destructured exports** fan out to one doc node per
binding with the member type extracted from the annotation.

### 1.3 JSDoc (src/js_doc.rs)

Hand-rolled parser, 33 tag variants (`Param {name, ts_type, optional,
default, doc}`, `Return`, `Template`, `Deprecated`, `Example`, `Throws`,
`TypeDef`, `Category`, `See`, `Since`, `Ignore`, `Internal`, `Module`, …).
Module doc = the first comment containing `@module` (NOT merely the first
comment). Leading `*` stripped by regex. `@description` overrides the body
text. We currently consume only `JsDoc::doc` (flat text) — the tags are
untapped headroom.

### 1.4 deno_graph contribution

`GraphKind::TypesOnly` walk from our roots with a `file://`-only loader;
follows `import type`, triple-slash `/// <reference>` directives, and
`@deno-types`/`@ts-self-types` pragmas (via `try_get_prefer_types()` types
redirects); `CapturingModuleAnalyzer` caches parses. Everything else
(entry-point discovery, declaration-root expansion) is already our code.

### 1.5 Known deno_doc limitations (our chance to exceed)

Not tracked upstream: `const enum`, type-parameter variance, static blocks,
accessor properties, constructor-body `this.x` properties, literal *values*
(only literal kinds), computed enum members, structured template-literal
types. All are first-class in oxc_ast.

---

## 2. Verified crate & tool inventory

| Component | Version | Notes |
|---|---|---|
| oxc monorepo crates | **0.139.0** (2026-07-06) | lockstep workspace version; pre-1.0, breaking changes every release — pin exactly. MSRV 1.95.0, edition 2024 |
| `oxc_parser`, `oxc_ast`, `oxc_ast_visit`, `oxc_semantic`, `oxc_span`, `oxc_allocator`, `oxc_syntax`, `oxc_diagnostics`, `oxc_isolated_declarations` + internals (`oxc_ast_macros` proc-macro, `oxc_data_structures`, `oxc_regular_expression`, `oxc_ecmascript`, `oxc_estree`, `oxc_str`, `oxc_cfg`*, `oxc_jsdoc`) | 0.139.0 | one source tarball serves all; **no build.rs anywhere** — generated AST code is checked in (`crates/oxc_ast/src/generated/`) |
| `oxc_index` | 5.0.0 | separate repo/crate (Boshen), needed by oxc_semantic |
| `oxc_resolver` | **11.23.0** (2026-07-02) | separate repo `oxc-project/oxc-resolver`, tags `vX.Y.Z`, MSRV 1.95 |
| tsgo (TypeScript 7) | **`typescript/v7.0.2`** (GA 2026-07-08) | native Go; static binary; `tsc`-compatible CLI + `--lsp -stdio`; Apache-2.0 |
| `async-lsp` (oxalica) | latest | LSP *client* over spawned stdio process (tower-lsp is server-only) |

\* `oxc_cfg`/petgraph only if we enable oxc_semantic's `cfg` feature — we
don't need control-flow graphs; skip.

External deps added (~23): `allocator-api2 =0.2.21` (exact pin), `bitflags
2.13`, `compact_str 0.9.1`, `cow-utils`, `hashbrown 0.17` (`inline-more`,
`allocator-api2`), `itertools 0.15`, `itoa`, `memchr`, `miette 3.0`,
`nonmax 0.5.5`, `num-bigint 0.5`, `num-traits`, `percent-encoding`, `phf 0.14`
(`macros`), `proc-macro2/quote/syn` (already vendored), `rustc-hash 2`,
`self_cell 1.2`, `seq-macro`, `smallvec`, `unicode-id-start`,
`dragonbox_ecma` (for `oxc_syntax/to_js_string`, needed by
isolated_declarations). For oxc_resolver: `dashmap 6`, `json-strip-comments`,
`simdutf8`, `simd-json 0.17`, `fast-glob`, `nodejs-built-in-modules`,
`indexmap` (already vendored), `once_cell`, `serde/serde_json` (already
vendored), `thiserror 2`, `tracing`.

Removed: deno_doc (git, patched) + 9 `deno_*` crates + 12 `swc_*` crates +
their unique externals (comrak, handlebars, wasm-bindgen, string_cache,
sourcemap, monch, …) ≈ 60 crates, ~45–50 net-unique.

Feature flags: `oxc_semantic` default features (NO `cfg`), **plus `jsdoc`**
(pulls `oxc_jsdoc`). `oxc_ast` WITHOUT `serialize`. `oxc_parser` defaults.
`oxc_allocator` needs `bitset` (required by semantic). `oxc_syntax` needs
`to_js_string` for isolated_declarations.

---

## 3. Target architecture

Three tiers, each independently shippable. Tier A alone reaches parity-plus;
Tiers B/C exceed.

```
                    ┌──────────────────────────────────────────────┐
                    │  TypescriptProducer (ProducerId "oxc/1")     │
                    └──────────────────────────────────────────────┘
  materialized tree ──► entry discovery (oxc_resolver::resolve_dts + resolve(root,"."))
                    ──► ModuleGraph build (worklist: oxc_parser + ModuleRecord
                        + triple-slash scan; edges resolved via resolver)
                    ──► per-module EXTRACT (oxc_semantic + oxc_jsdoc + inference
                        port) → ModuleFacts (owned, AST dropped)      [Tier A]
                    ──► cross-module LINK (re-export chase, visibility BFS,
                        symbol-accurate type_links) → ir::Index
        source pkgs ──► oxc_isolated_declarations normalizer; TS9xxx
                        diagnostics route exports to the oracle       [Tier B]
       opaque types ──► tsgo sidecar (sandboxed): batch --emitDeclarationOnly
                        + LSP hover/definition oracle; hover strings
                        re-parsed via oxc_parser into ir::Type        [Tier C]
```

### 3.1 Lifetime strategy (the load-bearing decision)

`Allocator` is `Send` but **not `Sync`**; every AST node borrows `'a` from its
file's arena. Holding all Programs alive for a whole-graph pass invites
self-referential pain. So: **two-pass, facts-first**.

- **Pass 1 (per module, arena-scoped):** parse → semantic → extract everything
  into an owned `ModuleFacts` (today's owned-string IR fragments + export/
  import tables + unresolved type-ref names + spans), then drop
  Program/Semantic/Allocator. One reusable `Allocator` per worker thread
  (`allocator.reset()` between files).
- **Pass 2 (cross-module, no ASTs):** operate purely on `ModuleFacts` —
  re-export resolution, visibility BFS, path/id assignment, `type_links`.

This mirrors the wave-ordered emit design and keeps the door open to
arena-fying `ir` later without coupling the two migrations.

### 3.2 New module layout

```
compile/typescript/
  mod.rs              — facade: generate_ir(root, name) (signature unchanged)
  producer.rs         — Producer impl; ID bumped "deno-doc/1" → "oxc/1"
  entry.rs            — entry discovery: resolver-first, hand-rolled fallbacks
  graph.rs            — ModuleGraph: worklist, parse, edges, external deps
  extract/
    mod.rs            — per-module driver: Program+Semantic → ModuleFacts
    facts.rs          — ModuleFacts / SymbolFacts / ExportTable types
    decl.rs           — declarations → Entry kinds (port of item.rs)
    func.rs           — functions/methods/overloads (port of function.rs)
    types.rs          — TSType → ir::Type (port of types.rs, §5 table)
    infer.rs          — expression inference (port of §1.2 table)
    jsdoc.rs          — oxc_jsdoc → doc text + tags (@ignore/@module/@deprecated)
  link.rs             — pass 2: re-exports, visibility BFS, type_links, paths
  oracle/             — Tier B/C (feature-gated initially)
    isolated.rs       — oxc_isolated_declarations wrapper + diagnostics triage
    tsgo.rs           — sidecar lifecycle, LSP client, hover re-parse
  error.rs            — new error taxonomy (keep public `Package`/`Parse` names)
```

`entry_point.rs` (repo-hint resolution) survives mostly intact; `package.rs`'s
tokio `block_on` machinery **dies** — the whole pipeline becomes synchronous
(oxc_resolver is sync; no more deno_graph futures). The thread-local runtime
hack in `package.rs:68-90` is deleted with it.

---

## 4. Phases

### Phase 0 — Vendoring + skeleton (no behavior change)

1. `git.bzl`: add `oxc-repo` http_archive (monorepo tarball; **pin by commit
   SHA of the 0.139.0 crates release** — the repo's version tags track apps
   (`oxlint_v*`), not crates, so record the SHA + workspace version in a
   comment), `oxc-index-repo` (5.0.0), `oxc-resolver-repo` (`v11.23.0`).
2. `BUCK` entries per crate; `oxc_ast_macros` as `rust_proc_macro`. No
   patches expected (everything we need is `pub`) — if something isn't,
   prefer an upstream PR over a patch; the lockstep releases are weekly.
3. Prove the toolchain: a throwaway test target that parses a fixture `.d.ts`
   (`Parser::new(&allocator, src, SourceType::d_ts()).parse()`), builds
   `SemanticBuilder::new().build(&program)`, and asserts on
   `ParserReturn.module_record.local_export_entries`.
4. Keep all deno crates in place — both stacks coexist through Phase 4.

Exit: `buck2 build` green with both stacks vendored.

### Phase 1 — Entry discovery + module graph (replaces deno_graph + half of package.rs)

1. **Entry discovery** (`entry.rs`):
   - Package root → `resolver.resolve(root, ".")` with
     `ResolveOptions { condition_names: ["types","import","node"],
     main_fields: ["types","typings","module","main"],
     extensions: [".d.ts",".ts",".tsx",".js",".json"],
     extension_alias: [(".js",[".d.ts",".ts",".js"]), (".mjs",[".d.mts",".mts"]),
     (".cjs",[".d.cts",".cts"])], builtin_modules: true, node_path: false }`.
   - Keep the existing conventional fallbacks (`mod.ts`, `index.ts`, …) and
     the `repo:` hint path from `entry_point.rs` for manifest-less trees.
   - Multi-root expansion (today's `documentation_roots_for_entry_point`):
     keep reading `exports` fan-out from package.json via serde_json (the
     resolver exposes `types()`/`typings()` but not arbitrary fields like
     `typesVersions` — thin manual read stays), but resolve each candidate
     through the resolver instead of the suffix-juggling candidate lists.
2. **Graph build** (`graph.rs`): worklist from roots. Per module:
   - Parse (`SourceType::from_path`, fall back to `d_ts()`), reuse
     thread-local `Allocator`.
   - Edges = `module_record.requested_modules` ∪ triple-slash
     `/// <reference path|types=...>` scanned from `program.comments`
     (replaces today's line-based `declaration_dependency_specifiers` string
     scraping with real comment tokens).
   - TypesOnly semantics: in `.d.ts` follow everything; in `.ts`/`.tsx`
     follow all static imports (deno_doc parses those modules anyway for
     value re-exports) — record `is_type` per edge for later pruning.
   - Resolve each specifier with `resolver.resolve_dts(containing_file,
     spec)`; classify errors: `NotFound` → external-package edge (recorded,
     not fatal — self-contained trees have no node_modules),
     `Builtin { .. }` → node builtin edge, `PackagePathNotExported` →
     diagnostic. Collect `ResolveContext.missing_dependencies` as witness
     material for the CAS layer.
3. Drop-in for `@deno-types` / `@ts-self-types`: scan leading comments on
   import statements for the pragmas and apply the redirect before
   resolution (small, documented function — deno_graph did this invisibly).

Exit: for a corpus of real packages, the set of reachable module files equals
deno_graph's (diff harness prints both sets).

### Phase 2 — Per-module extraction (replaces item.rs/function.rs/types.rs input side)

Port the existing lowering off `deno_doc::*` onto `oxc_ast` +
`oxc_semantic`, preserving output shape exactly. Key mechanical mappings in
§5. Structure:

1. **Symbol collection**: walk `program.body`; group declarations by exported
   name into `SymbolFacts { name, declarations: Vec<DeclFacts>, is_default }`
   (reproduces the patched deno_doc `Symbol` grouping our
   `pick_primary_declaration`/overload logic expects). Overload signatures are
   `Function { body: None }`; implementations have `body: Some(_)` — maps to
   today's `has_body`.
2. **Namespaces**: `TSModuleDeclaration` (kind Namespace/Module, nested via
   `TSModuleDeclarationBody`), `TSGlobalDeclaration` for `declare global`.
   Ambient `declare module "x"` (StringLiteral id) → module-level symbol,
   `DeclarationKind::Declare` equivalent.
3. **Classes**: `ClassElement` variants; `MethodDefinition { kind:
   Constructor|Method|Get|Set, accessibility, r#static, r#override, optional }`,
   abstract encoded in `MethodDefinitionType`; `PropertyDefinition` (readonly/
   optional/declare/definite); `TSIndexSignature`; **constructor parameter
   properties** detected via `FormalParameter { accessibility.is_some() ||
   readonly }` — match deno_doc: constructor-only, no synthetic class field.
4. **Interfaces**: `TSSignature` five variants → today's trait lowering
   (`__call`/`__index` synthesis unchanged).
5. **Enums**: `TSEnumDeclaration { r#const, members }` + member initializer
   inference; carry `r#const` forward (new capability, additive).
6. **Variables**: `VariableDeclaration { kind, declare }`; destructuring
   fan-out per §1.2; type via annotation → referenced-symbol → inference.
7. **Inference port** (`infer.rs`): implement §1.2 over `Expression` — the
   table is small and closed; port it verbatim including the recursive
   return-statement walk (`Function.body` statements) for the
   `void`/`Promise<void>` fallback.
8. **JSDoc** (`jsdoc.rs`): `SemanticBuilder` with the `jsdoc` cargo feature;
   `semantic.jsdoc().get_one_by_node(nodes, node)`. Port deno_doc's
   *semantics* on top of oxc's tag splitter: leading-`*` strip, `@ignore`
   suppression, `@internal` passthrough, `@module`-comment-as-module-doc,
   `@description` override. Map `@deprecated` → `ir` `deprecation` (today
   always `None` — free upgrade). oxc's `type_name_comment()` gives us
   `@param {T} name desc` parts for future parameter-doc enrichment.
9. **Module doc**: scan `program.comments` for the first `/**` block whose
   parsed tags contain `@module` (deno_doc semantics), not merely the first
   comment.

Exit: single-module goldens byte-identical (modulo ordering) to the deno path
for the existing test fixtures in `tests/parse_typescript_to_ir.rs`.

### Phase 3 — Cross-module linking (replaces parser.rs re-export machinery + fixes a known wart)

1. **Export tables** from `ModuleRecord`: `local_export_entries`,
   `indirect_export_entries` (`export {x} from`), `star_export_entries`,
   `exported_bindings`, each with `is_type`. `ImportEntry { module_request,
   import_name: Name|NamespaceObject|Default, local_name, is_type }` for
   import-then-export chains.
2. **Re-export resolution** (pass 2, over facts): follow
   `indirect`/`star` entries across `ModuleFacts` until a concrete
   `SymbolFacts` is found; cycle-guard by `(module, name)` set (deno_doc's
   `resolve_dangling_reference` equivalent). `export * from` synthesizes the
   flattened symbol list at the re-exporting module (match deno_doc's
   namespace-wrapping only where our current lowering observes it — audit
   shows we consume flattened `Document.symbols`, so flatten).
3. **Visibility BFS**: ports `SymbolVisibility` — roots' exports, then BFS
   over type references into `non_exported_public`; we run `private: true`
   today, so include non-exported symbols but *tag* them
   (`Visibility::Private`) exactly as `declaration_kind_to_visibility` does
   now.
4. **Symbol-accurate `type_links`** — the headline parity-exceeding fix in
   Tier A. Today `resolve_ir_type_to_entry_id` matches type names by
   string/suffix against a global map (`types.rs:371-386` — collision-prone).
   Replace: during extraction, each `TSTypeReference` resolves through
   `ident_ref.reference_id.get()` → `scoping.get_reference(ref_id)` →
   `reference.symbol_id()` → `scoping.symbol_declaration(symbol_id)`, giving
   the *declaring node* within the module; imports resolve through the
   ImportEntry to `(module, exported_name)`. Unresolved (truly global/
   ambient) refs keep the name-based fallback. `path_to_id` hashing and the
   `NudoxPath` layout stay unchanged so ids remain stable.
5. **Module naming**: keep `assign_unique_module_names` /
   `specifier_to_module_name` as-is (they operate on paths, not deno types) —
   specifiers become plain `PathBuf`s (no more `ModuleSpecifier` URLs; the
   `file://` round-trip disappears).

Exit: full-corpus IR diff vs the deno path (§7) — zero unexplained
regressions.

### Phase 4 — Cutover & removal

1. Producer flip: `ProducerId("deno-doc/1")` → `ProducerId("oxc/1")` (CAS
   entries keyed by producer id recompute — intended; same rule as the
   rust-analyzer migration). `ExecPlan::Library(WorkerLang::Typescript)`
   unchanged; sandbox profile stays LOW-tier static parser
   (`util/sandbox/profiles.rs` comment updates deno_doc → oxc).
2. Update stale references: `error.rs` doc comments, `compile/nix/mod.rs`
   comment, test module docs, `tests/parse_typescript_to_ir.rs` (note: its
   `use compiler::languages::typescript` import predates the `compile/` move —
   fix while touching).
3. Remove from `git.bzl`/`registry.bzl`/`BUCK`: deno_doc entry + patch file,
   `deno_ast`, `deno_graph`, `deno_media_type`, `deno_semver`,
   `deno_path_util`, `deno_terminal`, `deno_unsync`, `deno_error(+macro)`,
   all 12 `swc_*` crates, now-orphaned externals (run the orphan script from
   STREAMLINE). Verified: no consumer outside `compile/typescript` (other
   grep hits are comments).
4. Also deletable: the tokio current-thread runtime shim (`block_on`) and —
   if nothing else in the compiler needs it — the `tokio` dep from the
   TS path.

Exit: deno/swc gone; corpus green; PLANS.md updated.

### Phase 5 — Exceed, Tier B: in-process normalization + IR vocabulary upgrades

1. **`oxc_isolated_declarations`** (same 0.139.0 tarball):
   `IsolatedDeclarations::new(&allocator, IsolatedDeclarationsOptions {
   strip_internal: false }).build(&program)` → declaration-only `Program` +
   TS9xxx diagnostics. Use for **source packages** (`.ts` entry, no shipped
   `.d.ts`): extract from the normalized declaration AST instead of raw
   source — closes most inference gaps in-process at arena speed.
   Non-conforming exports produce per-node diagnostics: record them on the
   symbol (`diagnostic → "type requires checker"`) — this is the routing
   signal for Tier C.
2. **IR vocabulary upgrades** now expressible (each additive, schema-reviewed
   against [[types-refactor-plan]] before landing):
   - Literal types keep their **values** (oxc `TSLiteral` carries them;
     deno_doc dropped them) — needs an `ir::ty` literal-value slot.
   - `TSTemplateLiteralType` → structured (today: flat `string`).
   - `TSTypeQuery` (`typeof x`) → dedicated representation instead of a fake
     `TypeReference`.
   - `TSIndexedAccessType` → proper `QualifiedPath` (kill the
     `format!("{:?}")` name).
   - Named tuple members, `const` type params, `in`/`out` variance
     (`TSTypeParameter { r#in, r#out, r#const }` → `ir::generics::Variance` —
     today hardcoded `Invariant`).
   - `const enum`, static blocks, accessor properties, `export =`
     (`TSExportAssignment`), ambient module entries.

### Phase 6 — Exceed, Tier C: tsgo checker oracle (matches the Pyrefly/go-types/doclet precedent)

The only maintained complete TS checker is Microsoft's; as of TS 7.0 GA
(2026-07-08) it is a hermetic static Go binary. OXC will never build a
checker (official: backlog#158 closed "not planned"; type-awareness is
delegated to tsgolint's Go sidecar).

1. **Vendoring**: pin `typescript/v7.0.2`; the npm tarball wraps per-platform
   native binaries — vendor the binary per target under
   `build/third-party/tools/` (flake input or fetchurl; Apache-2.0). Wire as
   a sandboxed subprocess in the existing Cage machinery (LOW/MODERATE tier —
   it reads sources and tsconfig, no code execution; treat inputs as hostile
   data all the same, filesystem-scoped to the sealed input).
2. **Mode 1 — batch normalize** (cheap, first): for source packages where
   isolated-declarations diagnostics were non-empty, run
   `tsgo --declaration --emitDeclarationOnly --outDir <tmp>` inside the
   sandbox, then run the Tier-A extractor over the emitted `.d.ts`. This
   yields checker-inferred return types and evaluated public-surface types
   with **zero protocol work** (the API-Extractor pattern). Known GA gaps to
   corpus-test: tsgo emits nothing when type errors exist (#972), occasional
   declaration-transformer crashes (#1952) — on failure, fall back to Tier
   A/B output (producer honesty: record which tier produced the index in
   `AuxOutputs`).
3. **Mode 2 — LSP oracle** (targeted enrichment): spawn `tsgo --lsp -stdio`
   (single-dash `-stdio`; stdio only), drive with `async-lsp` client:
   `initialize` → `didOpen` per module → for each symbol the extractor marked
   opaque: `textDocument/hover` (printed type inside a ```typescript fence —
   strip, wrap as `type __T = <printed>;`, parse with oxc_parser, lower
   through the existing `TSType → ir::Type` path) and
   `textDocument/definition`/`typeDefinition` (symbol-accurate cross-module
   edges for `type_links` where syntax couldn't resolve). One warm process
   per package; batch all queries; kill with the sandbox. Caveats: checker
   truncates huge printed types; anonymous instantiations print structurally
   (usually what docs want).
4. **Upgrade path** (do not build now): TS 7.1+ promises a stable
   programmatic API; `tsgo --api` JSON-RPC exists but is undocumented; a
   custom Go shim via tsgolint's `//go:linkname` pattern is the
   deepest-access option and explicitly "not recommended for production" by
   oxc — revisit when hover strings prove insufficient.

### Phase 7 — Cleanup & records

Delete `oracle/` feature gates, update `README`s, memory files
(`go-java-producers-and-render`, this plan's memory), PLANS.md status, and
the sandbox profile comments. Consider promoting the facts-first two-pass
shape as the template for the other producers' arena migration.

---

## 5. Exact mapping tables

### 5.1 Parse & build (per module)

```rust
let allocator = Allocator::new();                       // reuse + reset() per thread
let st = SourceType::from_path(path).unwrap_or_else(|_| SourceType::d_ts());
let ret = Parser::new(&allocator, &source_text, st).parse();
// ret: { program, module_record, diagnostics, panicked, .. }
let sem = SemanticBuilder::new()
    .with_check_syntax_error(false)
    .with_build_nodes(true)
    .build(&ret.program);                               // jsdoc via cargo feature
let semantic = sem.semantic;                            // .scoping(), .nodes(), .jsdoc()
```

`panicked == true` or fatal diagnostics → per-module `Parse` error (today's
`DocParseFailed` equivalent), carry `miette` diagnostics into the error.

### 5.2 deno_doc declaration → oxc AST

| deno_doc (consumed today) | oxc_ast |
|---|---|
| `DeclarationDef::Function(FunctionDef)` | `Declaration::FunctionDeclaration(Function)`; `has_body` → `body.is_some()`; `is_async` → `r#async`; `is_generator` → `generator` |
| `DeclarationDef::Class(ClassDef)` | `Declaration::ClassDeclaration(Class)`; `extends` → `super_class` expr + `super_type_arguments`; `implements: Vec<TSClassImplements>`; `is_abstract` → `r#abstract` |
| `ClassMethodDef { accessibility, is_static, optional, is_abstract, is_override, kind }` | `MethodDefinition { accessibility: Option<TSAccessibility>, r#static, optional, r#override, kind: Constructor\|Method\|Get\|Set }`; abstract via `MethodDefinitionType::TSAbstractMethodDefinition` |
| `ClassPropertyDef { readonly, optional, is_static, decorators, ts_type }` | `PropertyDefinition { readonly, optional, r#static, decorators, type_annotation }` |
| ctor param props (`ClassConstructorParamDef`) | `FormalParameter { accessibility, readonly, r#override }` — param property iff `accessibility.is_some() \|\| readonly` |
| `DeclarationDef::Interface(InterfaceDef)` | `TSInterfaceDeclaration { extends: Vec<TSInterfaceHeritage>, body: TSInterfaceBody }`; members via `TSSignature::{TSPropertySignature, TSMethodSignature, TSCallSignatureDeclaration, TSConstructSignatureDeclaration, TSIndexSignature}` |
| `DeclarationDef::Enum(EnumDef)` | `TSEnumDeclaration { r#const, body }`; `TSEnumMember { id, initializer }` |
| `DeclarationDef::TypeAlias` | `TSTypeAliasDeclaration { type_parameters, type_annotation }` |
| `DeclarationDef::Namespace(NamespaceDef)` | `TSModuleDeclaration { id: Identifier\|StringLiteral, kind: Module\|Namespace, body }`, nested via `TSModuleDeclarationBody`; `declare global` → `TSGlobalDeclaration` |
| `DeclarationDef::Variable(VariableDef)` | `VariableDeclaration { kind: Var\|Let\|Const\|Using\|AwaitUsing, declare }` |
| `DeclarationDef::Reference(ReferenceDef)` | synthesized in `link.rs` from ModuleRecord entries (no AST node) |
| `DeclarationKind::{Export,Private,Declare}` | derived: exported (ModuleRecord/`exported_bindings`) / not / `declare` flag or ambient context |
| `ParamDef/ParamPatternDef::{Identifier,Rest,Assign,Array,Object}` | `FormalParameter.pattern: BindingPattern` kinds `{BindingIdentifier, ArrayPattern, ObjectPattern, AssignmentPattern}` + `BindingRestElement`; `optional` on FormalParameter; rest type from `TSTypeAnnotation` on the rest element |
| `TsTypeParamDef { name, constraint, default }` | `TSTypeParameter { name, constraint, default, r#in, r#out, r#const }` (variance/const are NEW) |
| `TruePlusMinus` | `TSMappedTypeModifierOperator::{True, Plus, Minus}` |
| `Accessibility::{Public,Protected,Private}` | `TSAccessibility::{Public, Protected, Private}` |
| `VarDeclKind::Const` | `VariableDeclarationKind::Const` |
| decorators `decorator.to_string()` | `Decorator.span().source_text(&src)` (or expression walk for name+args like deno's `DecoratorDef`) |

### 5.3 `TsTypeDefKind` → `TSType` → `ir::Type` (all 37 oxc variants accounted)

| oxc `TSType` variant | today's lowering (keep) / change |
|---|---|
| `TSStringKeyword` | `Primitive(String)` |
| `TSNumberKeyword` | `Primitive(Float(W64))` |
| `TSBooleanKeyword` | `Primitive(Bool)` |
| `TSBigIntKeyword` | `Primitive(Int(W128))` |
| `TSNullKeyword`, `TSUndefinedKeyword`, `TSVoidKeyword` | `Tuple([])` (unit) |
| `TSNeverKeyword` | `Never` |
| `TSAnyKeyword`, `TSUnknownKeyword` | `Any` |
| `TSObjectKeyword` | `TypeReference("object")` |
| `TSSymbolKeyword` | `TypeReference("Symbol")` |
| `TSIntrinsicKeyword` | `TypeReference("intrinsic")` (new; deno folded into keyword string) |
| `TSThisType` | `SelfType` |
| `TSTypeReference { type_name, type_arguments }` | `TypeReference { identifier, generic_args }`; identifier from `TSTypeName::{IdentifierReference, QualifiedName, ThisExpression}`; **resolve symbol id here for type_links (§ Phase 3.4)** |
| `TSUnionType` / `TSIntersectionType` | `Union` / `Intersection` |
| `TSArrayType` | `Slice` |
| `TSTupleType { element_types }` | `Tuple`; `TSTupleElement::{TSOptionalType, TSRestType, TSNamedTupleMember}` — rest → `Variadic`, optional → today's passthrough, named member label → (Tier B: keep label) |
| `TSFunctionType` / `TSConstructorType` | `FunctionPointer` (note `return_type` NOT optional here, unlike deno); `TSConstructorType.r#abstract` new |
| `TSParenthesizedType` | unwrap (or set `ParseOptions { preserve_parens: false }` and never see it — choose the option, drop the arm) |
| `TSTypeOperatorType { operator: keyof\|unique\|readonly }` | `TypeOperator` |
| `TSTypeQuery { expr_name, type_arguments }` | today `TypeReference(name)`; Tier B: structured typeof |
| `TSConditionalType` | `Conditional { check, extends, true, false }` |
| `TSInferType { type_parameter }` | `Infer` |
| `TSIndexedAccessType { object_type, index_type }` | `QualifiedPath` — replace the `format!("{:?}")` index name with a real lowering (Tier B) |
| `TSTypeLiteral` (members: `TSSignature`) | `RecordLiteral(type_literal_record)` — same five member kinds |
| `TSMappedType { key, constraint, name_type, type_annotation, optional, readonly }` | `Mapped { parameter: key.name, source_type: constraint, .. }` — note constraint is non-optional in oxc (deno's `MissingMappedTypeConstraint` error path disappears) |
| `TSImportType { source, qualifier, type_arguments }` | `TypeReference` (today drops the specifier; Tier B: keep `source` for cross-package links) |
| `TSTypePredicate { parameter_name: Identifier\|This, asserts, type_annotation }` | `Predicate { asserts, subject, r#type }` |
| `TSLiteralType { literal: Boolean\|Numeric\|BigInt\|String\|Template\|UnaryExpression }` | today kind→primitive only; Tier B: carry value; `UnaryExpression` covers negative literals (deno had none) |
| `TSTemplateLiteralType { quasis, types }` | today `Primitive(String)`; Tier B: structured |
| `TSNamedTupleMember` | label + element (inside tuple lowering) |
| `JSDocNullableType`, `JSDocNonNullableType`, `JSDocUnknownType` | map to inner type / `Any` (deno_doc: `Unsupported` → `Infer`) |

There is **no `Optional`/`Rest` top-level variant** in oxc `TSType` (they are
`TSTupleElement`-only) and **no `Unsupported`** — every deno arm has a total
mapping; the `Unsupported → Infer` escape hatch dies.

### 5.4 JSDoc access

```rust
let finder = semantic.jsdoc();                         // JSDocFinder (feature "jsdoc")
if let Some(doc) = finder.get_one_by_node(semantic.nodes(), node) {
    let text = doc.comment();                          // description part
    for tag in doc.tags() {
        match tag.kind.parsed() /* e.g. "param" */ {
            "deprecated" => { let msg = tag.comment(); /* → ir deprecation */ }
            "param"      => { let (ty, name, c) = tag.type_name_comment(); }
            "ignore"     => { /* suppress symbol (deno parity) */ }
            "module"     => { /* module doc marker */ }
            _ => {}
        }
    }
}
```

Attachment: comments carry `attached_to: u32` (start of the attachee token)
and `is_jsdoc()`; the finder only attaches to declaration-ish nodes — matches
deno_doc's placement rules closely; goldens will catch drift.

---

## 6. Producer & sandbox integration

- `Producer::ID` → `ProducerId("oxc/1")`. `plan()` still returns
  `ExecPlan::Library(WorkerLang::Typescript)`; the worker's in-process lower
  calls the same `TypescriptPackage::generate_ir` facade — the worker crate
  needs no structural change, just the recompiled library.
- `ThreatTier::Hostile` classification stays (input trees are hostile data
  even if the parser is pure Rust).
- Tier C adds a subprocess *inside* the cage: tsgo binary + sealed source
  tree, no network, tmpfs outDir. This slots into the SealedInput/Cage design
  from DAEMON-PLAN (producers that shell out already exist: rustdoc/doclet).
- `AuxOutputs`: record extraction tier per package (`syntactic` /
  `isolated-decls` / `tsgo-emit` / `tsgo-lsp`) for observability and
  reproducibility triage.

## 7. Testing & acceptance

1. **Golden fixtures**: existing `tests/parse_typescript_to_ir.rs` must pass
   unchanged (entry discovery + lowering shape).
2. **Differential corpus harness** (new, `tests/` + a bin): run `deno-doc/1`
   and `oxc/1` over a pinned corpus (top-~100 npm packages by download +
   pathological picks: `typescript` itself for giant `.d.ts`, `zod` for
   conditional-type stress, `@types/node` for ambient modules, `rxjs` for
   overloads, a JSR-style `.ts`-only package). Diff `ir::Index` entry sets:
   report added/removed/changed per entry kind. Acceptance: zero unexplained
   removals; every diff classified as {bug, deno-limitation-fixed,
   intentional-drop}.
3. **Unit tests per port**: inference table (§1.2 row-per-test), JSDoc
   semantics (`@ignore`/`@module`/`@description`), re-export chains
   (fixtures: `export *` diamond, import-then-export, default re-export,
   dangling), param patterns, ctor param props.
4. **Graph equivalence**: reachable-file-set diff vs deno_graph on the corpus
   (Phase 1 exit).
5. **Perf smoke**: parse+extract wall time per corpus package — expect a
   large win (oxc parser is ~3× swc, no URL round-trips, no tokio); record
   baseline for the perf memory.

## 8. Risks & mitigations

| Risk | Mitigation |
|---|---|
| oxc pre-1.0 breaks APIs every release | exact-pin 0.139.0 (single tarball); upgrade deliberately; no ranges in git.bzl |
| oxc monorepo crates tag ambiguity (apps tags ≠ crate versions) | pin commit SHA; document workspace version in git.bzl comment |
| Re-export semantics drift (deno's namespace-wrapping of `export *`) | corpus diff + dedicated fixtures; match observed output, not deno internals |
| `Allocator: !Sync` lifetime leakage into shared state | facts-first two-pass architecture (§3.1) makes it structurally impossible |
| oxc_semantic has no declaration merging | we already merge by name-grouping (SymbolFacts); flags (`SymbolFlags::{Interface, ValueModule, …}`) available if needed |
| JSDoc attachment rules differ subtly from swc's `get_leading` | goldens + fallback: manual scan of `program.comments` by `attached_to` when finder misses |
| tsgo emit gaps at GA (#972 no-emit-on-error, #1952 crashes) | Tier C is enrichment-only; every failure falls back to Tier A/B; tier recorded in AuxOutputs |
| `preserve_parens` default true adds a TSType variant deno never had | set `preserve_parens: false` in ParseOptions (decided) |
| CAS invalidation on producer-id bump | intended; same policy as rust-analyzer migration |
| windows_future rebuild blocking compiler builds (known env issue) | unrelated but will gate verification; use the /tmp-cargo trick if Buck is wedged |

## 9. Open questions (decide during Phase 2/3, none block Phase 0/1)

1. Do we keep emitting synthesized namespace entries for `export * from`
   (deno shape) or flatten into the re-exporting module? (Current lowering
   consumes flattened symbols; recommendation: flatten, record provenance.)
2. `export =` / CommonJS interop: deno_doc largely ignores it; oxc gives us
   `TSExportAssignment` — lower as default-export equivalent or dedicated IR?
3. How much of the JSDoc tag vocabulary to lift into IR now (`@param` docs →
   `LiteralParameter.description`, `@throws`, `@example`) vs. Tier B?
4. Literal values in `ir::ty` — coordinate with [[types-refactor-plan]]
   schema-bump rules before adding fields.
5. Should the differential harness live permanently in CI (deno side removed
   in Phase 4 makes it a one-shot tool — archive results in the repo?).

## 10. Execution order & size estimate

Phases 0–1: ~2 sessions (vendoring is mechanical; graph.rs is ~300 lines).
Phase 2: the big one — port of ~1,800 lines of lowering, mostly mechanical
renames per §5 tables; highly parallelizable (decl.rs / func.rs / types.rs /
infer.rs / jsdoc.rs are independent against the facts model).
Phase 3: ~400 lines of genuinely new logic (link.rs) — the only part with
design risk; fixtures first.
Phase 4: deletion + corpus sign-off.
Phases 5–6: independent follow-ons, each valuable standalone; Tier C reuses
the sidecar plumbing patterns from the Pyrefly worker.

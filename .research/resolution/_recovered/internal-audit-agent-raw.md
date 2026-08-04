---
recovered_from: claude-session 17cc2883-6b32-44ea-abc5-c1c9f07d0bf2
agent_id: afaa7790f40fb2b50
agent_description: Internal resolution architecture audit
status: complete_before_parent_session_limit
recovered_at: 2026-07-16
note: Raw agent output before Grok verification pass. Prefer 01-internal-architecture-audit.md.
---

# Resolve Arbitrary Codebase Tree-sitter Parse → IR Symbols: Ground Truth Report

---

## 1. The Tree-sitter Layer

### 1.1 LanguageSpec Abstraction

**Location:** `/Users/philocalyst/Projects/Backend/workspace/compiler/treesitter/spec.rs`

The `LanguageSpec` trait (spec.rs:153–180) is the language-agnostic boundary. It has four methods:

```rust
pub trait LanguageSpec: Send + Sync {
    fn definitions(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition>;
    fn imports(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding>;
    fn references(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference>;
    fn module_path(&self, rel: &Path, layout: &PackageLayout) -> Vec<String>;
    fn extract(&self, tree, src, rel, layout) -> Extraction { /* default */ }
}
```

The `Extraction` output (spec.rs:135–146) bundles all four. There is no `.scm` query machinery — all implementations are explicit cursor-walks in Rust post-processing, by design (spec.rs:152): "The structured output — parent indices, chain assembly, receiver shapes — needs Rust post-processing regardless."

### 1.2 Per-Language Extractor Coverage

**Rust** (`rust.rs`):
- **Definitions:** `function_item`, `struct_item`, `enum_item`, `union_item`, `trait_item`, `impl_item`, `mod_item`, `type_item`, `macro_definition`. Impl blocks use the `type` field as name; methods inside impls get `DefKind::Method`.
- **Imports:** Recursively expands `use` trees, handling braces, `as`-renames, globs, and nested `scoped_use_tree`. All paths emitted as `ImportSource::Internal` (external classification deferred to resolver, rust.rs:463).
- **References:** Full `scoped_identifier`/`scoped_type_identifier` chains as one `RawReference`. Method calls via `field_expression`. `ReceiverShape::SelfRef` for `self`/`Self`. Macro invocations. Type identifiers. Deduplication via covered ranges. **Macro body internals skipped** (`K_TOKEN_TREE` tracking).
- **Module path:** `src/` stripped, `lib.rs`/`main.rs`/`mod.rs` → parent dir, crate name as root segment.

**TypeScript** (`typescript.rs`):
- **Definitions:** `function_declaration`, `method_definition`, `class_declaration`, `interface_declaration`, `type_alias_declaration`, `enum_declaration`. Scope stack for parent tracking.
- **Imports:** ESM `import_statement`, re-export barrels (`export { x } from './y'`), CJS `require()` (best-effort, string literals only). Classifies specifier as `Internal` (starts with `.`/`/`) vs `External` (bare). Scoped packages (`@scope/pkg`) handled. Namespace import `* as ns` is NOT a Glob — bound prefix.
- **References:** `call_expression`, `member_expression` chains, `type_identifier`, `nested_type_identifier`, JSX `<Component />` (tsx grammar, uppercase only).
- **Deferred (doc comment, typescript.rs:27–32):** Deep re-export chains (>1 hop), method call target resolution.

**Python** (`python.rs`):
- **Definitions:** `function_definition`, `class_definition` only. Lambda anonymous → skipped.
- **Imports:** `import_statement`, `import_from_statement`, aliased imports, relative imports (leading dots → leading `""` segments in `Internal(["", "a", "b"])`, python.rs:13–17), wildcard imports → `Glob`.
- **References:** `call` nodes, `attribute` nodes, `type` annotations.
- **Out of scope (python.rs:19–24):** `getattr()` dynamic access, `__all__` re-exports, lambda definitions, comprehension scopes.

**Go** (`go.rs`):
- **Definitions:** `function_declaration`, `method_declaration`, `type_spec`. Methods synthesize a parent `DefKind::Type` frame from receiver type (pointer `*` stripped) so resolver can join `Type.Method`.
- **Imports:** `import_spec` with local alias and import path. Dot-import `import . "pkg"` → `Glob(ImportPrefix { dependency: Some(path), path: [] })`.
- **References:** `selector_expression` chains (`pkg.Func` → FunctionCall if head is package; `val.Method` → MethodCall otherwise with receiver). Bare identifier callee → FunctionCall. Type identifiers → TypeReference.
- **Oracle-deferred (go.rs:36–45, 407–408):** Dot-import name resolution, struct embedding promoted-method calls.

**Java** (`java.rs`):
- **Definitions:** `class_declaration`, `interface_declaration`, `enum_declaration`, `record_declaration`, `method_declaration`, `constructor_declaration`. Nesting via scope stack.
- **Imports:** `import_declaration` — single-type, on-demand glob, static single, static glob.
- **References:** Method invocations (qualified `obj.m()` → MethodCall, bare `m()` → MethodCall with `SelfRef`). Java's `m()` inside an instance method approximated as `SelfRef` (java.rs:26–31).
- **Not synthesized:** `java.lang.*` implicit import (java.rs:49–51).

**C#** (`csharp.rs`):
- **Definitions:** `class_declaration`, `interface_declaration`, `struct_declaration`, `enum_declaration`, `record_declaration`, `record_struct_declaration`, `method_declaration`, `constructor_declaration`, `property_declaration`. Namespace frames extracted.
- **Imports:** `using_directive` — ordinary, static, alias forms. Namespace maps to `ImportSource::Internal`.
- **Module path:** Prefers namespace from parse tree (`namespace Foo.Bar;` / block form); falls back to directory segments.
- **References:** `invocation_expression`, `member_access_expression`, `object_creation_expression`.

**Nix** (`nix.rs`):
- **Definitions:** Attrpath bindings in `binding_set` (attrset, rec-attrset, let, let-attrset). Multi-segment paths collapse to last segment (nix.rs:35–37). DefKind from value type (lambda → Function, attrset → Module, else → Type).
- **Imports:** `inherit a b` and `inherit (src) a b` sites.
- **References:** function applications, attribute selections, bare variable uses.
- **Limitation (nix.rs:48–52):** `with pkgs; <body>` — bare names inside `with` scope are emitted as `VariableUse` regardless; resolver matches only if unique in index.

### 1.3 What Is Emitted

`Extraction` contains:
- `definitions: Vec<RawDefinition>` — name, `DefKind`, `name_span: Range<usize>`, `body_span: Range<usize>`, `parent: Option<usize>`
- `imports: Vec<ImportBinding>` — `local: String`, `source: ImportSource`, `span`
- `references: Vec<RawReference>` — `segments: Vec<String>`, `span`, `kind: ReferenceKind`, `receiver: Option<ReceiverShape>`
- `module_path: Vec<String>`

The legacy `parse_and_extract` function (mod.rs:517–583) produces a different output: an s-expression string, `snippet_span`, and `Vec<ReferenceEntry>` (name as string, Debug-rendered kind, byte span). These use the flat `classify_for` classifiers (not `LanguageSpec`) and feed the snippet/embedding path, not the resolution path.

---

## 2. The Occurrence Contract

**Location:** `/Users/philocalyst/Projects/Backend/workspace/ir/syntax/occurrence.rs`

```rust
pub struct Occurrence {
    pub span: Range<usize>,        // byte range (tree-sitter native)
    pub target: NudoxPath,         // fully-qualified target
    pub kind: ReferenceKind,       // FunctionCall|MethodCall|TypeReference|VariableUse|MacroInvocation|FieldAccess|Import
    pub role: Role,                // Definition | Reference
    pub enclosing: Option<NudoxPath>, // innermost enclosing def's FQ path, or None at module top
    pub anchored: bool,            // enclosing matched an index entry
    pub confidence: Confidence,    // Syntactic < Suffix < Index < Import < Oracle
}
```

`ReferenceKind` values (types.rs:13–21): `FunctionCall=0`, `MethodCall=1`, `TypeReference=2`, `VariableUse=3`, `MacroInvocation=4`, `FieldAccess=5`, `Import=6`.

`Confidence` ordering (occurrence.rs:34–47): `Syntactic < Suffix < Index < Import < Oracle`. The `Oracle` tier is defined but **never produced in the codebase** — it is reserved for future oracle-resolved occurrences.

**Symbol naming scheme:** `NudoxPath` is the identifier, not a SCIP-like symbol string. For graph IRIs, the Linker mints `Symbol/{lang}%2F{pkg}%2F{fq}` (link.rs:69–74). There is no SCIP syntax in this codebase — the IR uses `NudoxPath::Local(PathBuf)` where the PathBuf often carries `::` separators embedded as single components.

**Serialization:** `OccurrenceSet` derives `Serialize`/`Deserialize` (feature-gated, `ir/Cargo.toml`). However, the `BlobManifest` today does **not** include an `occurrences_ref` field — this is an explicit gap (04-ir-audit.md:22, librarification-plan.md:150). The `GeneratedPackage` struct in `generate/mod.rs:56–73` carries `occurrences: OccurrenceSet` as a runtime field, but it is not persisted to the blob store. The `BlobInfo::assemble` (blob_info.rs:30–47) hashes only source archive files for `snapshot`.

**Storage gap (confirmed):** Occurrences exist in memory during generation and flow into `from_ir::project` → `project_references` → `GraphCorpus.references`, but are **not stored in the blob store** today. The LIBRARIFICATION-PLAN.md notes `occurrences_ref` as a planned addition (GD-10 / S3.2).

---

## 3. The Resolve Engine + SymbolTable

### 3.1 SymbolTable

**Location:** `/Users/philocalyst/Projects/Backend/workspace/compiler/graph/symtab.rs`

Three indexes:
- `exact: HashMap<String, NudoxPath>` — every FQ spelling (canonical + alias), `::` joined
- `suffix: HashMap<String, Option<NudoxPath>>` — last segment → path, `None` on collision
- `by_module: HashMap<Vec<String>, HashSet<String>>` — module path → leaf names

Built from `Index` via `build(index)`. Segment splitting handles embedded `::` and `.` (path_segments, symtab.rs:27–51): `"a.b::c"` → `["a","b","c"]`. All producer alias sets are indexed.

### 3.2 Resolution Engine

**Location:** `/Users/philocalyst/Projects/Backend/workspace/compiler/generate/resolve.rs`

Entry point: `resolve(lang, files: &[(PathBuf, Extraction)], index: &Index) -> OccurrenceSet`

The ladder per reference (resolve.rs:129–197):
1. **SelfRef/ClassRef receiver:** try enclosing type + leaf → `Confidence::Index`
2. **Rooted absolute marker** (Rust only: `crate`/`self`/`super`, folded against module path) → `Confidence::Index`
3. **Import table** (head segment lookup in extraction.imports) → `Confidence::Import` for both Internal (folded base + tail) and External (emits `NudoxPath::External`)
4. **Lexical scope walk** (innermost enclosing prefix → file root) → `Confidence::Index`
5. **Package-wide exact/alias** → `Confidence::Index`
6. **Unique suffix** → `Confidence::Suffix`
7. **Unresolved** → tallied only, not emitted

Attribution: `enclosing_def` finds the innermost `body_span` containing the reference span (resolve.rs:270–285). Definition occurrences always emitted with their name_span.

`RESOLVER_VERSION = "occ-v1"` — bump invalidates cached occurrence CAS entries (resolve.rs:33).

Language-specific: `fold_root_marker` handles Rust `crate`/`self`/`super` (resolve.rs:310–330). All other languages route through the import table only.

Python relative imports: leading `""` segments in Internal base pop module path levels (resolve.rs:294–304).

### 3.3 Reference Projection

**Location:** `/Users/philocalyst/Projects/Backend/workspace/compiler/graph/from_ir.rs:1149–1185`

The `project_references` function applies the graph assertion policy:
- Only `Role::Reference` occurrences
- `anchored == true` (enclosing is an index entry)
- `confidence >= Confidence::Index`
- kind ∈ {FunctionCall, MethodCall, TypeReference, MacroInvocation, Import}
- `enclosing` is `Some`

`VariableUse`, `FieldAccess`, and `Confidence::Suffix` are deliberately excluded from graph assertion. Each qualifying occurrence becomes a `model::Reference` with `source`, `target`, `kind`, `span_start`, `span_end`, `file`, `confidence`.

### 3.4 Test Coverage

`references_e2e.rs` contains:
- `rust_call_resolves_and_attributes_to_enclosing` — the "dream in miniature": two-function Rust source, resolves call at `Confidence::Index`, attributes to enclosing
- Per-language extraction smokes for all 7 languages (Rust, Go, Python, TypeScript, Java, Nix) — check definitions, imports, references emitted
- `render_example_highlights_call_site` — enclosing-function snippet + highlight

`references_live.rs` contains two live tests against real fixtures:
- `call_graph_is_reified_as_reference_edges` — generates `references` fixture, asserts `hello → yo` and `report → hello` as graph Reference edges
- `cross_module_references_resolve_across_modules` — `wsrefs` workspace, `run → add` at Index, `u → add` at Import

No CSharp extractor smoke test in `references_e2e.rs` (only extraction tests in csharp.rs itself).

---

## 4. The IR

### 4.1 Symbol Shape

`Symbol<T>` (kind.rs:96–115):
- `name: String`, `path: NudoxPath`, `aliases: Option<HashSet<Vec<String>>>`, `visibility: Visibility`, `documentation: Option<String>`, `deprecation: Option<Deprecation>`, `doc_links: Option<HashMap<String, NudoxPath>>`, `inner: T`

`Entry` variants (kind.rs:150–204): Module, RecordType, Info, UnionType, TraitDef, TraitImpl, SumType, Function, TypeAlias, Constant, Variable, Macro, PrimitiveType, Field, Event.

**No SymbolId in IR** (explicitly noted in 04-ir-audit.md:796). The `symbol_id: Option<String>` on `model::Symbol` is always `None` in compiler projection (from_ir.rs:1130).

### 4.2 Identity Scheme

Graph IRI: `Symbol/{lang}%2F{pkg}%2F{fq}` where `fq` is `::` joined segments. This is version-agnostic. `PackageVersion.declares` tracks membership.

NudoxPath inside the IR: `Local(PathBuf)` where the PathBuf frequently has a single component embedding `::` separators (e.g., `"calc::yo"` or `"crate::Type::method"`). `path_segments` re-splits on `::` and `.`.

### 4.3 Aliases

Each `Symbol` can have `aliases: Option<HashSet<Vec<String>>>` — sets of alternate segment lists. The `SymbolTable` indexes all of them in `exact`. Producers mint aliases for language-specific re-export patterns (e.g., Go registers `Type.Method` and `Type::Method` spellings).

### 4.4 Cross-Package / External Symbols

`NudoxPath::External { path: PathBuf, dependency: String }` — the dependency name identifies the package. In the `SymbolTable`, external imports produce `Confidence::Import` occurrences with `NudoxPath::External`. At graph assertion time, the `Linker` resolves them through `resolve_path` or mints stubs under `~extern` when not in the index. There is no cross-package `SymbolTable` (only one package index is built per `resolve` call).

---

## 5. Pipeline & Storage

### 5.1 Generation Pipeline

`generate_with(ctx, input)` (generate/mod.rs:96–138):
1. Compute `JobKey` from producer_version ‖ toolchain ‖ source hash ‖ dep lock
2. `surface::build` → `Index` (oracle compile)
3. `cst::extract` → `CstSet` (legacy flat classifiers, snippet extraction)
4. `occurrences::build(input, &surface)` → `OccurrenceSet` (LanguageSpec + resolve)
5. `source_archive::build` → per-file BLAKE3s
6. `BlobInfo::assemble` (archive only → snapshot hash)

`occurrences::build` (generate/occurrences.rs:64–125): walks source tree, grammar-dispatches, calls `spec.extract`, calls `resolve`. The input is `PackageInput` which has a `root: PathBuf` — a materialized source directory. **Currently always a published package**.

### 5.2 No Consumer-Codebase Notion

There is **no existing concept** in the pipeline of indexing code that is NOT a published package. `PackageInput.coordinates` has `origin: RegistryOrigin` and `name: PackageName` — these are registry-facing. The `occurrences::build` function uses `is_skipped_dir` to skip `.git`, `target`, `node_modules`, `vendor`, etc. — but those are all still within a single package root.

The pipeline is designed around the unit of a package (crate, npm package, PyPI package). Consumer codebases — downstream repos, example directories, test harnesses — have no first-class representation.

### 5.3 Storage Topology

- **OccurrenceSet in memory:** Exists in `GeneratedPackage.occurrences` during generation. Passed to `from_ir::project` for graph Reference extraction. Not persisted to blob store (`occurrences_ref` is missing from `BlobManifest`).
- **CstSet:** Legacy, persisted as `references_ref` in blob manifest. Targets are placeholder `Local(name)` — not FQ-resolved.
- **Graph Corpus:** Projected to JSON-LD, emitted via `linked_data::emit` waves to Terminus. `GraphStore` trait (registry) serves `get_occurrences` (symbols that hold an item in their signature) and `get_references` (callers/users) via WOQL queries.
- **No "occurrence search" layer** exists in `server/search/` — only text (tantivy), semantic (qdrant), and graph (Terminus) stores. The `get_occurrences` in `GraphStore` operates on TerminusDB `Occurrence`-kind `Relation` edges (graph-asserted occurrence claims), not the raw `OccurrenceSet` blob.

---

## 6. Gap Analysis

### 6.1 Confirmed Deferred Items from Memory Notes

**"blob occurrences_ref + Phase 4 query deferred":**
- `occurrences_ref` in `BlobManifest` is planned (LIBRARIFICATION-PLAN.md GD-10 / S3.2) but **absent today**. The `OccurrenceSet` is computed but not persisted to the blob store.
- "Phase 4 query" refers to serving occurrence queries from the blob store. Currently, `get_occurrences` queries TerminusDB for `Occurrence`-kind edges — not the `OccurrenceSet` blob. The graph assertion policy is strict (`confidence >= Index`, `anchored`), so only a subset of occurrences reaches the graph.

**"Go-method-anchoring + external-import gaps":**
- Go method anchoring (go.rs:407–408): references from dot-imports and promoted/embedded-type method calls are **oracle-deferred**, not emitted. The extractor knows about dot-imports (`alias_text == "."` → skipped in pkg_names, go.rs:424–427) and explicitly does not emit references for them.
- External import gaps: All languages route external references through the import table at `Confidence::Import`, minting `NudoxPath::External`. But since the resolver only has one package's `SymbolTable` at a time, external symbols always stay as External paths — they resolve to stubs `~extern` in the graph unless the dependency's package has been separately indexed and its symbols are somehow joined.

### 6.2 Cross-File Scope / Name Resolution

The resolver (resolve.rs) operates on a single package's `SymbolTable`. Within a package:
- Cross-module references resolve via the lexical scope walk + import table at `Confidence::Import` or `Confidence::Index`.
- Multi-module resolution works (tested in `cross_module_references_resolve_across_modules`).

**Missing for consumer codebases:**
- The consumer's import table points to library symbols. The library's symbols are in a *different* `SymbolTable`. No mechanism exists to combine `SymbolTable`s from multiple packages for resolution.
- Wildcard imports from external packages: `use serde::*` or Python `from numpy import *` — the Glob source emits `ImportSource::Glob(ImportPrefix { dependency: Some("serde"), ... })`. The resolver's Glob handling (resolve.rs:226–233) only tries `fold_internal_base(base) + chain` against the package-local SymbolTable — it **cannot** search the dependency's members because they are not in this package's index.
- Re-export chains: TypeScript multi-hop barrel exports are explicitly deferred (typescript.rs:28–29). Only one-hop re-exports are captured.

### 6.3 Receiver-Type Inference for Method Calls

A method call `x.foo()` emits a `RawReference { segments: ["foo"], kind: MethodCall, receiver: Some(Expr("x")) }`. The resolver step 1 (SelfRef/ClassRef) only handles `self`/`cls` — it tries the enclosing type's scope. For an arbitrary `Expr("x")` receiver, the resolver falls through to lexical scope, package-wide exact, and suffix matching.

**For consumer code resolution, this is a hard blocker.** `obj.serialize()` emits `receiver: Some(Expr("obj"))`. Without knowing that `obj: MySerializer`, you cannot bind this to the right `serialize` method. The resolver's suffix fallback would pick the unique suffix match, but if multiple types have a `serialize` method, suffix is `None` (ambiguous). Treesitter cannot infer the type of `obj` without the oracle.

This affects all OO/imperative languages: TypeScript (member_expression), Python (attribute), Java (method_invocation), C# (invocation_expression), Go (selector_expression where operand is not a package identifier).

### 6.4 Overload Disambiguation

Java and C# have overloaded methods with the same name but different parameter types. The `LanguageSpec` extractors emit one `RawReference` per call site with just the method name. The `SymbolTable` suffix index marks names with multiple entries as `None` (ambiguous). Overload resolution is entirely oracle-tier.

### 6.5 Generics / Instantiation

Generic method calls like `parse::<Foo>()` (Rust turbofish) or `box.get()` for `Box<T>` — treesitter sees the surface syntax. Type arguments are captured in references for scoped paths, but the resolver does not try to instantiate or unify them. The SymbolTable has no notion of generic instantiation — all entries are their declaration-site paths.

### 6.6 Language-Specific Import Semantic Differences Not Handled

| Language | Gap |
|---|---|
| Rust | Glob imports (`use foo::*`) — `ImportSource::Glob` in the extractor, resolver tries `fold_internal_base + chain` against local SymbolTable only. External dependency globs unresolvable. |
| Python | `__all__` re-exports unresolvable (python.rs:20). `getattr(obj, "name")` dynamic access skipped. `importlib.import_module` skipped. |
| TypeScript | Deep re-export chains (>1 hop) deferred (typescript.rs:28). Dynamic `require()` with non-literal paths skipped. |
| Go | Dot-imports (`import . "pkg"`) oracle-deferred (go.rs:36–38). Embedding promoted methods oracle-deferred (go.rs:43–45). |
| Java | `java.lang.*` implicit import not synthesized (java.rs:49–51). `import static pkg.Class.*` brings static members into scope — the resolver has no mechanism to expand this without the dependency's SymbolTable. Overloads (ambiguous suffix). |
| C# | `using System;` brings ALL of `System.*` into scope as a glob — the resolver has no external SymbolTable for this. Extension methods require type knowledge to route. |
| Nix | `with pkgs; <body>` — all names inside `with` body are unqualifiable statically (nix.rs:48–52). `import ./module.nix` paths need evaluation. |

### 6.7 Symbol Identity Mismatches (Treesitter vs IR)

The treesitter extractors derive module paths from file system positions (e.g., Rust: `src/foo/bar.rs` → `["mycrate","foo","bar"]`). IR paths come from oracle producers (rust-analyzer, OXC, etc.) and may use different conventions:
- Rust RA: single-segment `NudoxPath::Local("crate::Type::method")`. SymbolTable `path_segments` re-splits. These must align with the extractor's module_path derivation — the test `rust_call_resolves_and_attributes_to_enclosing` validates this alignment for the standard `src/` layout.
- Non-standard layouts: modules declared with `#[path = "custom.rs"]`, procedural macro-generated modules, re-exports, platform-specific modules — treesitter's module_path derivation will mismatch.
- TypeScript: `module_path` drops `src/` and collapses `index.ts` (typescript.rs:550–588). OXC's module naming may differ.
- Java: module_path from `src/main/java/` prefix stripping (java.rs:38–44) — alternative layouts (Bazel, non-Maven) will mismatch.

### 6.8 Cross-Package / Consumer Codebase Architecture Gap

There is **no pipeline entry point** for indexing a consumer codebase against a registry of already-indexed packages. Specifically:
- `occurrences::build` takes `PackageInput` (one package) and resolves against `Index` (that package's IR). There is no "multi-package SymbolTable" or "registry-wide SymbolTable."
- External symbols always land in `NudoxPath::External { dependency, path }` — never get resolved to actual `NudoxPath::Local` entries of the dependency package.
- No server-side occurrence query API exists for "find all usages of this symbol ID across all indexed packages." `get_references(item)` in `GraphStore` returns callers that were graph-asserted — meaning only within-package references at `confidence >= Index`, anchored. Cross-package calls would require the consumer package to have been indexed AND for its External occurrences targeting the library to have been wired to the library's SymbolIds.

### 6.9 `Confidence::Oracle` Tier

Defined in occurrence.rs:44 as `/// Resolved by the language's semantic oracle`. **Never produced.** No code in the codebase emits `Confidence::Oracle` — it is a reserved slot for future oracle-layer enrichment.

---

## 7. Feasibility Levers

### Architecture (a): Pure treesitter + heuristic name resolution against SymbolTable

**What exists:** This is exactly what the current pipeline does for intra-package resolution. The full stack is implemented and tested. The `SymbolTable.resolve_suffix` is the heuristic fallback.

**Code assets reusable as-is:**
- `LanguageSpec` extractors for all 7 languages (complete)
- `resolve::resolve()` engine with 7-rung ladder (complete)
- `SymbolTable::build` (complete)
- `Occurrence`/`OccurrenceSet` contract (complete)
- Graph assertion + `project_references` (complete)

**What would need to change for consumer codebases:**
- `PackageInput` concept extended: needs a "root codebase directory" unit that is not a published package. The `occurrences::build` function is very close — it takes any `root: PathBuf` plus a `PackageLayout { package }` name. Stripping `PackageCoordinates` requirement is minor.
- Multi-package `SymbolTable`: currently one package index → one table. For consumer code resolving against library symbols, you'd need to build a merged SymbolTable from N package indices and route `ImportSource::External { dependency }` through the matching package's sub-table.
- Glob expansion across packages: `use serde::*` where `serde`'s index is available — needs `by_module` traversal across the dependency's SymbolTable.
- Suffix collisions increase dramatically across all packages combined — suffix confidence becomes less useful.

**Hard limits of this architecture:**
- Receiver type inference: impossible without oracle. Method calls on typed variables stay at suffix or unresolved.
- Overload disambiguation: impossible.
- Dynamic imports, `with` expressions, `getattr()`: impossible.

### Architecture (b): Treesitter + scope-graph/stack-graph declarative binding rules

No stack-graph / scope-graph infrastructure exists in the codebase. No `tree-sitter-graph` or `tree-sitter-stack-graphs` crates. This would require building the binding rules per language from scratch. The `LanguageSpec` cursor-walks could be seen as a manual partial implementation of this, but without the declarative per-language scope graphs that enable complete name binding across files.

Benefit: would handle imports, re-exports, and lexical shadowing more completely than the current ladder. Still cannot handle receiver-type inference.

**No existing code assets to reuse** from this codebase specifically.

### Architecture (c): Reuse per-language oracles on consumer code, map to IR symbol IDs

The per-language oracles (rust-analyzer, OXC/deno_doc, Pyrefly, go/types, javadoc, Roslyn, snix) can resolve consumer code to fully-qualified names with type information. This would give `Confidence::Oracle` occurrences.

**Existing assets:**
- Producer infrastructure in `compile/` — rust-analyzer, OXC, Pyrefly, go/types, javadoc, Roslyn all have existing invocation infrastructure. These currently compile the **package under documentation**. They can also compile consumer code — rust-analyzer with a Cargo.toml referencing the library, for example.
- `ForgeRuntime` / sandboxing in `compile/producer/` for safe invocation.
- The oracle's resolved FQ names would need to be mapped to `NudoxPath` paths from the library's `Index`. The `SymbolTable` exact lookup handles this: `SymbolTable::resolve_exact(&fq)` returns the `NudoxPath`.
- The `Confidence::Oracle` tier is already defined but unused.

**What's missing:**
- No "compile consumer code with dependency awareness" pipeline path. The current `surface::build` passes the package's root; for consumer code you'd need the consumer's root plus the library packages as dependencies.
- No oracle result → `Occurrence` mapping layer. RA/OXC etc. emit their own reference/definition models; a mapping stage would translate their outputs to `RawReference`/`RawDefinition` and then the existing `resolve` engine, or bypass `resolve` entirely and emit `Confidence::Oracle` occurrences directly.
- The `Extraction` boundary (LanguageSpec output) is designed for the treesitter layer. Oracle-layer mapping could go directly to `Occurrence` or through an oracle-specific extraction path.

**Most practical reuse path for architecture (c):** Rust consumer code — run rust-analyzer on the consumer Cargo workspace (which depends on the library). RA's file reference output maps naturally to `NudoxPath::External` targets. The existing `compile/rust/ra/` infrastructure processes RA output already. For TypeScript, OXC already resolves types and imports. For Go, `go/types` in the oracle does full type checking including method receiver binding. These oracles handle receiver inference, overloads, and generics — exactly what treesitter cannot.

---

## 8. Hard Blockers vs. Soft Gaps

### Hard Blockers (impossible with treesitter alone for consumer-codebase resolution)

1. **Receiver-type inference.** `x.method()` cannot be bound to a specific symbol without knowing `x`'s type. Affects TypeScript, Python, Java, C#, Go, Rust (non-`self` receivers). **No treesitter workaround.**

2. **Overload disambiguation.** Java and C# overloaded methods share a name. Treesitter emits one reference per call; the resolver has no parameter type information to pick the right overload. **No treesitter workaround.**

3. **Cross-package SymbolTable.** The `SymbolTable` is per-package. Consumer code's `use serde::Serialize` can be resolved to `NudoxPath::External` but never mapped to the *actual* IR `NudoxPath` of `serde`'s `Serialize` entry without loading serde's `Index` into a merged table. **Architectural gap — no merged table exists.**

4. **`with` scope (Nix) / `getattr` (Python) / dynamic imports (JS).** These bring names into scope without static evidence. **Fundamentally non-static.**

5. **OccurrenceSet not persisted.** Cannot query or serve "usages of symbol X across all packages" because the blob store does not contain occurrences. **Pipeline gap.**

### Soft Gaps (solvable with existing infrastructure)

6. **`occurrences_ref` in BlobManifest.** Planned (GD-10). Would enable blob-based occurrence queries. Requires adding the field, an emit path, and a query serving path.

7. **Multi-package SymbolTable.** The `SymbolTable::build` function accepts any `&Index`. A merged table could be built by inserting all packages' entries with their External-namespace paths. The suffix collision rate would increase but exact/alias resolution stays accurate.

8. **Glob expansion across packages.** Once a dependency's SymbolTable is available, `by_module` lookup resolves `use dep::*` by iterating the dependency's module members. The data structure already exists; the multi-package composition does not.

9. **Go dot-import + embedded method.** Explicitly oracle-deferred in code (go.rs:36–45). Could be handled with architecture (c) using `go/types`.

10. **Java `java.lang.*` implicit import.** A synthetic `ImportBinding` for `java.lang.*` could be injected in `java::imports` as a hardcoded Glob. The Java standard library's `Index` (if indexed) would then resolve names through the merged table.

11. **Python relative import counting.** Currently `n` leading `""` segments in `Internal(...)`. Works correctly for standard packages; edge cases with namespace packages and `sys.path` manipulation are unhandled but are oracle-tier problems.

12. **CSharp `using Namespace;` resolution.** Emits `ImportSource::Internal(["Namespace"])`. Without the library's index, this stays at suffix or unresolved. With the library's merged SymbolTable, exact resolution would work.

13. **TypeScript multi-hop barrel exports.** The extractor captures one-hop re-exports. Multi-hop chains require following the re-export graph across files, which is implementable with a file-graph pass over the package's `Extraction` set.

14. **No consumer-codebase `PackageInput` variant.** Minor: `occurrences::build` could accept any `(root, package_name, language, index)` tuple. The current `PackageInput` enforces registry coordinates, but the underlying `LanguageSpec` + `resolve` machinery is coordinate-agnostic.

15. **`Confidence::Oracle` never emitted.** Defined but unused. Wiring oracle outputs to `Confidence::Oracle` occurrences would enable a clear semantic tier above the treesitter ladder, compatible with all existing consumers.
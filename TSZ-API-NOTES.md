# tsz-core API Reference (for OXC-adjacent integration)

**Pinned target:** `tsz-core` v0.1.9 (crates.io, org `tsz-org` / repo owner `mohsen1` / Mohsen Azimi), Apache-2.0, released 2026-02-23.
**Crate description (crates.io):** "Core TypeScript compiler and type checker library." Internal project name "Project Zang." Tracks TypeScript 6.0.0-dev; aims to be a drop-in `tsc` replacement.

**Our two use cases:**
- (A) Type-check a materialized TS package and read **checker-inferred** types (return types, object shapes, `Promise<T>`) that a syntactic pass cannot recover.
- (B) Emit `.d.ts` declarations.
- We will **not** use the LSP (`tsz-lsp` / `tsz_core::lsp`).

---

## ⚠️ TOP RISK — read this first: version skew & the native-vs-WASM API split

Research surfaced **two different snapshots** of this project, and they disagree on the exact API. Do not conflate them.

| Aspect | **v0.1.9 (our pin, Feb 2026)** | **current `main` branch (mid-2026, `tsz-org/tsz`)** |
|---|---|---|
| Conformance (type checker) | **62.7%** (7,884/12,574) | claimed 100% (marketing/site) |
| Declaration emit | **39.0%** (776/1,989) | claimed 100% |
| Repo owner shown on crates.io | `mohsenazimi/tsz` (mirror `mohsen1`) | `tsz-org/tsz` |
| High-level TS-compatible API | **not present as a separate `tsz-wasm` crate at 0.1.9** (verify) | `tsz-wasm` crate: `TsProgram`, `TsTypeChecker`, `TsType`, `TsSourceFile`, `TsDiagnostic` |
| `TypeData` shape | older/flatter layout | De Bruijn-indexed (`BoundParameter(u32)`, `Recursive(u32)`) |

**Consequence for us:** The clean, TS-compiler-mirroring API (`TsProgram.getTypeChecker() -> TsTypeChecker`, `typeToString`, `getTypeAtLocation`, `emitFile`) belongs to the **`tsz-wasm` crate on `main`**, NOT verified to exist in the `tsz-core` **0.1.9** artifact we pinned. In 0.1.9 the entry point is the lower-level **`WasmProgram`** (from `create_program()`), whose `check_all()` returns **an opaque JSON string**, not typed handles.

**Biggest single ambiguity:** whether v0.1.9 exposes a native (non-WASM) path to the checker's *inferred type of a symbol/node as a printable string*. See §3/§4 — the printable-type path we can confirm (`TypeFormatter::format`) lives in `tsz-solver` and is native-usable, but wiring a checked program to a `TypeId` at 0.1.9 requires going through `CheckerState`/`TypeCache` internals rather than a tidy `getTypeAtLocation`. **Validate against the actual 0.1.9 rustdoc/source before committing the integration design.** If we can bump to a newer `tsz-wasm`, the ergonomics improve dramatically.

Everything below is annotated with **[0.1.9]** (confirmed/likely in our pin) vs **[main]** (newer branch; may not exist at 0.1.9).

---

## Crate & dependency facts [0.1.9]

- Runtime deps (23): `anyhow`, `bitflags`, `dashmap`, `ena`, `fixedbitset`, `indexmap`, `memchr`, `once_cell`, `rayon`, `rustc-hash`, `serde`, `serde-wasm-bindgen`, `serde_json`, `smallvec`, `tracing`, **`wasm-bindgen`**, plus internal `tsz-*` crates.
- Dev deps: `criterion`, `once_cell`, `tempfile`.
- Doc coverage: 78.95% (630/798 items). Source ~8.47 MB.
- Workspace crates (all 0.1.9): `tsz-common`, `tsz-scanner`, `tsz-parser`, `tsz-binder`, `tsz-solver`, `tsz-checker`, `tsz-emitter`, `tsz-lowering`, `tsz-lsp`, `tsz-core`.
- **Feature flags:** not explicitly enumerated on docs.rs. `wasm-bindgen` is a hard dependency (not obviously behind a feature). The declaration emitter is noted as gated on a **`dts`** feature on `main` — confirm whether that gate exists at 0.1.9. **Action: check `[features]` in the 0.1.9 `Cargo.toml` before assuming native-only builds compile without a JS host shim.**

### Module re-export aliases (top-level `tsz_core::…`)
```
tsz_core::scanner  → tsz_scanner
tsz_core::binder   → tsz_binder
tsz_core::checker  → tsz_checker
tsz_core::solver   → tsz_solver
tsz_core::lsp      → tsz_lsp
```

### Public modules of `tsz_core`
`char_codes, comments, common, config, context, declaration_emitter, diagnostics, emitter, enums, exports, imports, interner, lib_loader, limits, lowering, module_graph, module_resolution_debug, module_resolver, parallel, parser, printer, safe_slice, scanner_impl, source_file, source_map, source_writer, span, syntax, transforms`

---

## 1. Program / multi-file setup

### 1a. `WasmProgram` — the confirmed 0.1.9 entry point [0.1.9]

Path: `tsz_core::WasmProgram` (re-exported from `crate::api::wasm::program::WasmProgram`).
Factory: `tsz_core::create_program()` (wasm-bindgen `js_name = createProgram`).

Fields (illustrative of what it holds — private):
```rust
#[wasm_bindgen]
pub struct WasmProgram {
    files: Vec<(String, String)>,               // (file_name, source_text)
    merged: Option<MergedProgram>,
    bind_results: Option<Vec<BindResult>>,
    lib_files: Vec<(String, String)>,
    compiler_options: CompilerOptions,
    check_all_cache: Option<String>,
    diagnostic_codes_cache: Option<String>,
    all_diagnostic_codes_cache: Option<Vec<u32>>,
}
```

Methods (Rust name → JS `js_name`):
```rust
pub fn new() -> Self                                              // constructor
pub fn add_file(&mut self, file_name: String, source_text: String)     // addFile
pub fn add_lib_file(&mut self, file_name: String, source_text: String) // addLibFile
pub fn clear(&mut self)                                           // clear
pub fn get_file_count(&self) -> usize                            // getFileCount
pub fn set_compiler_options(&mut self, options_json: &str) -> Result<(), JsValue>  // setCompilerOptions
pub fn check_all(&mut self) -> String                            // checkAll → JSON Vec<FileCheckResultJson>
pub fn get_diagnostic_codes(&mut self) -> String                 // getDiagnosticCodes → JSON {file → [codes]}
pub fn get_all_diagnostic_codes(&mut self) -> Vec<u32>           // getAllDiagnosticCodes
```

**How to add files:** `add_file(path, text)` and `add_lib_file(path, text)` for stdlib `.d.ts`.
**How to configure options:** `set_compiler_options(json)` — takes a **JSON string** deserialized into `CompilerOptions` (see §1c).
**How to trigger bind + check across files:** `check_all()` runs the full parallel parse→bind→merge→check pipeline and returns a **JSON string** of per-file results. Binding/checking is implicit inside `check_all()`.

**Is `WasmProgram` usable natively?** It is annotated `#[wasm_bindgen]`, but the *methods* take/return plain Rust (`String`, `usize`, `Vec<u32>`, `Result<(), JsValue>`). The only non-native leak is `JsValue` in `set_compiler_options`'s error type. So it is *mostly* callable from native Rust **if** the crate compiles for a native target (the `wasm-bindgen` dep must not force `wasm32`). The `JsValue` return type is the friction point — you may need the WASM target or a shim. **Confirm native compilation of `tsz-core` 0.1.9 as the first integration spike.**

**Key limitation for use case (A):** `check_all()` returns JSON, not typed `TypeId` handles. To read a *specific inferred type as a string*, `WasmProgram` alone is insufficient at 0.1.9 — you must drop to `CheckerState` + `TypeFormatter` (see §3/§4), OR upgrade to `TsProgram`/`TsTypeChecker` (see §1b).

### 1b. `TsProgram` / `TsTypeChecker` — the ergonomic path [main only — verify presence at 0.1.9]

Lives in the **`tsz-wasm`** crate (`crates/tsz-wasm/src/wasm_api/program.rs`) on `main`. This mirrors the real TypeScript compiler API and is what we *want*, if available.
```rust
#[wasm_bindgen]
pub struct TsProgram { /* files, lib_files, merged, type_interner: Arc<TypeInterner>, options, type_checker, source_files */ }

pub fn new() -> Self                                   // constructor
pub fn add_source_file(name, text)                     // addSourceFile
pub fn add_lib_file(name, text)                        // addLibFile
pub fn set_compiler_options(json)                      // setCompilerOptions
pub fn get_type_checker(&self) -> TsTypeChecker        // getTypeChecker   ← gateway to inferred types
pub fn emit_json(&self) -> String                      // emitJson
pub fn emit_file(&self, name) -> String                // emitFile         ← per-file JS/.d.ts emit
pub fn get_syntactic_diagnostics(name?) -> Vec<TsDiagnostic>   // getSyntacticDiagnostics
pub fn get_semantic_diagnostics(name?) -> Vec<TsDiagnostic>    // getSemanticDiagnostics
pub fn get_pre_emit_diagnostics() -> Vec<TsDiagnostic>        // getPreEmitDiagnostics
pub fn get_compiler_options_json(&self) -> String     // getCompilerOptionsJson
pub fn dispose()                                       // dispose
// UTF-16 offset helpers for Monaco: byte_offset_to_utf16(...), byte_length_to_utf16(...)
```
> **If `tsz-wasm`/`TsProgram` is NOT in our 0.1.9 dependency closure, this whole path is unavailable and we fall back to §1a + §3/§4.** This is the single most important thing to verify before design.

### 1c. Compiler options [0.1.9]

Path: `tsz_core::api::wasm::compiler_options::CompilerOptions` (re-exported as `WasmCompilerOptions`). Deserialized from JSON.
```rust
#[derive(Deserialize, Clone, Debug, Default)]
pub struct CompilerOptions {
    pub strict: Option<bool>,
    pub no_implicit_any: Option<bool>,
    pub strict_null_checks: Option<bool>,
    pub strict_function_types: Option<bool>,
    pub strict_bind_call_apply: Option<bool>,
    pub strict_property_initialization: Option<bool>,
    pub no_implicit_returns: Option<bool>,
    pub no_implicit_this: Option<bool>,
    pub use_unknown_in_catch_variables: Option<bool>,
    pub strict_builtin_iterator_return: Option<bool>,
    pub target: Option<u32>,   // ScriptTarget as u32
    pub module: Option<u32>,   // ModuleKind as u32
    pub downlevel_iteration: Option<bool>,
    pub exact_optional_property_types: Option<bool>,
    pub no_lib: Option<bool>,
    pub no_unchecked_indexed_access: Option<bool>,
    pub sound_mode: Option<bool>,
}
pub fn to_checker_options(&self) -> CheckerOptions
```
> **`lib` is NOT a field here.** Library `.d.ts` files are supplied explicitly via `add_lib_file(name, text)` / `add_lib_file` — you materialize and feed the lib set yourself (see `lib_loader`, `resolve_lib_files()`, `core_lib_name_for_target()`, `default_lib_name_for_target()` in `tsz_core`). `no_lib: true` suppresses defaults. **Module resolution** is configured via `target`/`module` here plus the `module_resolver` module (`ModuleResolver`, `ResolvedModule`, `ModuleResolutionKind`, `PackageType`); there is also full `tsconfig` support: `load_tsconfig()`, `parse_tsconfig()`, `resolve_compiler_options()`, types `TsConfig`, `ParsedTsConfig`, `CompilerOptions`, `ResolvedCompilerOptions`, `PathMapping`.

`ScriptTarget` (enum, values): `ES3=0, ES5=1, ES2015=2, … ES2025=12, ESNext=99, JSON=100`.
`ModuleKind`: `None=0, CommonJS=1, AMD=2, UMD=3, System=4, ES2015=5, ES2020=6, ES2022=7, ESNext=99, Node16=100, Node18=101, Node20=102, NodeNext=199, Preserve=200`.

### 1d. Native pipeline building blocks (`tsz_core::parallel`) [0.1.9]

If you bypass `WasmProgram` and drive the pipeline directly in native Rust:
```
parse_files_parallel()     // Rayon parse many files
parse_and_bind_parallel()  // parse + bind
merge_bind_results() -> MergedProgram
check_files_parallel() / check_functions_parallel()
compile_files()            // full: parse → bind → merge
ensure_rayon_global_pool() // MUST call to set correct stack size (deep recursion)
load_lib_files_for_binding()
```
Result structs: `ParseResult`, `BindResult`, `BoundFile`, `MergedProgram`, `CheckResult`, `FileCheckResult`, `FunctionCheckResult`, plus `*Stats`. **This is the most native-friendly route** and is likely how we should drive checking if we need typed handles at 0.1.9.

---

## 2. Parsing

Factory: `tsz_core::create_parser(file_name: String, source_text: String) -> Parser` (wasm `js_name = createParser`).
Scanner factory: `tsz_core::create_scanner(text: String, skip_trivia: bool) -> ScannerState` (`createScanner`).

Types:
- `tsz_core::Parser` — re-export of `crate::api::wasm::parser::Parser`. "Optimized 16-bytes-per-node" parser.
- `tsz_core::parser::ParserState` — the recursive-descent engine (in `tsz_parser`); cache-optimized, produces a TypeScript-compatible AST.
- `tsz_core::parser::NodeArena` — "Arena for thin nodes with typed data pools. O(1) allocation, cache-efficient." The AST is arena-backed.
- `tsz_core::parser::NodeIndex` — index into the arena (serialization-friendly; replaces pointers/`Rc`).
- `tsz_core::parser::NodeList` — list of child `NodeIndex`.
- `tsz_core::parser::TextRange` — `{ start, end }` as **character** indices (not byte indices).
- `tsz_core::parser::ParseDiagnostic` — parse-time error/warning.

AST access:
- **`SyntaxKind`** (`tsz_core::SyntaxKind`, also `tsz_scanner::SyntaxKind`): enum, token/node kinds indices ~0–186, with 200+ associated constants: `SOURCE_FILE`, `CLASS_DECLARATION`, `INTERFACE_DECLARATION`, `ENUM_DECLARATION`, `FUNCTION_DECLARATION`, `ARROW_FUNCTION`, `IMPORT_DECLARATION`, `EXPORT_DECLARATION`, `TYPE_REFERENCE`, etc.
- **`NodeAccess`** trait (`tsz_core::NodeAccess`) — "Access into AST node data."
- **`Spanned`** trait — types with a source span.
- 120+ typed node-data structs in the parser: `ClassData`, `FunctionData`, `BinaryExprData`, `AccessExprData`, `IfStatementData`, `TypeRefData`, `JsxElementData`, … Node payloads live in typed pools indexed by `NodeIndex`.
- Parser submodules: `parser::base`, `parser::flags`, `parser::node` (thin 16-byte node architecture), `parser::parse_rules`, `parser::state`, `parser::syntax_kind_ext`.

**[main] node navigation via `TsSourceFile`** (if `tsz-wasm` available): `getRootHandle`, `getStatementHandles`, `getNodeKind`, `getNodePos/End`, `getNodeFlags`, `getNodeText`, `getParentHandle`, `getChildHandles`, `getIdentifierText`, `isKind`, `forEachChild`. At 0.1.9 you instead walk the `NodeArena` directly via `NodeIndex`/`NodeList` + `for_each_child()` (see `tsz_solver::for_each_child` and `syntax` helpers).

---

## 3. Type checking & turning a `Type` into a printable string ← **critical for use case (A)**

### 3a. Running the checker

**High-level [main]:** `TsProgram::get_type_checker() -> TsTypeChecker`. Diagnostics/checking are computed lazily on demand.

**Low-level / native [0.1.9]:** `tsz_checker::CheckerState`. Path `tsz_core::CheckerState` = `tsz_checker::state::CheckerState`.
```rust
pub struct CheckerState<'a> { pub ctx: CheckerContext<'a> }
```
Constructors (all in `tsz-checker`):
```rust
pub fn new(arena: &'a NodeArena, binder: &'a BinderState, types: &'a dyn QueryDatabase,
           file_name: String, compiler_options: CheckerOptions) -> Self
pub fn with_options(arena, binder, types, file_name, compiler_options: &CheckerOptions) -> Self
pub fn with_cache(arena, binder, types, file_name, cache: crate::TypeCache, compiler_options: CheckerOptions) -> Self
pub fn with_cache_and_options(arena, binder, types, file_name, cache, compiler_options: &CheckerOptions) -> Self
pub fn new_with_shared_def_store(arena, binder, types, file_name, compiler_options,
           definition_store: Arc<tsz_solver::def::DefinitionStore>) -> Self
pub fn with_shared_def_store variants (…, Arc<DefinitionStore>) -> Self
```
Auxiliary:
```rust
pub const fn enable_source_file_test_pragmas(&mut self)
pub const fn has_parse_errors(&self) -> bool
pub const fn has_syntax_parse_errors(&self) -> bool
pub fn extract_cache(self) -> crate::TypeCache   // pull out the populated TypeCache after checking
```
> To construct `CheckerState` natively you need: a `&NodeArena` (from parsing), a `&BinderState` (from `tsz_binder`), and a `&dyn QueryDatabase` (the `tsz_solver` type DB). This is the real, gnarly integration surface at 0.1.9. The `parallel::check_*` helpers wrap this; prefer them.

The actual per-node checking entry points live across the 42 `tsz_checker` modules (`dispatch`, `expr`, `statements`, `declarations`, `type_checking`, `type_computation`, `type_node`, `call_checker`, `promise_checker` (for `Promise<T>` inference!), `object_type`, `class_type`, `interface_type`, `signature_builder`, `symbol_resolver`, `state_type_resolution`, `state_type_analysis`, `state_type_environment`, …). The results are cached in **`TypeCache`** keyed by node — that cache is what maps a checked node → its `TypeId`.

### 3b. The `Type` representation

Types are **interned handles**: `tsz_solver::types::TypeId` (a `u32` newtype). The payload is `tsz_solver::types::TypeData` (see §7). Sentinel ids:
```
TypeId::ERROR=1, NEVER=2, UNKNOWN=3, ANY=4, VOID=5, UNDEFINED=6, NULL=7,
BOOLEAN=8, NUMBER=9, STRING=10, BIGINT=11, SYMBOL=12, BOOLEAN_TRUE=14,
BOOLEAN_FALSE=15, FUNCTION=16, STRICT_ANY=19
```
`TypeId` predicates: `is_error()`, `is_any()`, `is_unknown()`, `is_never()`, `is_nullish()`, `is_nullable()`, `is_top_type()`, `is_any_or_unknown()`, `is_local()`, `is_global()`.

### 3c. `Type` → printable string (THE key API) ← **use this to read inferred types**

**`tsz_solver` `TypeFormatter`** — native, no JS host. Path: `tsz_solver::diagnostics::format::TypeFormatter` (also surfaced as `tsz_solver::TypeFormatter`).
```rust
pub struct TypeFormatter<'a> { /* config */ }
pub fn new(interner: &'a dyn TypeDatabase) -> Self
pub fn with_symbols(interner, symbol_arena) -> Self
pub const fn with_def_store(self, def_store) -> Self
pub const fn with_strict_null_checks(self, strict: bool) -> Self
pub const fn with_diagnostic_mode(self) -> Self
// ~25 builder-style config methods …
pub fn format(&mut self, type_id: TypeId) -> std::borrow::Cow<'static, str>   // ← main entry
pub fn render(&mut self, pending: &PendingDiagnostic) -> TypeDiagnostic
```
Usage shape (native):
```rust
let mut f = TypeFormatter::new(&interner /* &dyn TypeDatabase */)
    .with_strict_null_checks(true);
let s: String = f.format(type_id).into_owned();   // e.g. "Promise<{ id: number; name: string }>"
```
**[main] convenience:** `TsTypeChecker::typeToString(type_handle: u32) -> String`, which is literally:
```rust
pub fn type_to_string(&self, type_handle: u32) -> String {
    let type_id = TypeId(type_handle);
    let mut formatter = TypeFormatter::new(&*self.interner);
    formatter.format(type_id).into_owned()
}
```
So even the high-level `typeToString` bottoms out in `tsz_solver::TypeFormatter::format`. **For native 0.1.9, call `TypeFormatter::format` directly** — this is the reliable, host-independent way to read an inferred type as text. There is also a `tsz_solver::TypePrinter`/`tsz_emitter::emitter::type_printer` ("Convert `TypeId` to TypeScript syntax") and `tsz_core::printer::TypePrinter` for emit-oriented type printing.

---

## 4. Symbol / type queries (getTypeOfSymbol / getTypeAtLocation equivalents)

### 4a. High-level [main] — on `TsTypeChecker`
```rust
#[wasm_bindgen(js_name = typeToString)]      pub fn type_to_string(&self, type_handle: u32) -> String
#[wasm_bindgen(js_name = getTypeOfSymbol)]   pub fn get_type_of_symbol(&self, symbol_handle: u32) -> u32   // ⚠ stub at that snapshot: returns TypeId::ANY.0
#[wasm_bindgen(js_name = getApparentType)]   pub fn get_apparent_type(&self, type_handle: u32) -> u32
// plus (per source scan): getTypeAtLocation, getSymbolAtLocation, isTypeAssignableTo,
//   isUnionType, isArrayType, isNullableType, and intrinsic getters getAnyType/getStringType/…
```
> ⚠️ **`getTypeOfSymbol` was observed returning `TypeId::ANY` (a stub)** in the scanned source. **Do not trust the high-level symbol→type methods to be fully implemented** — verify each returns real types at whatever version we use. `getTypeAtLocation` is the more likely-to-work path (node handle → `TypeId`).

### 4b. Native [0.1.9] — the real query surface

The mapping *checked node → `TypeId`* is the **`TypeCache`** populated by `CheckerState` (extract via `CheckerState::extract_cache()`; view via `tsz_emitter`'s `type_cache_view` / `TypeCacheView`). To resolve a *symbol* to its declaration/type you go through:
- `tsz_binder`: `Symbol`, `SymbolId`, `SymbolTable`, `SymbolArena`, `symbol_flags` — name → symbol, symbol → declaration `NodeIndex`(es). `DeclarationArenaMap = Map<(SymbolId, NodeIndex), SmallVec<Arena>>` handles cross-arena decls. Exported names resolve via `tsz_core::exports` (`ExportTracker`, `ExportedBinding`, `ExportResolution`) and imports via `tsz_core::imports` (`ImportTracker`, `ImportedBinding`).
- `tsz_checker::symbol_resolver` + `state_type_resolution` modules — the checker-side "type of symbol at a location" logic.
- `tsz_solver` type queries (module `type_queries`, trait `QueryDatabase`, `TypeResolver`): `is_subtype_of()`, `query_relation()`, `instantiate_type()`, `contains_type_parameters()`, plus `type_resolver` for resolving a type reference to its `DefId`/declaration.

**Cross-module type-reference → declaration:** a `TypeData::Lazy(DefId)` / `TypeData::Application(TypeApplicationId)` resolves through `tsz_solver::def::DefinitionStore` (the `DefId → definition` map). Sharing one `Arc<DefinitionStore>` across `CheckerState`s (see `*_with_shared_def_store` ctors) is how cross-file links are kept coherent. For our cross-module link needs, `DefId` + `DefinitionStore` is the anchor.

> **Practical read path for use case (A) at 0.1.9:**
> 1. Drive `parallel::compile_files` / `check_files_parallel` to check the package (or `WasmProgram::check_all`).
> 2. Get the `TypeCache`/`TypeCacheView` and the shared `TypeInterner`/`DefinitionStore`.
> 3. For a target declaration `NodeIndex` (found via `SymbolTable`/`ExportTracker`), look up its `TypeId` in the cache.
> 4. `TypeFormatter::new(interner).format(type_id)` → the inferred type string.
> Steps 2–3 use crate-internal surfaces that may not be cleanly public at 0.1.9 — **the largest 0.1.9 integration unknown.** Confirm the `TypeCache`→`TypeId` accessor is public.

---

## 5. Declaration emit (`.d.ts`) ← **use case (B)**

### 5a. Native [0.1.9] — `tsz_emitter::declaration_emitter::DeclarationEmitter`
Also surfaced under `tsz_core::declaration_emitter`. Noted as gated on a **`dts`** feature on `main` (confirm at 0.1.9).
```rust
pub struct DeclarationEmitter<'a> { /* arena, type_cache, binder refs + state */ }

pub fn new(arena: &'a NodeArena) -> Self
pub fn with_type_info(arena, type_cache: TypeCacheView, type_interner: &'a TypeInterner,
                      binder: &'a BinderState) -> Self
pub fn with_shared_type_info(arena, type_cache: Arc<TypeCacheView>, type_interner, binder) -> Self

pub fn emit(&mut self, root_idx: NodeIndex) -> String        // ← returns the .d.ts text

// configuration setters:
set_source_map_text, enable_source_map, set_used_symbols, set_foreign_symbols,
set_binder, set_declaration_summary, set_export_surface, set_current_arena,
set_file_idx_to_path, set_root_file_paths, set_remove_comments, set_strip_internal,
set_strict_null_checks, set_isolated_declarations, take_diagnostics
```
- **`emit(root_idx) -> String`** is the core call: give it the source file's root `NodeIndex`, get back `.d.ts` text.
- **`set_isolated_declarations(true)`** enables isolated-declarations mode.
- To get *inference-accurate* declarations (not just syntactic), construct via **`with_type_info` / `with_shared_type_info`** so the emitter has the checker's `TypeCacheView` + `TypeInterner` + `BinderState`. Without type info it can only emit what's syntactically explicit.
- `take_diagnostics` returns emit-time diagnostics (e.g. isolated-declarations violations).

### 5b. High-level [main] — via `TsProgram`
```rust
pub fn emit_file(&self, name) -> String     // emitFile → JS or .d.ts for one file
pub fn emit_json(&self) -> String           // emitJson → all outputs as JSON
```
And free transpile functions (`tsz-wasm` `emit.rs`, [main]):
```rust
#[wasm_bindgen(js_name = transpileModule)] pub fn transpile_module(source: &str, options_json: &str) -> String
#[wasm_bindgen(js_name = transpile)]       pub fn transpile(source: &str, target: Option<u8>, module: Option<u8>) -> String
// Result types: EmitResult { emit_skipped, diagnostics: Vec<EmitDiagnostic>, emitted_files: Vec<EmittedFile> }
//   EmittedFile { name, text, declaration: bool, source_map: bool }
//   TranspileOptions { file_name, target, module, source_map, inline_source_map, declaration,
//                      remove_comments, jsx, downlevel_iteration, module_detection }
//   TranspileOutput { output_text, source_map_text: Option<String>, declaration_text: Option<String>, diagnostics }
```
> `TranspileOptions.declaration = true` / `TranspileOutput.declaration_text` is the terse path for single-module `.d.ts`, but transpile is **single-file & syntactic** (no cross-file inference). For inference-accurate `.d.ts` across a package, use §5a `DeclarationEmitter::with_type_info` fed from a checked program.

**Note the 0.1.9 conformance caveat:** declaration emit is **39.0%** at 0.1.9 — expect gaps on nontrivial inferred declarations. This directly affects use case (B) quality.

---

## 6. Diagnostics

### Native [0.1.9] — `tsz_core::diagnostics`
```
Diagnostic (struct)             // message + location + severity + error code
DiagnosticBag (struct)          // aggregation during compilation
DiagnosticRelatedInfo (struct)
DiagnosticSeverity (enum)       // Error, Warning, Info, Hint
DiagnosticDomain (enum)
// helpers: format_code_snippet(), format_message()
```
From `WasmProgram`: `check_all() -> String` (JSON of per-file check results incl. diagnostics), `get_diagnostic_codes() -> String` (JSON `{file → [u32 codes]}`), `get_all_diagnostic_codes() -> Vec<u32>`. Diagnostic code constants exist in `tsz_core` (`CANNOT_FIND_MODULE`, `DUPLICATE_IDENTIFIER`, `IMPORT_PATH_NEEDS_EXTENSION`, `MISSING_ES2015_LIB_SUPPORT`, …).

### High-level [main] — `TsDiagnostic`
```rust
#[wasm_bindgen] pub struct TsDiagnostic {
    file_name: Option<String>, start: u32, length: u32,
    message_text: String, category: u8 /* DiagnosticCategory */, code: u32,
}
// getters: fileName,start,length,messageText,category,code; methods isError(), isWarning(), toJson()
// DiagnosticCategory { Warning=0, Error=1, Suggestion=2, Message=3 }
```
Obtained from `TsProgram::get_semantic_diagnostics(name?)` (type errors), `get_syntactic_diagnostics(name?)`, `get_pre_emit_diagnostics()`.

---

## 7. `tsz_solver` — the type system (for reading/printing types)

Modules: `canonicalize, classes, def, evaluation, judge, objects, operations, recursion, relations, type_queries, type_resolver, types, unsoundness_audit, utils, visitor`.

Traits: **`TypeDatabase`** (query interface — what `TypeFormatter::new` needs), `QueryDatabase` (higher-level ops — what `CheckerState` takes as `&dyn QueryDatabase`), `Judge` (pure type algebra), `TypeResolver`, `TypeVisitor`, `AssignabilityOverrideProvider`, `AssignabilityChecker`.

Key structs: **`TypeInterner`** (owns interned `TypeData`; usually `Arc<TypeInterner>`), **`TypeFactory`**, **`TypeFormatter`** (§3c), `TypeInstantiator`, `TypeEvaluator`, `SubtypeChecker`, `InferenceContext`, `ConstraintSet`, `DefaultJudge`, `QueryCache`, `RelationContext`.

`types` submodule — the payloads you'll read when interpreting inferred types:
```rust
pub enum TypeData {
    Intrinsic(IntrinsicKind),
    Literal(LiteralValue),          // "hello", 42, true
    Object(ObjectShapeId),          // → ObjectShape { properties: PropertyInfo[], index sigs, … }
    ObjectWithIndex(ObjectShapeId),
    Union(TypeListId),              // A | B | C
    Intersection(TypeListId),       // A & B & C
    Array(TypeId),                  // T[]
    Tuple(TupleListId),             // [T, U, V]
    Function(FunctionShapeId),      // → FunctionShape { params: ParamInfo[], return: TypeId, … }
    Callable(CallableShapeId),      // multiple CallSignatures
    TypeParameter(TypeParamInfo),
    BoundParameter(u32),            // [main] De Bruijn
    Lazy(DefId),                    // unresolved named type → DefinitionStore
    Recursive(u32),                 // [main] De Bruijn recursive ref
    Enum(DefId, TypeId),
    Application(TypeApplicationId), // Base<Args>  ← e.g. Promise<T>, Array<T>
    Conditional(ConditionalTypeId), // T extends U ? X : Y
    Mapped(MappedTypeId),           // { [K in Keys]: V }
    IndexAccess(TypeId, TypeId),    // T[K]
    TemplateLiteral(TemplateLiteralId),
    TypeQuery(SymbolRef),           // typeof expr
    KeyOf(TypeId),
    ReadonlyType(TypeId),
    UniqueSymbol(SymbolRef),
    Infer(TypeParamInfo),
    ThisType,
    StringIntrinsic { kind, type_arg },
    ModuleNamespace(SymbolRef),
    NoInfer(TypeId),
    Substitution { base_type, constraint },
    Error,
    UnresolvedTypeName(Atom),
}
```
Supporting types (in `types`): `ObjectShape`/`ObjectShapeId`, `FunctionShape`/`FunctionShapeId`, `CallableShape`, `CallSignature`, `PropertyInfo`, `IndexSignature`/`IndexInfo`, `ParamInfo`, `TypeParamInfo`, `TupleElement`, `TypePredicate`, `SymbolRef`, `Variance`, `ObjectFlags`, `LiteralValue`, `IntrinsicKind`. For **`Promise<T>` / object shapes**: `Application(TypeApplicationId)` gives base + args; `Object(ObjectShapeId)` → `ObjectShape.properties: [PropertyInfo]` (name → `TypeId`), which is exactly what use case (A) needs. Prefer letting `TypeFormatter::format` render these rather than hand-walking `TypeData`, unless you need structured (non-string) access.

Useful free fns: `is_array_type()`, `is_function_type()`, `is_union_type()`, `is_intersection_type()`, `is_literal_type()`, `is_empty_object_type()`, `instantiate_type()`, `remove_nullish()`, `split_nullish_type()`, `narrow_by_typeof()`, `for_each_child()`, `walk_referenced_types()`, `evaluate_conditional()`, `evaluate_mapped()`, `is_subtype_of()`, `query_relation_with_resolver()`.

Recursion guards (constants): `MAX_TYPE_RECURSION_DEPTH`, `MAX_INSTANTIATION_DEPTH`, `MAX_CONSTRAINT_ITERATIONS`, `MAX_VISITING_SET_SIZE`.

---

## 8. Caveats, thread-safety, native-vs-WASM, known gaps

**WASM-oriented API surface.** The public entry points (`WasmProgram`, `create_program`, `create_parser`, `create_scanner`, and all of `tsz-wasm`) are `#[wasm_bindgen]`. `wasm-bindgen` is a hard dependency. The crate is *designed for a JS/WASM host* (Monaco/web playground on tsz.dev). Native use is possible but is going against the grain:
- `set_compiler_options` returns `Result<(), JsValue>` — `JsValue` is a wasm-bindgen type; on native targets this may not exist / may need a shim. **Prefer the JSON-string in / JSON-string out methods and the `tsz_core::parallel::*` + `tsz_checker`/`tsz_solver`/`tsz_emitter` native surfaces, which take/return plain Rust.**
- **Confirm `tsz-core` 0.1.9 compiles for `x86_64`/`aarch64` native** (not just `wasm32`) as the very first spike. Check `[features]` for any `wasm`/`dts` gate.

**Global / thread state.** Uses `rayon` for parallel parse/bind/check — **you must call `ensure_rayon_global_pool()`** to set a large stack (the checker recurses deeply; default thread stacks overflow). This installs a **process-global** Rayon pool (global mutable state; init once). `once_cell` is used for lazy globals. `dashmap` (concurrent maps) and `Arc<TypeInterner>` / `Arc<DefinitionStore>` indicate the type layer is built to be shared across threads (`Send`/`Sync`), but interners are shared mutable state — treat one `Arc<TypeInterner>` as the single source of truth for a compilation and don't mix `TypeId`s across interners.

**`unsafe`.** Not specifically enumerated in what was fetched; arena + interner + wasm-bindgen glue crates typically contain some `unsafe`. Assume present; audit if it matters.

**Pre-release maturity (v0.1.9) — known gaps that bite our use cases:**
- Type checker conformance **62.7%** (7,884/12,574 tsc tests). Inferred types for advanced generics/conditional/mapped types may be wrong or `any`.
- **Declaration emit 39.0%** (776/1,989) — expect real gaps in use case (B); many complex inferred `.d.ts` won't match `tsc`.
- Language service 18.3% — irrelevant (we skip LSP).
- **`getTypeOfSymbol` observed as a stub returning `any`** in scanned source — don't rely on symbol→type high-level methods; verify each, prefer `getTypeAtLocation`/`TypeCache` + `TypeFormatter`.

**Recommended integration stance for us:**
1. First spike: confirm native compilation + native access to `TypeCache`→`TypeId` at 0.1.9. If blocked, evaluate bumping to a newer `tsz-wasm`/`tsz-core` where `TsProgram`/`TsTypeChecker`/`getTypeAtLocation` exist.
2. For (A) inferred types: drive `parallel::compile_files`/`check_*` → obtain `TypeInterner` + `TypeCache` → node/symbol `TypeId` → `TypeFormatter::format`.
3. For (B) `.d.ts`: `DeclarationEmitter::with_type_info(...).emit(root_idx)`, fed by the checked program; enable `set_isolated_declarations`/`set_strict_null_checks` to match our tsconfig. Accept 39%-era gaps or upgrade.

---

### Sources
- crates.io: https://crates.io/crates/tsz-core (v0.1.9 metadata, deps, 62.7%/39.0% conformance, repo `mohsenazimi/tsz`)
- docs.rs: https://docs.rs/tsz-core/0.1.9/tsz_core/ (modules, types, functions, traits)
- GitHub `tsz-org/tsz` (mirror `mohsen1/tsz`) — `crates/tsz-core/src/api/wasm/*`, `crates/tsz-checker/src/state/state.rs`, `crates/tsz-solver/src/{types.rs,diagnostics/format/mod.rs}`, `crates/tsz-emitter/src/declaration_emitter/*`, and (newer-branch) `crates/tsz-wasm/src/wasm_api/*`
- https://tsz.dev/
> Signatures quoted from source are from the GitHub repo; where the repo `main` branch is newer than v0.1.9, entries are flagged **[main]** vs **[0.1.9]**. Re-verify every signature against the exact 0.1.9 artifact before coding against it.

# IR / Graph / Tree-sitter Audit

**Date:** 2026-07-16  
**Scope (live tree only):** `workspace/ir`, `workspace/compiler/graph`, `workspace/compiler/treesitter`, `workspace/compiler/generate` (linked_data, cst, occurrences, resolve, surface, source_archive, blob_info), `workspace/heart/identity` + content addressing, registry blob + graph runtime consumers  
**Purpose:** Feed librarification — dual store (INDEX + REGISTRY) of IR + resolved tree-sitter trees + source; Terminus only for hottest packages; symbol-level incrementality and cross-generation identity  
**Out of scope:** Building the project; root-level legacy `compiler/`; fixing producers

---

## 0. Executive map

```
Producers (rust-analyzer / OXC / pyrefly / go oracle / javadoc / Roslyn / snix)
    │
    ▼  ir::Index  (flat HashMap<NudoxPath, Entry>)
generate::generate_with
    ├─ surface     CAS JobKey
    ├─ cst         JobKey.with_tag(b"cst")     → CstSet (ResolvedReference spans; trees dropped)
    ├─ occurrences JobKey.with_tag(RESOLVER_VERSION="occ-v1") → OccurrenceSet
    └─ archive     JobKey.with_tag(b"archive") → SourceArchive (per-file BLAKE3)
    │
    ├─► BlobManifest { ir_ref, references_ref, files[] }   ← no occurrences_ref today
    └─► graph::from_ir::project → GraphCorpus → linked_data::emit (waves 1–5) → Terminus
```

**Load-bearing identities (three systems, not one):**

| Layer | Key | Stable across recompile? | Across package versions? |
|---|---|---|---|
| IR entry | `NudoxPath` (Local / External) | Yes if producers deterministic | No — version lives outside path |
| Graph node | `Symbol/{lang}%2F{pkg}%2F{fq}` IRI | Yes | **Yes** (version-agnostic; membership via `PackageVersion.declares`) |
| heart `SymbolId` | UUIDv5(`SYMBOL`, `instance‖PackageId/segments`) | Yes given same instance + PackageId | **No** — `PackageId` includes version |

---

## 1. Exact IR shape

### 1.1 Crate layout and conventions

File: `workspace/ir/lib.rs:1–16`

- Modules: `entry`, `function`, `generics`, `kind`, `module`, `parameter`, `pipeline`, `primitives`, `protocols`, `record`, `syntax`, `ty`.
- **Convention (doc comment lines 1–3):** fields typed `Option<Vec<T>>` distinguish *absent* (`null`) from *empty* (`[]`). Callers double-unwrap intentionally.
- Features (`workspace/ir/Cargo.toml:14–29`): default `facet` + `serde`; always-on deps `arborium-tree-sitter` 2.18, `rustc-hash`, `strum`, `thiserror`, `yoke` 0.8.
- **Facet:** declared as optional dep; **no `#[facet(...)]` attributes** appear in IR types today — serde is the real wire path. Facet is a reserved/future path, not active encoding.

### 1.2 Top-level containers

#### `NudoxPath` — the IR’s only inter-entry pointer

```15:24:workspace/ir/entry.rs
pub enum NudoxPath {
	External { path: PathBuf, dependency: String },
	Local(PathBuf),
}
```

- **Not** a filesystem path necessarily: producers commonly store a single `PathBuf` component that already embeds `::` (e.g. `"crate::Type::method"` as one segment). Graph linker and `SymbolTable::path_segments` re-split on `::` and sometimes `.` (`workspace/compiler/graph/symtab.rs:21–51`).
- External deps carry owning package name in `dependency`; path is relative to that package (Rust drops the crate segment for externals — `workspace/compiler/compile/rust/ra/ctx.rs:402–431`).

#### `Index` — flat table, not a nested tree

```26:35:workspace/ir/entry.rs
pub struct Index {
	pub root_ids:        Vec<NudoxPath>,
	pub entries_by_path: HashMap<NudoxPath, Entry>,
}
```

Comment at lines 28–29 claims “stable integer IDs” — **stale**. Keys are `NudoxPath`, not integers. Nesting is reconstructed via `members: Option<Vec<NudoxPath>>` on modules/records/traits/functions and inverted to `member_of` in the graph.

#### Pipeline stages

```5:68:workspace/ir/pipeline/pipeline.rs
pub struct Ir<S: Stage> { data: S::Data }
// Collected: Vec<Entry> → Indexed: Index
// Roots: Local paths with exactly one path component and no "::" in the lossy string
```

Root heuristic is crude: multi-segment Local paths and `::`-spelling single components are non-roots even if they are crate-level modules.

### 1.3 `Entry` / `Symbol<T>` shell

File: `workspace/ir/kind.rs`

**`Symbol<T>`** (lines 94–116) wraps every payload:

| Field | Type | Role |
|---|---|---|
| `name` | `String` | Leaf display name |
| `path` | `NudoxPath` | Primary identity inside the package |
| `aliases` | `Option<HashSet<Vec<String>>>` | Alternate segment vectors (re-exports) |
| `visibility` | `Visibility` | Public/Private/Protected/Internal/Package |
| `documentation` | `Option<String>` | Doc string |
| `deprecation` | `Option<Deprecation>` | since/note |
| `doc_links` | `Option<HashMap<String, NudoxPath>>` | Intra-doc link resolution → graph `mentions` |
| `inner` | `T` | Kind payload |

**`Entry` variants** (lines 150–204), serde `tag = "kind", content = "value"`:

| Variant | Payload | Graph `SymbolKind` |
|---|---|---|
| `Module` | `Module { members }` | Module |
| `RecordType` | `Record` | RecordType |
| `Info` | `String` | Info |
| `UnionType` | `Vec<Type>` | UnionType |
| `TraitDef` | `TraitDef` | TraitDef |
| `TraitImpl` | `TraitImpl` | TraitImpl |
| `SumType` | `SumType` | SumType |
| `Function` | `Function` | Function |
| `TypeAlias` | `TypeAliasBody { generics?, target }` | TypeAlias |
| `Constant` / `Variable` / `Field` / `Event` | `TypedBinding { ty?, value?, mutable? }` | same |
| `Macro` / `PrimitiveType` | `()` | Macro / PrimitiveType |

Accessors: `path`, `name`, `kind_tag`, `documentation`, `aliases`, `schema_class` (`kind.rs:206–327`).

### 1.4 Function / Record / Module / Protocols

**`Function`** (`function.rs:9–32`):

- `input_parameters` / `output_parameters`: `Option<Vec<Parameter>>`
- `type_links`: `Option<HashMap<String, i64>>` — **deliberately dropped** in graph projection (“superseded by the linker”, `from_ir.rs:16–17`)
- `attributes`: Variadic/Generator/Const/Pure/Async/Unsafe
- `generics`, `receiver` (`ReceiverKind`), `overloads: Option<Vec<Function>>`
- `implemented: bool`
- `members`, `implemented_protocols`: path lists for namespace-like functions

**`Record`** (`record.rs:8–45`): name?, generics?, fields (always `Vec`, not Option — empty means dynamic), call_signatures/constructors/methods/index_signatures/super_types/members/implemented_protocols.

**`SumType`** (`record.rs:140–169`): variants + optional generics/methods/protocols/supers/members/underlying — expanded beyond historical `Vec<SumVariant>`-only.

**`Module`** (`module.rs:8–10`): only `members: Option<Vec<NudoxPath>>`.

**`TraitDef` / `TraitImpl`** (`protocols.rs`): full trait surface including object_safe, sealed, cfg; impls with negative/blanket/unsafe flags. Methods live inline as `TraitMethod` **and** can dual-emit as free `Entry::Function` via `members`.

### 1.5 Types, generics, parameters

**`Type`** (`ty.rs:11–125`): large closed enum — TypeReference, SelfType, DynTrait, GenericParam, Primitive, FunctionPointer, Tuple, RecordLiteral, Slice, Array, ImplTrait, Infer, Never, Any, RawPointer, BorrowedRef, Union, Intersection, Sum, QualifiedPath, Variadic, TypeOperator, Conditional, Mapped, Predicate, Literal, TemplateLiteral, TypeQuery, NamedTuple. Serde: `tag = "type", content = "value"`.

**`TypeReference`** (`ty.rs:175–180`): `identifier: String` + optional `generic_args`. **No NudoxPath** — names resolve later via linker/`SymbolTable`.

**`Generics` / `Constraint` / `ConstExpr` / `Kind` / `Variance` / `GenericArg`:** `generics.rs` full surface (including LogicalPredicate, FunctionalDependency, ImplicitBound). Graph flattens `LogicalPredicate` to `format!("{pred:?}")` (`from_ir.rs:559–561`) — structured tree lost at projection.

**`Parameter`** (`parameter.rs:15–35`): Literal | Type | Const | Lifetime | Dependent | Module — unified value-level and generic params.

**`Primitive` / `Width`:** `primitives.rs`.

### 1.6 How symbols are identified *in the IR*

There is **no** `SymbolId` inside IR. Identity is:

1. **`Entry.path: NudoxPath`** — primary key in `Index.entries_by_path`.
2. **`Entry.name`** — leaf name (can diverge from last path segment in edge cases).
3. **`aliases`** — extra spellings for resolution only.

Construction is producer-owned:

- **Rust** (`compile/rust/ra/ctx.rs:179–193, 402–431`): module chain + name, `::`-joined into a single Local PathBuf component for local crate; External for other crates with dependency name + relative path.
- **TypeScript OXC:** often `PathBuf::from(fe.local_path.join("::"))` single-component FQNs.
- **Python/Go/Java/C#/Nix:** each has conventions; module wiring via `producer/wire.rs` (`FsPathParent` for `/`-nesting, `apply_members_to_index` for explicit maps).

**Stability:**

| Event | `NudoxPath` |
|---|---|
| (a) Recompile identical source | Stable **iff** producer is deterministic (path construction, alias collection order into HashSet). FxHashMap iteration order is **not** stable for serialization without sorting. |
| (b) Source change elsewhere in package | Stable for unchanged entries **iff** path is content-addressed by FQN, not ordinal. Path deps changing rustc names can flip Local↔External. |
| (c) Rename | Path **changes** (name segment changes). No lineage. |
| (d) Signature change | Path **stable** (signature is payload, not key). |
| (e) Move between modules/files | Path **changes** (module prefix changes). File path is **not** in NudoxPath for most producers — only FQN. |

**No ordinals in IR keys.** Instability creeps in via: HashMap/HashSet serde order, float `ConstExpr::Float(f64)` (`generics.rs:114–115`), producer FQN spelling drift (`crate` vs package name, `.` vs `::`), and single-component `::`-embedded PathBufs that confuse `Path` APIs.

### 1.7 Docs and attributes attachment

- Docs: `Symbol.documentation` + field/method/variant-level docs on nested structs.
- Deprecation: `Symbol.deprecation` (optional, skip_serializing_if none).
- Doc links: `Symbol.doc_links` → graph mentions (`from_ir.rs:1116–1121`).
- Function/trait attributes: typed enums, not freeform except `TraitAttribute::Custom` and `FieldAttributes.decorators: Vec<String>`.

### 1.8 Syntax submodule (sibling corpus, not inside Entry)

```1:14:workspace/ir/syntax/mod.rs
// body, error, occurrence, types, walker
```

**Design principle** (`occurrence.rs:9–10`): occurrences sit **beside** surface IR, never inside `Entry`.

Legacy body path still exists: `FunctionBody` yokes a live `tree_sitter::Tree` to `Arc<str>` (`body.rs:8–20`). Graph comments say body-path References were retired in “Phase 0” in favor of `OccurrenceSet` (`from_ir.rs:13–14, 900–902`).

---

## 2. Serialization and blobs

### 2.1 What generate produces

```56:73:workspace/compiler/generate/mod.rs
pub struct GeneratedPackage {
	surface: Index,
	cst: CstSet,
	occurrences: OccurrenceSet,
	archive: SourceArchive,
	blob_info: BlobInfo,
	snapshot: ContentHash,
}
```

Job keys (`generate/mod.rs:106–121`):

| Artifact | CAS key |
|---|---|
| Surface | `JobKey::derive(producer_version, toolchain, source, dep_lock).as_hash()` |
| CST | `job.with_tag(b"cst")` |
| Occurrences | `job.with_tag(RESOLVER_VERSION)` where `RESOLVER_VERSION = "occ-v1"` (`resolve.rs:33`) |
| Archive | `job.with_tag(b"archive")` |

`JobKey` layout (`heart/content.rs:60–92`): length-prefixed BLAKE3 of four parts; `with_tag` = `H(jobkey ‖ len(tag) ‖ tag)`.

### 2.2 Producer / surface encoding

- In-process CAS: **postcard** encode of `ProducerOutput { index, aux }` (`compile/producer/runtime.rs:136–184`).
- Oracle/worker JSON decode path still exists (`producer/mod.rs:537` serde_json).
- `Index` / `Entry` use **serde** under feature flag; postcard uses serde traits.

### 2.3 Registry blob model (what actually lands in CAS)

```46:66:workspace/registry/blob/mod.rs
pub struct BlobManifest {
	pub package: PackageId,
	pub files: NonEmpty<FileEntry>,  // path + BLAKE3 + size
	pub ir_ref: ContentHash,         // serialized Index
	pub references_ref: ContentHash, // ResolvedReference set (CST-free)
	pub toolchain: Toolchain,
}
```

**Critical gap for librarification:**

| Generated artifact | In BlobManifest? |
|---|---|
| IR Index | Yes (`ir_ref`) |
| Source files | Yes (`files[]`) |
| CST ResolvedReference spans | Yes (`references_ref`) via `ReferenceSet` wire codec |
| OccurrenceSet | **No** |
| Live tree-sitter trees | **No** (by design, non-serializable) |
| Graph documents | Separate Terminus path, not blob sections |

Two distinct hashes on manifests (`blob/mod.rs:68–106`):

1. **Generation stamp** — `identity_bytes()` length-prefixed path/hash/ir/refs/toolchain (stable across unrelated serde field adds).
2. **CAS key** — BLAKE3 of full postcard(manifest).

Compiler-side `BlobInfo::assemble` (`generate/blob_info.rs:30–47`) hashes only archive files for `snapshot` — **does not include ir_ref/occurrences**. Registry’s `identity_bytes` is the production definition of package freshness for stored blobs.

### 2.4 Determinism / canonicity

| Concern | Status |
|---|---|
| Source archive file order | Sorted by path (`source_archive.rs:72`) |
| Snapshot fold | Sorted (path, hash) with length-prefixed paths |
| JobKey source hash | Sorted path‖file-hash (`parse_cache.rs:13–23`) |
| Index HashMap serde | **Non-deterministic order** unless consumers sort |
| Graph projection order | Explicit sort by IRI (`from_ir.rs:909–911`) |
| Occurrence files | Sorted by path; occurrences by span (`resolve.rs:48–49, 125`) |
| ResolutionStats buckets | Binary-search sorted Vec (deterministic) (`occurrence.rs:91–124`) |
| `ConstExpr::Float(f64)` | Non-canonical bit patterns possible |
| `HashSet` aliases | Unordered |
| Linked-data body hash | `DefaultHasher` of `document.to_string()` (`emit.rs:77–80`) — process-local, not CAS |

### 2.5 Linked-data emit format

- TerminusDBModel derive → JSON-LD instances (`graph/model.rs` header).
- Wave order (`linked_data/emit.rs:116–144`): packages → bare symbols (edges stripped) → full symbols → Implementation/Reference → PackageVersion.
- Base IRI `https://nudox.org/ir/` (`schema.rs:16`).
- Edge fields stripped in wave 2: `member_of`, `implements`, `extends`, `mentions`, `takes`, `returns`, `resolves_to` (`emit.rs:26–27`).

---

## 3. Occurrence / reference layer (REFERENCES-PLAN landings)

### 3.1 What landed

| Piece | Location | Status |
|---|---|---|
| Occurrence contract | `ir/syntax/occurrence.rs` | **Landed** — Role, Confidence, Occurrence, FileOccurrences, OccurrenceSet, ResolutionStats |
| ResolvedReference (legacy flat) | `ir/syntax/types.rs` | **Landed** — still used by CST stage + registry ReferenceSet |
| walk_references | `ir/syntax/walker.rs` | **Landed** — preorder tree walk + classify callback |
| LanguageSpec extractors | `compiler/treesitter/spec.rs` + per-lang | **Landed** — definitions, imports, RawReference chains, receivers |
| SymbolTable | `compiler/graph/symtab.rs` | **Landed** — shared by linker + occurrence resolver |
| resolve engine | `compiler/generate/resolve.rs` | **Landed** — ladder + attribution; CAS version `occ-v1` |
| occurrences stage | `compiler/generate/occurrences.rs` | **Landed** |
| Graph Reference projection | `from_ir::project_references` | **Landed** with assertion policy |
| Example rendering | `treesitter::render_example` | **Landed** — snippet + highlight |
| Occurrence storage in BlobManifest | registry | **Not landed** |
| Oracle Confidence tier | extractors | Scaffolded in enum; producers mostly Index/Import/Suffix |

### 3.2 Occurrence shape

```49:68:workspace/ir/syntax/occurrence.rs
pub struct Occurrence {
	pub span: Range<usize>,
	pub target: NudoxPath,
	pub kind: ReferenceKind,
	pub role: Role,              // Definition | Reference
	pub enclosing: Option<NudoxPath>,
	pub anchored: bool,          // enclosing matched surface index
	pub confidence: Confidence,  // Syntactic < Suffix < Index < Import < Oracle
}
```

**References point at symbols by `NudoxPath`**, not SymbolId/IRI. Graph materialization converts via `Linker::resolve_path`.

### 3.3 Resolution ladder (`resolve.rs:129–197`)

1. Local shadow for SelfRef/ClassRef receivers  
2. Rooted absolute markers (`crate`/`self`/`super` — Rust only, `fold_root_marker`)  
3. Import table (Internal / External / Glob)  
4. Lexical scope walk (enclosing prefixes)  
5. Package-wide exact/alias  
6. Unique suffix  
7. Unresolved (tallied only, **not emitted**)

Attribution: innermost `body_span` containing use (`enclosing_def`). Definition occurrences always emitted.

### 3.4 Graph assertion policy (`from_ir.rs:1149–1185`)

Only assert `Reference` when **all** of:

- `role == Reference`
- `anchored == true`
- `confidence >= Index`
- kind ∈ {FunctionCall, MethodCall, TypeReference, MacroInvocation, Import}
- `enclosing` is Some

VariableUse / FieldAccess / Suffix confidence stay **blob-only** (when occurrences are stored).

### 3.5 Dual reference pipelines (operational debt)

1. **Legacy CST:** `generate/cst.rs` + flat classifiers → `CstSet` → registry `references_ref`. Targets often placeholder `Local(name)` only (`treesitter/mod.rs:155–158`).
2. **Occurrences:** LanguageSpec + SymbolTable → FQ NudoxPath + confidence + enclosing.

Librarification should **collapse** to OccurrenceSet as the sole persisted reference corpus; CstSet becomes a recompute-or-delete artifact.

---

## 4. Graph lowering

### 4.1 Document model (`graph/model.rs`)

| Class | Keying | Role |
|---|---|---|
| `Package` | client id `Package/{lang}%2F{name}` | Version-agnostic package |
| `PackageVersion` | `PackageVersion/{lang}%2F{name}@{ver}` | `declares: Vec<Symbol>` membership |
| `Symbol` | `Symbol/{lang}%2F{pkg}%2F{fq}` | Node + edges + optional Shape |
| `Implementation` | `value_hash` | Reified subject implements interface |
| `Reference` | `value_hash` | Reified call/use edge |

**Symbol fields** (`model.rs:1021–1048`):

- Identity: `id`, `uri` (`lang/pkg/fq` with real `/`), `fq_name`, `name`, `path`, `aliases`
- `symbol_id: Option<String>` — **always `None` in compiler projection** (`from_ir.rs:1130`); “stamped by the uploader”
- `kind`, `visibility`, `documentation`, `resolved`
- Edges: `package`, `member_of`, `implements`, `extends`, `mentions`, `takes`, `returns`
- `shape: Option<Shape>` — Info/Record/Function/TraitDef/TraitImpl/Sum/Union/Alias

**Shape losses vs IR:**

- SumType drops methods/generics/members/underlying/protocols (only variants in `SumShape`)
- TraitDef drops object_safe/sealed/cfg/members
- Function drops members/implemented_protocols/type_links
- Constant/Variable/Field/Event types stuffed into `AliasShape` if present
- Record members/protocols projected as edges, not in shape
- Subdocuments mostly `key = "random"` — **not content-addressed**; re-insert may mint new subdoc ids (parent Symbol id is stable client-minted)

### 4.2 Linker (`graph/link.rs`)

```66:74:workspace/compiler/graph/link.rs
pub fn symbol_iri(language, package, fq_name) -> String {
	format!("Symbol/{}", encode_id_segment(&[language, package, fq_name]))
}
```

- `%2F` encodes hierarchy because EntityIDFor allows only one id segment.
- Resolution total: unknown names → stub under `~extern` package (`EXTERN_PACKAGE`, line 24).
- Lookup: exact → alias (via SymbolTable) → unique suffix → stub (`link.rs:194–214`).
- Parent inversion from IR `members` lists (`link.rs:135–148`).

**IRI is version-agnostic.** Cross-version symbol “sameness” in the graph is “same IRI appears in two `PackageVersion.declares` sets” (`model.rs:995–997` comment).

### 4.3 from_ir walk (`project`)

```903:957:workspace/compiler/graph/from_ir.rs
pub fn project(index, occurrences, ctx) -> GraphCorpus
```

Per entry: build shape + accumulate mentions/takes/returns while walking types; TraitImpl mints Implementation; then package-level References from occurrences; then stubs.

Deliberate losses listed at `from_ir.rs:16–19`.

### 4.4 Graph operations the system needs

**Declared store API** (`registry/runtime/graph/mod.rs:72–96`):

- `get_occurrences(item)` — who holds `item` in signature/decl
- `get_references(item)` — callers/users
- `are_related(from, to)` → `RelationKind`

**RelationKind** (lines 50–64): Member, Reference, Occurrence, Implements, Extends, ReExport.

**Expansion** (`expansion.rs`): bounded BFS over outgoing edges, score `1/depth`.

**Cross-version** (`resolution.rs:29–80`): pure diff of heart `Symbol` slices:

1. Same FQ path → Stable (next id)
2. Same plain name + kind → Moved (Jaccard path similarity)
3. else Removed / Added

**Server fan-out** (`server/poll.rs:252–267`): graph sink `insert_symbols` from heart `Symbol` records — **not** the full GraphCorpus wave emit. Linked-data emit exists on the compiler side; runtime insert path is thinner (symbols only). Integration of full Implementation/Reference documents vs heart Symbol projection is a **wiring gap** to treat carefully in librarification.

**GUI** still mentions old `terminusdb:///data/Entry/...` URIs (`gui/src/search_panel.rs`) — legacy surface.

### 4.5 Graph-over-blobs sketch

For non-hot packages, serve graph ops without Terminus by:

```
load BlobManifest
  → fetch ir_ref → deserialize Index
  → fetch occurrences (when stored) or re-resolve from source+trees
  → SymbolTable::build(index)
  → Linker::build / from_ir::project (in-memory GraphCorpus)
  → indexes:
       by_iri: Symbol
       reverse member_of, implements, extends, mentions, takes, returns
       refs_by_target / refs_by_source (from Reference list or OccurrenceSet)
```

**Needed module API (sketch):**

```text
GraphView::from_blobs(ir: Index, occ: OccurrenceSet, ctx: PackageCtx) -> GraphView
  .neighbors(iri, RelationFilter) -> impl Iterator
  .expand(origin, bounds) -> Vec<ExpandedEdge>
  .are_related(a, b) -> Option<RelationKind>
  .version_diff(prev: &Index, next: &Index) -> PathDiff  // path-set, not SymbolId
```

**Simpler path:** keep `GraphCorpus` as the intermediate; persist optional postcard(GraphCorpus) for hot-path, recompute from IR+occ for cold. Projection is pure and already deterministic-order.

**What cannot be recovered without extra data:**

- Live WOQL multi-package joins (stubs across packages need dep IR loaded)
- `symbol_id` instance salt (uploader concern)
- Subdocument random keys (irrelevant if Shape is rebuilt)

---

## 5. Tree-sitter

### 5.1 Two production paths

| Path | Entry | Output | Trees kept? |
|---|---|---|---|
| Package CST stage | `generate/cst.rs::extract` | `CstSet` of `ResolvedReference` | No — parse, walk, drop |
| Occurrences stage | `generate/occurrences.rs::build` | `OccurrenceSet` via LanguageSpec | No — same |
| Snippet embedding | `treesitter::parse_and_extract` | snippet text + `TreesitterRepr` JSON (sexp + spans + refs) | No live tree; sexp string only |
| IR FunctionBody | `syntax/body.rs` | `Yoke<FunctionBody, Arc<str>>` | Yes, in memory only |

### 5.2 Arborium / yoke

- Grammar load: `arborium::get_language(name)` (`cst.rs:95`, `treesitter/mod.rs:138–150`).
- Yoke used only for `ParsedBody` to keep Tree tied to source cart (`body.rs:20–32`). **Not** used in generate pipeline.
- Classifiers: flat `(kind, parent_kind)` for CST/snippet; structured LanguageSpec for occurrences.

### 5.3 What “store resolved trees + source efficiently” requires

Today:

- Source: already content-addressed per file (`SourceArchive` / `FileEntry`).
- Trees: **never stored**. Comments in `registry/blob/mod.rs:22–25` and `generate/cst.rs:3–7` assert trees are non-portable C memory.

Options for librarification:

| Strategy | Pros | Cons |
|---|---|---|
| **A. Reparse on demand** (status quo) | Zero tree storage; arborium is fast | CPU on every CST consumer; no cross-process tree share |
| **B. Store s-expression / custom CST** | Serializable | Huge; lossy for edits; not incremental |
| **C. Store tree-sitter external format** if available | Native | Portability across grammar versions; version pin must enter JobKey |
| **D. Store only derived artifacts** (OccurrenceSet + optional TreesitterRepr snippets) | Small; matches consumers | Cannot re-run novel walks without reparse |

**Recommendation:** D for default; A for local IDE-like tools with warm parse cache keyed by `ContentHash(file) × grammar_version`. Do **not** put live Trees in INDEX/REGISTRY blobs.

If full trees are mandated: key by `(file_hash, grammar_name, grammar_version)`, store vendor-neutral **node array** (type id, start, end, child indices) built from a walk — not raw C Tree. Version pin in JobKey tag.

### 5.4 Snippet path details

`parse_and_extract` (`treesitter/mod.rs:517–583`):

1. Parse full file  
2. Find enclosing function-like node or centered line window  
3. Re-parse snippet → sexp + walk refs  
4. Serialize `TreesitterPayload` JSON into `TreesitterRepr(Vec<u8>)`

Used for embedding chunks / examples, not the package blob IR section.

---

## 6. Symbol identity readiness

### 6.1 heart identity stack

**`Id<T>`** (`identity/id.rs:10–29`): branded UUID; `from_name(namespace, bytes)` = UUIDv5.

**Namespaces** (`namespace.rs:7–10`):

- PACKAGE = fixed u128 `…0001`
- SYMBOL = fixed u128 `…0002`

**`PackageId`** (`package/coordinates.rs:21–39`):

```
identity_bytes = len_pref(origin) ‖ 0 ‖ len_pref(name.canonical) ‖ 0 ‖ len_pref(version.canonical) ‖ 0
PackageId = UUIDv5(PACKAGE, identity_bytes)
```

**Includes version.** Same crate at 1.0.0 and 1.0.1 → **different PackageId**.

**`EntryUri`** (`identity/symbol.rs:20–47`):

```
canonical = package_id_string + "/" + path_segments joined by "/"
SymbolId = UUIDv5(SYMBOL, instance_token ‖ 0 ‖ canonical)
```

**Includes instance salt** (Terminus instance) and **versioned PackageId**.

### 6.2 Graph IRI vs SymbolId vs NudoxPath

```
NudoxPath::Local("foo::Bar")
  → graph IRI Symbol/rust%2Fmypkg%2Ffoo::Bar   (versionless package name)
  → EntryUri { package: PackageId(ver), path: ["foo","Bar"] }
  → SymbolId (instance-salted)
```

Graph `Symbol.uri` = `lang/package/fq` with `/` (`from_ir.rs:1109–1110`). Cross-store join intended on this uri / SymbolId; **today projection leaves `symbol_id: None`**.

### 6.3 Stability matrix (precise)

| Part of representation | (a) recompile | (b) change elsewhere | (c) rename | (d) signature change | (e) move module |
|---|---|---|---|---|---|
| NudoxPath | stable* | stable* | **breaks** | stable | **breaks** |
| Graph Symbol IRI | stable* | stable* | **breaks** | stable | **breaks** |
| heart SymbolId | stable* | stable* | **breaks** | stable | **breaks** |
| PackageId | stable | stable | stable | stable | stable |
| PackageId across versions | n/a | n/a | n/a | n/a | n/a — **always new** |
| ContentHash(file) | stable | stable if file untouched | may change | may change | may change |
| JobKey | stable | **changes** (source tree hash) | changes | changes | changes |
| Occurrence span | stable* | may shift if file edits | may shift | may shift | may shift |
| Implementation/Reference value_hash | stable if ends stable | — | changes | may change mentions | changes |

\*Assuming deterministic producers and ignoring HashMap serialization order.

### 6.4 Instability sources (checklist)

1. **Path spelling** — `::` vs `.`, crate rename `odd-duck`→`odd_duck` (`ra/ctx.rs:180`).  
2. **Local vs External** classification depends on which crate is “current”.  
3. **Alias HashSet** order in serde.  
4. **Index root_ids** construction order from HashMap iteration (`pipeline.rs:34–45` insert order follows input Vec — producers control).  
5. **SymbolId instance token** — two Terminus instances never share ids (by design).  
6. **Version inside PackageId** — cross-version identity **cannot** use SymbolId equality; must use FQ path matching (`resolution.rs`) or graph IRI.  
7. **No content hash of entry payload** — cannot detect “same path, different body” without full structural compare.  
8. **Graph subdocuments key=random** — not identity-bearing.  
9. **Floats in ConstExpr**.  
10. **Dual FQN expansion** in SymbolTable vs linker (`path_segments` splits `a.b::c`; linker `coordinates` does not always split dots the same way — risk of IRI vs resolve mismatch for some producers).

### 6.5 Cross-generation design feed

What exists that a lineage design can reuse:

- Versionless **graph IRI** / `uri` string as primary cross-version key  
- **NudoxPath FQ spelling** as package-local key  
- **PackageVersion.declares** set-diff (documented intent)  
- **registry `resolution::diff`** FQ + name/kind matching on heart Symbols  
- **per-file ContentHash** for file-level incrementality  
- **Occurrence** spans for definition coordinates  

What is missing:

- Explicit `lineage_of: Option<SymbolId/IRI>` edge  
- Canonical **per-entry content hash** (payload only, exclude docs? include?)  
- Versionless PackageId / “package stem id”  
- Path-normalization contract shared by all producers  
- OccurrenceSet in blob manifest  
- Uploader stamp of `symbol_id` wired through compiler GraphCorpus

---

## 7. Incrementality

### 7.1 What exists today

| Level | Mechanism |
|---|---|
| Package job | JobKey CAS hit skips surface/cst/occ/archive rebuilds |
| File | SourceArchive per-file hashes; cross-version dedupe in object store |
| Manifest | identity_bytes / snapshot for freshness |
| Graph docs | value_hash on Implementation/Reference; Symbol upsert by @id |
| Heart symbols | resolution::diff for Stable/Moved/Removed/Added |
| IR entries | **No structural diff** between generations |

There is **no** IR-level or symbol-level re-embed/re-index pipeline driven by entry diffs. Fan-out materializes **all** package symbols (`server/poll.rs:164–175`).

### 7.2 Natural symbol-level diff seat

**Prefer IR level** (before graph):

```
diff_index(prev: &Index, next: &Index) -> EntryDiff {
  removed: paths in prev \ next
  added: paths in next \ prev
  changed: paths in both where entry_hash(prev[p]) != entry_hash(next[p])
  unchanged: rest
}
```

Why IR not graph-doc:

- Shape is complete (graph loses fields)
- NudoxPath keys are the unit producers already use
- Occurrences keyed by path; can invalidate only files whose ContentHash changed **or** whose defining entries changed
- Graph projection can be re-run only for changed IRIs + stubs they touch

Graph-doc-level diff is still useful for Terminus upsert minimization (skip unchanged Symbol JSON bodies).

### 7.3 Canonical per-symbol hash (from existing data)

**Proposed `entry_content_hash(entry: &Entry) -> ContentHash`:**

Inputs (canonical encoding):

1. kind tag  
2. name  
3. path encoding (Local/External + utf8 path + dependency)  
4. visibility  
5. sorted aliases  
6. documentation / deprecation / sorted doc_links (or exclude docs if “API identity” vs “doc identity”)  
7. **serde/postcard of `inner` with sorted maps**  

Missing for production-grade:

- Canonical serializer (BTreeMap discipline, no HashMap, no f64 or quantize floats)  
- Explicit policy: does doc-only change invalidate embedding? (probably yes for vector, no for graph edges)  
- Split hashes: `api_hash` (signature/shape) vs `doc_hash` vs `span_hash`  

**Package-level composition:** sorted fold of `(path, entry_hash)` → package IR fingerprint distinct from source JobKey (oracle/toolchain can change IR without source change — already in JobKey via producer_version + toolchain).

### 7.4 Incremental pipeline sketch

```
on commit:
  new_source_hashes = hash files
  changed_files = setdiff(old_archive, new_archive)
  if JobKey hit: stop
  re-run producer (today whole package — future: incremental producer)
  entry_diff = diff_index(old_ir, new_ir)
  for p in entry_diff.changed|added:
    re-embed, re-tantivy, re-project graph symbol
  for p in entry_diff.removed:
    tombstone sinks
  re-resolve occurrences for changed_files ∪ files containing changed defs
  re-emit References touching those symbols
```

**Blocker:** producers are whole-package (rust-analyzer load, etc.). Symbol-level incrementality of **downstream sinks** is still valuable even when IR production is whole-package.

---

## 8. File atlas (absolute paths + roles)

### IR

| Path | Role |
|---|---|
| `/Users/philocalyst/Projects/Backend/workspace/ir/lib.rs` | Module tree + Option\<Vec\> convention |
| `.../entry.rs` | NudoxPath, Index |
| `.../kind.rs` | Entry, Symbol, Visibility, TypedBinding, TypeAliasBody |
| `.../function.rs` | Function, Attribute |
| `.../record.rs` | Record, Field, SumType, SumVariant |
| `.../ty.rs` | Type universe |
| `.../generics.rs` | Generics, Constraint, ConstExpr, Kind, … |
| `.../parameter.rs` | Parameter variants |
| `.../module.rs` | Module |
| `.../protocols.rs` | TraitDef, TraitImpl, ReceiverKind |
| `.../primitives.rs` | Primitive, Width |
| `.../pipeline/pipeline.rs` | Ir\<Collected\|Indexed\> |
| `.../pipeline/parse_common.rs` | output_parameters_from_type, parameter_link_key |
| `.../syntax/occurrence.rs` | Occurrence contract |
| `.../syntax/types.rs` | ResolvedReference, ReferenceKind |
| `.../syntax/walker.rs` | walk_references |
| `.../syntax/body.rs` | Yoke FunctionBody |
| `.../Cargo.toml` | features, arborium, yoke |

### Graph / generate / treesitter

| Path | Role |
|---|---|
| `.../compiler/graph/model.rs` | Terminus document types |
| `.../compiler/graph/from_ir.rs` | project, GraphCorpus |
| `.../compiler/graph/link.rs` | Linker, IRIs |
| `.../compiler/graph/symtab.rs` | SymbolTable |
| `.../compiler/generate/mod.rs` | generate pipeline |
| `.../compiler/generate/surface.rs` | producer dispatch |
| `.../compiler/generate/cst.rs` | CstSet |
| `.../compiler/generate/occurrences.rs` | OccurrenceSet build |
| `.../compiler/generate/resolve.rs` | resolution ladder |
| `.../compiler/generate/source_archive.rs` | per-file digests |
| `.../compiler/generate/blob_info.rs` | snapshot fold |
| `.../compiler/generate/linked_data/emit.rs` | wave emit |
| `.../compiler/generate/linked_data/schema.rs` | JSON-LD context |
| `.../compiler/treesitter/mod.rs` | classifiers, parse_and_extract |
| `.../compiler/treesitter/spec.rs` | LanguageSpec |
| `.../compiler/treesitter/{rust,go,java,python,typescript,nix,csharp}.rs` | per-lang extractors |

### Heart / registry / runtime

| Path | Role |
|---|---|
| `.../heart/content.rs` | ContentHash, JobKey |
| `.../heart/identity/id.rs` | Id\<T\> |
| `.../heart/identity/symbol.rs` | EntryUri, SymbolId |
| `.../heart/identity/namespace.rs` | UUIDv5 namespaces |
| `.../heart/package/coordinates.rs` | PackageId derivation |
| `.../heart/symbol.rs` | heart::Symbol, coarse SymbolKind |
| `.../registry/blob/mod.rs` | BlobManifest, ReferenceSet |
| `.../registry/runtime/graph/mod.rs` | GraphStore trait |
| `.../registry/runtime/graph/expansion.rs` | BFS expand |
| `.../registry/runtime/graph/resolution.rs` | cross-version Symbol diff |
| `.../server/poll.rs` | sink materialization |

---

## 9. Gaps vs librarification goals

| Goal | Status |
|---|---|
| Store IR in INDEX + REGISTRY | Partial — IR in CAS via ir_ref; dual clients must share BlobManifest contract |
| Store resolved tree-sitter trees | **Not done** — trees transient; only spans/sexp |
| Store source efficiently | **Done pattern** — per-file BLAKE3 |
| Terminus only for hot packages | Projection pure enough to recompute; **no graph-over-blobs module yet** |
| Graph ops over IR blobs | Feasible via project()+indexes; API exists at registry GraphStore |
| Symbol identity across generations | Dual keys (IRI vs SymbolId); version in PackageId blocks SymbolId continuity; resolution::diff is FQ-based |
| Lineage links between IR generations | **Absent** |
| Symbol-level re-embed/re-index | **Absent** — whole package fan-out |
| Occurrence corpus as first-class blob | Generated but **not** in BlobManifest |
| Unified reference pipeline | CST + Occurrences dual; registry still CST-era references_ref |

---

## 10. Concrete recommendations (for architecture plan)

1. **Adopt NudoxPath FQ + versionless package stem as cross-generation identity;** treat graph `uri`/`symbol_iri` as the durable key; keep SymbolId as instance-local join key with documented version coupling.  
2. **Add `occurrences_ref` to BlobManifest** (or replace `references_ref`); stop dual-writing weak CstSet.  
3. **Define `entry_api_hash` / `entry_doc_hash`** with BTree canonical encoding; use for symbol-level sink invalidation even when producers stay whole-package.  
4. **Build `graph_over_ir`** crate: `project` + adjacency indexes + GraphStore impl over blobs; Terminus becomes cache for hot PackageIds.  
5. **Normalize path_segments once** — single function used by producers, SymbolTable, linker, EntryUri path construction.  
6. **Stamp `Symbol.symbol_id` in one place** (uploader) from EntryUri; never leave dual-null.  
7. **Parse cache keyed by file ContentHash × grammar version** for “resolved trees” without storing C Trees.  
8. **Split JobKey invalidation:** surface vs occ vs archive already tagged — extend with entry-hash map artifact for partial fan-out.  
9. **Deprecate FunctionBody-in-IR** path; occurrences only.  
10. **Document identity matrix** in heart README: PackageId (versioned), stem id (new), EntryUri, SymbolId, graph IRI.  
11. **Fix Index comment** (“integer IDs”) and entry.rs architecture note about unsafe structure — decide on arena/graph-native IR or keep flat map.  
12. **Canonical serde for Index:** serialize entries sorted by path string to make IR blobs byte-stable.

---

## 11. Open questions / risks

1. Should PackageId remain versioned? If yes, cross-version SymbolId equality is impossible without a new stem id.  
2. Is graph IRI package name enough across registry renames / scoped npm names?  
3. How should dual-emitted methods (inline TraitMethod + Entry::Function) identity-collapse?  
4. Full GraphCorpus vs heart Symbol insert_symbols — which is production path? Divergence risk.  
5. Oracle Confidence tier: when will RA/go-types attach true Oracle resolutions?  
6. Facet feature: implement or remove to reduce Cargo surface.  
7. Grammar version pins for arborium 2.18 — must enter cache keys if trees/sexp stored.  
8. Float and HashMap determinism for content hashes.  
9. Multi-package stub resolution without Terminus (load dep IR set).  
10. Whether documentation-only changes should re-embed (cost vs freshness).  
11. GUI still on legacy Entry URIs — migration scope.  
12. SumType/TraitDef shape losses — are they acceptable for graph-only queries or must Shape expand?

---

## 12. Executive summary

The live IR is a **language-agnostic, flat, path-keyed declaration store**: `Index` maps `NudoxPath` → `Entry`, where each entry is a `Symbol<T>` shell (name, path, aliases, visibility, docs, deprecation, doc_links) plus a rich kind payload (functions, records, sum types, traits/impls, typed bindings, modules). Nesting is explicit via `members: Option<Vec<NudoxPath>>`, not tree-shaped storage. Types are a large closed enum with stringly `TypeReference` leaves; resolution to other entries is deferred. The IR intentionally uses `Option<Vec<T>>` for absent-vs-empty semantics and is serde-first (facet is unused). **There is no SymbolId inside IR.**

Generation materializes four artifacts under a BLAKE3 **JobKey** (producer‖toolchain‖source‖lock): surface Index, CstSet, OccurrenceSet (`occ-v1`), and per-file source digests. Registry blobs persist **IR + source files + legacy ResolvedReference sets** — **not** OccurrenceSet and **not** tree-sitter Trees. Trees are always transient; yoke exists only for in-memory `FunctionBody`. The REFERENCES-PLAN largely landed: LanguageSpec extractors, shared SymbolTable, resolution ladder, Occurrence contract, and graph Reference projection with a strict assertion policy. Dual CST/occurrence pipelines remain debt.

Graph lowering (`from_ir::project`) builds a Terminus-native **GraphCorpus**: versionless Package/Symbol IRIs (`%2F`-encoded), PackageVersion.declares membership, Shape subdocuments, Implementation/Reference value_hash relations, and total name resolution via stubs under `~extern`. Symbol→symbol edges (member_of, implements, extends, mentions, takes, returns) support the operations GraphStore exposes (occurrences, references, relatedness, BFS expand). Projection is pure and sortable — a **graph-over-blobs** module can recompute it without Terminus. Shape drops several IR fields; `symbol_id` is never stamped in the compiler.

**Identity is the critical readiness gap.** heart `PackageId` and therefore `SymbolId` **include package version** and an **instance salt**; graph Symbol IRIs are **version-agnostic**. Cross-generation sameness cannot be SymbolId equality; it must be FQ path / graph uri, as `registry/runtime/graph/resolution.rs` already assumes for heart Symbols. No IR entry content hash, no lineage edges, and no symbol-level sink invalidation exist — incrementality stops at whole-package JobKey CAS and per-file source dedupe.

For librarification: persist OccurrenceSet + source + IR; reparse trees on demand (or optional grammar-versioned node arrays); use versionless IRI/path as lineage key; add entry-level api/doc hashes for partial fan-out; implement GraphStore over projected IR for cold packages; keep Terminus as a hot cache. Normalize path segmentation across producers and the linker before trusting cross-layer joins.

---

## 13. Appendix — Confidence enum and graph policy (copy for design docs)

```
Syntactic < Suffix < Index < Import < Oracle
Graph asserts Reference only if:
  role=Reference ∧ anchored ∧ confidence≥Index
  ∧ kind∈{FunctionCall,MethodCall,TypeReference,MacroInvocation,Import}
  ∧ enclosing≠None
```

Evidence: `occurrence.rs:36–47`, `from_ir.rs:1159–1173`.

## 14. Appendix — Producer path examples (Rust)

```
Local crate item:  NudoxPath::Local("my_crate::mod::Type")
Path dep item:     NudoxPath::External { dependency: "helper", path: "Marker" }
Builtin:           NudoxPath::Local("i32")  // no crate
```

Evidence: `compile/rust/ra/ctx.rs:402–431`.

## 15. Appendix — Wave emit order

1. Package nodes  
2. Symbols bare (no edges)  
3. Symbols full  
4. Implementation + Reference  
5. PackageVersion (declares)

Evidence: `linked_data/emit.rs:116–144`.

## 16. Appendix — heart Symbol vs graph SymbolKind

heart (`heart/symbol.rs:59–67`): Function, Type, Module, Constant, Variable, Trait, Impl, Other — **coarse**.

graph (`model.rs:927–944`): Module, RecordType, Info, UnionType, TraitDef, TraitImpl, SumType, Function, TypeAlias, Constant, Variable, Macro, PrimitiveType, Field, Event, Unresolved — **IR-aligned**.

Mapping for search/graph joins is non-trivial (TODO in heart/symbol.rs:9).

## 17. Appendix — Option\<Vec\> field inventory (semantic)

Fields that use absent-vs-empty (non-exhaustive, high traffic):

- Function: input_parameters, output_parameters, attributes, generics, overloads, members, implemented_protocols  
- Record: call_signatures, constructors, methods, index_signatures, super_types, members, implemented_protocols  
- TraitDef: almost every list field  
- Symbol shell: aliases, documentation, deprecation, doc_links  

Graph collapses to empty Vec/BTreeSet (`from_ir.rs:18–19, 91–93`).

## 18. Appendix — ResolvedReference wire vs serde

IR type has serde under feature (`types.rs:1–7`). Registry still uses bespoke postcard wire (`blob/mod.rs:208–336`) for validation (span order, kind FromRepr, UTF-8 paths). Either path is fine; document single codec for librarified INDEX/REGISTRY.

## 19. Appendix — Graph-over-blobs pseudocode

```text
fn load_package_graph(store, package_id, version) -> GraphView {
    let man = store.get_manifest(package_id)?;
    let index: Index = postcard::from_bytes(&store.get(man.ir_ref)?)?;
    let occ: OccurrenceSet = load_occ(store, man)?; // new section
    let ctx = PackageCtx { language, package, version: Some(version) };
    let corpus = project(&index, &occ, ctx);
    GraphView::index(corpus)
}

impl GraphStore for GraphView {
    fn get_references(&self, id) -> stream of sources that Reference→target
    fn get_occurrences(&self, id) -> stream of symbols whose shape mentions id
    fn are_related(&self, a, b) -> edge lookup in adjacency
}
```

## 20. Appendix — Incrementality artifact proposal

New CAS object `EntryFingerprintSet`:

```text
struct EntryFingerprintSet {
  entries: BTreeMap<String /*path*/, [u8;32] /*api_hash*/>,
  docs:    BTreeMap<String, [u8;32]>,
}
```

JobKey child tag `b"entry-fp"`. Fan-out compares previous vs next fingerprints; only dirty paths hit Tantivy/Qdrant/Terminus.

## 21. Appendix — Test anchors (behavior contracts)

| Test area | Path |
|---|---|
| Graph project edges/stubs | `compiler/graph/from_ir.rs` tests ~1303+ |
| Resolution ladder | `compiler/generate/resolve.rs` tests |
| Linked data waves | `compiler/tests/linked_data_emit.rs` |
| References e2e | `compiler/tests/references_e2e.rs`, `references_live.rs` |
| Treesitter extraction | `compiler/tests/treesitter_extraction.rs` |
| Blob hash pins | `registry/tests/blob_hash_pins.rs` |
| SymbolId determinism | `registry/tests/global_id_determinism.rs`, `metadata_guid_hash.rs` |
| IR fidelity snaps | `compiler/tests/snap_*.rs` |

## 22. Appendix — Non-goals of IR (from README)

`workspace/ir/README.md:10–11`: IR is **not** meant to represent original syntax form. Tree-sitter/source archive cover that. Confirms storing trees is a storage concern, not an IR shape concern.

## 23. Final readiness scorecard

| Capability | Score (0–5) | Notes |
|---|---|---|
| IR expressiveness | 5 | Broad multi-language surface |
| IR identity rigor | 2 | Path-only; no content hash; spelling drift |
| Serialization determinism | 2 | HashMaps, floats, dual codecs |
| Blob completeness | 3 | Source+IR; missing occ; no trees (ok) |
| Occurrence contract | 4 | Solid; storage + oracle incomplete |
| Graph projection | 4 | Pure, tested; shape loss; id stamp gap |
| Graph ops defined | 4 | Clear trait; dual write paths |
| Tree-sitter storage | 1 | Explicitly not stored |
| Cross-gen identity | 2 | Pieces exist, not unified |
| Symbol incrementality | 1 | Package-level only |

**Bottom line:** IR and graph projection are strong enough to build graph-over-blobs and dual stores. Symbol-level incrementality and cross-generation identity need **new contracts** (versionless stem, entry hashes, occurrence persistence, path normalization) before “only changed symbols re-embed” is honest.

---

*End of audit. All line citations refer to the live tree under `/Users/philocalyst/Projects/Backend/workspace` as of 2026-07-16.*

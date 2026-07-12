# Performance & Memory Audit — Nudox Backend

**Scope:** all ~22.5k lines under `source/` across the 14 workspace crates.
**Method:** the central IR data model (`intermediate-representation/`, `core/types/`) was read directly and sized by hand; the runtime subsystems (parsers, pipeline, ingest, search, storage, identity, emit, orchestrator) were swept file-by-file. Every claim below is line-referenced. Sizes are for 64-bit (`std::mem::size_of`, pointer = 8B, `String`/`Vec` = 24B, `Box`/`Arc` thin = 8/16B, std `HashMap`/`HashSet` = 48B).

**State of the tree:** *none* of `rustc-hash`, `ahash`, `smallvec`, `compact_str`, `triomphe`, `lasso`/`string-interner`, `im`, `ecow` are dependencies yet. Every recommendation to adopt one is net-new. All maps/sets in the codebase use std SipHash.

---

## 0. TL;DR — the five things that matter

1. **The IR is an AoS of fat, deeply-owned enums that get deep-cloned constantly.** `Type` is ~112 B per node (should be ~56), `Entry` is ~424 B for *every* variant including 1-field `Field`/`Constant` (should be ~16). Fix the layout (§1) and the clone storm (§2) collapses on its own.
2. **`NudoxPath` is a `PathBuf`** used for logical `::`-separated symbol paths, forcing `to_string_lossy` + `\\`→`/` + `Vec<String>` rebuilds on the hottest identity paths, and making path segments un-shareable. Interning segments as `Arc<str>` is the single highest-leverage structural change (§3).
3. **The JSON-LD emit path materializes every document 2–3× as `serde_json::Value`** and **deep-clones the `@context` into every node** (§5). This is both the worst CPU sink and the worst memory blowup in emit.
4. **Ingest/search are full of serial `.await` loops, per-request client construction, blocking IO under async locks, and whole-record clones (vectors + source text)** (§6, §7).
5. **Storage commits/flushes per item:** Tantivy commits per document, Qdrant `wait(true)` per blob, the registry rewrites its entire JSON file on every state change, and git version resolution re-walks full history × full tree (§8).

Adopt `rustc-hash` + `smallvec` + an `Arc<str>` interner crate-wide and you claw back a large constant factor before touching a single algorithm.

---

## 1. The central problem: IR memory layout (Array-of-Structs of fat enums)

The IR is a tree of `enum`s stored by value in `Vec`/`HashMap`. Two compounding issues: **enum size bloat** (every value pays for the largest variant) and **`Box`-per-node pointer chasing** (cache-hostile traversal). Concrete sizings:

### 1.1 `Type` is ~112 B; it should be ~56 B — `ty.rs:11`
The largest variant dominates the whole enum:
- `QualifiedPath` = `String`(24) + `Option<Vec>`(24) + `Box<Type>`(8) + `Option<TypeReference>`(48) = **104 B**
- `FunctionPointer` = 3× `Option<Vec>` = **72 B**
- `ConditionalType` = 4× `Box` = 32 B; `MappedType` ≈ 56 B; `TypePredicate` ≈ 40 B

So `Type` ≈ 104 + tag ≈ **112 B**. That means `Type::Infer`, `Type::Never`, `Type::Primitive(Bool)` — zero-payload or 1-byte variants — each occupy **112 bytes**, and every `Tuple(Vec<Type>)`/`Union(Vec<Type>)`/`Intersection(Vec<Type>)` (`ty.rs:38,77,81`) allocates 112 B *per element*.

**Fix:** box the rare, fat variants:
```rust
QualifiedPath(Box<QualifiedPath>),
FunctionPointer(Box<FunctionPointer>),
Conditional(Box<ConditionalType>),
Mapped(Box<MappedType>),
```
After boxing, the largest inline variant is `TypeReference`/`DynTrait` (48 B) → `Type` drops to **~56 B**. Every type-tree allocation, clone, and `memcpy` in both parsers roughly halves. `QualifiedPath`/conditional/mapped are exotic (TS-heavy) and already behind a pointer in practice, so the extra indirection is on the cold side of the distribution. **Severity: HIGH (memory + clone cost).**

### 1.2 `Entry` is ~424 B for *every* variant — `kind.rs:62`
`Entry`'s largest payloads are `Symbol<Function>` and `Symbol<Record>`. Sizing `Symbol<T>` (`kind.rs:25`):
`name: String`(24) + `path: NudoxPath`(56, see §3) + `aliases: Option<HashSet<Vec<String>>>`(48) + `visibility`(1→padded) + `documentation: Option<String>`(24) + `inner: T`.

`Function` (`function.rs:10`) ≈ 272 B (2× `Option<Vec<Parameter>>`, `Option<HashMap>`, attributes, generics, receiver, `Option<Vec<Function>>` overloads, two `Option<Vec<NudoxPath>>`, and `Option<ParsedBody>`). So `Symbol<Function>` ≈ **~424 B**.

Because `Entry` is sized to its largest variant, **`Entry::Field(Symbol<())>`, `Entry::Constant`, `Entry::Variable`, `Entry::Macro`, `Entry::PrimitiveType`, `Entry::Event` — all of which carry `Symbol<()>` (~152 B of real data) — still occupy ~424 B each.** The pipeline stores these in `Vec<Entry>` and `HashMap<NudoxPath, Entry>` (`entry.rs:32-35`, `pipeline.rs:13`), so for a crate dominated by fields/constants you waste ~270 B × (#small entries). On a 50k-entry crate that is multiple **megabytes of pure padding**, and it wrecks cache density during any pass over the index.

**Fix:** box the heavy payloads so the discriminant + a pointer is all each slot costs:
```rust
Function(Box<Symbol<Function>>),
RecordType(Box<Symbol<Record>>),
TraitDef(Box<Symbol<TraitDef>>),
TraitImpl(Box<Symbol<TraitImpl>>),
// …box everything for uniformity → Entry ≈ 16 B
```
This shrinks `Entry` from ~424 B to ~16 B, makes `HashMap<_, Entry>` dense and cache-friendly, and cuts every `Entry::clone()` (rampant in the Rust parser, §4) to a single pointer-deep copy of only the real data. **Severity: HIGH (memory).**

### 1.3 `Symbol<T>::clone_with` clones name/path/aliases/docs on every kind dispatch — `kind.rs:46`
`convert_item` in the Rust parser builds a `symbol_template` and then every match arm calls `symbol_template.clone_with(inner)`, which clones `name`, `path` (a `PathBuf`), `aliases` (a whole `HashSet<Vec<String>>`), `visibility`, and `documentation` (`kind.rs:48-53`) — and the `Module` arm clones the template *first* (`parse.rs:442`). Net: name/aliases/docs cloned 2–3× per item. **Fix:** add a consuming `with_inner(self, inner) -> Symbol<U>` and *move* the template into the single matched arm. **Severity: HIGH.**

### 1.4 `aliases: Option<HashSet<Vec<String>>>` is an allocation pathology — `kind.rs:28`
A `HashSet` (48 B + heap table) of `Vec<String>` (each path = Vec + N `String`s) per symbol, hashed with SipHash, where the set almost always has 0–2 entries. This is the worst container choice in the IR: maximum per-element allocation, maximum hashing overhead, for a tiny collection. **Fix:** `Box<[CompactPath]>` (or `SmallVec<[CompactPath; 2]>`) where `CompactPath = Box<[Arc<str>]>` interned segments (§3). Dedup, if needed, can be a sort+dedup on the small slice. **Severity: HIGH (memory + alloc count).**

### 1.5 `Option<Vec<T>>` everywhere — semantic, not memory, cost
`Vec` has a niche, so `Option<Vec<T>>` is 24 B (same as `Vec`). The cost is *ergonomic and branchy*: `None` vs `Some(vec![])` is an ambiguity the whole codebase must handle, and every access is an extra branch. Across `Function`, `Record`, `TraitDef`, `TraitImpl` there are ~30 such fields. **Fix (low priority):** prefer plain `Vec<T>` (empty = absent) except where `None` is semantically distinct from empty. Reduces branches and `match` noise. **Severity: LOW.**

### 1.6 `nonempty` opportunities (the user asked specifically)
A handful of fields are invariantly non-empty and could use a `NonEmpty`/`Vec1` type to make the invariant load-bearing and drop runtime "is it empty" checks:
- `Type::Tuple`/`Union`/`Intersection`/`Sum` — a 0-element union/intersection is meaningless; `Tuple(vec![])` is specifically the Unit type (`ty.rs:37`), so Tuple should arguably be `Option<NonEmpty<Type>>` or a dedicated `Unit` variant + `Tuple(NonEmpty<Type>)`.
- `TraitRef.args` is genuinely sometimes empty (keep `Vec`).
- `Generics { params, constraints }` — a `Generics` with both empty should be `None` at the `Option<Generics>` site, so the inner vecs being possibly-empty is fine.
`nonempty` is mostly a *correctness/clarity* win here, not a memory one; I'd apply it only to `Type::Sum`/`Union`/`Intersection` to encode the invariant. **Severity: LOW (clarity).**

### 1.7 Cache-friendliness: AoS → consider an arena/SoA for the type tree
Every `Box<Type>` is an independent heap allocation; walking a nested type (`Vec<Vec<...>>`, conditional types, `for<'a>` trait bounds) is pointer-chasing across the heap — terrible for the prefetcher. The clean structural fix for a read-mostly IR is an **arena**: store all `Type` nodes in one `Vec<Type>` and replace `Box<Type>` with a `u32` index (`TypeId(u32)`). Benefits: (1) `Type` shrinks (indices are 4 B vs 8 B pointers and have a niche), (2) traversal is linear and cache-dense, (3) clone becomes "copy a range / share the arena via `Arc`", (4) structural sharing of identical subtrees becomes trivial (hash-cons). This is a larger refactor; flag it as the **long-term** direction once the boxing wins (§1.1–1.2) are banked. **Severity: MED (architectural, high ceiling).**

---

## 2. The clone storm (downstream of §1)

Because the IR types are fat and `Clone`, the code clones them constantly. The worst sites:

| Location | What's cloned | Fix |
|---|---|---|
| `rust/parse.rs:259-274` | `Entry` cached *and* re-cloned on every cache hit | cache `Arc<Entry>`; box payloads (§1.2) |
| `rust/parse.rs:376-388,442` | name/path/aliases/docs cloned 2–3× via `clone_with` | consuming `with_inner` (§1.3) |
| `typescript/parse.rs:322` | **whole deno-doc `Document`** cloned per specifier | borrow the symbols |
| `typescript/parse.rs:456,625,674` | whole `Symbol` cloned per path dispatch | borrow for the parse duration |
| `orchestrator/lib.rs:63` | **entire `BlobInfo`** (source text + all embedding vectors) cloned to set one `Option` field | `&mut info`, mutate in place |
| `ingest/embedding.rs:273` | `record.clone()` (vector + text) to build a payload that uses neither | `mem::take(&mut record.vector)` then move |
| `search.rs:113` (`SymbolMatch.blob: BlobInfo`) | full source + vectors per result × `limit` | `Arc<BlobInfo>` or a lightweight projection |
| `search/session.rs:92,132` | whole session graph cloned + serialized on every merge / response | `Arc<JsonValue>` docs; persist deltas |
| `terminus/schema.rs:98,138` | whole `Value` cloned on every insert / `into_documents` | insert by move; `mem::take` + drain |

The throughline: **wrap the few genuinely-shared heavy payloads in `Arc` and make the consuming paths consume.** `Arc<str>` for `raw_code`/`treesitter_repr` (`core/types/primitives.rs:33,54`) turns `BlobInfo::clone` from a multi-KB deep copy into a refcount bump and fixes the orchestrator/ingest/search clone sites at once.

---

## 3. `NudoxPath = PathBuf`: the identity hot-path tax — `entry.rs:18`

`NudoxPath::{External{path: PathBuf, dependency: String}, Local(PathBuf)}` models logical `::`-separated symbol paths as *filesystem* paths. Consequences, all on per-symbol hot paths:

- **`path.rs:14` `nudox_path_to_str`** — `to_string_lossy().replace('\\','/')`. On Unix `\\` never appears, but `replace` still scans + allocates a fresh `String` unconditionally. Called via `entry_uri`, `kind_uri`, `uri_path` — multiple times per symbol. *Fix:* `if s.contains('\\')` guard to keep `Cow::Borrowed`; prefer `path.to_str()` over lossy.
- **`path.rs:24` `path_segments`** — `Vec<String>` with one `String` per segment, fresh every call; external arm clones `dependency` too. *Fix:* `SmallVec<[Cow<str>; 8]>` or borrow.
- **`path.rs:36` `fq_name`** — `path_segments(path).join("::")` allocates a `Vec<String>` (N+1 allocs) only to immediately `join` and drop it. *Fix:* write segments straight into one `String` with `write!`.
- **`pipeline.rs:30`** — `HashMap<NudoxPath, Entry>` hashes a `PathBuf` (OS-string hashing) with SipHash.
- Path **segments are un-shareable**: `serde`, `tokio`, `std`, `sync`, `mpsc` recur across thousands of entries, each its own heap `String`.

**Fix (highest-leverage structural change):** model the path as interned segments.
```rust
type Sym = Arc<str>;                 // interned via a crate-wide pool
enum NudoxPath { Local(Box<[Sym]>), External { dependency: Sym, path: Box<[Sym]> } }
```
With an interner (`lasso` for `Spur` u32 keys, or a `DashMap<Arc<str>>` pool), every repeated segment collapses to one allocation + a refcount/`u32`. This eliminates *all* the `to_string_lossy`/`replace`/`Vec<String>` churn, makes `NudoxPath` `Copy`-ish-cheap to clone, shrinks it from 56 B to ~16–24 B (and `Symbol`/`Entry` with it), and makes path `HashMap` keys hash a few `u32`s. **Severity: HIGH (memory + alloc count + hashing), the keystone refactor.**

Related allocation storm it fixes: `rust/parse.rs:17` `id_to_paths: HashMap<Id, HashSet<Vec<String>>>` and the BFS at `parse.rs:143-198` that does `current_path.clone()` + `name.clone()` + `add_path`-clone for every child. Interned segments + `FxHashMap` gut this.

---

## 4. Hot path: parsing & tree-walking

### 4.1 CRITICAL — `walk_references` extracts `utf8_text` for *every* node — `syntax/walker.rs:19` (verified)
```rust
let node = cursor.node();
let parent_kind = node.parent().map(|p| p.kind());   // upward pointer-chase per node
let span = node.byte_range();
if let Ok(name) = node.utf8_text(source.as_bytes())   // UTF-8 re-validate whole subtree span
    && let Some(rr) = classify(name, node.kind(), parent_kind, span.clone()) { … }
```
Only leaf-ish tokens (`identifier`, `type_identifier`, `field_identifier`, `scoped_identifier` — see `treesitter.rs:42`) ever classify to `Some`. But `utf8_text` is called on **every interior node** (`source_file`, blocks, expressions), re-slicing and re-validating that node's entire source span — O(nodes × span_len), effectively O(n²) over the snippet. This runs on every embedded chunk.
**Fix:** gate on `node.kind()`/`parent_kind` first; only call `utf8_text` once the node is a known candidate. Also maintain an ancestor-kind stack from the cursor instead of `node.parent()` per node. **Runs on the hottest loop in the parser.**

### 4.2 CRITICAL — source file re-read & re-split per function — `rust/mod.rs:286-312`
`source_map_from_crate` does `fs::read_to_string(&source_file)` *inside the per-function loop*, then `source.lines().enumerate().filter(...).collect::<Vec<_>>().join("\n")` — a file with K functions is read from disk and fully scanned K times. **Fix:** read each file once into a `HashMap<PathBuf, Arc<str>>`; precompute line byte-offsets once and slice by byte range.

### 4.3 HIGH — source parsed twice — `pipeline/treesitter.rs:178-210`
Full `raw_code` is parsed to find the enclosing fn, then the extracted `snippet` is parsed *again* with a fresh `Parser`. When no enclosing fn is found (the common case), snippet == whole source and you parse identical bytes twice. **Fix:** reuse the first `Tree` when `snippet_span == [0, len]`.

### 4.4 HIGH — `format!("{:?}", ty)` to stringify whole type trees — `rust/parse.rs:1260,1338,1348,1377…`, `typescript/parse.rs:1507,1610`
Debug-formatting an entire parsed `Type`/`Term`/`WherePredicate` into a `String` per generic arg / predicate — large allocation *and* semantically lossy. **Fix:** carry the structured `TypeExpr`; if a string is truly needed, render with a purpose-built formatter.

### 4.5 HIGH — O(n) `ends_with` map scans with in-loop `format!` — `rust/parse.rs:1066`, `typescript/parse.rs:1759`, plus `typescript/parse.rs:1132` O(methods²) class grouping
`resolve_path_to_id` recomputes `format!("::{path}")` *inside* a linear scan of `path_to_id` for every type resolution. `resolve_ir_type_to_entry_id` does the same `ends_with` scan per parameter. `parse_class_def` re-iterates all methods per method to group same-name overloads. **Fix:** hoist the `format!`; build a reverse index keyed by last segment once; group methods in a single pass via `HashMap<&str, Vec<&…>>`.

### 4.6 MED — `parse_common.rs:3` allocates a 1-element `Vec<Parameter>` per return type
`output_parameters_from_type` wraps every function/method return in `vec![LiteralParameter{…}]`. **Fix:** `SmallVec<[Parameter; 1]>`, or store the return as `Option<Box<Type>>` directly.

### 4.7 MED — `yoke` is used *correctly* — note for the record
`ParsedBody = Yoke<FunctionBody, Arc<str>>` (`syntax/body.rs:20`) stores source once behind `Arc<str>` and the tree borrows it — **source text is not duplicated**, which is the right call. The only caveat: `FunctionBody: Clone` (`body.rs:8`) clones the whole `tree_sitter::Tree`; keep `body: None` until after parsing (the parsers already do) and never clone a populated `Function`.

---

## 5. Hot path: JSON-LD emit (`document/` and the duplicated `server/emit/`)

> **Duplication, verified:** `server/src/emit/ld.rs` is **byte-identical** to `document/src/ld.rs` (modulo the `use` path), and `server/src/emit/mod.rs` is functionally identical to `document/src/emit.rs` but **already diverging** in module wiring. Every finding below applies to both copies. *Fix:* delete one and `pub use document::{…}` — halves compiled code and stops the drift. **Severity: HIGH (maintenance + binary size).**

### 5.1 CRITICAL — triple `Value` materialization — `ld.rs:145-303` + `emit.rs:172-179`
Each `LDRecord`/`LDFunction`/… builds every inner field via `json!(x)` into a `serde_json::Value` (a boxed tree: every object a `Map<String,Value>`, every key a `String`, every node heap-allocated). These trees are embedded into the LD struct, then `serde_json::to_value(to_emit)` (`emit.rs:175`) serializes the whole thing into **another** full `Value` tree, then it's serialized to bytes downstream. That's 2–3× redundant materialization per document. **Fix:** implement `Serialize` directly on the LD types borrowing the IR (`#[serde(flatten)]`, borrowed `&str`), serialize straight to the output writer, delete the `json!` intermediates and the `to_value` round-trip.

### 5.2 CRITICAL — `@context` deep-cloned into every document — `emit.rs:132,177`
`json!(ctx.context())` / `ctx.context().clone()` embeds a full deep copy of the (identical) context object into *every* emitted node. Memory ≈ context_size × N_docs, plus a `Value`-tree alloc per doc. **Fix:** reference the context (`"@context": "<url>"`) or attach it once to a graph envelope / share via `Arc<Value>`.

### 5.3 CRITICAL — `DocStore::insert` clones key + whole `Value` every call — `schema.rs:98`
`self.docs.insert(uri.clone(), value.clone())` deep-clones the entire document tree *and* the URI on every insert (≈2× per symbol), purely to keep `value` for a rare collision compare. **Fix:** insert by move; inspect the returned old value; only compare on `Some`.

### 5.4 CRITICAL — `emit` owns `self` but matches `&self` and clones everything — `emit.rs:16-109`
`emit(self)` takes ownership yet the giant match borrows and `.clone()`s `members`, `implemented_protocols`, `aliases` (`Vec<Vec<…>>`), `documentation`, `name`. All movable. Plus `emit.rs:111-122` calls `ctx.entry_uri(member)` (which itself runs the full `EntryUri::new` allocation storm) and then `.as_str().to_owned()` — a **redundant second clone** of the just-allocated URI — for every member/protocol. **Fix:** destructure and move; drop the `.as_str().to_owned()`; cache rendered URIs.

### 5.5 HIGH — `DocStore` redundant re-sort & clone-instead-of-drain — `schema.rs:120-142`
`documents_sorted`/`documents_cloned`/`into_documents` collect keys, `sort()`, then re-index the `BTreeMap` — but `BTreeMap` *already* iterates in sorted order, so the whole dance is dead work. `into_documents` takes `self` by value yet clones every `Value` instead of draining. **Fix:** `self.docs.iter()` / `mem::take(&mut self.docs).into_values()`.

### 5.6 HIGH — `LDInheritor` size bloat — `ld.rs:58`
Sized to its largest variant (`TraitImpl` with ~7 `Option<Value>` + 2 `Value`), so unit variants (`Module`/`Info`/`None`) pay for it. **Fix:** `Box` the big variants.

---

## 6. Hot path: ingest & embedding

### 6.1 CRITICAL — embedding response parsed via generic `Value` — `embed/src/remote.rs:116`
`resp.json::<serde_json::Value>()` then `json["data"][0]["embedding"].as_array()...map(as_f64 as f32).collect()`. A 1536-float vector becomes 1536 heap `Value::Number` nodes, then re-iterated with an `f64→f32` round trip. **Fix:** typed `#[derive(Deserialize)] struct{ data: Vec<Emb> }`, `Emb{ embedding: Vec<f32> }` — one allocation, no `Value` tree.

### 6.2 HIGH — one HTTP request per symbol; no embedding batching — `embedding.rs:155-198`, `remote.rs:95`
The OpenAI-compatible API takes an array of inputs; the code sends one input per request, capped at 16 concurrent. **Fix:** batch 64–256 inputs per request → 1–2 orders of magnitude fewer round-trips. This is the dominant ingest latency.

### 6.3 HIGH — serial `.await` ingest loops
- `ingest/sink.rs:182` — orchestrator ingests each symbol, awaiting before the next.
- `search/qdrant.rs:45` — queries each collection sequentially (+ clones the query vector per collection).
- `search/mod.rs:192` — resolves each match over the network one at a time (+ `result.clone()`).
**Fix:** `buffer_unordered(K)` / `JoinSet`. (Note `search/mod.rs` shares `&mut cache` — drop it for the fan-out or use a concurrent map.)

### 6.4 HIGH — per-request client/provider construction
- `search/qdrant.rs:33,36` — new `OpenAIEmbeddingProvider` (new `reqwest::Client`, re-reads `OPENAI_API_KEY`) **and** new `Qdrant` per `/search`.
- `ingest/qdrant.rs:17,74` — new `Qdrant` per upload, twice (upload + `ensure_collection`).
**Fix:** build one of each at startup, store in `AppState` behind `Arc`. Connection pools are being thrown away every request.

### 6.5 MED — per-symbol string re-allocation in the projection — `parsed_symbol.rs:228-238`, `embedding.rs:142,177`
URI allocated twice (`uri`/`record_key`); `coord.package`/`coord.version` (already `Arc<str>`) re-`.to_string()`-ed into owned `String`s per document; model name re-allocated per record and per provider clone. **Fix:** thread `Arc<str>` through `EmbeddingDocument`/`EmbeddedRecord`; share one `Arc<str>` URI.

### 6.6 MED — `build_entry_embedding_text` allocates ~4–7 throwaway strings — `embedding_types.rs:97`
`Vec<String>` of `format!`ed parts then `join`. **Fix:** one pre-sized `String` + `write!`.

### 6.7 LOW — whole-corpus-in-memory — `embedding.rs:175`, `sink.rs:290`
All embedded records (with vectors) buffered, then all `PointStruct`s built, before any upload → peak ≈ 2× corpus. **Fix:** stream records into bounded upsert batches.

---

## 7. Hot path: search & sessions

### 7.1 CRITICAL — blocking `fs::write` under the tokio `Mutex` on every merge — `search/session.rs:56-96`
`persist_session` does synchronous `fs::write(to_vec_pretty(whole graph))` *while holding the async lock*, blocking a tokio worker and globally serializing session writes. Compounded by **cloning + re-serializing the entire accumulated graph on every merge** (`session.rs:92`) — O(n²) over a session's life — and `to_response` deep-cloning every node doc + edge (`session.rs:132`). **Fix:** serialize outside the lock → `tokio::fs::write`/`spawn_blocking`; persist deltas, not the whole graph; `Arc<JsonValue>` docs so responses share.

### 7.2 HIGH — Tantivy `index_batch` holds a blocking `std::Mutex` + commits inside async — `search/text.rs:64`
CPU/IO-heavy `add_document` per symbol + `commit()` (fsync) on the runtime thread. **Fix:** `spawn_blocking` the whole batch.

### 7.3 HIGH — N+1 over-fetch on the query path — `search/symbols.rs:162-200`
Per merged result: serial `blobs.get(...)` (full `BlobInfo` with source + vectors) just to read `metadata.lang`/`repo_id`/`kind`, then a *second* serial N+1 for occurrences (`:200`), all **before** `truncate(limit)`. **Fix:** filter/score on cheap index payloads; fetch full blobs only for the final truncated page; batch the gets.

### 7.4 HIGH — graph build clones URIs and whole documents repeatedly — `search/graph.rs:44-60`
URI cloned for the visited set, again for the node, once per edge; the entire document `Value` cloned into each node (on top of the cache clone). **Fix:** intern URIs as `Arc<str>`; `Arc<JsonValue>` docs end-to-end. Also `extract_links` builds a full `BTreeSet` then keeps only `breadth` (`:66`) — bounded collection instead; `relation_name` returns `Cow<'static,str>`.

### 7.5 MED — `fetch_document` clones the doc on every cache hit and insert — `search/mod.rs:278`; Tantivy `IndexReader` rebuilt per query — `text.rs:97`
**Fix:** `Arc<JsonValue>` cache values; build the reader once and `reload()`.

### 7.6 MED — `AppState.pipeline: PipelineConfig` cloned by value per request — `http/mod.rs:17`
Axum clones `AppState` per request; `PipelineConfig` holds ~8 `String`s + a parsed `Url`, deep-cloned every endpoint hit. `registry`/`sessions` are already `Arc`. **Fix:** `Arc<PipelineConfig>`.

### 7.7 MED — DTO `From<SymbolMatch>` clones owned data, incl. full snippet — `http/dto.rs:17-36`
`m` is owned but every field (incl. `raw_code`) is `.clone()`-ed. **Fix:** destructure `m.blob`/`m.blob.source` and move.

---

## 8. Storage & IO

### 8.1 CRITICAL — Tantivy commits per document — `search/integration/tantivy.rs:108`
`writer.commit()` (fsync + seal segment) on **every** `index()` — 10k symbols = 10k commits behind one mutex. **Fix:** `index_many` with a single commit per upload chunk/package.

### 8.2 HIGH — Qdrant `wait(true)` forces a flush per blob — `search/integration/qdrant.rs:145`
**Fix:** batch points across blobs into one `upsert_points`; `wait(false)` + a single final wait per sync. Also hoist the per-record `blob_ref`/`global_id` `.to_string()` out of the inner loop (`:134`).

### 8.3 HIGH — registry rewrites its entire JSON file on every state change — `registry/mod.rs:353`
`persist` clones every package's spec/handle/state, `to_vec_pretty`s the whole registry, and rewrites the full file — on add, enqueue, every `record_*`, every monitor refresh. O(N) per single-package event. Also a **torn-write risk** (no temp+rename). **Fix:** compact serialize, temp-file + atomic rename, debounce/coalesce; long-term per-package rows (SQLite/sled). Related: `update_progress` spawns a tokio task per progress emission, each taking the write lock (`:233`).

### 8.4 HIGH — blob storage: full read-modify-write to flip one flag; JSON-with-float-vectors — `blobstore/src/lib.rs:48,60,72`
`update_resolution` GETs the whole blob (source + vectors), mutates one `Option`, re-serializes, PUTs it back. Blobs are JSON-encoded including embedding floats as decimal text — size-inflating and slow. **Fix:** store resolution status separately; binary codec (bincode/CBOR); ideally keep embeddings out of the blob (they live in Qdrant).

### 8.5 CRITICAL — git version resolution: full-history × full-tree re-scan — `git/cargo.rs:16-115,237-266`
`find_commit_for_version` walks every commit from every ref; each commit's `extract_package_version` recursively traverses the **entire tree** and parses every `Cargo.toml` — and does it **twice** per commit (member resolution + fallback). `collect_manifest_directories` even `out.clone()`s the accumulator at every recursion level (`:265`). Cost ≈ O(commits × tree × manifests) re-parsing TOML across trees that barely change. **Fix:** prefer version-encoding tags before walking; memoize by tree OID (adjacent commits share trees); scan the tree once; drop the per-node clone. `git/typescript.rs:14` has the lighter single-file version of this.

### 8.6 MED — `store/src/lib.rs:194` one INSERT round-trip per URI in the register loop
(Inside a single transaction, prepared-statement-cached — so not terrible.) **Fix:** chunked multi-row `INSERT OR IGNORE … VALUES (…),(…)`. *Positive:* all SQL is parameterized, WAL + foreign_keys on, indexes match query patterns — this file is otherwise clean.

### 8.7 Note — in-memory `blobstore`/`search` backends clone whole collections and full-scan (`*/memory.rs`)
These are test/demo-gated; fine, but they're `pub` and re-exported. Don't let them reach prod (`InMemoryVectorIndex::search` recomputes the query norm per record, full scan, `dedup_by_key` clones per element).

---

## 9. Cross-cutting: hashing (SipHash → FxHash)

Every `HashMap`/`HashSet` uses std SipHash (DoS-resistant, ~slow). None of these maps face adversarial keys — they're internal indexes keyed by small ints, `Id`s, `Uuid`s (16 known bytes), or short strings. Switch to `rustc_hash::FxHashMap`/`FxHashSet` (or `ahash`) crate-wide. Highest-value sites:
- `rust/parse.rs` — `id_to_paths`, `path_to_id`, `primitive_map`, `entry_cache`, `visiting`, `type_links`
- `typescript/parse.rs` — `path_to_id`, `type_name_to_id`, `module_name_to_specifier`, `entry_cache`, `visiting`, `seen_methods`
- `pipeline.rs:30` — `entries_by_path` (`NudoxPath` keys)
- `orchestrator/memory.rs:11,13,74` — esp. the `GlobalSymbolId`-keyed map (hashing 16 known bytes)
- `embedding.rs:42` — per-record payload `HashMap` (also `with_capacity(11)`)

**Severity: HIGH (free, broad).**

Plus `orchestrator/memory.rs:46` builds `(name.clone(), version.clone(), symbol.to_string())` — **3 heap allocations per lookup** — just to probe. Use the `Borrow`/`Equivalent` (hashbrown `raw_entry`) pattern to probe with `(&str,&str,&str)`. **Severity: HIGH** (it's on the ingest lookup path).

---

## 10. Identity hot path (`identity/`)

- `entry_uri.rs:9` — `lang: Arc<str>` for a closed set (`"rust"`/`"typescript"`) → use a `Copy` `Language` enum (already exists in `core::primitives`); `path: String` → `Box<str>` (URIs are immutable).
- `entry_uri.rs:25` — `Arc::from(...)` per `new()` with **no interning pool** buys nothing: fresh alloc + atomic overhead, zero sharing (URIs are built fresh per symbol then `to_string`-ed). Either drop `Arc` or introduce a real interner so the thousands of symbols of one crate share one `Arc<str>` package.
- `entry_uri.rs:63` — `Display`/`to_string` **recomputes and reallocates the full URI every call** (id derivation, edge building, map key) and never caches it. **Fix:** render once in `new()` (all callers need it) into a cached `Box<str>`.
- `global_id.rs:12` + `store/lib.rs:72` — `compute()` does `uri.to_string()` (alloc) → `format!("{}\x00{}", …)` (alloc) → `Uuid::new_v5` (SHA-1). Two avoidable allocs before the hash. **Fix:** cached URI `&str` + write parts into a reused scratch buffer fed to the hasher.
- `canonical_rust_package` (`entry_uri.rs:53`) — two `replace('_','-')` allocations just to `eq_ignore_ascii_case`. **Fix:** byte-compare treating `_`==`-`; allocate only the winner.

All of this collapses if §3 (interned segments + `Language` enum) lands.

---

## 11. `core/types` sizing notes

- `EmbeddingPurpose::Other(String)` / `ModelType::Other(String)` (`primitives.rs:64-95`) force 24 B into the common no-`Other` case → `Other(Box<str>)` shrinks the enum; matters because they're stored in `Vec<EmbeddingRecord>`.
- `ChunkMetadata` (`primitives.rs:149`) field ordering leaves padding around `lang: Language`(1) and `blob_schema_version: u32`(4); group small fields. `file_path: PathBuf` repeats per chunk of a file → `Arc<Path>`.
- `SourceChunk.raw_code: String` + `TreesitterRepr(Vec<u8>)` (`primitives.rs:33,54`) → `Arc<str>` / `Arc<[u8]>` so `BlobInfo::clone` is a refcount bump (fixes §2 at the type level).
- `Language::from_str`/`SymbolKind::from_str` (`primitives.rs:139,245`) allocate via `to_ascii_lowercase()` to compare → match on `trim()` with `eq_ignore_ascii_case`.
- `RepoId(String)` (`core/ids.rs:15`) is low-cardinality and cloned widely → `Arc<str>`. `GlobalSymbolId(Uuid)`/`OccurrenceId(Uuid)` are fine (16 B `Copy`).

---

## 12. Robustness issues surfaced incidentally

- **Non-deterministic ordering:** `rust/parse.rs:100` sorts by comparing `HashSet`s via `.iter().cmp()` — `HashSet` iteration order is unspecified, so the sort is non-deterministic (and does 2 hash lookups per comparison). Materialize `(primary_path, id)` once and sort that.
- **Torn writes:** registry `persist` (`registry/mod.rs:353`) and session `persist_session` (`session.rs:85`) write in place with no temp+rename — a crash mid-write corrupts state.
- **Unbounded growth:** `orchestrator/memory.rs` associations `Vec`s append forever with no dedup (`push` without set semantics); test-gated but noted.
- **Out-of-order updates:** `registry/mod.rs:233` spawns a task per progress event — no ordering guarantee between them.
- **`resolve.rs:120`** `path.canonicalize()` (blocking syscall) inside `async fn` — `spawn_blocking` if add-package latency matters.

---

## 13. Recommended dependencies (none present today)

| Crate | Use | Where |
|---|---|---|
| `rustc-hash` (`FxHashMap`) | replace SipHash on all internal maps | §9, parsers, pipeline, orchestrator |
| `lasso` or `string-interner` | intern path segments / package / URIs | §3, §10 |
| `smallvec` | inline small param/alias/segment lists | §1.4, §1.6, §4.6 |
| `compact_str` (opt.) | inline short identifiers (≤24 B) without heap | names, segments |
| `triomphe` (opt.) | `Arc` without weak-count word (8 B saved/Arc) | once `Arc<str>`/`Arc<Type>` are pervasive |
| `bincode`/`ciborium` | binary blob codec instead of JSON+float-text | §8.4 |
| `im` / `ecow` (situational) | only if you need cheap structural sharing of vecs/strings across many clones | not a default; arena (§1.7) is usually better for the IR |

On `triomphe`/`im`/`ecow` specifically (you asked): `triomphe::Arc` is a clean win *after* you've made `Arc` pervasive (saves the weak-count word and is faster to clone), but it's a constant-factor finish, not a starting move. `im::Vector`/`ecow::EcoVec` pay off when the *same* collection is cloned many times with small edits — that pattern barely exists here (the IR is build-once/read-many), so an **arena (§1.7) beats persistent data structures** for this workload. Don't reach for `im` reflexively.

---

## 14. Prioritized roadmap

**Tier 0 — free, broad, low-risk (do first):**
1. `FxHashMap`/`FxHashSet` everywhere internal (§9).
2. Delete the duplicated `server/src/emit/{ld,mod}.rs`, re-export from `document` (§5 header).
3. Hoist in-loop `format!`s; fix O(n)/O(n²) scans with reverse indexes (§4.5).
4. Share clients/providers in `AppState` behind `Arc`; `Arc<PipelineConfig>` (§6.4, §7.6).

**Tier 1 — high-leverage, contained:**
5. Fix the walker `utf8_text`-per-node O(n²) (§4.1) and the per-function file re-read (§4.2).
6. Box fat `Type`/`Entry`/`LDInheritor` variants (§1.1, §1.2, §5.6).
7. Kill the emit triple-`Value` + `@context` clone + insert-by-clone (§5.1–5.5).
8. Typed embedding deserialize + request batching (§6.1, §6.2).
9. `spawn_blocking` + batch commits for Tantivy/Qdrant; session IO off the async lock (§7.1, §7.2, §8.1, §8.2).
10. `Arc`-wrap `BlobInfo` heavy fields + `JsonValue` docs; mutate-in-place instead of clone (§2, §11).

**Tier 2 — structural, highest ceiling:**
11. Intern path segments as `Arc<str>`; replace `NudoxPath = PathBuf`; `Language` enum + cached rendered URI (§3, §10). This is the keystone — most of §10 and much of §2/§4 fall out of it.
12. Registry → per-package atomic persistence; git version resolution memoized by tree OID + tag-first (§8.3, §8.5).
13. Arena/SoA for the `Type` tree with `u32` indices + hash-consing (§1.7).

---

*No source files were modified. All findings are line-referenced against `main` at audit time; re-verify line numbers before acting, as the tree was mid-refactor.*

# TYPES-ARCHITECTURE — a type-precision refactor of the `source/` workspace

Status: proposed plan, not yet implemented.
Scope: every workspace crate under `source/` **except** `intermediate-representation`
(owned by the `clang` branch — see the in-flight-IR note) and the vendored
`deno_doc/`.

Decisions baked into this plan (confirmed):

- **Schema break is allowed.** `BlobInfo` is the system of record but is
  reproducible from source, so changes to its serialized form are handled by a
  single `BLOB_SCHEMA_VERSION` bump (2 → 3) with **no in-place migrator**; old
  blobs are rebuilt.
- **Parsers: shared scaffolding + shared traits**, but the Rust and TypeScript
  *item models stay separate*. They serve different endpoints; a merged AST
  would conflate language semantics for negative value.

---

## 0. The thesis

Eleven independent crate audits converged on the **same ~12 types in `core`**.
Search, store, embed, blobstore, orchestrator, pipeline, and document each
re-derive the *same* local workaround for the *same* weak core type:

| Weak core type | Symptom that shows up downstream |
| --- | --- |
| `BlobRef(pub String)` | `HashMap<String, _>` keys in `search/symbols.rs`, `search/memory.rs`, `orchestrator/memory.rs` instead of keying on the ref |
| `SymbolQuery.limit: usize` | `query.limit.saturating_mul(2).max(1)` patch in `search/symbols.rs` |
| `EmbeddingRecord.vector: Vec<f32>` | every backend treats vectors as untyped, empty is representable |
| `EmbeddingRecord.model: String` | hand-maintained label tables in `search/integration/qdrant.rs` |
| `BlobInfo.symbol_name: String` | `symbol_name.to_string()` re-alloc on every map insert in orchestrator |
| `score: f32` | `partial_cmp(...).unwrap()` (NaN panic) in every ranker |
| `ByteSpan { pub start, pub end }` | re-invented as `[usize; 2]` in `pipeline/treesitter.rs` |

So the leverage is **core-outward**: fix the spine once, and most per-crate
findings evaporate without ever being touched. The phases below are ordered by
that leverage, not by crate.

---

## Phase 0 — the `core` newtype foundation (the multiplier)

Everything else gets cheaper after this phase. Five moves.

### 0.1 A newtype macro for the string spine

Today (`core/src/ids.rs`):

```rust
pub struct GlobalSymbolId(pub Uuid);   // Copy — fine
pub struct OccurrenceId(pub Uuid);     // Copy — fine
pub struct RepoId(pub String);         // pub field, no validation
pub struct BlobRef(pub String);        // pub field, no validation, "opaque"
```

…plus bare `String` domain values scattered across `core/types`:
`BlobInfo.symbol_name`, `EmbeddingRecord.model`, `LibRef { name, version }`.

Introduce one declarative macro that emits a validated, parse-don't-validate
newtype with the boilerplate every consumer already wants:

```rust
// core/src/newtype.rs
macro_rules! str_newtype {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Box<str>);          // Box<str>: 16 bytes vs String's 24

        impl $name {
            pub fn new(s: impl Into<String>) -> Result<Self, EmptyValue> {
                let s = s.into();
                if s.trim().is_empty() { Err(EmptyValue) } else { Ok(Self(s.into())) }
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl std::fmt::Display for $name { /* writes as_str */ }
        impl std::str::FromStr for $name { type Err = EmptyValue; /* … */ }
        impl AsRef<str> for $name { /* … */ }
    };
}
```

Apply to: `BlobRef`, `RepoId`, `SymbolName`, `ModelId`, `LibName`, `Version`.

Why it pays off:

- `#[serde(transparent)]` means **the JSON wire form is unchanged** for these —
  they cost nothing at the serialization boundary even though we bump the schema
  for other reasons.
- `HashMap<String, _>` keys in search/orchestrator become `HashMap<BlobRef, _>`;
  the `.0.clone()` / `.to_string()` dance disappears.
- `Box<str>` over `String` shaves 8 bytes per id and signals immutability.

`LibRef` then becomes `{ name: LibName, version: Version }`, and the three-tuple
`HashMap<(String,String,String), _>` key in `orchestrator/memory.rs` becomes
`HashMap<(LibRef, SymbolName), _>`.

> Distinctness note: keep `RepoId`, `BlobRef`, `SymbolName`, `ModelId` as
> *separate* types even though they wrap the same primitive. The whole point is
> that they stop being interchangeable.

### 0.2 Bulletproof `ByteSpan`

Today the `start <= end` invariant lives only in `new()`, and is bypassed by
struct literals (10+ call sites) and by serde.

```rust
pub struct ByteSpan { start: usize, end: usize }   // fields now private

impl ByteSpan {
    pub fn new(start: usize, end: usize) -> Option<Self> { /* unchanged */ }
    pub const fn start(&self) -> usize { self.start }
    pub const fn end(&self) -> usize { self.end }
    pub const fn len(&self) -> usize { self.end - self.start }
    pub const fn is_empty(&self) -> bool { self.start == self.end }
    pub fn contains(&self, off: usize) -> bool { self.start <= off && off < self.end }
    /// Re-base into a sub-slice that starts at `base.start`.
    pub fn relative_to(self, base: ByteSpan) -> ByteSpan { /* saturating sub */ }
}
// + a hand-written Deserialize that funnels through new() and rejects inverted spans
```

Then **delete the `[usize; 2]` duplicates** in `pipeline/treesitter.rs`
(`TreesitterPayload.snippet_span`, `ReferenceEntry.span`) and use `ByteSpan`
directly (already `Serialize`). `relative_to` replaces the open-coded
`saturating_sub` in `pipeline/lib.rs`.

### 0.3 `Score` — an orderable similarity/relevance value

Every ranker sorts `f32` with `partial_cmp(&b).unwrap()`: a single NaN panics
the search path.

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score(f32);                 // invariant: never NaN

impl Score {
    pub fn new(v: f32) -> Option<Self> { (!v.is_nan()).then_some(Self(v)) }
    pub fn get(self) -> f32 { self.0 }
}
impl Eq for Score {}
impl Ord  for Score { /* total_cmp */ }
impl PartialOrd for Score { /* Some(cmp) */ }
```

`SearchHit.score`, `VectorHit.score`, `SymbolMatch.score`, and `ScoredRef.score`
become `Score`. Sorts become `.sort_by_key`/`.sort_unstable` with no `unwrap`.

> Rejected: the audit suggestion to *normalize* cosine into `[0,1]`. That
> silently rewrites ranking semantics. We wrap and order; we do not rescale.
> Keep the doc distinction (cosine ∈ [-1,1] vs index-relative) in prose.

### 0.4 `NonZero` for quantities that can't be zero

- `SymbolQuery.limit: usize` → `NonZeroUsize` (deletes the `.max(1)` patch).
  `SearchQuery::search`/`find_by_global_id`/`list_all` and `VectorQuery::search`
  in `core/traits.rs` take `NonZeroUsize`.
- `OccurrenceFilter.{min,max}_count: Option<usize>` → `Option<NonZeroUsize>`
  (`min_count: Some(0)` is just "no filter").
- embedding `dim` (embed crate) and retry `max_attempts` (server) → `NonZero*`.

serde supports `NonZero*` natively — **ignore** the audit notes that hand-roll
custom serde for these.

### 0.5 `NonEmpty<T>` for collections that are never legitimately empty

A small in-tree type (head + tail) or the `nonempty` crate. Apply where empty is
a bug, not a state:

- `EmbeddingRecord.vector` → `NonEmpty<f32>` (empty vector ⇒ meaningless
  dot-product, currently silently wrong). Folds into the `Embedding` type (2.4).
- `Pipeline.embedders: Vec<Box<dyn Embedder>>` → a validated `EmbedderSet`
  (a pipeline with zero embedders produces empty `BlobInfo.embeddings`).

Do **not** blanket-apply NonEmpty to parser output lists that are legitimately
sometimes-empty; the audit over-reached there. Use it only where the invariant
is real.

**Phase 0 ripple:** touches every crate's *call sites* but is mechanical. The
serde-transparent newtypes and `ByteSpan` keep most wire forms stable; `Score`,
`NonZero`, and `NonEmpty` are in-memory-only.

---

## Phase 1 — make illegal states unrepresentable

Highest correctness-per-line tier. These are the structural wins.

### 1.1 `ResolutionState` replaces `Option<GlobalSymbolId>`

`BlobInfo.resolved_global_id: Option<GlobalSymbolId>` conflates *never
attempted* / *deferred on an unparsed lib* / *resolved*. There is **already an
unused `ResolutionOutcome` enum in the same file**. Introduce the stored form:

```rust
pub enum ResolutionState {
    Unresolved,
    Deferred(LibRef),
    Resolved(GlobalSymbolId),
}
```

`BlobInfo.resolution: ResolutionState`. `orchestrator::index_resolved`'s
`.expect("requires resolved_global_id")` precondition becomes a match arm that
can't be reached with the wrong variant. (Schema-affecting → part of the v3
bump.)

### 1.2 `Criteria` sum type for `SymbolQuery`

Today `name_pattern: Option<_>` + `body_query: Option<_>` + `combine:
CombineMode` can encode **(None, None)** — a query with no criteria — and makes
`combine` meaningless unless both are `Some`.

```rust
pub enum Criteria {
    Name(NamePattern),
    Body(BodyQuery),
    Both { name: NamePattern, body: BodyQuery, combine: CombineMode },
}

pub struct SymbolQuery {
    pub criteria: Criteria,          // empty query now unrepresentable
    pub scope: Option<ScopeFilter>,
    pub kind: Option<SymbolKind>,
    pub occurrence_filter: Option<OccurrenceFilter>,
    pub limit: NonZeroUsize,
}
```

`combine` only exists in the `Both` arm — the dead-config case is gone.

### 1.3 Orchestrator typestate

`Orchestrator.symbol_searcher: Option<Arc<dyn SymbolSearch>>` lets `.search()`
compile on an orchestrator that can only ever return a runtime "no symbol
searcher attached" error.

```rust
pub struct Orchestrator<S = NoSearcher> { /* … */ _state: PhantomData<S> }
pub struct NoSearcher;
pub struct WithSearcher;

impl Orchestrator<NoSearcher> {
    pub fn with_symbol_search(self, s: Arc<dyn SymbolSearch>) -> Orchestrator<WithSearcher> { /* … */ }
}
impl Orchestrator<WithSearcher> {
    pub async fn search(&self, q: &SymbolQuery) -> Result<Vec<SymbolMatch>> { /* infallible unwrap */ }
}
```

`search()` now only exists once a searcher is attached. The string-boxed error
construction in `orchestrator/lib.rs:48-52` disappears.

### 1.4 `Option<QdrantConfig>` collapses scattered config

Both `indexer/src/main.rs` and `server` hold `qdrant_url: Option<String>`
alongside an *always-present* `qdrant_collection: String` — the collection is
dead when the url is `None`. Group them:

```rust
struct QdrantConfig { url: QdrantUrl, collection: QdrantCollection, vector_dim: NonZeroU32 }
struct Config { /* … */ qdrant: Option<QdrantConfig> }
```

`QdrantUrl`/`QdrantCollection` are validating newtypes (collection-name charset,
url scheme) so failures surface at config-load, not first upsert. `Config::from_env`
returns `Result` instead of swallowing parse errors into silent defaults.

---

## Phase 2 — per-crate symptom cleanup (now cheap)

With core carrying the weight, these get small.

### 2.1 search — one typed schema module, no silent masking

`search/integration/tantivy.rs` and `qdrant.rs` repeat field-name string
literals in three places each (schema build, query, extract) and mask failures
with `unwrap_or_default()` / `unwrap_or(Uuid::nil())`. Replace with a single
`mod schema { pub const … }` source of truth, and make extraction return
`Result` (or log + skip) instead of fabricating empty `BlobRef`s and nil UUIDs.
`model_type_label`/`purpose_label` tables fold into `ModelId`/`EmbeddingPurpose`
via serde `rename_all`.

### 2.2 store — delete redundant columns

`store/src/lib.rs` stores:

- `symbol_name` — always `entry_uri.rsplit('/').next()`; derive at query time.
- `terminus_instance` — invariant per `NudoxStore` instance; it's already a
  struct field. Drop the column and the unused `idx_gs_library` index.

Also: `register_library` returns `usize` but sums `u64` `rows_affected()` →
return `u64`; and take `&LibRef` uniformly instead of `(lang, lib_name,
lib_version)` triples. Wrap `entry_uri` handling in `identity::EntryUri`
(below) instead of fragile `rsplit`.

### 2.3 identity — close the `KindUri`/`EntryUri` asymmetry

`EntryUri` has normalization + `parse()` + round-trip tests. `KindUri` has a
typo-able `&'static str` prefix, no `parse()`, no normalization — yet both are
primary keys across Terminus/Qdrant/Tantivy/SQLite.

- `prefix: &'static str` → `KindPrefix` enum (`Function`, `RecordType`, …).
- add `KindUri::parse()` + round-trip test mirroring `EntryUri`.
- share the lang/package normalization (`canonical_rust_package`) between both.
- `TerminusInstance` newtype (`org/db`) for `global_id::compute`, so a typo in
  the instance can't silently produce a different `GlobalSymbolId`.

### 2.4 embed — `Embedding` + `ModelId`, drop the dead `dim`

- `Embedder::embed` returns `Embedding` (= `NonEmpty<f32>`) not `Vec<f32>`.
- model ids use `ModelId` (Phase 0).
- remote.rs `v.as_f64().unwrap_or(0.0)` silently zeroes malformed floats →
  propagate an error instead (this is a latent corruption bug, worth doing).
- delete `InProcessEmbedder.dim` (always `DEFAULT_DIMENSION`); name the LCG
  magic constants.

### 2.5 document — typed URIs and honest errors

- replace `pub type URI = String` with a `DocumentUri` that delegates to
  `identity::EntryUri`/`KindUri` (stop ignoring the crate that already solved
  this).
- `LDTraitImpl`'s `is_negative`/`is_blanket`/`is_unsafe` bools → `Polarity` /
  `ImplScope` / `Safety` two-variant enums (serde `rename_all` keeps JSON
  compact).
- `LDInheritor::try_from` failure currently logs and substitutes `None`,
  emitting invalid JSON-LD silently → `EmitJsonLD::emit` returns
  `Result<DocumentUri, EmitError>`; `Runner` aggregates failures.
- drop the stored, derivable `kind`/`kind_tag` fields.

### 2.6 parsers — shared scaffolding + shared traits (models stay split)

Per the decision, extract the *common parse steps* behind traits while keeping
`rust::*` and `typescript::*` item structs distinct:

```rust
trait VisibilityMap { fn visibility(&self, raw: …) -> Result<ir::kind::Visibility>; }
trait ReceiverExtract { fn receiver(&self, first_param: …) -> Option<ReceiverKind>; }
```

Plus the easy, low-risk wins: structured error variants instead of
`String` reasons, `#[non_exhaustive]` on the public `Parse` enums, replace
`format!("{:?}", ty)` round-tripping with a real `Display`, and a single
`empty_to_none`/`NonEmpty` helper for the `if v.is_empty() { None }` pattern
repeated ~15×. Do **not** merge the item models.

---

## Phase 3 — derivable-state removal & polish

- `orchestrator::ResolveLibReport.blobs_skipped` → method (`seen - resolved`);
  and split the type so `rebuild_indexes` vs `resolve_lib` stop sharing one
  ambiguously-named report. Batch the paired `docs`/`vectors` vecs into one
  `IndexWriteBatch` so they can't desync.
- `#[non_exhaustive]` on the extensible enums (`ModelType`, `EmbeddingPurpose`,
  the parser error enums).
- server: `GitSHA`, `BranchRef`, `SessionId`, `SearchLimit` newtypes (the audit
  found ~20 bare-`String` SHA sites and magic `unwrap_or(6)` limits); custom
  axum extractors so handlers receive `PackageId`/`SessionId` already-parsed.
- structural: split the two oversized modules (`server/registry/mod.rs`,
  `parsers/typescript/package.rs`); name magic constants
  (`REBUILD_BATCH = 256`).

---

## Things deliberately out of scope

- **IR crate** — owned by the `clang` branch; do not touch.
- **Merging rust/typescript item models** — different endpoints, see decision.
- **Streaming/pagination of `list()`/`drain_for_lib`** — that's the *perf*
  track (PERF-ARCHITECTURE), not types.
- **`secrecy`/`zeroize` for API keys, `BTreeMap`-for-determinism, SQL CHECK
  constraints** — real but tangential; not part of a types pass.
- **In-memory blobstore's UUID-vs-content-hash `BlobRef`** — flagged for intent
  review (may be a deliberate test double), not auto-"fixed".

---

## Suggested branch / commit shape

One branch, commits ordered by phase so each compiles:

1. `types(core): newtype macro for id/string spine` (0.1)
2. `types(core): seal ByteSpan + delete pipeline [usize;2] dupes` (0.2)
3. `types(core): Score, NonZero limits, NonEmpty` (0.3–0.5)
4. `types(core): ResolutionState + Criteria; bump BLOB_SCHEMA_VERSION→3` (1.1–1.2)
5. `types(orchestrator): typestate searcher` (1.3)
6. `types(config): Option<QdrantConfig>` (1.4)
7. one commit per crate for Phase 2, then Phase 3 polish.

Phase 0 commits are large but mechanical; Phases 1–3 are the ones to review
carefully.

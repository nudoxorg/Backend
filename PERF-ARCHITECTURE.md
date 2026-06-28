# Performance Architecture v2 — validated, deep, exotic

Supersedes the v1 draft. This rewrite is grounded in three feasibility investigations against the actual code (string provenance, concurrency access patterns, parse-stage parallelism). Where v1 was wrong, it says so. The plan at the end is rebuilt from the validated ground truth.

Companion: `PERF-MEMORY-AUDIT.md` (per-type/per-line findings — still valid).

> **Branch safety:** everything here is in `server/`, `orchestrator/`, `search/`, `core/`, `intermediate-representation/`. `origin/clang` adds only `Type::Void`/`Width::W80` on the old `compiler/`+`ir/` layout — no collision.

---

## 0. What the validation changed (intellectual honesty first)

| v1 claim | Verdict | Correction |
|---|---|---|
| Make the IR zero-copy / `Type<'src>` borrowing source | **WRONG** | IR strings come from rustdoc/deno-doc **JSON subprocess output** (owned, deserialized, dropped). Paths are *synthesized* (`join("::")`), not substrings. No `'src` to borrow. → use an **arena**, not source-borrowing. |
| Single-owner actors for shared-mutable state (broadly) | **TOO BROAD** | Registry is read-mostly snapshot → **ArcSwap**. Per-package state is trivial RMW → **keep RwLock**. Actor is right only for the **Tantivy writer** (single-writer + batch-commit + off-thread) and arguably **sessions**. |
| Stream everything | **PARTLY** | IR-gen is gated behind a single-threaded subprocess — can't stream/parallelize *inside* it. **Projection→sinks** can stream and parallelize. |
| Batch the trait seams | **CONFIRMED** | per-doc commit, per-blob `wait(true)`, per-symbol embed/dispatch all verified. Strongest lever, unchanged. |
| `triomphe::Arc` everywhere | **NARROWED** | Use it for *sized immutable* data only; keep `std::Arc` for the `dyn` trait-object seams (coercion ergonomics). |

The one fact that unlocks the exotic path: **the IR is never deserialized** — it's built, emitted to JSON-LD documents, and dropped (one-way; `Function.body` is even `#[serde(skip)]`). So we can **drop `Deserialize` and adopt arena-scoped `&'arena` references with real lifetimes** without breaking anything. That is precisely the "stronger borrow-checker adherence" you asked for, and it's feasible *because* nothing round-trips the IR.

---

## 1. Validated ground truth (the constraints everything must respect)

**G1 — IR strings are owned, from JSON, transient.** `cargo rustdoc --output-format json` / `deno doc` → subprocess → `serde_json::from_str` into owned structs → our walk produces `String`s via `.clone()`/`.to_string()`/`join("::")` (`rust/parse.rs:289-437`, `typescript/types.rs:206-354`). The JSON buffer dies inside `spawn_blocking`. *Implication:* borrowing source is impossible; **interning + arena** is the cache-friendly play, and identifiers repeat heavily (paths, primitives, `Option`, `String`) so interning also dedups.

**G2 — The IR is one-way and never deserialized.** Built in `generate_ir`, consumed once by `project(&index)` + `emit_store(index)`, then dropped (`ingest/mod.rs:84-189`). No `Deserialize` of `Entry`/`Index` exists anywhere. *Implication:* `&'arena`/lifetime-infected IR is safe; Serialize-only is sufficient.

**G3 — Two stages, two parallelism profiles.**
- *Stage 1 (IR gen):* single-threaded subprocess + our recursive JSON→IR walk with shared `entry_cache`/`visiting` + a mandatory build-maps prelude. **Not internally parallel**; only coarse package-level subprocess fan-out is available (currently sequential, `rust/mod.rs:162`).
- *Stage 2 (projection):* `entries_by_path.values().map(project_entry)` (`parsed_symbol.rs:88`) — **embarrassingly parallel**, zero shared mutable state, per-function granularity, each call already makes its own `Parser`/`Tree`, IDs are pure hashes (`types.rs:73`, no counter). **The clean rayon target.**

**G4 — Concurrency reality (per resource):** no lock is held across `.await` incorrectly; the real bugs are *blocking work on executor threads* (session `fs::write` under a global tokio mutex; Tantivy `commit()` with no `spawn_blocking`, committing per document) and *client churn* (new Qdrant gRPC client per call).

**G5 — The `ParsedBody` yoke is built but dead.** `Yoke<FunctionBody, Arc<str>>` (`syntax/body.rs:20`) is the one correct source-borrowing structure; `Function.body` is **never set in production**. Decide: wire it up (it's the right design for body analysis) or delete it.

---

## 2. The ownership model — five classes, validated per-resource

The discipline: every shared value belongs to exactly one class. v1 collapsed too much into "actor." Corrected:

| Class | Tool | Why | Resources (validated) |
|---|---|---|---|
| **Shared-immutable** | `Arc<T>` → `triomphe::Arc<T>` (sized) | clone = refcount; never locks | `PipelineConfig`, `@context`, `schema.json`, interner, package/version/model `Arc<str>`, `BlobInfo` source via `Arc<str>` |
| **Read-mostly snapshot** | **`ArcSwap<T>`** (RCU, lock-free reads) | readers clone-and-release already; writes rare | registry `packages`/`keys` maps |
| **Per-key independent, trivial CS** | `RwLock` (keep) or `dashmap` | O(1) critical sections, one lock per key | per-package `TrackedPackage.state` (keep RwLock); sessions (→ `dashmap`, per-session) |
| **Single-writer + batch + blocking** | **single-owner actor** (mpsc + `spawn_blocking`) | serializes, batches commits, off executor | Tantivy `IndexWriter`; (optionally the Qdrant/Terminus upsert pumps) |
| **Owned-transient** | move / arena | one consumer, no sharing | every `.clone()`-to-move site in the audit |

Concrete corrections from v1:
- **Registry → `ArcSwap`, not actor.** Reads (list/get/status/monitor) dominate; every reader already does `.read().await.values().cloned().collect()` then drops the guard (`registry/mod.rs:123,185,354`). That's the ArcSwap access pattern exactly: `packages.load()` is wait-free; `add_package` does a copy-on-write `packages.store(new)`. No actor, no dashmap (no per-key write hotspot, and ArcSwap keeps the cheap whole-map snapshot). Keep the per-package `RwLock<state>` and the `sync_lock`/`Semaphore` (correct as-is).
- **Sessions → `dashmap` + move the `fs::write` out of the lock.** The bug is a global `tokio::Mutex` held across a blocking `fs::write` (`session.rs:61→93`), falsely serializing unrelated sessions on disk IO. Per-session `dashmap` entry + persist-after-release (or a per-session actor if you want ordered durability).
- **Tantivy → actor is the right call** (the one place v1 was right): single writer is mandated, so wrap it in one owner task on `spawn_blocking` that batches `add_document` and commits per N — fixes commit-per-doc (`tantivy.rs:108`) and blocking-on-executor (`sink.rs:157`) together.
- **Qdrant → no primitive at all,** just stop rebuilding the client (`ingest/qdrant.rs:17,74`, `search/qdrant.rs:36`); store one `Arc<Qdrant>` in `AppState`.
- **sqlx pool, in-memory test stores → leave alone.**

---

## 3. The IR: the rustc `Ty<'tcx>` model (arena + lifetimes + hash-consing)

This is the exotic centerpiece, and it's the *correct* shape for a read-mostly, build-once type IR — it's literally how rustc represents types (`Ty<'tcx>` = an interned, arena-allocated, `Copy` reference with a lifetime). G1+G2 make it feasible here.

### 3.1 The design
```rust
// One arena per package-parse, dropped wholesale at the end (no per-node Drop).
pub struct IrArena<'a> {
    bump:   bumpalo::Bump,             // Stage 1 is single-threaded → plain Bump is fine
    strings: lasso::Rodeo,            // intern identifiers → Spur (u32), resolve to &str
    types:  HashCons<'a>,             // dedup identical Type subtrees
}

// Lifetime-tagged, Copy, pointer-thin. No Box, no String, no Vec<Type> deep-owning.
#[derive(Clone, Copy)]
pub struct Ty<'a>(&'a TyKind<'a>);

pub enum TyKind<'a> {
    Reference { id: Spur, args: &'a [GenericArg<'a>] },   // &'a [_] = arena slice, not Vec
    Tuple(&'a [Ty<'a>]),
    Slice(Ty<'a>),                                        // Ty is Copy → no Box
    Conditional(&'a ConditionalTy<'a>),                  // rare/fat variants behind one &
    // …
}
```
Every `Box<Type>` becomes a `Copy` `Ty<'a>`; every `Vec<Type>`/`String` becomes an arena slice/interned `Spur`. `TyKind` shrinks dramatically (audit §1.1: ~112 B → the largest inline variant), and because it's interned the *same* `Ty<'a>` is reused for every `String`/`i32`/`Option<T>` in the crate.

### 3.2 What it buys
- **Borrow-checker enforces the invariant**: an `Entry<'a>` cannot outlive its `IrArena<'a>`. The compiler guarantees no dangling, no use-after-free of the IR — exactly the discipline you asked for, statically.
- **Cache density**: all nodes contiguous in the bump; traversal is near-linear, not heap pointer-chasing across independent `Box`es.
- **Hash-consing**: identical subtrees collapse to one node. In real crates this is huge — `String`, `&str`, `Option<…>`, `Result<…, Error>`, `Vec<u8>` recur thousands of times. `PartialEq` on `Ty<'a>` becomes pointer equality (`Copy` + interned).
- **O(1) bulk free**: dropping the `Bump` frees the entire IR at once — no thousands of recursive `Drop` calls.
- **Clones vanish**: `Ty<'a>` is `Copy`; the audit's "clone storm" (§2) on `Type`/`Entry` evaporates because there's nothing to deep-clone — you copy a reference.

### 3.3 The costs (be honest)
- **Lifetime infection**: `Type` → `Ty<'a>` propagates `'a` through `Entry<'a>`, `Function<'a>`, the parsers, `project`, `emit`. Mechanical but broad.
- **Serialize-only**: drop `Deserialize` (G2 says nothing uses it). `Serialize` through `&'a` is fine for emit.
- **`emit` must borrow, not consume**: today `emit_store` does `index.into_values()` (`ingest/mod.rs:185`). With an arena it borrows `&'a` and the arena drops after — which also *fixes* the `emit(self)`-clones-everything problem (audit §5.4) since there's nothing to move out, only `Copy` refs to read.
- It's a large refactor. **Fallback ladder** (§7) lets you get 80% of the win incrementally before committing.

### 3.4 The fallback if `&'a` infection is too much: index-based arena
Same arena, but `TyId(NonZeroU32)` indices instead of `&'a` references. Loses static borrow-safety and pointer-equality, keeps contiguity + dedup + cheap copy + serializability. This is the pragmatic 80%. Recommendation: prototype the `&'a` version on one module; if the lifetime churn is tolerable, it's strictly better; if not, fall back to indices.

---

## 4. Exotic parallelism (validated against G3)

### 4.1 Split the runtime: tokio for IO, a dedicated rayon pool for CPU
Today CPU work (`generate_ir`) is shoved through `spawn_blocking`, which uses tokio's *blocking* pool (designed for IO waits, can balloon to 512 threads). CPU-bound work belongs on a **bounded rayon pool sized to physical cores**, kept separate from the async reactor so parsing never starves request handling:
```rust
static CPU: Lazy<rayon::ThreadPool> =
    Lazy::new(|| rayon::ThreadPoolBuilder::new().num_threads(num_cpus::get_physical()).build().unwrap());
// bridge: async side awaits a oneshot; CPU pool fulfils it.
async fn on_cpu<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = oneshot::channel();
    CPU.spawn(move || { let _ = tx.send(f()); });
    rx.await.unwrap()
}
```

### 4.2 Projection: `par_iter` (the embarrassingly-parallel win, G3)
```rust
use rayon::prelude::*;
let symbols: Vec<ParsedSymbol> = index.entries_by_path
    .par_iter()                               // validated: no shared mutable state
    .map_init(
        || Parser::new(),                     // thread-local Parser — also kills the
        |parser, (_, entry)| project_entry(parser, entry, source_map, &identity),
    )
    .collect();
```
`map_init` gives each worker its own `Parser`, eliminating the two `Parser::new()` allocations per entry (audit §4.3 double-parse) *and* parallelizing. Confirm `arborium_tree_sitter::{Tree,Node,Parser}: Send` with a one-line `assert_send` (the fork mirrors tree-sitter 0.26 which is `Send+Sync`; agent couldn't read the vendored source).

### 4.3 Coarse subprocess fan-out (Stage 1, the only parallelism available there)
`generate_ir_with_sources` runs per-package subprocesses sequentially (`rust/mod.rs:162`). Run the independent `cargo rustdoc`/`deno` subprocesses concurrently (bounded by a semaphore) — process-level parallelism for multi-crate workspaces.

### 4.4 Lock-free interner for any concurrent interning
If projection (or a future parallel IR build) interns concurrently, use `lasso::ThreadedRodeo` (lock-free-ish, returns the same `Spur` for equal strings across threads). The single-threaded Stage-1 arena uses plain `Rodeo`. For a concurrently-built arena, `bumpalo-herd::Herd` hands each rayon worker its own `Bump` sharing one `'a` — the exotic-correct way to bump-allocate in parallel.

### 4.5 SIMD for vector scoring
The in-memory `VectorIndex` cosine loop (`search/memory.rs`) and any local rerank are pure FMA over `&[f32]`. Use stable SIMD (`wide` or `pulp`; `std::simd` is still nightly) for the dot/norm — 4–8× on the scoring inner loop. Qdrant does this server-side, so this matters for the in-memory/local path and any client-side rerank.

### 4.6 Allocator + JSON parser (free, high-leverage given the allocation profile)
- **Global allocator → `mimalloc` or `jemallocator`.** This workload is allocation-storm-shaped (audit is full of per-node/per-symbol allocs); a better allocator is a one-line `#[global_allocator]` win, typically 10–30% on alloc-heavy Rust.
- **`simd-json` for the rustdoc/deno JSON.** That JSON is large and parsed per package (`rust/mod.rs:139`); `simd-json::from_slice` (over an `mmap2` of the file, skipping `read_to_string`) is a real Stage-1 win.

---

## 5. Streaming + batched seams (refined per G3)

The pipeline can't stream Stage 1 (gated subprocess), but **everything from projection onward streams**, and the batched trait seams are the keystone (confirmed strongest lever):

1. **Batch the trait APIs** (`core/traits.rs`): `embed_many`/`upsert_many`/`index_many`/`put_many`/`associate_many`. One dynamic dispatch + one boxed future + one commit/wait/request *per batch*, not per symbol. This single change fixes: per-doc Tantivy commit, per-blob Qdrant `wait`, per-symbol OpenAI request (→ array inputs), and the `async-trait` per-call `Box` tax. Keep `dyn` at these coarse seams (correct use); never on per-symbol paths.
2. **Stream projection → sinks** with bounded backpressure: rayon produces `ParsedSymbol`s into a bounded channel; async sinks consume via `ready_chunks(BATCH).map(batched_call).buffer_unordered(K)`. Peak memory O(batch × concurrency), not O(corpus); stages overlap.
3. **Fan sinks out concurrently** (`run_sinks` is sequential today, `ingest/mod.rs:153`) — they're independent (SQLite/Tantivy/Terminus/Qdrant/orchestrator).
4. **Query side**: score on cheap index payloads, `truncate(limit)`, *then* `buffer_unordered` the heavy blob fetches for that page only (audit §7.3); reuse one Qdrant/embedding client from `AppState`.

---

## 6. The rewritten plan

Ordered so each phase is independently shippable and each unlocks the next. Risk in brackets.

**Phase 0 — free wins, no design change [trivial]**
- `#[global_allocator]` = mimalloc; `FxHashMap` for internal maps (audit §9); reuse one `Arc<Qdrant>` + one embedder in `AppState`; `Arc<PipelineConfig>`.

**Phase 1 — ownership hygiene [low]**
- `ArcSwap` for registry maps; `dashmap` + persist-outside-lock for sessions; move Tantivy/session blocking work onto `spawn_blocking`. `Arc<str>` for `raw_code`/`treesitter_repr`/package/version/model; `Arc<Value>` `@context`; `OnceLock` schema. Mutate-in-place instead of `BlobInfo` clone.

**Phase 2 — batch the seams [low/medium]**
- Add `*_many` trait methods; implement with one commit/wait/request per batch. (Subsumes audit §6.2, §8.1, §8.2.)

**Phase 3 — actor for the Tantivy writer [medium]**
- Single-owner writer task on the CPU/blocking pool; batch + commit-per-N. (Sessions optionally get a per-session actor if dashmap+persist-outside-lock proves insufficient.)

**Phase 4 — parallelism [medium]**
- Dedicated rayon CPU pool + `on_cpu` bridge; `par_iter().map_init(Parser::new, …)` projection; concurrent subprocess fan-out for multi-crate; `simd-json` + `mmap` for the rustdoc/deno JSON; `wide` SIMD for in-memory vector scoring.

**Phase 5 — stream the pipeline [medium]**
- Replace `project→Vec→run_sinks` with rayon→bounded-channel→concurrent batched async sinks; stream emit out of `entries_by_path` where dedup allows.

**Phase 6 — the arena IR (`Ty<'a>`) [high, highest ceiling]**
- Introduce `IrArena<'a>`, intern strings (`Spur`), arena-allocate + hash-cons `TyKind<'a>`, make `Ty<'a>`/`Entry<'a>` `Copy`-thin, drop `Deserialize` (G2), make `emit` borrow. Prototype on the `Type` module first; choose `&'a` (full borrow-safety) vs `TyId(u32)` index (pragmatic) per §3.4. Banks the audit's entire §1 (size bloat) and §2 (clone storm) at once.

**Phase 7 — finishing passes [low]**
- `triomphe::Arc` for sized immutable data; `smol_str`/`compact_str` for any remaining short owned identifiers not interned; wire up or delete the dead `ParsedBody` yoke (G5); binary blob codec (`bincode`/`rkyv`, audit §8.4).

---

## 7. Risk / fallback ladder for the exotic bits

- **Arena IR** is the only "high risk" item. De-risk: (1) start with index-based `TyId(u32)` (no lifetime infection, keeps serde) to get contiguity+dedup; (2) if smooth, migrate to `&'a` for static borrow-safety; (3) if lifetime churn is intolerable, stop at indices — still a large win. Everything Phases 0–5 is valuable *without* Phase 6.
- **rayon Send assumption**: gate Phase 4 on a one-line `assert_send::<arborium_tree_sitter::Tree>()`. If the fork isn't `Send` (unlikely), fall back to per-task `Parser` on the blocking pool with `buffer_unordered` instead of `par_iter`.
- **`&'a` + Serialize**: confirmed fine (serialize-through-reference); the only loss is `Deserialize`, which G2 proves unused.
- **ArcSwap for registry**: if a writer ever needs read-modify-write across the whole map atomically (not today), guard the *write* side with a small mutex while keeping reads lock-free — standard ArcSwap+CAS pattern.

---

---

## 8. The hyper-specialized type toolkit (definitive)

Every specialized type the plan uses, what it replaces, where, and why. Grouped by job. "Class" ties back to §2.

### 8.1 String & identifier representation
| Type (crate) | Replaces | Where | Why |
|---|---|---|---|
| `lasso::Spur` + `Rodeo`/`ThreadedRodeo` | `String` path segments, type/symbol names | IR arena (§3), `NudoxPath`, interner | u32 handle; dedups the thousands of repeated `serde`/`tokio`/`Option`/`String` identifiers; `Copy`, hashes as a u32 |
| `&'a str` (bump-arena) | `String` in IR | `Ty<'a>`/`Entry<'a>` (§3) | contiguous, borrow-checked lifetime, zero per-node alloc |
| `compact_str::CompactString` *or* `smol_str::SmolStr` | `String` for short owned ids not worth interning | `EntryUri.path`, `BlobRef`, `RepoId`, DTO fields | ≤24 B inline, no heap for the common short case; `SmolStr` is `Clone`-cheap (Arc-backed) for repeated values |
| `Box<str>` | `String` for immutable-after-build fields | `Symbol.name`, `documentation`, rendered URIs | drops the 8-B capacity word; signals immutability |
| `ecow::EcoString` | `String` cloned across many docs | JSON-LD string values, shared doc fields | clone-on-write, refcounted — cheap clones, mutates only when needed |
| `Arc<str>` (→ `triomphe`) | `String` shared widely | `package`/`version`/`lang`/`model`, `SourceChunk.raw_code` | refcount-bump clone; the §2 shared-immutable spine |

### 8.2 Collections & containers
| Type (crate) | Replaces | Where | Why |
|---|---|---|---|
| `&'a [T]` (bump-arena) | `Vec<T>` / `Box<[T]>` in IR | `Ty::Tuple/Union/Sum`, generic args, params | contiguous, no per-node heap alloc, `Copy` |
| `smallvec::SmallVec<[T; N]>` | `Vec<T>` for small-N lists | params, generic args, `aliases`, `decorators`, attributes | inline ≤ N (1–4); most of these are tiny → zero heap |
| `nonempty`/`vec1::Vec1` | `Vec<T>` where empty is meaningless | `Type::Sum`/`Union`/`Intersection`, `TraitRef` w/ args | encodes the invariant in the type; removes "is it empty" branches; clarity++ |
| `Arc<[T]>` (→ `triomphe::ThinArc`) | `Vec<T>` shared read-only | shared embedding batches, schema `Vec<Value>` | refcount-bump clone of a whole slice; `ThinArc` stores len inline (one word saved) |
| `im::Vector`/`im::HashMap` | `Vec`/`HashMap` cloned-with-small-edits | **session graph only** (`SessionGraphState`) | structural sharing: merge + clone-for-response become O(log n), not O(n) deep copy — this is the *one* place `im` fits (the IR is build-once, so `im` is wrong there; the session graph is mutate-and-snapshot, so `im` is right) |
| `ecow::EcoVec<T>` | `Vec<T>` cloned but rarely mutated | doc `Value` arrays, edge lists | clone-on-write vec; cheap clone, copy only on mutate |
| `rustc_hash::FxHashMap`/`FxHashSet` | std `HashMap`/`HashSet` | every internal map (audit §9) | non-DoS, ~2–3× faster hashing of small keys |
| `dashmap::DashMap` | `Mutex<HashMap>` per-key | session store (§2) | lock-free per-key concurrent access |
| `bumpalo::Bump` + `bumpalo-herd::Herd` | the global allocator for IR nodes | `IrArena` (single-thread `Bump`; `Herd` if ever built in parallel) | bump allocation + O(1) bulk free; `Herd` gives per-rayon-worker bumps sharing one `'a` |

### 8.3 Concurrency primitives (validated §2)
| Type (crate) | Replaces | Where | Why |
|---|---|---|---|
| `arc_swap::ArcSwap<T>` | `Arc<RwLock<T>>` read-mostly | registry `packages`/`keys` maps | wait-free reads (RCU); writers copy-on-write |
| `tokio::sync::mpsc` + owner task | `Arc<Mutex<Writer>>` | Tantivy writer actor (§2) | serializes, batches commits, off the executor |
| `triomphe::Arc<T>` | `std::sync::Arc<T>` for sized immutable | all §2 shared-immutable data (not `dyn` seams) | no weak-count word (8 B/Arc saved), faster clone |
| `bytes::Bytes` | `Vec<u8>` blob buffers | blobstore get/put through `object_store` | refcount-shared, zero-copy slicing of blob bytes |
| keep `tokio::Mutex`/`Semaphore`/`RwLock`/sqlx pool | — | per-package state, sync_lock, sql | already correct (§2) — do **not** churn |

### 8.4 Serialization, IO, allocator, SIMD (free/high-leverage)
| Type (crate) | Replaces | Where | Why |
|---|---|---|---|
| `#[global_allocator] mimalloc`/`jemallocator` | system malloc | binary entrypoint | 10–30% on this alloc-heavy workload, one line |
| `simd-json` | `serde_json::from_str` | rustdoc/deno JSON parse (`rust/mod.rs:139`) | SIMD-accelerated parse of the large doc JSON |
| `memmap2::Mmap` | `fs::read_to_string` | rustdoc JSON, source files | parse over a mapped slice, skip the read-into-`String` copy |
| `rkyv` *or* `bincode` | `serde_json` blob codec | blobstore (§10.1) | binary, compact, fast; `rkyv` adds zero-copy deserialize |
| `blake3` | random `uuid::v4` blob id | content-addressed `BlobRef` (§10.1) | content hash → dedup + immutability + integrity |
| `wide::f32x8` *or* `pulp` | scalar cosine loop | in-memory `VectorIndex`, client rerank | stable SIMD (std::simd is nightly); 4–8× on dot/norm |

> On the original suggestions: **`nonempty`** → §8.2 (invariant types). **`evec`/`ecow::EcoVec`** → §8.2 (clone-on-write vecs for shared docs/edges). **`im::Vec`** → §8.2, but *only* for the session graph, not the IR. **`triomphe::Arc`** → §8.3 (sized immutable, last). The IR's "exotic" type is none of these — it's the bump-arena `&'a`/`Spur` model (§3), because the IR is build-once.

---

## 9. State isolation → parallelism + devex (ports/adapters + CQRS)

The throughline you're pointing at: **isolated state is what makes both parallelism and developer experience possible.** Two units that share no mutable state can run on different cores *and* be tested independently. The refactor should make state isolation a structural property, not a convention.

### 9.1 Ports & adapters (the seams already half-exist — finish them)
`core/traits.rs` already splits read from write (`SearchQuery` vs `SearchIndex`, `VectorQuery` vs `VectorIndex`, `GlobalSymbolQuery` vs `GlobalSymbolStore`). Make this a hard rule:
- **Each backend is an adapter behind a narrow port; no adapter depends on another.** Today they're separate crates (`blobstore`, `search`, `store`) — good. Keep the dependency graph a star: adapters depend only on `core` (ports), the orchestrator/searcher compose ports, nothing composes adapters directly.
- **Devex payoff:** the whole pipeline runs on in-memory fakes (already exists — `InMemory*`), so logic is testable without Qdrant/Terminus/Tantivy running. Each adapter gets its own integration tests behind a feature flag (`qdrant-integration` already does this). A contributor changing the Qdrant adapter never recompiles or breaks the IR.
- **Parallelism payoff:** because sinks share no state, the fan-out (§5) is trivially correct — `run_sinks` can become `join!` with zero coordination.

### 9.2 CQRS: split `AppState` into a read plane and a write plane
Today `AppState { registry, pipeline, sessions, targets }` is cloned per request and mixes admin/ingest (write) with query (read) concerns. Split it:
```rust
struct QueryState  { searcher: Arc<dyn SymbolSearch>, text: Arc<dyn SearchQuery>,
                     vector: Arc<dyn VectorQuery>, blobs: Arc<dyn BlobStore>, sessions: SessionHandle }
struct AdminState  { registry: Arc<LocalRegistry>, targets: IngestTargets, config: Arc<PipelineConfig> }
```
- **Isolation:** query handlers (`/search`, `/text-search`, `/symbol-search`) take only read ports — they hold *no* handle that ingest writes through, so a sync in progress never contends with a query (no shared lock on the read path at all).
- **Cheap per-request clone:** `QueryState` is a handful of `Arc`s → refcount bumps, not the current deep `PipelineConfig` clone.
- **Deploy/scale separation:** the read plane and write plane can later run as separate processes/replicas (read replicas scale independently of the single-writer ingest), because they're already decoupled in the type system.
- **Devex:** a handler's signature now *documents* whether it reads or writes. No more "does this endpoint mutate the registry?" archaeology.

### 9.3 Pure-stage discipline (the rule that makes parallelism free)
Design each pipeline stage as **`fn(input, &immutable_ctx) -> output`** with no shared mutable state. Projection already satisfies this (G3) — that's *why* it parallelizes. Generalize:
- The only shared thing a parallel stage may touch is **immutable** (`Arc`/`&`) or a **lock-free accumulator** (`ThreadedRodeo` interner, `dashmap`, an mpsc to an actor).
- Cross-item dependencies (cross-reference resolution) become an explicit **separate reduce phase** after the parallel map — which is already the design (refs are stored as placeholders and resolved downstream in the orchestrator, G3).
- **Devex:** a pure stage is trivially unit-testable (feed input, assert output) and trivially benchmarkable in isolation.

### 9.4 Typed handles over stringly-typed state
Extend the existing newtype discipline (`BlobRef`, `RepoId`, `GlobalSymbolId`) to interned handles (`Spur`-backed `SymbolPath`, `PackageId`). Typed ids are `Copy`, prevent mixing (a `RepoId` can't be passed where a `BlobRef` is expected), and make refactors compiler-checked. This is both a perf win (Copy u32 vs `String`) and a devex win (the type system enforces correctness).

---

## 10. Storage-layer refactors (the entire-refactor proposals you invited)

Functionality-preserving structural rewrites of the IO/index layer. Each is independent.

### 10.1 Blobstore → content-addressed, binary, immutable + SQL sidecar
**Today** (`blobstore/lib.rs`): `BlobRef` = random UUIDv4; blob = `serde_json::to_vec(BlobInfo)` **including the float embedding vectors as decimal text**; `update_resolution` does a full **read-modify-write** of the whole blob to flip one `Option` (`:72-83`).

**Refactor:**
1. **Content-address the immutable part:** `BlobRef = blake3(canonical_content)`. Identical symbols (same source across versions/occurrences) collapse to one blob — free dedup. Blobs become immutable → cacheable, integrity-checked, never rewritten.
2. **Split mutable state into a SQL sidecar:** `resolved_global_id` and occurrence associations already belong in the SQLite `GlobalSymbolStore`. Move them there entirely; `update_resolution` becomes a row upsert, not a blob rewrite. The blob is write-once.
3. **Binary codec:** `bincode`/`rkyv` instead of JSON (`rkyv` gives zero-copy reads on `get`). 
4. **Drop embeddings from the blob:** they live in Qdrant; the search path only needs metadata + source snippet (confirmed: `symbols.rs` reads `metadata`/`kind`/`source`, never `embeddings`). This shrinks blobs by the vector payload (often the majority of bytes).
5. **`bytes::Bytes`** end-to-end through `object_store` for zero-copy buffer handling.

**Preserved:** same data, same lookups. **Won:** no read-modify-write, dedup, smaller blobs, zero-copy reads, integrity. **Migration:** re-ingest, or a one-shot copy that re-keys by content hash and moves resolution into SQL.

### 10.2 Qdrant → ONE collection + payload filters (the search-scaling fix)
**Today:** collection-per-package-version (`{prefix}_{lang}_{pkg}_{ver}`, `ingest/mod.rs:191`), and search **fans out sequentially across every collection** (`search/qdrant.rs:45`). With N indexed package-versions, every vector search is N sequential gRPC round-trips. This is a hard scaling wall.

**Refactor:** a **single collection** `{prefix}_symbols` with `lang`/`package`/`version`/`global_id`/`blob_ref`/`purpose` as **indexed payload fields** (Qdrant keyword payload indexes). Search = **one** `query_points` with an optional `Filter` for scope.
- **Won:** one query regardless of corpus size; no fan-out; a single global HNSW graph (better ANN recall than thousands of tiny per-package graphs); scope/version filtering via payload filter; delete/re-index by filter. This is the single biggest search-throughput change in the whole plan.
- **Preserved:** identical scoping semantics (filter replaces collection selection). Point-id scheme (`uuid5(blob_ref|model|purpose)`) unchanged → idempotent upserts still work.
- Also fold in the audit items: `wait(false)` + one final wait per batch; one shared `Arc<Qdrant>`; `Cow<'static,str>` labels; build the per-record payload once where keys are constant.

### 10.3 Search → push filters down, score-then-fetch
**Today** (`symbols.rs:160-224`): fetches full `BlobInfo` for **every** merged candidate (serially) to apply `lang`/`repo_id`/`kind`/occurrence post-filters, *then* sorts and truncates to `limit`. So it deserializes O(2×limit) full blobs (source + vectors) and throws most away.

**Refactor:**
1. **Push scope/kind into the index query** — Qdrant payload filter (§10.2) and Tantivy term/facet filter carry `lang`/`repo_id`/`kind`, so non-matches never come back. Filtering becomes free, at the index.
2. **Score → truncate → fetch.** Merge the (already-filtered) hits, sort by score, `truncate(limit)`, then fetch full blobs for **exactly the final page**, concurrently (`buffer_unordered`). Fetch count drops from O(2×limit) to O(limit), with the heavy `source`/`embeddings` read only for results the user actually sees.
3. **Key merge maps by `BlobRef`/`FxHashMap`,** not cloned `String`s; carry `global_id` from the vector hit so occurrence lookup needs no blob round-trip.

**Preserved:** identical ranking and filter semantics. **Won:** an order-of-magnitude fewer blob fetches + deserializes per query, all concurrent.

### 10.4 Terminus → streaming emit, no intermediate `Value` corpus
**Today:** emit builds a whole `DocStore` (`BTreeMap<URI, Value>`) of the entire crate, dedups, sorts (redundantly — it's a BTreeMap), then clones every `Value` again to upload (audit §5). Triple `Value` materialization per doc.

**Refactor:** implement `Serialize` directly on borrowed IR (`&'a Entry`) and **stream NDJSON documents in bounded batches** straight to the Terminus uploader — no intermediate `Value` trees, no whole-corpus `DocStore`, `@context` referenced once. Where URI-dedup is required, dedup on a streaming `FxHashSet<Spur>` of URIs rather than buffering values. **Preserved:** identical documents uploaded. **Won:** O(batch) memory instead of O(corpus); eliminates the 2–3× `Value` materialization.

### 10.5 Tantivy → writer actor + persistent reader (from §2/§5)
Single-owner writer actor on `spawn_blocking`, batched commits; one `IndexReader` built at open and `reload()`-ed, not per query. Covered in §2.4/§5 — listed here for storage-layer completeness.

---

## 11. Consolidated plan (rev 2 — storage + types + isolation woven in)

| Phase | Theme | Key moves | Risk |
|---|---|---|---|
| **0** | Free wins | mimalloc; FxHashMap; one shared `Arc<Qdrant>`/embedder; `Arc<PipelineConfig>` | trivial |
| **1** | Ownership + isolation | ArcSwap registry; dashmap sessions + persist-outside-lock; **CQRS split `AppState`** (§9.2); `Arc<str>`/`Arc<Value>`/`bytes::Bytes` spine; mutate-in-place | low |
| **2** | Batch the seams | `*_many` trait methods; one commit/wait/request per batch | low/med |
| **3** | Storage refactors | **Qdrant single-collection + payload filters (§10.2)**; **content-addressed binary blobs + SQL sidecar (§10.1)**; **search push-down + score-then-fetch (§10.3)** | med |
| **4** | Actors + streaming | Tantivy writer actor; streaming Terminus emit (§10.4); concurrent sink fan-out; bounded backpressure pipeline | med |
| **5** | Parallelism | dedicated rayon CPU pool + bridge; `par_iter().map_init(Parser::new)` projection; concurrent subprocess fan-out; `simd-json`+`mmap`; `wide` SIMD scoring | med |
| **6** | Arena IR | `IrArena<'a>` + `Spur` interning + hash-cons + `Ty<'a>` Copy; drop `Deserialize`; emit borrows. Index-`u32` fallback if `&'a` infection too broad | high |
| **7** | Finishing | `triomphe::Arc`; `smol_str`/`compact_str`/`ecow` for remaining strings; `im::Vector` session graph; `nonempty` invariants; wire/delete `ParsedBody` yoke | low |

Each phase ships alone. The storage refactors (Phase 3) are the highest-throughput, lowest-architectural-risk of the structural changes — they don't touch the IR types at all, so they can land in parallel with the IR work and **before** the arena refactor. If forced to pick three: **Qdrant single-collection (§10.2), batched seams (§2/§5), CQRS split (§9.2)** — biggest throughput-per-effort, all low/medium risk.

---

*Precedent: the `Ty<'a>` arena/intern/hash-cons model is exactly rustc's `Ty<'tcx>`; the single-collection-with-payload-filters model is Qdrant's own recommended multi-tenancy pattern; content-addressed immutable blobs + mutable SQL sidecar is the git object-store model. We're adopting canonical patterns, not inventing. No source files modified.*

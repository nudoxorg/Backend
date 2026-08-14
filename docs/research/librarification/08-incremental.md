# Incremental Computation Architecture for nudox

**Research date:** 2026-07-16  
**Scope:** Salsa; Build Systems à la Carte; Bazel/Nix/Turborepo/Gradle content-addressed memoization; rust-analyzer/Zoekt/Sourcegraph/GitHub/tantivy incremental indexing; Cursor Merkle embeddings; differential/streaming alternatives; gitoxide change feed.  
**Goal:** Design a **symbol-level incremental pipeline spine** for nudox that works locally (desktop embed) and remotely (k8s fleet), with git commit as the invalidation gate.

---

## 0. Executive framing

nudox’s pipeline is:

```
git commit N
  → acquire source (tree of blobs)
  → per-language producer → typed IR (symbols: id, signature, docs, refs)
  → content-addressed IR blobs (heart::ContentHash / BLAKE3)
  → fan-out stages:
        render docs | embed vectors | tantivy index | TerminusDB graph | (trees)
```

**Requirement:** only symbols whose *canonical IR content hash* changed must flow through downstream stages. Uncommitted edits trigger nothing. Incrementality must survive process restarts (desktop reopen, pod recycle).

This document maps industrial and academic incremental-computation machinery onto that requirement and recommends a **hand-rolled constructive-trace store in SQLite + BLAKE3 digests**, not Salsa-as-orchestrator.

**Existing codebase hooks (live under `workspace/`):**

- `heart::ContentHash` — 32-byte BLAKE3 (`workspace/heart/content.rs`)
- `hash_source_tree` — sorted path ‖ file blake3 (`workspace/compiler/generate/parse_cache.rs`)
- `SourceArchive` / `FileDigest` — per-file content-addressed archive (`workspace/compiler/generate/source_archive.rs`)
- producer CAS hit/miss observer (`workspace/compiler/sandbox/observer.rs`)
- generation/snapshot folding (`workspace/compiler/generate/blob_info.rs`, `mod.rs`)

These already implement the **input half** of a content-addressed build system. The missing piece is a **persistent per-stage constructive trace** at **symbol** granularity, plus a **SymbolDelta** contract between generations.

---

## 1. Salsa (rust-analyzer’s engine) — 2026 state

### 1.1 Version and product trajectory

| Fact | Status (2026-07-16) | Source |
|---|---|---|
| crates.io `salsa` | **v0.28.0** (recent; WIP branding still present) | [crates.io/crates/salsa](https://crates.io/crates/salsa) |
| “Salsa 3” / new salsa | Ported into rust-analyzer; enables future parallel eval + persistency | [Changelog #277](https://rust-analyzer.github.io/thisweek/2025/03/17/changelog-277.html), [PR #18964](https://github.com/rust-lang/rust-analyzer/pull/18964) |
| Disk persistence in RA | **Not implemented** | [Issue #4712](https://github.com/rust-lang/rust-analyzer/issues/4712) |
| salsa serialization goal | Open since 2018; design intent, not production cross-process story | [salsa#10](https://github.com/salsa-rs/salsa/issues/10) |

Salsa remains the best-in-class **intra-process, fine-grained, on-demand** incremental engine for compilers and language servers. It is **not** a multi-process pipeline orchestrator with durable stage caches.

### 1.2 Core model

Documented at [salsa overview](https://salsa-rs.github.io/salsa/overview.html) and [red-green algorithm](https://salsa-rs.github.io/salsa/reference/algorithm.html); deep dive: [Lakhin, “Salsa Algorithm Explained”](https://medium.com/@eliah.lakhin/salsa-algorithm-explained-c5d6df1dd291).

| Concept | Meaning for nudox |
|---|---|
| **Inputs** | Mutable base facts (file text, crate graph edges) |
| **Tracked / derived queries** | Pure `K → V` with memoized results |
| **Revision** | Global counter; bumps on input `set` |
| **Verified-at / changed-at** | Each memo stores when computed and when last validated |
| **Red-green revalidation** | Walk dependency edges; if deps green (unchanged), result green without re-exec |
| **Early cutoff (value equality)** | Even if a dep re-ran, if its *value* is equal, dependents stay green |
| **Durability** | Inputs classified LOW/MEDIUM/HIGH; version vectors per durability so stdlib/deps skip revalidation when only user files change ([Durable Incrementality](https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html)) |
| **Interning** | Structural sharing of strings/paths/IDs in the DB |
| **LRU / GC** | Evict unused memos under memory pressure (implementation-dependent) |

**Symbol-level isolation in RA:** `AstIdMap` + position-independent `ItemTree` means body edits do not invalidate module item trees. That is the same *granularity goal* nudox wants at the IR-symbol layer — but RA achieves it **inside one process’s memo tables**, not via persistent digests.

### 1.3 Costs and assumptions

| Dimension | Salsa reality |
|---|---|
| Memory | Full memo tables for hot queries live in RAM; RA post-salsa-3 migrations saw large memory regressions for some users ([#19402](https://github.com/rust-lang/rust-analyzer/issues/19402)) |
| Process model | Single DB process (multi-threaded OK); queries must be pure |
| Persistence | RA: none across sessions. salsa: experimental/partial serialization work; **not a productized cross-process CAS** |
| Scheduler | On-demand (lazy); not a batch stage graph with explicit workers |
| Side effects | First-class stages that write tantivy/vectors/Terminus **break** the pure-query model unless carefully staged outside the DB |

### 1.4 Verdict for nudox

| Use salsa? | When |
|---|---|
| **Yes (optional, local)** | Inside a language producer that is itself a language server / multi-query analysis (e.g. if a producer embeds RA-like queries). |
| **No (pipeline spine)** | Cross-process: commit → producer → embed → index → graph. Needs durable digests, worker isolation, restart recovery. |

**Recommendation:** treat salsa as an **intra-producer** optimization, not the generation orchestrator. Persist **stage digests**, not salsa memo tables.

---

## 2. Build Systems à la Carte — taxonomy and early cutoff

Primary sources:

- [Mokhov, Mitchell, Peyton Jones — ICFP 2018 PDF](https://www.microsoft.com/en-us/research/wp-content/uploads/2018/03/build-systems.pdf)
- [JFP extended version](https://simon.peytonjones.org/assets/pdfs/build-systems-jfp.pdf)
- [Wagner summary](https://thewagner.net/blog/2019/06/19/build-systems-a-la-carte/)
- [Signals & Threads interview with Mokhov](https://signalsandthreads.com/build-systems/)

### 2.1 Two axes

**Scheduler** (how to order / suspend work):

| Scheduler | Behavior | Examples |
|---|---|---|
| Topological | Static order from known deps | Make (static Makefile) |
| Restarting | If blocked on dep, kill and restart later | Excel, Bazel (classic model) |
| Suspending | Pause task; resume when dep ready | Shake |

**Rebuilder** (how to decide “must re-run?” and what to store):

| Rebuilder | Stores | Properties |
|---|---|---|
| Dirty bit | “stale?” flag | Cheap; weak early cutoff |
| Verifying traces | Dep keys + input hashes | Minimal rebuilds; re-hash inputs |
| **Constructive traces** | Input deps + **output value/digest** | **Early cutoff + cloud cache share** |
| Deep constructive | Whole deep dep cloud | Cloud builds; **weak/no early cutoff** at intermediate levels |

### 2.2 Early cutoff (the load-bearing property)

**Definition:** After re-executing task *T*, if `hash(output_T) == hash(previous_output_T)`, **do not** dirty dependents.

This is exactly “body comment changed → IR symbol hash unchanged → skip re-embed / re-index / re-publish.”

| System | Early cutoff? |
|---|---|
| Make | No (mtime propagation) |
| Shake | Yes (verifying/constructive hybrid; value equality) |
| Bazel | Yes at action boundary (output digests in ActionResult) |
| Nix IA (input-addressed) | No — input change ⇒ new store path even if output identical |
| Nix CA (content-addressed / experimental) | Yes — output path from content; see §3.2 |
| Salsa | Yes — value equality in red-green |

**Minimal machinery for early cutoff:**

1. Pure stage function `f: In → Out`
2. Canonical digest `D_in = digest(In)`, `D_out = digest(Out)`
3. Persistent map: `D_in → D_out` (and pointer to stored `Out` bytes)
4. On rebuild: if `D_out_new == D_out_old`, stop propagation

That is a **constructive-trace rebuilder**. nudox should implement exactly this at **symbol** keys, not only package keys.

### 2.3 Map nudox onto the taxonomy

| nudox component | BSàC role |
|---|---|
| git commit / tree | Input keys (like source files) |
| Producer (per package/file) | Task with dynamic deps (imports) |
| Per-symbol IR hash | **Early-cutoff boundary** (critical) |
| embed / tantivy / terminus / render | Independent constructive-trace stages keyed by symbol content hash |
| SQLite stage_traces table | Constructive-trace store (like Bazel action cache, local) |
| CAS blob store | Content-addressable storage for Out blobs |
| Desktop app / k8s worker | Same rebuilder; different CAS backends |
| Scheduler | Topological DAG of stages + parallel workers (restarting is fine; stages are coarse) |

**Chosen design point:**  
`scheduler = topological_parallel` + `rebuilder = constructive_trace` + `early_cutoff = true` at symbol granularity.

---

## 3. Content-addressed memoization in build tools

### 3.1 Pattern (universal)

```
canonical_inputs → Digest_in
run(stage, inputs) → outputs
outputs → Digest_out
store: Digest_in ↦ (Digest_out, blob_refs)
if Digest_in hit: reuse outputs (cache hit)
if Digest_out equal to prior generation's Digest_out for same logical key: early cutoff
```

### 3.2 Bazel / Buck2 + Remote Execution API

Sources: [remote-apis](https://github.com/bazelbuild/remote-apis), [RE API proto](https://github.com/bazelbuild/remote-apis/blob/main/build/bazel/remote/execution/v2/remote_execution.proto), [BuildBuddy RE explainer](https://www.buildbuddy.io/blog/bazels-remote-caching-and-remote-execution-explained/), [Buck2 RE](https://buck2.build/docs/users/remote_execution/).

| Concept | Role |
|---|---|
| **CAS** | Blobs addressed by content digest (SHA256 or BLAKE3) |
| **Action** | `command_digest` + `input_root_digest` + platform + timeout |
| **Action Cache** | `digest(Action) → ActionResult` (output digests, exit code) |
| **Directory Merkle** | Sorted file/dir nodes → tree digest (canonicalization required) |

**Canonicalization pitfalls Bazel guards:**

- Directory entries sorted by path (UTF-8 code points)
- No absolute paths in action inputs (exec-root relative)
- Explicit env / platform in Action (no ambient machine leak)
- Timestamps stripped from tar/zip inputs where possible
- Toolchain identity part of command digest

**nudox borrow:** Stage keys should look like Bazel Actions:

```
StageKey {
  stage_id: "embed.v3",
  input_digest: ContentHash,   // symbol IR digest or tree of digests
  tool_digest: ContentHash,    // embedding model id + version + prompt template
  config_digest: ContentHash,  // dims, normalize flags, …
}
```

### 3.3 Nix: input-addressed vs content-addressed

Sources: [Nix CA outputs manual](https://nix.dev/manual/nix/2.34/store/derivation/outputs/content-address.html), [RFC 62](https://github.com/nixos/rfcs/blob/master/rfcs/0062-content-addressed-paths.md), [Tweag CAS series](https://tweag.io/blog/2021-12-02-nix-cas-4/), [haskell.nix CA note](https://input-output-hk.github.io/haskell.nix/tutorials/ca-derivations.html) (explicit **early cutoff** language).

| Model | Store path depends on | Early cutoff | Trust |
|---|---|---|---|
| **Input-addressed (classic)** | Hash of derivation graph | No | Need signatures: “builder ran these inputs” |
| **Content-addressed (floating)** | Hash of **output content** | **Yes** | Path is content; multi-builder consensus possible |

CA-derivations remain experimental-ish in ecosystem politics (stabilization incomplete as of early 2026 commentary), but the **idea** is the gold standard for nudox IR: **symbol identity for downstream work is the content hash of canonical IR**, not “how we produced it.”

### 3.4 Turborepo

Sources: [Turborepo caching](https://turborepo.dev/docs/crafting-your-repository/caching), [Remote caching](https://turborepo.dev/docs/core-concepts/remote-caching), [task_hash.rs](https://github.com/vercel/turborepo/blob/main/crates/turborepo-lib/src/task_hash.rs), Rust migration blogs ([Vercel](https://vercel.com/blog/finishing-turborepos-migration-from-go-to-rust)).

- **Global hash** + **task hash** fingerprints
- Inputs: sources (git-aware globs), env (`globalEnv` / `env`), dependency task outputs, `turbo.json`
- Local FS cache dir named by hash; remote cache shares across CI
- Pitfalls: env leaks, accidental inclusion of outputs in inputs, platform-specific paths

**nudox borrow:** separate **global** (toolchain, model, schema version) from **task** (symbol content) hashes so a model bump invalidates all embeds cleanly without conflating with IR changes.

### 3.5 Gradle Build Cache

Sources: [Build cache concepts](https://docs.gradle.org/current/userguide/build_cache_concepts.html), [debugging misses](https://docs.gradle.org/current/userguide/build_cache_debugging.html), [common problems](https://docs.gradle.org/current/userguide/common_caching_problems.html).

**Fingerprint** includes: task class identity, action bytecode, normalized inputs.

**Pitfalls (map 1:1 to nudox):**

| Poison | Fix |
|---|---|
| Absolute paths in IR / docs | Store package-relative paths only |
| Timestamps / build ids | Strip from canonical IR |
| Map/set iteration order | Sort keys before serialize |
| Line endings (CRLF) | Normalize to LF in source acquire **or** hash git blobs (already LF in objects) |
| Float nondeterminism in embeddings | Fixed model seed; store model version in tool_digest; never re-hash float bytes as identity without quantization policy |
| Wall-clock in rendered docs | Exclude or pin |

### 3.6 Canonicalization checklist for nudox IR

Before `symbol_content_hash = BLAKE3(canonical_bytes)`:

1. Deterministic serialization (protobuf/cbor/json with **sorted keys**, fixed field order)
2. No file offsets / byte ranges that move when unrelated text shifts (or isolate body vs signature hashes — see §8)
3. Stable `SymbolId` (from symbol-identity research tracks 05/06) independent of row insertion order
4. Docs: normalized whitespace policy (decide: preserve markdown vs collapse)
5. References: sorted list of target SymbolIds
6. Toolchain versions **out of band** of symbol hash (they belong in stage tool_digest)

---

## 4. Incremental indexing in real code-intel systems

### 4.1 rust-analyzer + salsa

Already covered in §1. Summary table row:

| Property | Value |
|---|---|
| Granularity | Query / item-tree / body (function-level isolation) |
| Persistence | Session-only |
| Invalidation | Revision + durability + red-green |

Architecture: [RA contributing architecture](https://rust-analyzer.github.io/book/contributing/architecture.html).

### 4.2 Zoekt (Sourcegraph trigram search)

Sources: [sourcegraph/zoekt](https://github.com/sourcegraph/zoekt), [design.md](https://github.com/sourcegraph/zoekt/blob/main/doc/design.md), [DeepWiki indexing](https://deepwiki.com/sourcegraph/zoekt/4-indexing-system), [issue #29731 incremental](https://github.com/sourcegraph/sourcegraph-public-snapshot/issues/29731).

| Mechanism | Detail |
|---|---|
| Normal incremental | Skip unchanged **repos** |
| **Delta builds** | Diff prev indexed commit → HEAD; index only changed/added files into small delta shard |
| **File tombstones** | Mark deleted/changed paths in old shards; query-time filter |
| Compound shards | Merge small deltas; `vacuum` physically drops tombstones |
| Fallback | After ~150 deltas, full rebuild |
| Granularity | **File**, not symbol |

**nudox borrow for tantivy:** batch symbol deletes + adds per commit (like delta shard), then merge; do not update one doc at a time in a tight loop without batching.

### 4.3 Sourcegraph Precise (SCIP/LSIF)

Sources: [SCIP announcement](https://sourcegraph.com/blog/announcing-scip), [writing an indexer](https://sourcegraph.com/docs/code-navigation/writing-an-indexer), [scip repo](https://github.com/sourcegraph/scip/).

- SCIP is a **full-repo snapshot** per commit upload
- **No standard delta SCIP format** in production (incremental indexing long on roadmap for large repos)
- Server replaces index for repo@commit; backend dedup not fully public

**Lesson:** precise semantic index *producers* often stay package/repo-scoped; **downstream stores** still can be symbol-delta if IR is content-addressed.

### 4.4 GitHub Code Search (Blackbird)

Source: [GitHub Engineering — technology behind new code search](https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/).

| Design choice | Effect |
|---|---|
| Shard by **git blob OID** | Same content across 1000 repos indexed once |
| Push events → Kafka | Incremental crawl |
| Diff trees for new blob IDs | Only unseen blobs processed |
| Compaction | Physical deletes |
| Consistency | Search reflects consistent commit, not partial push |

**115 TB → 28 TB unique → 25 TB index** — content-addressing is the biggest win.

**nudox borrow:** CAS blobs and embedding keys by **content hash** so cross-repo / cross-user dedup is free (same as Cursor chunk cache, §5).

### 4.5 tantivy

Sources: [ARCHITECTURE.md](https://github.com/quickwit-oss/tantivy/blob/main/ARCHITECTURE.md), [Life of a Segment](https://github.com/quickwit-oss/tantivy/wiki/Life-of-a-Segment), [indexing blog](https://fulmicoton.com/posts/behold-tantivy-part2/), [docs.rs/tantivy](https://docs.rs/tantivy/).

| Operation | Mechanism |
|---|---|
| Add | Buffer → flush segment (UUID) → update `meta.json` |
| Update | **delete_term(primary_key) + add_document** |
| Delete | `.del` alive-bitset tombstone per segment |
| Query | Skip deleted docs via bitset |
| Merge | `LogMergePolicy`; physically drops deletes; background I/O |

**No built-in PK.** Schema must include `symbol_id` (or `symbol_id + content_hash`) as a term field for delete-by-term.

**Cost model:** fine for **batched** symbol deltas per commit; poor for keystroke-level single-doc updates (aligns with commit-gated design).

**Recommended tantivy delta apply:**

```
for id in delta.removed ∪ delta.changed:
    writer.delete_term(Term::from_field_text(symbol_id_field, id))
for doc in delta.added ∪ delta.changed_new:
    writer.add_document(doc)
writer.commit()  // one commit per generation batch
```

---

## 5. Incremental embedding pipelines

### 5.1 Cursor Merkle-tree sync (canonical public design)

Primary: [Cursor — Securely indexing large codebases](https://cursor.com/blog/secure-codebase-indexing) (2026-01-27).  
Secondary analyses: [Engineer's Codex](https://read.engineerscodex.com/p/how-cursor-indexes-codebases-fast), [Towards Data Science](https://towardsdatascience.com/how-cursor-actually-indexes-your-codebase/).

**Pipeline:**

1. Client scans workspace (respect `.gitignore` / `.cursorignore`)
2. Build **Merkle tree**: leaf = file content hash; internal = hash of children
3. Sync tree (hashes, not necessarily full files) to server
4. Diff trees → **only diverging paths** need work
5. Chunk changed files (tree-sitter / AST-aware chunks; avoid mid-function splits)
6. Embed chunks; store vectors (Turbopuffer / remote VDB in public descriptions)
7. Periodic resync (order of minutes in third-party writeups; exact interval product-dependent)

**Properties:**

- Small edit flips leaf + ancestors only → O(log n) path to identify change set
- Chunk content hash ⇒ embedding cache hit if chunk text unchanged
- Security: cross-copy isolation via cryptographic binding of hashes to content

**nudox mapping:**

| Cursor | nudox |
|---|---|
| File Merkle | git tree (already Merkle!) at commit N |
| Chunk hash | **symbol content hash** (better semantic unit) |
| Re-embed changed chunks | Re-embed changed symbols only |
| Server VDB upsert | Qdrant/pgvector/… upsert by `symbol_id` |
| Periodic FS sync | **Commit gate** (stricter, intentional) |

nudox should **not** invent a second Merkle over working trees for the core pipeline; **git is the Merkle**. Optional live FS Merkle is a future “uncommitted preview” feature, out of scope for v1 invalidation.

### 5.2 LlamaIndex / LangChain ingestion caches

Sources: [LlamaIndex ingestion pipeline](https://developers.llamaindex.ai/python/framework/module_guides/loading/ingestion_pipeline/), [0.9 announcement](https://www.llamaindex.ai/blog/announcing-llamaindex-0-9-719f03282945), doc management examples.

- Hash `(node, transformation)` → cache transform output
- Docstore compares document content hashes; skip unchanged docs
- Cross-run iteration speedup; not multi-tenant CAS by default

**Pattern:** content-hash keys on **immutable transform steps** — identical to constructive traces for embed/render.

### 5.3 Vector DB upsert strategies

| Store | Incremental API | Notes |
|---|---|---|
| Qdrant | upsert / delete by id | Good for symbol_id PK |
| Weaviate | merge/replace | Schema versioning care |
| Pinecone | upsert | Serverless; cost ~ ops |
| pgvector | INSERT ON CONFLICT | Transactional with SQLite traces if same host |

**Policy:**

- `changed`: upsert new vector under same `symbol_id`; store `content_hash` payload for debugging
- `removed`: delete point
- `unchanged`: no-op (never touch)
- **Dedup across projects:** optional secondary index `content_hash → embedding_blob`; if hash seen globally, copy pointer instead of re-embed (GitHub Blackbird / Cursor chunk cache)

### 5.4 Embedding nondeterminism

Even with identical text, some model stacks are nondeterministic. Treat:

```
embed_stage_in = H(symbol_content_hash ‖ model_id ‖ model_revision ‖ quant_policy)
```

If the model is bit-deterministic, `embed_out_hash` can early-cutoff further consumers (rare). If not, still skip **calling the model** when `symbol_content_hash` + tool digest unchanged (constructive trace hit on inputs).

---

## 6. Differential / streaming alternatives — applicability

| System | Model | Fit for nudox spine? |
|---|---|---|
| **[differential-dataflow](https://github.com/TimelyDataflow/differential-dataflow)** | Collection diffs over time; Timely Dataflow | **Overkill** for commit-batch symbol graph; excellent if live multi-writer analytical views become core product |
| **[Materialize](https://materialize.com)** / IVM | SQL views maintained by DD | Wrong embedding surface; possible future for “live dependency dashboards” |
| **[Adapton](https://github.com/Adapton)** | Demand-driven self-adjusting | Research ancestor of salsa; no Rust production path for us |
| **Jane Street `incremental`** (OCaml) | Adaptive graphs | Same niche as salsa; not Rust |
| **[comemo](https://github.com/typst/comemo)** | Constrained memoization (Typst) | Lighter than salsa; **in-process** only; good if a renderer needs fine tracked access |
| **memoize / cached crates** | Function memo HashMap | Toy for pure helpers; no dep tracking |
| **Salsa** | Query DB | See §1 — producer-local only |

**Verdict:** do **not** put Timely/DD under the critical path of commit→embed. Use **batch constructive traces**. Revisit DD only if product requires continuous multi-tenant relational queries over symbol graphs at streaming rates.

---

## 7. Git as the change feed

### 7.1 Why commit-gated

| Uncommitted | Commit |
|---|---|
| High churn, partial files | Atomic, reviewable snapshot |
| No stable identity for “generation” | `commit_oid` is Generation id |
| Desktop CPU thrash | Aligns with CI / registry mental model |

**Policy:** debounce until commit (or explicit “index now” command). Intermediate states between commits are **squashed** — only N-1 → N matters.

### 7.2 gitoxide / gix

Sources: [GitoxideLabs/gitoxide](https://github.com/GitoxideLabs/gitoxide), [gix-diff](https://docs.rs/gix-diff), [gix::diff](https://docs.rs/gix/latest/gix/diff/index.html), [lib.rs gix](https://lib.rs/crates/gix) (0.85.x class versions mid-2026).

Capabilities relevant to nudox:

- Pure-Rust repo access (no `git` CLI spawn required)
- **Tree-to-tree diff** via `gix_diff::tree` / `for_each` change callbacks
- Rewrite/rename tracking optional (`tree_with_rewrites`)
- Blob OID = content address (same as Blackbird insight)

**Sketch: files changed between generations**

```rust
// Pseudocode — API names may track gix version; use gix_diff::tree::for_each
fn changed_paths(repo: &gix::Repository, old: gix::ObjectId, new: gix::ObjectId)
    -> Result<Vec<PathChange>>
{
    let old_tree = repo.find_commit(old)?.tree_id()?;
    let new_tree = repo.find_commit(new)?.tree_id()?;
    let mut out = Vec::new();
    // walk tree diff; record Addition / Deletion / Modification { path, old_blob, new_blob }
    // prefer comparing blob OIDs first (content-addressed early cutoff at file level)
    Ok(out)
}
```

**File-level early cutoff:** if `old_blob_oid == new_blob_oid`, path is not “modified” even if rename detection is fuzzy — OID equality is ground truth.

### 7.3 File-level → symbol-level

```
git tree diff (file OIDs)
  → set F of dirty paths
  → producers re-run on packages intersecting F
  → emit IR symbols with content hashes
  → join with previous generation’s symbol hash map
  → SymbolDelta
```

**Producer partial-compilation feasibility (research summary):**

| Language / tool | Partial unit | Notes for nudox |
|---|---|---|
| rust-analyzer | Crate + file queries | Fine-grained; heavy dep |
| rustc incremental | CGU / query | Not embeddable as library easily |
| **OXC / SWC** | Per-file parse + module graph | Good JS/TS partial |
| go/types | **Package** | Dirty import → package recheck |
| tree-sitter | Per-file | Fast CST; not types |
| SCIP indexers | Often whole project | May force package scope |

**Pragmatic rule:**

1. Always re-parse dirty **files** (cheap with tree-sitter / oxc)
2. Re-run **type-aware** producer at the **smallest unit the tool supports** (file or package)
3. Symbol content hashes provide **downstream** early cutoff even if producer over-approximates dirty set

Over-approx on producer is OK; **under-approx is not**. Prefer dirty-superset → symbol-hash filter.

### 7.4 Mapping generations

```
Generation {
  id: u64,                    // monotonic local
  git_commit: ObjectId,       // source truth
  parent_generation: Option<u64>,
  tree_digest: ContentHash,   // optional fold of path→blob
  created_at: Timestamp,
}
```

---

## 8. Synthesis — the incremental spine

### 8.1 Stage graph

```
                    ┌─────────────┐
                    │ Generation  │
                    │ git commit  │
                    └──────┬──────┘
                           │ file delta (gix)
                           ▼
                    ┌─────────────┐
                    │  Producer   │  (per language / package)
                    │  → IR blob  │
                    └──────┬──────┘
                           │ SymbolDelta (content hashes)
              ┌────────────┼────────────┬────────────┐
              ▼            ▼            ▼            ▼
          ┌───────┐   ┌────────┐  ┌─────────┐  ┌──────────┐
          │render │   │ embed  │  │ tantivy │  │ terminus │
          └───┬───┘   └───┬────┘  └────┬────┘  └────┬─────┘
              │           │            │            │
              └───────────┴────────────┴────────────┘
                           │
                           ▼
                    constructive traces
                    (sqlite + CAS)
```

Each box is a **Stage** with `digest(in)` and stored `out_digest`.

### 8.2 Hash layers (coarse → fine)

| Layer | Key | Purpose |
|---|---|---|
| L0 Tree | git commit / tree OID | Gate work |
| L1 File | blob OID / BLAKE3 of bytes | Skip unchanged files |
| L2 Package IR | fold of symbol hashes + package meta | Producer cache (exists directionally in generate/) |
| **L3 Symbol** | `BLAKE3(canonical_symbol_ir)` | **Primary early-cutoff for fan-out** |
| L4 Stage | `BLAKE3(stage_id ‖ tool ‖ L3)` | Per-downstream memo |

**Optional split hashes** (advanced):

```
symbol_sig_hash   = H(name, signature, visibility, …)   // API surface
symbol_body_hash  = H(docs, body summary, …)            // narrative / embed text
symbol_ref_hash   = H(sorted refs)
symbol_content_hash = H(sig ‖ body ‖ refs)              // full
```

Then embed stage may depend only on `body_hash` (or full), while Terminus graph edges depend on `ref_hash`. A docs-only edit re-embeds and re-indexes text but may skip graph edge rewrite if refs unchanged.

### 8.3 SymbolDelta contract

```rust
/// Computed at generation boundary after producers finish.
pub struct SymbolDelta {
    pub generation: u64,
    pub parent: Option<u64>,
    pub added: Vec<SymbolId>,
    pub removed: Vec<SymbolId>,
    pub changed: Vec<SymbolChange>,
    // unchanged: implicit = previous_universe - removed - changed.ids
}

pub struct SymbolChange {
    pub id: SymbolId,
    pub old_hash: ContentHash,
    pub new_hash: ContentHash,
    // optional split hashes for partial stage invalidation
    pub old_parts: Option<SymbolPartHashes>,
    pub new_parts: Option<SymbolPartHashes>,
}

pub struct SymbolPartHashes {
    pub sig: ContentHash,
    pub body: ContentHash,
    pub doc: ContentHash,   // doc-v1 — required for EmbedStage partial invalidation
    pub refs: ContentHash,
    // embed_key is derived (see 09b §3.3 / §16); may be cached on vector payload
}
```

**Where computed:** after all dirty packages’ IR is sealed into CAS; compare `symbol_id → content_hash` map for generation N vs N-1 (SQLite table `symbol_heads`).

**How each downstream consumes:**

| Stage | added | removed | changed |
|---|---|---|---|
| **tantivy** | add_document | delete_term(id) | delete_term + add |
| **vectors** | embed+upsert | delete point | re-embed+upsert **only if `embed_key` (facet) changed**; skip model when stage_traces/CAS hit — see `09b-retrieval-pipeline-plan.md` §16 |
| **terminus** | insert doc | delete / replace | JSON diff-patch or replace doc |
| **render** | write page | delete page | rewrite page |
| **trees** | store CST blob | drop | replace if file blob changed |

**Vector incrementality invariant (GD-28):** local EmbedStage iterates only `added ∪ changed`; unchanged symbols never call the embedder. Part-hash matrix (doc-only vs body-only vs sig-only) is frozen in 09b §16.3.

### 8.4 Trace store (persistence)

**Why SQLite:** embeds in desktop app; simple ops on server (or migrate to Postgres later with same schema); transactional generation commits.

```sql
-- logical head of each symbol at a generation
CREATE TABLE symbol_heads (
  generation     INTEGER NOT NULL,
  symbol_id      TEXT    NOT NULL,
  content_hash   BLOB    NOT NULL,  -- 32 bytes
  part_sig       BLOB,
  part_body      BLOB,
  part_refs      BLOB,
  ir_blob        BLOB    NOT NULL,  -- ContentHash of CAS object
  PRIMARY KEY (generation, symbol_id)
);
CREATE INDEX symbol_heads_by_hash ON symbol_heads(content_hash);

-- constructive traces: stage memo
CREATE TABLE stage_traces (
  stage_id       TEXT NOT NULL,      -- e.g. 'embed.v3'
  input_digest   BLOB NOT NULL,      -- 32 bytes
  output_digest  BLOB NOT NULL,
  output_blob    BLOB NOT NULL,      -- CAS pointer or inline small
  tool_digest    BLOB NOT NULL,
  created_at     INTEGER NOT NULL,
  PRIMARY KEY (stage_id, input_digest, tool_digest)
);

-- generation metadata
CREATE TABLE generations (
  id             INTEGER PRIMARY KEY,
  git_commit     BLOB NOT NULL UNIQUE,
  parent_id      INTEGER,
  status         TEXT NOT NULL,  -- 'running'|'sealed'|'failed'
  sealed_at      INTEGER
);

-- optional: file blob index for producer skip
CREATE TABLE file_blobs (
  generation     INTEGER NOT NULL,
  path           TEXT NOT NULL,
  blob_oid       BLOB NOT NULL,
  PRIMARY KEY (generation, path)
);
```

**Generation seal protocol (atomic):**

1. Insert `generations` row `status=running`
2. Run producers; write IR to CAS; write staging symbol rows
3. Compute SymbolDelta vs parent
4. Apply fan-out stages (idempotent by stage_traces)
5. Transaction: flip `status=sealed`; promote staging → `symbol_heads`

Crash mid-run: leave `running`; next process reaps or restarts generation.

### 8.5 Stage trait sketch (Rust)

Align with existing `heart::ContentHash`:

```rust
use heart::ContentHash;

pub trait Stage: Send + Sync {
    type In: StageInput;
    type Out: StageOutput;

    /// Stable stage identity including semantic version.
    fn stage_id(&self) -> &'static str;

    /// Tool/model/config fingerprint (global invalidation lever).
    fn tool_digest(&self) -> ContentHash;

    /// Canonical input digest. Must be pure and deterministic.
    fn input_digest(&self, input: &Self::In) -> ContentHash;

    /// Expensive work. Must be deterministic w.r.t. tool_digest policy.
    fn run(&self, input: &Self::In) -> Result<Self::Out, StageError>;

    /// Digest of output for early cutoff / CAS keying.
    fn output_digest(&self, out: &Self::Out) -> ContentHash;
}

pub trait StageInput {
    fn canonical_bytes(&self) -> Vec<u8>; // or stream into ContentHasher
}

/// Orchestrator: constructive-trace rebuilder
pub fn run_stage<S: Stage>(
    traces: &TraceStore,
    cas: &Cas,
    stage: &S,
    input: &S::In,
) -> Result<S::Out, StageError> {
    let din = stage.input_digest(input);
    let tool = stage.tool_digest();
    if let Some(hit) = traces.get(stage.stage_id(), &din, &tool)? {
        return cas.get_decoded(&hit.output_blob);
    }
    let out = stage.run(input)?;
    let dout = stage.output_digest(&out);
    let blob = cas.put_encoded(&out)?;
    traces.put(stage.stage_id(), din, tool, dout, blob)?;
    Ok(out)
}
```

**Example embed stage input:**

```rust
pub struct EmbedIn {
    pub symbol_id: SymbolId,
    pub vector_name: VectorName,     // "sym" | "sig" | "body"
    pub embed_key: ContentHash,      // 09b §3.3 — recipe+model+facet hashes
    pub text: String,                // EmbedTextBuilder output (pure fn of IR parts)
}

impl Stage for EmbedStage {
    fn stage_id(&self) -> &'static str { "embed.v2.sym" /* or per vector_name */ }

    fn tool_digest(&self) -> ContentHash {
        // model_id @ rev, recipe_id, dim, metric, embedder build fingerprint
        // EP choice (CPU vs CoreML) should not fork CAS identity — see 09b §20.4
        ...
    }

    fn input_digest(&self, input: &EmbedIn) -> ContentHash {
        // Do NOT use coarse content_hash alone once multi-rep + recipe versioning exist.
        // embed_key already domain-separates facets (doc-only edit ≠ body-only).
        input.embed_key
    }
    // ...
}
```

Full write algorithm, delete/rename matrix, and acceptance test: **`09b-retrieval-pipeline-plan.md` §16**.

### 8.6 Desktop vs k8s

| Concern | Desktop | k8s fleet |
|---|---|---|
| Trace store | SQLite in app data dir | Postgres or SQLite on PVC; or split: traces in Postgres, CAS in object store |
| CAS | `heart` DiskCas local | S3/GCS + CDN; same ContentHash keys |
| Workers | In-process stage runners | Separate deployables per stage; pull jobs by generation |
| Concurrency | Single writer generation | Lease generation id; workers idempotent via stage_traces |
| Dedup | Local only | Global CAS: same symbol hash → one embed blob cluster-wide |

**Idempotency:** workers may re-run `run_stage`; PRIMARY KEY on traces makes second writer a no-op.

### 8.7 End-to-end algorithm (one commit)

```
on_git_commit(commit):
  parent = last sealed generation for repo
  files = gix_tree_diff(parent.git_commit, commit)  // OID-based
  if files empty: return

  gen = open_generation(commit, parent)

  dirty_packages = map_files_to_packages(files)
  for pkg in dirty_packages:
      if producer_cache_hit(pkg, file_oids): reuse IR
      else: ir = producer.run(pkg); cas.put(ir)

  heads = load_symbol_heads(parent)
  new_heads = fold_ir_to_symbol_map(dirty_packages) ∪ (heads \ symbols_from_dirty)
  delta = diff_maps(heads, new_heads)

  parallel:
    apply_tantivy(delta)
    apply_embed(delta)      // only added/changed; stage_traces skip re-embed
    apply_terminus(delta)
    apply_render(delta)

  seal_generation(gen, new_heads)
```

### 8.8 Recommendation: salsa vs hand-rolled

| Criterion | Salsa | Hand-rolled digest + SQLite traces |
|---|---|---|
| Cross-process | Weak / unproven for pipeline | Native |
| Restart survival | No (RA) | Yes |
| Side-effect stages | Awkward | First-class |
| Coarse batch commits | Overkill machinery | Natural |
| Fine keystroke IDE | Excellent | Wrong tool |
| Cloud CAS share | Not designed for | Same digests as Bazel/Nix |
| Ops complexity | High (memo GC, revisions) | Low (SQL + blobs) |
| Fit nudox | Producer-internal only | **Pipeline spine** |

**Decision: hand-rolled constructive-trace store.**  
Optionally use salsa/comemo *inside* a producer binary. Do not serialize salsa DBs as the product’s incrementality mechanism.

---

## 9. Concrete recommendations for librarification

### R1 — Introduce `Generation` + `SymbolDelta` in `heart`

Shared types used by compiler, registry, desktop, workers. Do not redefine per crate.

### R2 — Make L3 symbol content hash mandatory in IR seal

Every emitted symbol must carry `content_hash` (and optional part hashes). Producer tests: reorder fields / change unrelated whitespace policy → stable hash golden tests.

### R3 — SQLite `stage_traces` + `symbol_heads` MVP

Ship local desktop first; same schema on server. Generation seal in a single transaction.

### R4 — Fan-out consumers are pure functions of SymbolDelta

tantivy / embed / terminus / render adapters implement `Stage` and share `run_stage`. No consumer may scan full repo “just in case.”

### R5 — gitoxide for file deltas; blob OID short-circuit

Prefer gix over shelling to `git`. File OID equality ⇒ skip producer inputs for that path.

### R6 — Global embedding dedup by content hash

CAS key for vectors: `H(model ‖ symbol_content_hash)`. Cross-repo savings like Blackbird/Cursor.

### R7 — Tool digests for model/schema bumps

Changing embed model writes new `tool_digest`; old traces remain for rollback; new generation recomputes embeds without IR re-extract if IR hashes unchanged.

### R8 — Commit gate only for v1

Do not index uncommitted buffers in the durable pipeline. Optional future: ephemeral session index.

### R9 — Over-approx producer dirty set; never under-approx

Correctness of SymbolDelta depends on producers seeing all files that could affect symbols. When unsure, re-run package.

### R10 — Observability

Metrics: files_dirty, symbols_added/removed/changed, stage_cache_hit_ratio, early_cutoff_ratio, embed_calls_saved. Without these, incrementality regressions are silent.

### R11 — Align with Terminus tiering (report 07)

Graph publish only for delta documents; layer/rollup costs already studied. SymbolDelta.removed/changed feed Terminus replace/patch.

### R12 — Do not build on salsa for orchestration

Re-evaluate only if salsa ships production-grade multi-process persistence *and* side-effect story — not true as of 2026-07-16.

---

## 10. Risks and open questions

| Risk / question | Impact | Mitigation |
|---|---|---|
| Unstable SymbolId across renames | False remove+add; embed churn | Identity research (05/06); rename detection via content similarity / git renames |
| Canonical IR nondeterminism | Cache miss storm | Golden hash tests; sort everything; forbid wall-clock |
| Producer package-level only | Extra work on fan-out… mitigated by L3 | Accept; invest in L3 |
| Embedding float noise | Spurious “changed” if hashing vectors | Key cache on inputs, not float bytes |
| SQLite write contention on server | Seal latency | Postgres or per-repo SQLite; generation leases |
| Tombstone accumulation in tantivy | Query slowdown | Scheduled merge; batch commits |
| Partial SCIP-like precision | Cross-file refs stale if under-approx | Package dirty rules; ref hash part |
| CA Nix still experimental | N/A directly | Borrow idea only |
| Multi-branch generations | Parent selection | Track generation per (repo, commit); build DAG not only linear |
| Very large SymbolDelta (reformat) | Embed cost spike | Detect mass-change; rate-limit; optional “sig-only” skip for pure formatting if body_hash policy ignores format |
| Security multi-tenant CAS | Hash collision / poison | BLAKE3 256-bit; authenticate tenants; don’t trust client-supplied hashes without rehash |

**Open design choices to resolve in implementation RFCs:**

1. JSON vs protobuf for canonical IR bytes  
2. Whether docs whitespace is significant for embed hash  
3. Single SQLite vs split traces/CAS metadata  
4. Rename: treat as remove+add vs stable id move  
5. Cross-user embedding sharing legal/privacy policy  

---

## 11. Comparison matrix (systems → nudox)

| System | Granularity | Persistence | Early cutoff | Takeaway |
|---|---|---|---|---|
| Salsa / RA | Query | Session | Yes (value) | Producer-local |
| Shake | Task | Disk DB | Yes | Trace store model |
| Bazel RE | Action | CAS+AC | Yes (output digest) | StageKey shape |
| Nix IA | Derivation | Store | No | Avoid pure IA for symbols |
| Nix CA | Output content | Store | Yes | Ideal identity model |
| Turborepo | Task | Local+remote | Input-hash skip | Global vs task hash |
| Gradle | Task | Build cache | Fingerprint | Canonicalization hygiene |
| Zoekt | File | Shards | Delta+tombstone | tantivy batching |
| SCIP | Repo snapshot | Server | Limited | Don’t wait for delta SCIP |
| Blackbird | Blob | Persistent | Blob dedup | Content-address everything |
| Cursor | File/chunk | Server VDB | Merkle diff | Symbol hash > file chunk |
| tantivy | Document | Segments | delete+add | PK=symbol_id |
| DD/Materialize | Tuple | Cluster | Continuous | Overkill for commit batch |
| gitoxide | Tree entry | git objects | Blob OID | Change feed |

---

## 12. Minimal MVP implementation plan

| Phase | Deliverable | Success metric |
|---|---|---|
| **P0** | `ContentHash` already exists; add `SymbolDelta` + `Generation` types | Types compile in heart |
| **P1** | `symbol_heads` + seal on producer output | Second commit with no code change → empty delta |
| **P2** | gix tree diff → dirty packages | Only touched packages re-produced |
| **P3** | `stage_traces` + embed stage | Re-commit docs-only on one symbol → 1 embed call |
| **P4** | tantivy delete+add batch | Index size stable; search sees updates |
| **P5** | Terminus delta publish | Graph query reflects delta only |
| **P6** | Metrics + hit ratio dashboard | hit ratio > 90% on typical commits |
| **P7** | Remote CAS + worker idempotency | Desktop and k8s same digests |

---

## 13. Worked example

**Commit N-1 → N:** edit function body of `foo` in `src/lib.rs`; comment-only change that **does not** affect canonical IR (e.g. ignored whitespace in body if policy strips it) vs **does** affect docs string.

### Case A — IR-canonical unchanged (pure formatting stripped)

1. gix: `lib.rs` blob OID changed  
2. Producer re-runs file/package  
3. `foo.content_hash` **equal** → not in `changed`  
4. All stages early-cutoff; traces unused for fan-out  
5. Work: producer only  

### Case B — docs string changed

1. File dirty → producer  
2. `foo` in `changed` with new hash  
3. embed: miss on input_digest → 1 model call → upsert  
4. tantivy: delete+add one doc  
5. terminus: patch docs field  
6. Other symbols: untouched  

### Case C — signature changed

1. As B, plus ref edges may change  
2. If `part_refs` changes: graph edge rewrite for dependents **if** producer re-emits reverse refs; may dirty more symbols (correctness over minimalism)

---

## 14. Appendix — key URLs

### Salsa / rust-analyzer
- https://crates.io/crates/salsa  
- https://salsa-rs.github.io/salsa/overview.html  
- https://salsa-rs.github.io/salsa/reference/algorithm.html  
- https://github.com/salsa-rs/salsa/issues/10  
- https://github.com/rust-lang/rust-analyzer/issues/4712  
- https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html  
- https://rust-analyzer.github.io/thisweek/2025/03/17/changelog-277.html  
- https://medium.com/@eliah.lakhin/salsa-algorithm-explained-c5d6df1dd291  

### Build systems theory
- https://www.microsoft.com/en-us/research/wp-content/uploads/2018/03/build-systems.pdf  
- https://simon.peytonjones.org/assets/pdfs/build-systems-jfp.pdf  
- https://thewagner.net/blog/2019/06/19/build-systems-a-la-carte/  
- https://signalsandthreads.com/build-systems/  

### Bazel / Buck / RE
- https://github.com/bazelbuild/remote-apis  
- https://github.com/bazelbuild/remote-apis/blob/main/build/bazel/remote/execution/v2/remote_execution.proto  
- https://www.buildbuddy.io/blog/bazels-remote-caching-and-remote-execution-explained/  
- https://buck2.build/docs/users/remote_execution/  

### Nix
- https://nix.dev/manual/nix/2.34/store/derivation/outputs/content-address.html  
- https://github.com/nixos/rfcs/blob/master/rfcs/0062-content-addressed-paths.md  
- https://tweag.io/blog/2021-12-02-nix-cas-4/  
- https://input-output-hk.github.io/haskell.nix/tutorials/ca-derivations.html  

### Turborepo / Gradle
- https://turborepo.dev/docs/crafting-your-repository/caching  
- https://turborepo.dev/docs/core-concepts/remote-caching  
- https://docs.gradle.org/current/userguide/build_cache_concepts.html  
- https://docs.gradle.org/current/userguide/common_caching_problems.html  

### Code search / indexing
- https://github.com/sourcegraph/zoekt  
- https://github.com/sourcegraph/sourcegraph-public-snapshot/issues/29731  
- https://sourcegraph.com/blog/announcing-scip  
- https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/  
- https://github.com/quickwit-oss/tantivy/blob/main/ARCHITECTURE.md  
- https://github.com/quickwit-oss/tantivy/wiki/Life-of-a-Segment  

### Embeddings / RAG
- https://cursor.com/blog/secure-codebase-indexing  
- https://read.engineerscodex.com/p/how-cursor-indexes-codebases-fast  
- https://developers.llamaindex.ai/python/framework/module_guides/loading/ingestion_pipeline/  

### Differential / memo
- https://github.com/TimelyDataflow/differential-dataflow  
- https://github.com/typst/comemo  

### gitoxide
- https://github.com/GitoxideLabs/gitoxide  
- https://docs.rs/gix-diff  
- https://docs.rs/gix/latest/gix/diff/index.html  

---

## 15. Executive summary

nudox needs **deep incrementality** at **symbol** granularity, gated by **git commits**, durable across **process restarts**, identical in spirit on **desktop and k8s**. Research across seven areas converges on one design:

**Model the pipeline as a Build Systems à la Carte *constructive-trace* system with early cutoff**, not as a Salsa database. Salsa (v0.28 on crates.io; “new salsa” in rust-analyzer) remains excellent for **intra-session, pure, fine-grained** queries, but rust-analyzer still does **not** persist caches across restarts, and salsa’s process model fights multi-stage side effects (tantivy, embeddings, Terminus). Use salsa only inside producers if at all.

**Early cutoff** is the load-bearing property: when stage inputs change but **canonical output digests** do not, dependents must not run. Bazel Action Cache, Nix CA-derivations (conceptual), Shake, and salsa value-equality all implement this; classic Make and input-addressed Nix do not. nudox should implement constructive traces explicitly:

`StageKey(stage_id, input_digest, tool_digest) → (output_digest, CAS blob)`.

**Content-address everything with `heart::ContentHash` (BLAKE3).** Git blob OIDs and sorted path trees (already in `parse_cache::hash_source_tree`) provide file-level skip. **Symbol content hashes** of canonical IR provide the fan-out boundary: embeddings regenerate only for changed symbols; tantivy uses delete-by-term + add; Terminus publishes document deltas; render rewrites pages. Cursor’s Merkle sync is the product-proof for “hash tree → re-embed only diffs”; nudox should use **git as the Merkle** and **symbols as the embed unit** (better than file chunks).

**Gitoxide** supplies efficient tree diffs between sealed generations; map dirty files → packages → producers (over-approximate safely) → **SymbolDelta {added, removed, changed}**. Persist `symbol_heads` and `stage_traces` in **SQLite** (desktop) with the same schema remotely. Workers are idempotent. Global CAS enables Blackbird-style cross-repo embed dedup.

**Reject for spine:** differential dataflow/Materialize (streaming IVM overkill for commit batches), pure dirty-bit Make semantics, waiting for delta-SCIP, keystroke-driven durable reindex.

**MVP path:** Generation types → symbol hash seal → gix file delta → stage_traces for embed → tantivy batch → Terminus delta → metrics on hit ratios. Success is a docs-only edit to one symbol causing **one** embed and **one** index update, not a repository-wide rebuild.

---

*End of report.*

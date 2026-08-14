# Nested synthesis for task 08

# Nested agent agent-ac518ff6043909742
Description: Research Salsa 2026 state and persistence story
Size: 162679

Task: Research the current 2026 state of the Salsa incremental computation framework, focusing on version, persistence, memory model, and cross-process capability.


# Nested agent agent-ac44f92b89c37c065
Description: Research Build Systems a la Carte taxonomy and early cutoff
Size: 221947

Researching "Build Systems à la Carte" taxonomy and early cutoff implementations.


# Nested agent agent-acaec5ab08959c138
Description: Research content-addressed memoization in Bazel, Nix, Turborepo, Gradle
Size: 232700

I'll research content-addressed memoization patterns in build tools for the architecture document.


# Nested agent agent-a66acf37d59caaa46
Description: Research incremental indexing in rust-analyzer, Zoekt, Sourcegraph, tantivy
Size: 150392

I'll research incremental indexing in real code-intelligence systems across all five areas.

---

**Task:** Research incremental indexing in rust-analyzer, Zoekt, Sourcegraph SCIP, GitHub Code Search, and tantivy. Findings below:

---

## 1. rust-analyzer + Salsa

**Model:** rust-analyzer uses [salsa](https://github.com/salsa-rs/salsa) — an incremental computation framework where everything is a query `K → V`. Two kinds: *Inputs* (mutable base facts) and *tracked functions* (memoized pure transforms). Salsa tracks which outputs depend on which inputs and recomputes only what changed.

**Durability tiers:** Salsa uses a "version vector" rather than a single global version counter. Queries are classified by durability (e.g. "durable" for stdlib/deps, "volatile" for user-edited files). When a user edits `lib.rs`, only the volatile version increments; queries about stdlib can skip revalidation because the durable version hasn't changed. This is rust-analyzer's [Durable Incrementality](https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html) optimization.

**File change propagation:** Changes propagate via salsa's revision mechanism: salsa bumps a revision counter, waits for in-flight threads to finish, then allows dependent queries to rerun lazily on next access. The key invariant: **typing inside a function body never invalidates global derived data** — e.g., name resolution queries for other modules remain memoized.

**Symbol-level granularity:** RA uses `AstIdMap` — a position-independent mapping from `AstId` to syntax nodes — so lowering queries produce `ItemTree` structures in position-independent form. A change to a function body doesn't invalidate the item tree for the module. This is how function-body-level isolation works.

**Cross-session persistence:** **Salsa does NOT persist to disk.** Each IDE session rebuilds from scratch. This is a long-standing open issue ([#4712](https://github.com/rust-lang/rust-analyzer/issues/4712), labeled E-hard, S-unactionable). The salsa 3.0 migration ([PR #18964](https://github.com/rust-lang/rust-analyzer/pull/18964)) is in-progress as of late 2024/early 2025 (tests pass, but performance regressions exist). The RA team says salsa 3.0 makes persistence *possible in the future* — not implemented. **Bottom line: salsa is an intra-session engine only in 2026.**

**Salsa 3.0 improvements:** Parallel evaluation planned; tracked structs now first-class; interned structs optimized (removing `MemoTable`/`SyncTable` when not needed). See [changelog #277](https://rust-analyzer.github.io/thisweek/2025/03/17/changelog-277.html) (March 2025): RA upgraded to latest salsa, enabling parallel evaluation and persistency "in the future."

---

## 2. Zoekt (Sourcegraph's trigram code search)

**Source:** [github.com/sourcegraph/zoekt](https://github.com/sourcegraph/zoekt), [issue #29731](https://github.com/sourcegraph/sourcegraph-public-snapshot/issues/29731), [DeepWiki indexing overview](https://deepwiki.com/sourcegraph/zoekt/4-indexing-system).

**Normal indexing:** Zoekt produces `.zoekt` shard files per repository. Each shard contains trigram posting lists. Incremental flag: skip repos that haven't changed since last run (coarse-grained check).

**Delta builds (per-commit incremental):** The key innovation. Rather than re-indexing an entire repo per commit, delta builds:
- Fetch the diff between the previous indexed commit and the new HEAD
- Index **only changed/added files** → write a new small delta shard
- For **removed/changed files** in old shards: write a **file tombstone** — a metadata marker saying "any search results involving this file should be ignored"
- Search at query time filters out tombstoned files transparently

**Granularity:** File-level (not symbol-level). Zoekt is a trigram text search engine, not a semantic indexer.

**Compound shards:** Small delta shards are periodically merged into larger compound shards via the `merge` operation. The inverse is `explode` (split compound back into simples, used during vacuuming). `vacuum` physically removes tombstoned entries. `cleanup` moves unreferenced shards to `.trash/` with 24-hour retention.

**Compaction trigger:** After ~150 accumulated delta shards, falls back to a full rebuild. This is a pragmatic bound to prevent unbounded shard proliferation and passively clear corrupted state.

**Allowlist:** Delta indexing is opt-in per repository; enabled via allowlist.

---

## 3. Sourcegraph Precise Indexes (SCIP/LSIF)

**Sources:** [SCIP announcement](https://sourcegraph.com/blog/announcing-scip), [indexers docs](https://sourcegraph.com/docs/code-navigation/writing-an-indexer), [scip repo](https://github.com/sourcegraph/scip/).

**Current model:** SCIP (successor to LSIF) is a snapshot format — an indexer runs over a full repo at a commit and produces a `.scip` file containing all occurrences, definitions, and references. This is uploaded to Sourcegraph per commit (usually triggered by CI/CD). **No delta/incremental SCIP format exists** — each upload is a full snapshot.

**Incremental indexing roadmap:** Sourcegraph has stated that incremental indexing is on the roadmap: for large repos, full indexing can take 1-2 hours; incremental indexing would reduce this to minutes. The goal is to index every commit. As of 2025, this is **not yet shipped** as a production feature in SCIP.

**Backend handling:** When a new SCIP upload arrives, Sourcegraph's backend replaces the previous index for that repo+commit. The deduplication/diffing happens server-side. No public documentation of the exact store mechanism for deltas.

---

## 4. GitHub Code Search (Blackbird)

**Source:** [GitHub Engineering Blog — The technology behind GitHub's new code search](https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/).

**Key design:** Blackbird is written in Rust and shards its index by **Git blob object ID** (content hash), not by repository. This is the critical insight: because Git already content-addresses file contents, the same file content appearing in 1000 repos is indexed exactly once.

**Incremental updates on push:** Blackbird listens to `git push` events via Kafka. On push, it diffs the new commit tree against the repository's parent in the "delta tree" — fetching only new blob IDs not yet indexed. Since sharding is by blob SHA, previously-seen content requires zero reprocessing. This reduces crawl volume by >50%.

**Deduplication:** 115 TB raw code → 28 TB unique content → 25 TB final index. The blob-addressed design means cross-repo deduplication is automatic.

**Deletion:** Handled during compaction runs (shorter intervals for ongoing ops than initial indexing). Document deletion is done during compaction.

**Consistency:** Commit-level query consistency — users won't see partial states from in-flight pushes. Each search reflects a consistent prior commit.

**Throughput:** ~120,000 documents/second indexing; 640 QPS search.

---

## 5. tantivy

**Sources:** [ARCHITECTURE.md](https://github.com/quickwit-oss/tantivy/blob/main/ARCHITECTURE.md), [Life of a Segment wiki](https://github.com/quickwit-oss/tantivy/wiki/Life-of-a-Segment), [tantivy indexing blog](https://fulmicoton.com/posts/behold-tantivy-part2/), [docs.rs](https://docs.rs/tantivy/).

**Segment model:** A tantivy index is a collection of immutable segments, each identified by UUID. Documents accumulate in memory, then are flushed to a new segment on commit. `meta.json` is atomically updated to list all current segments.

**Update pattern:** tantivy has no in-place update. The pattern is: `delete_term(primary_key_term) + add_document(new_version)`. Delete marks the document in a `.del` bitset file (alive bitmap) per segment — `segment_id.commit_opstamp.del`. Query time skips deleted doc IDs via the bitset.

**Delete-by-term:** The `IndexWriter::delete_term(term)` call fans out across all segments to find and tombstone matching documents. This is how you update a symbol entry: delete by `symbol_id` term, then add the new document.

**Segment merging:** `LogMergePolicy` (default) merges segments logarithmically. Merges: (1) reduce segment count to keep query performance reasonable; (2) physically eliminate tombstoned documents (they're not removed until merge). Merges run in background threads. The cost is I/O: reading old segments, writing merged output, updating meta.json.

**Opstamp:** Every operation (add/delete/commit) gets a monotonically increasing `opstamp`. This is used to version the delete bitsets.

**No built-in primary key / versioning:** You must implement this via a schema field. Tantivy is "designed to handle updates in large batches" — it's not optimized for single-document live updates, but works well for batched symbol delta processing.

---

## Summary Table

| System | Granularity | Persistence | Incremental model |
|---|---|---|---|
| rust-analyzer/salsa | Function/query | In-session only (disk: not implemented) | Red-green revalidation, durability tiers |
| Zoekt | File | Persistent shards on disk | Delta shards + tombstones, compound merge |
| Sourcegraph SCIP | Repo snapshot | Server-side per commit | Full re-upload per commit (incremental on roadmap) |
| GitHub Blackbird | Blob (content hash) | Persistent, blob-addressed | Blob-addressed dedup, event-driven diff on push |
| tantivy | Document (by term) | Persistent segments on disk | delete-by-term + add + background merge |

Sources:
- [Durable Incrementality — rust-analyzer blog](https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html)
- [rust-analyzer architecture](https://rust-analyzer.github.io/book/contributing/architecture.html)
- [Persistent caches issue #4712](https://github.com/rust-lang/rust-analyzer/issues/4712)
- [Port to salsa 3.0 (HackMD)](https://hackmd.io/@salsa/B19OUlA71l)
- [Changelog #277 (salsa upgrade)](https://rust-analyzer.github.io/thisweek/2025/03/17/changelog-277.html)
- [Incremental indexing to Zoekt — issue #29731](https://github.com/sourcegraph/sourcegraph-public-snapshot/issues/29731)
- [Zoekt DeepWiki: Indexing System](https://deepwiki.com/sourcegraph/zoekt/4-indexing-system)
- [sourcegraph/zoekt GitHub](https://github.com/sourcegraph/zoekt)
- [SCIP announcement](https://sourcegraph.com/blog/announcing-scip)
- [SCIP indexers docs](https://sourcegraph.com/docs/code-navigation/writing-an-indexer)
- [GitHub Code Search technology](https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/)
- [tantivy ARCHITECTURE.md](https://github.com/quickwit-oss/tantivy/blob/main/ARCHITECTURE.md)
- [Life of a Segment — tantivy wiki](https://github.com/quickwit-oss/tantivy/wiki/Life-of-a-Segment)
- [tantivy indexing blog](https://fulmicoton.com/posts/behold-tantivy-part2/)


# Nested agent agent-a762043a92e214f16
Description: Research Cursor Merkle tree sync and incremental embedding pipelines
Size: 141232

Task: Research incremental embedding pipeline architectures — Cursor Merkle sync, LlamaIndex caching, LangChain, general RAG incrementality, vector DB upsert strategies.


# Nested agent agent-a6ca0eff45f390ad9
Description: Research differential dataflow, comemo, adapton, incremental crates
Size: 211066

I'll research differential/streaming incremental computation frameworks for applicability to a cross-process code-intelligence pipeline.


# Nested agent agent-a30459d8f35b787ed
Description: Research gitoxide tree diff and per-language partial compilation
Size: 113500

Researching git-as-change-feed: gitoxide tree diff API, commit-gated invalidation, and per-language partial compilation feasibility.



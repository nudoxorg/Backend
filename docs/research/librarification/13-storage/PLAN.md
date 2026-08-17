# Source + Tree + IR Storage Design for INDEX / REGISTRY

**Research date:** 2026-07-16  
**Scope:** Efficient storage of (1) source code, (2) resolved tree-sitter artifacts, and (3) typed IR for both remote INDEX (sqlite + S3) and local REGISTRY (sqlite + disk). Reference platforms, tree-sitter persistence decision, compression/CAS tradeoffs, local layout, sync manifests.  
**Codebase anchors:** `workspace/heart/content.rs`, `workspace/heart/cache/disk.rs`, `workspace/registry/store.rs`, `workspace/registry/blob/mod.rs`, `workspace/compiler/generate/source_archive.rs`, `workspace/compiler/generate/cst.rs`.

---

## 0. Problem statement and design goals

nudox must store, for every package **generation** (content-addressed snapshot):

| Artifact | Why it exists | Access pattern |
|---|---|---|
| **Source files** | GUI code views, re-parse, audit | Random single-file read; bulk sync |
| **Tree-sitter “resolved trees”** | CST snippet extraction, implementation resolution | Hot path on open-file / go-to-impl |
| **Typed IR** (`ir::entry::Index` etc.) | Search, symbols, lineage, API surface | Whole-blob load + index fan-out |
| **Extracted references** | Cross-file refs without holding CST | Load with IR or on demand |

**Goals (ranked):**

1. **Integrity** — every durable blob is self-authenticating under BLAKE3 (already `ContentHash` in `heart`).
2. **Cross-version dedupe** — package v1.2 and v1.3 share unchanged files.
3. **Sync-friendliness** — REMOTE→LOCAL is “pull missing hashes,” not rsync of trees.
4. **Bounded memory** — never require full-package materialization.
5. **Local footprint** — desktop REGISTRY must stay under user quota with LRU/pin GC.
6. **Single-file random access** — open one source file without downloading the whole generation.

**Non-goals:**

- Persist live `arborium_tree_sitter::Tree` / `TSTree` as opaque C memory.
- Git-object compatibility (we are not a forge).
- Chunk-level CDC across the whole corpus as a day-1 requirement.

---

## 1. docs.rs storage internals (reference pattern)

### 1.1 What docs.rs stores

[docs.rs](https://docs.rs/) hosts rustdoc HTML for crates.io packages. Operational docs live on [Rust Forge — docs.rs](https://forge.rust-lang.org/docs-rs/index.html). Source: [github.com/rust-lang/docs.rs](https://github.com/rust-lang/docs.rs).

Historically (and still relevant as the tension that drove design):

- **DB vs storage split:** metadata (crate name, version, build status, features) in Postgres; generated documentation **files** in object storage (S3-compatible) or, in early local mode, a files table.
- **Per-file S3 objects:** each HTML/CSS/JS asset was its own object. Advantage: single-page fetch is cheap. Disadvantage: crates with thousands of pages pay S3 PUT/LIST costs and make “download whole crate docs” painful ([issue #1004](https://github.com/rust-lang/docs.rs/issues/1004), closed via [#1342](https://github.com/rust-lang/docs.rs/pull/1342)).

### 1.2 Archive storage migration (the reusable pattern)

Issue [#1004](https://github.com/rust-lang/docs.rs/issues/1004) states the tradeoff cleanly:

> Currently, docs.rs stores each generated HTML file individually on S3. This has the advantage that downloading a single page is fast and efficient, but means that it's very expensive to download all files for a crate… Docs.rs should instead store a single archive per crate and compress the entire archive.

The resulting **archive storage** pattern (confirmed in codebase discussions and `src/db/file.rs` call sites such as `add_path_into_remote_archive` / `store_all_in_archive` in public issue/PR text):

1. **One archive object per crate-version build** (ZIP is the practical choice because the central directory + per-member local headers enable **random member access** without full extract).
2. **Optional local index of member offsets** (an “archive index”) so the server can issue an S3 **HTTP Range** request for just one HTML page instead of downloading the whole ZIP.
3. **DB still owns the catalog** (which builds exist, which path maps to which archive).

Why ZIP rather than tar.gz:

- **tar.gz is not random-access** without an external index; gzip is a single stream.
- ZIP compresses **per member** (or stores uncompressed) and the central directory lists offsets — see [zip crate](https://docs.rs/zip) and [piz](https://docs.rs/piz) notes that each file is independently compressible with a directory locating members.
- S3 **byte-range GETs** ([AWS Range header docs](https://docs.aws.amazon.com/AmazonS3/latest/userguide/range-get-olap.html), [byte-range best practices](https://docs.aws.amazon.com/whitepapers/latest/s3-optimizing-performance-best-practices/use-byte-range-fetches.html)) then pull only the compressed member + needed headers.

MinIO even documents a server-side [S3 ZIP extension](https://docs.min.io/aistor/developers/s3-zip-extension/) that surfaces objects *inside* a ZIP as if they were S3 keys (with the constraint that the ZIP central directory must sit in the last 100 MB). That validates the industry pattern; nudox does not need MinIO-specific features if we keep an explicit offset index.

### 1.3 Compression on docs.rs

Early discussion: [Compress documentation uploaded to S3 #379](https://github.com/rust-lang/docs.rs/issues/379). The archive migration is about **aggregating** objects and compressing at crate grain, not trained zstd dictionaries. There is **no evidence** that docs.rs trains per-language zstd dictionaries as of this research. They care about:

- fewer PUT operations,
- cheaper whole-crate download,
- still-acceptable single-page latency via range reads.

### 1.4 CDN / caching

Public docs.rs sits behind Cloudflare/CDN (infra details on Forge). Cacheability of immutable versioned doc objects is high: a published crate version’s docs are content-stable. Mutable “latest” redirects are the exception.

### 1.5 Reusable pattern for nudox

| docs.rs lesson | nudox application |
|---|---|
| Catalog in DB, bulk in object store | Postgres/sqlite catalog + `cas/` blobs (already) |
| Per-file vs archive tension | Prefer **per-file CAS** for *source* (dedupe); optional **archive pack** for cold transfer only |
| Range-read + index | Needed only if we pack many files into one object |
| Immutable version = long CDN TTL | Generation hash is the cache key |

**Critical difference:** docs.rs HTML is **not shared across crate versions** (every rebuild rewrites paths). Source code **is** heavily shared across versions. Therefore docs.rs’s archive-per-version default is wrong as the *primary* layout for nudox source — CAS-per-file wins on storage.

---

## 2. Other reference platforms

### 2.1 Sourcegraph — SCIP + gitserver

**SCIP** ([Source Code Intelligence Protocol](https://scip-code.org/), [announcement](https://sourcegraph.com/blog/announcing-scip), [future of SCIP](https://sourcegraph.com/blog/the-future-of-scip)):

- Protobuf schema; human-readable symbol IDs (not LSIF’s opaque graph IDs).
- Index files are **~4–5× smaller** than LSIF compressed; Meta’s Glean integration reported **~8× smaller / 3× faster** to process vs LSIF ([announcement blog](https://sourcegraph.com/blog/announcing-scip)).
- SCIP stores **semantic facts** (definitions, references, relationships) + document paths/ranges — **not** full CSTs and **not** full source.

**Storage split at Sourcegraph (architectural):**

| Layer | What | Dedup / access |
|---|---|---|
| **gitserver** | Git packfiles / repos as source of truth for file bytes | Git’s own delta compression + pack locality |
| **code-intel / SCIP store** | Uploaded indexes processed into backend tables / blobs | Per-commit index; incremental indexing is a design goal of SCIP |
| **search indices** | Zoekt etc. | Separate derived store |

**Lesson for nudox:** separate **source substrate** from **semantic index**. SCIP is closest to our **IR + references** blobs, not to source storage. Do not invent a second “tree store” when IR already carries navigation facts; use source + tree-sitter only where IR is intentionally coarse (implementation bodies, snippet fidelity).

Cross-repo navigation notes: [Cross-repository code navigation](https://sourcegraph.com/blog/cross-repository-code-navigation) (2026).

### 2.2 GitHub — packfiles + code-nav artifacts

GitHub’s substrate for source is **git**:

- Packfiles store objects with delta compression against similar objects (often prior versions of the same blob).
- Tree objects provide hierarchical manifests; commits pin trees.
- Source archives (tarball/zip of a ref) are **derived** and intentionally **not** bit-stable forever ([GitHub blog on archive hash stability](https://github.blog/2023-02-21-update-on-the-future-stability-of-source-code-archives-and-hashes)).

Code navigation (stack graphs / precise nav) produces **artifacts separate from git**, analogous to SCIP — regenerated per analysis version.

**Lesson:** git pack delta is excellent for **line-of-development** history of one repo. nudox’s unit is **package generation across a registry**, where cross-package and cross-version identical files (e.g. vendored LICENSE, same `src/lib.rs` across patch versions) benefit more from **global content addressing** than git deltas.

### 2.3 crates.io — `.crate` on S3 + CDN

[crates.io](https://crates.io) stores published crate tarballs (`.crate` = gzipped tar) on S3, served via CDN. One object per **crate version**. No cross-version file-level dedupe of tarball members at the CDN layer; compression is gzip at archive grain.

Metadata (crate index) is a separate git/sparse index — the famous “index vs cargo” split.

**Lesson:** simple and operationally proven for **publish-once** artifacts. Poor fit when many versions share 90% of files and clients want single-file access without unpacking.

### 2.4 deps.dev

[deps.dev](https://deps.dev) / [google/deps.dev](https://github.com/google/deps.dev) focuses on **dependency graphs, advisories, licenses** — metadata at package-version grain, not full source hosting. Storage lessons are closer to “catalog + external source pointers” than blob design.

### 2.5 Zeal / Dash docsets — sqlite-per-docset (highly relevant to local REGISTRY)

Official format ([Kapeli docset guide](https://kapeli.com/docsets)):

```
Name.docset/
  Contents/
    Info.plist
    Resources/
      Documents/          # HTML (or other) payload files
      docSet.dsidx        # SQLite search index
  icon.png                # optional
```

SQLite schema (minimal):

```sql
CREATE TABLE searchIndex(
  id INTEGER PRIMARY KEY,
  name TEXT,
  type TEXT,
  path TEXT
);
CREATE UNIQUE INDEX anchor ON searchIndex (name, type, path);
```

Distribution: `tar -czf Name.tgz Name.docset` + XML feed with `<version>` + `<url>` ([Kapeli](https://kapeli.com/docsets)). Zeal consumes the same format ([zealdocs.org](https://zealdocs.org/), [zealdocs/zeal](https://github.com/zealdocs/zeal)).

**Lessons for local REGISTRY:**

1. **Catalog (sqlite) + payload directory** is the proven desktop offline pattern.
2. Index is **rebuildable** from payloads + a generator script — same as our derived tantivy/qdrant from blobs.
3. **Per-docset isolation** simplifies GC (delete one folder) but **destroys cross-docset dedupe**. For nudox, invert: **shared global CAS**, sqlite rows point at hashes (Dash cannot do this because docsets are third-party).

### 2.6 DevDocs

[DevDocs](https://github.com/freeCodeCamp/devdocs) is an offline-capable docs browser: documentation is scraped into a structured format, indexed client-side, with offline storage in the browser (IndexedDB / service worker caches historically). Grain is **docset / language**, not content-addressed multi-version package IR. Relevant only as “offline first UI over a local index,” not as blob layout.

### 2.7 Nix NAR + narinfo (best prior art for signed CAS package artifacts)

Nix binary caches ([cache.nixos.org](https://cache.nixos.org/), [binary cache spec writeup](https://fzakaria.com/2021/08/12/a-nix-binary-cache-specification)):

1. **`{hash}.narinfo`** — small metadata: store path, **NAR hash** (content hash of the archive), references, compression, optional **signature**.
2. **`{hash}.nar.xz`** (or similar) — the payload bytes, addressed by the hash advertised in narinfo.
3. Client flow: fetch narinfo → verify signature → fetch NAR → verify hash → unpack to store.

This is exactly “manifest + content-addressed blob + optional signature” and maps almost 1:1 onto nudox generation sync (see §6).

Related: [nix-casync](https://flokli.de/posts/2021-12-10-nix-casync-intro) applies casync chunking to NAR storage for better dedupe of large binaries — relevant only if we ever store multi-MB single blobs (native libs), not typical source files.

### 2.8 IPLD / Merkle-DAG

[IPLD](https://ipld.io/) / [IPFS Merkle-DAG docs](https://docs.ipfs.tech/concepts/merkle-dag/) model content-addressed linked data: each node’s identifier is a hash of its payload **and** the identifiers of its children ([ProtoSchool verifiability](https://proto.school/merkle-dags/05/)). Benefits called out by IPLD ([benefits of content addressing](https://ipld.io/docs/motivation/benefits-of-content-addressing/)): immutability, **dedupe across DAGs**, cacheability, partial retrieval of subgraphs.

A generation manifest whose entries are BLAKE3 hashes **is** a shallow Merkle DAG (root = hash of manifest encoding; leaves = file/IR blobs). We do not need full IPLD codecs or CIDs day one; we need the **invariant**: root hash commits the entire generation, and leaves can be fetched/verified independently.

### 2.9 Comparison matrix

| System | Unit of storage | Cross-version dedupe | Single-file random access | Local offline model |
|---|---|---|---|---|
| docs.rs | archive or per-file HTML | low | range+index or per-file | N/A (online) |
| crates.io | `.crate` tarball | none (whole archive) | extract only | cargo cache |
| Sourcegraph | git + SCIP | git packs | git blob | Zoekt/local clones |
| Dash/Zeal | docset folder + sqlite | none across docsets | filesystem path | **docset** |
| Nix cache | narinfo + NAR | store-path / CA paths | full NAR | `/nix/store` |
| **nudox (proposed)** | **manifest + per-file CAS** | **global BLAKE3** | **direct CAS get** | **sqlite + cas/** |

---

## 3. Tree-sitter tree persistence — definitive recommendation

### 3.1 Status of official Tree serialization (2026)

**There is no stable public API to serialize/deserialize `TSTree` / Rust `tree_sitter::Tree` to disk as a live tree.**

Evidence:

1. **C API** ([tree_sitter/api.h](https://github.com/tree-sitter/tree-sitter/blob/master/lib/include/tree_sitter/api.h)): `ts_tree_copy` is **in-memory only** (deep copy of the tree for concurrent use). `ts_tree_print_dot_graph`, S-expression debug printers exist; no `ts_tree_serialize`.
2. **Discussion #2293** ([How to build a Tree from sexp?](https://github.com/tree-sitter/tree-sitter/discussions/2293), May 2023) — maintainer **ahlinc** answered:

   > Tree-sitter doesn't support such case.  
   > 1. Trees are much more complex internally and the S-Expression representation shows just a part.  
   > 2. If it were possible to serialize the tree it would take **much more space on disk than the original text**.  
   > So for the first parse it's better to **store an original text and maybe in a compressed form**.

3. **Issue #1942 / PR #2594** ([Caching or Serializing a TSQuery](https://github.com/tree-sitter/tree-sitter/issues/1942), [PR #2594](https://github.com/tree-sitter/tree-sitter/pull/2594)): work is about serializing **queries and highlight configurations**, **not** syntax trees. PR exposes `SerializableQuery` / highlight configs via serde — open/unmerged discussions emphasize versioning risks. Irrelevant to CST persistence.
4. **WASM / web-tree-sitter**: trees are JS/WASM heap objects with explicit dispose; still no durable binary format ([Pulsar modern tree-sitter series](https://blog.pulsar-edit.dev/posts/20240902-savetheclocktower-modern-tree-sitter-part-7/)).

### 3.2 Re-parse cost: is it cheap enough?

Tree-sitter is designed for **editor-scale** full and incremental parse:

- **Primary source claim (Max Brunsfeld, FOSDEM/talks):** a ~20,000-line / ~600+ KB JavaScript file (React dev build) parses in **~54–69 ms** — on the order of **~10 MB/s** for that grammar ([talk transcript excerpts widely cited](https://www.youtube.com/watch?v=Jes3bD6P0To); similar figure ~69 ms in related talks). That is the load-bearing “re-parse is free enough” number.
- tree-sitter-rust is “2–3× slower than rustc’s hand-written parser” per [tree-sitter-rust](https://github.com/tree-sitter/tree-sitter-rust) README — still interactive for multi-MB files.
- Incremental `ts_tree_edit` + reparse is **sub-millisecond to low-millisecond** for keystroke edits (Pulsar/Neovim deployments; [Modern Tree-sitter part 7](https://blog.pulsar-edit.dev/posts/20240902-savetheclocktower-modern-tree-sitter-part-7/)).
- Grammar quality matters: pathological grammars can be far slower until fixed ([tree-sitter-haskell 50× speedup story](https://owen.cafe/posts/tree-sitter-haskell-perf/)).
- For **offline package open** (not per-keystroke): a 50k-LOC package (~1–2 MB source) re-parses in well under a second on modern CPUs even single-threaded; parallel per-file makes wall time smaller.

**Quantified sketch (order-of-magnitude; scale from ~10 MB/s JS figure and 2–5× worse languages):**

| Package | Source size | Parse @ 10 MB/s | Parse @ 2 MB/s |
|---|---|---|---|
| 100 files / 50k LOC | ~1.5 MB | ~0.15 s | ~0.75 s |
| Single hot file 2k LOC | ~50 KB | ~0.005 s | ~0.025 s |
| 20k-LOC single file (~0.6 MB) | 0.6 MB | ~0.06 s (matches talk) | ~0.3 s |

GUI “open package” can parse **in parallel per file**. Single-file code view is negligible. **Persisting trees cannot beat “open file + 5–50 ms parse” for interactive paths.**

**Storage cost of full tree projection** (kind + ranges only, ~16–24 bytes/node, often **1–3 nodes per LOC**):

| Package | LOC | Nodes ~2×LOC | Raw projection | zstd ~3× |
|---|---|---|---|---|
| 50k LOC | 50k | 100k | ~2–2.5 MB | ~0.7–1 MB |

That is **comparable to compressed source**, while losing the ability to run arbitrary queries without either re-parse or a much richer projection. Maintainer claim that full internal trees exceed source size remains the upper bound.

### 3.3 Options evaluated

| Option | Description | Pros | Cons |
|---|---|---|---|
| **(A) Don’t persist trees** | Persist source + grammar version; re-parse on demand | Matches upstream guidance; zero format risk; already in codebase | CPU on cold open; grammar upgrade = re-parse |
| **(B) Persist projection** | Flat array of `(kind_id, start, end, parent)` | Fast snippet bounds without re-parse; versioned | Redundant with source; grammar-version invalidation; extra write path |
| **(C) Zero-copy projection (rkyv/flatbuffers)** | mmap projection blobs | Fast reload | Same as B + rkyv versioning discipline |

### 3.4 What the codebase already does (authoritative)

`workspace/registry/blob/mod.rs`:

> The CST is **not** stored — it is re-parsed on demand (tree-sitter is fast and its output is non-portable). What we *do* persist from parsing is the set of extracted `ResolvedReference` spans…

`workspace/compiler/generate/cst.rs`:

> The live tree-sitter `Tree` is C-allocated and non-serializable… The tree itself is transient — parsed, walked for references, and dropped.

`BlobManifest` fields: `files`, `ir_ref`, `references_ref` — **no tree_ref**.

### 3.5 RECOMMENDATION: Option A by default; optional B as a cache tier

**Primary:** **Do not persist trees.** Persist:

1. Source bytes (CAS).
2. **Grammar identity**: language + grammar crate/name + version (or BLAKE3 of grammar WASM/so) in generation metadata.
3. **`references_ref`** IR-side projection (already).
4. IR blobs (already).

**Optional process-local / disk cache (not part of generation identity):**

- Key: `blake3(source_hash ‖ grammar_id ‖ projection_format_version)`.
- Value: compact projection (§3.6) for files that were recently used for snippet extraction.
- Evict freely; never sync as part of generation manifest.

This matches how editors work (parse cache is ephemeral) and how docs.rs treats rebuildable HTML.

### 3.6 Optional projection format sketch (if we ever need B)

Versioned, little-endian, mmap-friendly without rkyv required:

```
magic: b"NDXT"              // nudox tree projection
u16 format_version = 1
u16 kind_table_len
u32 node_count
u32 source_len
[grammar_id: 32 bytes blake3]
[kind_table: kind_table_len × { u16 id; u8 name_len; utf8 name }]  // or omit names, use grammar built-in ids
// nodes in postorder (children before parent) — parent_idx points forward
nodes: node_count × {
  u16 kind_id
  u32 start_byte
  u32 end_byte
  u32 parent_idx   // u32::MAX = root
  u32 first_child_idx  // or 0 + child_count
  u16 child_count
}
// optional: error node flags bitset
// trailer: blake3 of all above for integrity
```

**Encoding choices:**

- **Postorder + parent_idx** allows walking up for “enclosing function” without full child lists if we also store first_child/child_count.
- **kind_id** is grammar-relative — **projection is invalid when grammar_id changes**.
- Prefer **not** to use rkyv for a structure this simple; raw POD arrays mmap cleanly. Use **rkyv** for IR if/when IR load latency dominates (rkyv 0.8.x is stable; [rkyv.org](https://rkyv.org/), [crates.io rkyv](https://crates.io/crates/rkyv), latest 0.8.8 as of late 2024 release stream — verify pin at implement time).
- `yoke` (already in IR path) is for self-referential zero-copy views; only worth it if projection is large and we want to borrow from mmap without copy.

**Do not** put projection hashes into `BlobManifest::identity_bytes` unless we want grammar bumps to invalidate generation freshness — they should not.

---

## 4. Source storage design

### 4.1 Status quo in nudox (keep and harden)

Per-file CAS is already the product direction:

- `SourceArchive` / `FileDigest` in `compiler/generate/source_archive.rs` — path + BLAKE3 + size.
- `BlobManifest.files: NonEmpty<FileEntry>` — same idea for registry.
- `heart::ContentHash` = BLAKE3-256.
- `registry::Store`: `cas/{blake3-hex}` + `ptr/{package-uuid}`.
- `heart::cache::disk::DiskCas`: local `cas/{hex}` with envelope `blake3(value) ‖ value`.

### 4.2 Per-file CAS vs per-package archive

| Dimension | Per-file CAS | Per-package ZIP/tar archive |
|---|---|---|
| Cross-version dedupe | **Excellent** (shared files) | **None** (unless external CDC) |
| Single-file access | One GET | Range + index or full download |
| PUT cost (S3) | High file count | One PUT |
| Sync | Hash set difference | Whole archive or custom |
| Small-file overhead | Object metadata per file | Amortized |
| Integrity | Per-file hash | Archive hash + member checksums |

**Empirical intuition for libraries:** patch releases often change **&lt;10%** of files; minor releases **10–40%**. Per-file CAS then stores ~0.1–0.4× of naive multi-version retention.

**When archives still win:**

- **Cold egress** of an entire generation to a user with empty cache (one stream, better compression via larger window if using solid zstd — see §4.4 tradeoff).
- **Air-gapped export** (one signed tarball).

**Recommendation:** **CAS-per-file is the system of record.** Optionally build a **transfer pack** (seekable zstd of concatenated files + offset table, or ZIP of already-compressed members) as a pure **cache optimization** for first sync, never as the only copy.

### 4.3 Chunk-level dedupe (FastCDC / casync / restic)

[FastCDC](https://docs.rs/fastcdc) and [casync](https://github.com/systemd/casync) split large blobs at content-defined boundaries so similar large files share chunks.

| Use case | Verdict |
|---|---|
| Typical source file 1–50 KB | **Overkill** — file is already the chunk; CDC overhead dominates |
| Minified single-file bundles, multi-MB generated sources | Maybe |
| Native `.so` / JDK dumps inside packages | Maybe |
| Whole-registry global compression | Engineering cost high; restic/borg-class system |

**Recommendation:** **no FastCDC day one.** Revisit only for blobs above a threshold (e.g. &gt;1 MB) if metrics show waste.

### 4.4 Compression strategy

#### 4.4.1 zstd defaults

- **Level 3–5** for interactive compress (ingest path).
- **Level 9–12** for cold archival packs offline.
- Store **uncompressed** when size &lt; ~128 bytes after framing (or when compressed size ≥ raw).

#### 4.4.2 Trained dictionaries (small files)

Facebook’s zstd docs show dramatic gains on **small homogeneous** samples ([zstd dictionary mode](https://github.com/facebook/zstd), [FB engineering post](https://engineering.fb.com/2016/08/31/core-infra/smaller-and-faster-data-compression-with-zstandard/)): e.g. JSON set **2.8× → 6.9×**.

Gregory Szorc’s Firefox `omni.ja` experiment: discrete zstd-12 files ~9.2 MB → dictionary mode ~7.9 MB (+dict) (~12%+ savings on already-similar small files) — [blog](https://gregoryszorc.com/blog/2017/03/07/better-compression-with-zstandard/).

RocksDB documents meaningful wins for **blocks ≤8–16 KB** with preset dictionaries ([RocksDB dictionary compression](http://rocksdb.org/blog/2021/05/31/dictionary-compression.html)).

**For nudox source:**

- Source languages are **diverse** across packages; a single global dict is weak.
- **Per-ecosystem dictionaries** (`dict/rust-src-v1`, `dict/python-src-v1`) trained offline on a corpus can help **1–5 KB** files.
- IR/postcard blobs are **highly homogeneous** → **dictionary compression is a clear win** for IR and reference sets.

**Practical policy:**

| Blob class | Compression |
|---|---|
| Source ≥ 4 KB | zstd-3, no dict (or ecosystem dict if measured ≥15% gain) |
| Source &lt; 4 KB | ecosystem dict if available else raw/zstd-3 |
| IR / references / manifests | zstd-3 + **trained dict** pinned by `dict_id` |
| Transfer pack (whole gen) | seekable zstd, large window, no per-file dict |

Record `codec: raw | zstd | zstd+dict` and `dict_id` in blob envelope or sidecar so readers can decompress.

#### 4.4.3 Seekable zstd

Spec: [zstd seekable format](https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md). Independent frames + skippable seek table at end (magic `0x8F92EAB1`). Rust: [zeekstd](https://news.ycombinator.com/item?id=44284871), [zstd-framed](https://crates.io/crates/zstd-framed).

**Use for:** optional generation transfer packs and very large single files.  
**Do not use for:** primary per-file CAS (files are already random-accessible as separate objects).

#### 4.4.4 Envelope recommendation (unify remote + local)

Extend the local `DiskCas` idea to a versioned envelope:

```
u32 magic = 0x4E445842  // "NDXB"
u8  version = 1
u8  codec    // 0=raw, 1=zstd, 2=zstd+dict
u16 dict_id  // 0 if none
u32 raw_len
u32 comp_len
[comp_len bytes payload]
// key is still blake3(raw_payload) OR blake3(envelope)? 
```

**Integrity rule (critical):** the CAS key must be **`blake3(raw logical bytes)`** of the user-visible content (source file, IR postcard, …), **not** the compressed envelope. Compression is a transport/storage encoding. Remote `get_section` already verifies `blake3(bytes)==key` on stored bytes today — if we introduce compression, either:

1. Store **raw** in CAS (simplest, current), compress only on the wire; or  
2. Store compressed envelope but key by **raw hash**, and verify after decompress (preferred for disk savings).

Recommend **(2)** for local REGISTRY; remote INDEX may start with **(1)** and migrate.

### 4.5 Dedup rates — quantitative model

Assumptions for a popular crate family (e.g. 20 versions retained):

- Mean version delta: 15% of files changed; changed files average 30% size of package.
- Unchanged file bytes fraction ≈ 0.85.

Storage for N versions:

- **Archive-per-version:** `N × S` (minus gzip redundancy only within version).
- **Per-file CAS:** `S + (N−1) × 0.15 × S` ≈ `S × (0.85 + 0.15N)`.

For N=20: archives ~20S; CAS ~3.85S → **~5× savings**.

Cross-package dedupe (same `LICENSE`, vendored copies) is extra free under global CAS.

---

## 5. Local REGISTRY layout (desktop) and remote INDEX mapping

### 5.1 Design principles

1. **Same logical schema** local and remote; only the blob backend differs (filesystem vs S3 via `object_store`).
2. **Derived stores are disposable** (tantivy, vectors) — rebuild from blobs + catalog.
3. **Dash-like UX**, Nix-like addressing: folders users can wipe, hashes machines trust.

### 5.2 Proposed directory tree (local)

```
$NUDOX_HOME/                          # e.g. ~/Library/Application Support/nudox
  registry.sqlite                     # catalog (WAL mode)
  registry.sqlite-wal
  registry.sqlite-shm
  cas/
    ab/
      cd/
        abcd…{64 hex}.blob            # sharded by first 2+2 hex (or 2+2+2)
  tmp/                                # atomic publish staging
  tantivy/
    main/                             # package search index
  vectors/
    <model-id>/                       # qdrant-lite / custom or sqlite-vss — TBD by 09-vector
  trees/                              # OPTIONAL ephemeral projection cache (not synced)
    <source_hash>_<grammar_id>.ndxt
  packs/                              # OPTIONAL downloaded transfer packs
  dicts/
    rust-src-v1.zstd-dict
    ir-postcard-v1.zstd-dict
  pins.json                           # or table in sqlite — active project pins
```

**Sharding:** flat `cas/{64hex}` (current `DiskCas`) is fine until tens of thousands of files; move to `cas/ab/cd/{full}` when directory entries hurt (ext4/APFS still fine for 100k files; shard preemptively for Windows friendliness).

### 5.3 SQLite catalog schema (sketch)

Aligned with existing `BlobManifest` concepts and Dash’s “sqlite index + files” split:

```sql
-- packages & generations
CREATE TABLE package (
  package_id   BLOB PRIMARY KEY,      -- UUIDv5 16 bytes
  origin       TEXT NOT NULL,
  name         TEXT NOT NULL,
  version      TEXT NOT NULL,
  ecosystem    TEXT NOT NULL,
  UNIQUE(origin, name, version)
);

CREATE TABLE generation (
  generation_hash BLOB PRIMARY KEY,   -- blake3 of identity_bytes (Hash ①)
  package_id      BLOB NOT NULL REFERENCES package(package_id),
  manifest_hash   BLOB NOT NULL,      -- CAS key of postcard manifest (Hash ②)
  toolchain       TEXT NOT NULL,      -- or blob ref
  created_at      INTEGER NOT NULL,
  last_access_at  INTEGER NOT NULL,   -- for LRU
  pin_level       INTEGER NOT NULL DEFAULT 0,  -- 0=evictable, 1=user pin, 2=active project
  source_bytes    INTEGER NOT NULL,   -- accounting
  ir_bytes        INTEGER NOT NULL,
  ref_bytes       INTEGER NOT NULL,
  file_count      INTEGER NOT NULL
);

CREATE TABLE generation_file (
  generation_hash BLOB NOT NULL,
  path            TEXT NOT NULL,
  content_hash    BLOB NOT NULL,      -- CAS key
  size            INTEGER NOT NULL,
  PRIMARY KEY (generation_hash, path)
);

CREATE TABLE blob_meta (
  content_hash BLOB PRIMARY KEY,
  kind         TEXT NOT NULL,         -- source|ir|refs|manifest|dict|pack
  raw_size     INTEGER NOT NULL,
  store_size   INTEGER NOT NULL,      -- on-disk compressed
  codec        INTEGER NOT NULL,
  refcount     INTEGER NOT NULL       -- optional; can recompute from generation_file
);

CREATE TABLE symbol (                 -- optional denormalized for offline browse
  package_id   BLOB NOT NULL,
  generation_hash BLOB NOT NULL,
  symbol_id    BLOB NOT NULL,
  name         TEXT NOT NULL,
  kind         TEXT NOT NULL,
  path         TEXT,
  start_byte   INTEGER,
  end_byte     INTEGER
);
CREATE INDEX symbol_name ON symbol(name);

CREATE TABLE lineage_edge (           -- if offline lineage needed; else terminus remote only
  src_symbol BLOB,
  dst_symbol BLOB,
  edge_kind  TEXT,
  generation_hash BLOB
);

CREATE TABLE sync_peer (
  peer_id TEXT PRIMARY KEY,
  base_url TEXT,
  last_success_at INTEGER
);

-- GC accounting
CREATE TABLE quota (
  key TEXT PRIMARY KEY,               -- 'cas_bytes', 'max_cas_bytes', ...
  value INTEGER NOT NULL
);
```

**Notes:**

- Do **not** store live trees in sqlite.
- `symbol` table is a **cache** of IR for UI snappiness; source of truth remains IR blob.
- Remote INDEX: same tables can live in **Postgres** (server already uses postgres for orchestration) with `cas/` on S3; desktop uses sqlite.

### 5.4 mmap strategy

| Data | mmap? |
|---|---|
| zstd-compressed CAS blobs | No — decompress to buffer (or streaming) |
| raw source if stored uncompressed | Optional mmap for large files in GUI viewer |
| rkyv IR (if adopted) | Yes — ideal |
| tantivy | Library-managed |
| tree projection `.ndxt` | Yes — designed for it |

### 5.5 Quota / GC / pinning

**Policy:**

1. **Pins:** active project packages `pin_level=2`; user-starred `=1`; everything else `=0`.
2. **LRU:** among `pin_level=0`, evict generations by `last_access_at` until `cas_bytes ≤ max`.
3. **Mark-and-sweep blobs:** live set = union of all `generation_file.content_hash` + `manifest_hash` + `ir`/`refs` from live generations + pinned dicts. Delete unreferenced `cas/*`.
4. **Never delete** a blob mid-read: refcount or generation lock file.
5. Server-side GC race is already noted in `server/save/blobs.rs` (need snapshot fence) — desktop is single-writer easier.

**Default quota (proposal):** 5–20 GB cas depending on product tier; surface in GUI.

### 5.6 Remote INDEX layout (S3)

Mirror `registry::Store` (already correct):

```
s3://nudox-index/
  cas/{blake3-hex}           # raw or enveloped blobs
  ptr/{package-uuid}         # 32-byte manifest content hash
  packs/{generation-hash}    # OPTIONAL transfer pack
  dicts/{dict_id}            # public dictionaries
```

Catalog: Postgres (orchestration) + optional sqlite replicas for edge.

CDN: cache `cas/*` immutably forever (hash-addressed). `ptr/*` short TTL or no CDN.

---

## 6. Sync-friendliness and generation manifest

### 6.1 Why content-addressed sync is trivial

Client algorithm:

```
want = set(hashes in generation manifest)
have = set(local cas keys)
fetch = want \ have
for h in fetch: GET /cas/{h} → verify blake3 → store
write local generation row + pointer
```

No rsync, no tree walk, no rename detection. This is Nix narinfo + CAS, IPFS block exchange, and git’s “have/want” simplified to flat blobs.

### 6.2 Generation manifest format (wire)

Build on existing `BlobManifest` (postcard) but define a **sync-facing** JSON/CBOR view for debuggability and multi-language clients:

```jsonc
{
  "format": "nudox.generation/1",
  "package_id": "uuid",
  "coordinates": { "origin": "...", "name": "serde", "version": "1.0.210" },
  "generation_hash": "<hex blake3 of identity_bytes>",
  "manifest_hash": "<hex blake3 of postcard BlobManifest>",
  "toolchain": { "...": "..." },
  "grammar": {
    "rust": { "name": "tree-sitter-rust", "version": "0.23.x", "hash": "<hex>" }
  },
  "files": [
    { "path": "src/lib.rs", "hash": "<hex>", "size": 1234, "codec": "zstd", "dict": null }
  ],
  "ir": { "hash": "<hex>", "size": 999, "codec": "zstd", "dict": "ir-postcard-v1" },
  "references": { "hash": "<hex>", "size": 400, "codec": "zstd", "dict": "ir-postcard-v1" },
  "created_at": "2026-07-16T00:00:00Z",
  "signature": null
}
```

**Rules (align with `blob/mod.rs` two-hash discipline):**

- `generation_hash` = Hash ① over identity encoding (freshness / “same snapshot?”).
- `manifest_hash` = Hash ② CAS key of full manifest bytes.
- Never put ephemeral fields (access times, pins) into either hash.

### 6.3 Prior art mapping

| Prior art | Mapping |
|---|---|
| Nix **narinfo** | generation manifest + signature fields |
| Nix **NAR** | optional transfer pack of all raw files |
| docs.rs **archive index** | optional pack offset table |
| IPLD root CID | `generation_hash` |
| Dash **feed XML** | product update channel for desktop app, not per-package |

### 6.4 Signed generations (optional later)

```
signature = sign(ed25519, generation_hash ‖ manifest_hash)
```

Publish public keys in trust config. Mirrors Nix binary cache signatures without requiring Nix.

### 6.5 HTTP API sketch

```
GET /v1/packages/{id}/generations/latest → GenerationManifest
GET /v1/cas/{hash} → bytes  (immutable, CDN)
HEAD /v1/cas/{hash} → exists?
POST /v1/cas/has → body: [hash…] → missing subset   // batch like git
GET /v1/packs/{generation_hash} → seekable zstd pack // optional
```

Batch `has` avoids thousands of HEADs for large packages.

---

## 7. Synthesis — recommended architecture

### 7.1 One-paragraph summary

Store **source, IR, and references as global BLAKE3 content-addressed blobs**; address each package generation with a small **manifest** (already `BlobManifest`) that lists per-file hashes and section refs. **Do not persist tree-sitter Trees** — re-parse from source with a pinned grammar id; keep only `ResolvedReference` projections and optional ephemeral node projections. Compress IR with **zstd+dictionary**; compress source with plain zstd when beneficial. Local REGISTRY = **sqlite catalog + sharded cas/** (Dash layout, Nix addressing). Remote INDEX = **same catalog semantics + S3 cas/**. Sync = **hash set difference**. Optional seekable-zstd **transfer packs** for cold first download only.

### 7.2 Exact formats (normative sketch)

1. **CAS key:** `blake3(raw_logical_bytes)` as 32-byte digests; hex lowercase for paths (matches `ContentHash::hex`).
2. **Manifest:** existing postcard `BlobManifest` + optional sync JSON projection.
3. **Tree:** none durable; grammar id in generation metadata; optional `.ndxt` cache.
4. **Compression:** envelope v1 with codec + dict_id; verify hash on raw.
5. **Crates:**
   - `blake3`, `object_store` (already)
   - `zstd` / `zstd-safe` for compress
   - `postcard` for manifests/IR (already)
   - `rkyv` **only if** IR mmap profiling demands it — not for trees day one
   - `fastcdc` **not** day one
   - `zip` / `zstd-framed` only for optional packs

### 7.3 Size model — 100 files / 50k LOC package

Assumptions: 1.5 MB raw source; IR postcard ~400–800 KB raw; references ~100–200 KB; ~100 file objects.

| Component | Raw | zstd-3 | zstd+dict |
|---|---|---|---|
| Source | 1.5 MB | **0.45–0.6 MB** | 0.40–0.55 MB |
| IR | 0.6 MB | 0.15–0.25 MB | **0.10–0.18 MB** |
| References | 0.15 MB | 0.04–0.06 MB | **0.03–0.05 MB** |
| Manifest | &lt;4 KB | ~1 KB | ~1 KB |
| **Tree (if full projection)** | ~2 MB | ~0.7 MB | ~0.6 MB |
| **Tree (recommended: none)** | 0 | 0 | 0 |

**Totals (recommended stack):** ~**0.6–0.9 MB** compressed per generation cold store (plus sqlite rows).  
**With trees persisted:** add ~0.6–0.7 MB and ongoing invalidation complexity — **not worth it**.

**10 versions, 15% churn:** CAS shared store ≈ **1.5–2.5 MB** source-equivalent vs **6–9 MB** archive-per-version.

### 7.4 GC / pinning policy (copy-ready)

1. Pin active project’s dependency closure (`pin_level=2`).
2. LRU-evict unpinned generations by `last_access_at`.
3. Sweep unreferenced CAS objects weekly / on quota pressure.
4. Keep grammar dicts and zstd dicts pinned while any generation references them.
5. Ephemeral `trees/` cache cleared on grammar upgrade or LRU independently.

### 7.5 How this layers on the live codebase

| Existing piece | Keep / change |
|---|---|
| `ContentHash` BLAKE3 | Keep |
| `DiskCas` envelope | Extend with codec; or dual-write |
| `Store` cas/ + ptr/ | Keep as remote INDEX layout |
| `BlobManifest` | Keep; add optional grammar metadata field carefully (Hash ① impact!) |
| `SourceArchive` | Keep as producer input to manifest |
| `Cst` / no tree storage | **Keep — confirmed by external research** |
| `get_section_range` | Keep for large single files |

**Caution:** adding fields to `BlobManifest` affects Hash ② (CAS key) but Hash ① is bespoke — put grammar pins in identity_bytes **only if** re-analysis must invalidate freshness (probably yes for IR, no for ephemeral tree cache).

### 7.6 Decision table (executive)

| Question | Decision |
|---|---|
| Persist tree-sitter Tree? | **No** |
| Persist tree projection? | **Optional cache only** |
| Source layout | **Per-file CAS** |
| Archive packs? | **Optional transfer optimization** |
| FastCDC? | **No (until &gt;1MB blobs hurt)** |
| zstd dictionaries? | **Yes for IR/refs; measure for source** |
| Local layout | **sqlite + cas/ + tantivy + vectors** |
| Sync | **manifest + missing hashes** |

---

## 8. Implementation roadmap

### Phase 0 — Document current invariants (1–2 days)

- Freeze the two-hash discipline documentation for clients.
- Emit generation manifest JSON from server for sync prototyping.

### Phase 1 — Local REGISTRY skeleton (1–2 weeks)

- sqlite schema §5.3
- sharded cas with decompress-verify
- import from remote via `has` + GET
- LRU GC + pins

### Phase 2 — Compression (1 week)

- zstd on IR/refs with trained dict
- metrics: ratio, CPU, cache hit

### Phase 3 — Transfer packs (optional, 1 week)

- seekable zstd pack builder/consumer for first-time large syncs
- still expand into per-file CAS on disk

### Phase 4 — Projection cache (only if profiled)

- `.ndxt` for hot snippet paths
- never part of generation hash

---

## 9. Open questions and risks

1. **S3 small-object costs:** very large multi-tenant INDEX may need pack aggregation for cold packages; measure PUT/$ before inventing packs.
2. **Grammar upgrades:** re-parse is free; **IR regeneration** is not — grammar id must participate in producer `JobKey` (already toolchain/producer versioned).
3. **Hash ① pollution:** adding fields to identity_bytes busts “fresh” for all packages — use explicit versioning.
4. **Partial range reads** cannot verify full-object hash (`get_section_range` comment) — GUI should prefer full-file gets for source &lt; few MB.
5. **Windows path lengths / antivirus** on many small cas files — sharding + occasional pack may help.
6. **Legal/license of stored source** on desktop — product policy, not format.
7. **docs.rs source path 404s during research:** GitHub raw/API returned 404 from this environment; archive design inferred from public issues [#1004](https://github.com/rust-lang/docs.rs/issues/1004), Forge, and secondary references — re-clone `rust-lang/docs.rs` when implementing range-index details if packing is chosen.
8. **Vector/tantivy on-disk formats** owned by reports 09/10 — this report only reserves directories.
9. **Whether references belong in IR** eventually — if merged, drop separate `references_ref` carefully with migration.
10. **Multi-device sync** of pins/LRU — out of scope; generation blobs remain pure CAS.

---

## 10. Executive summary

nudox should treat **content-addressed per-file storage** as the single system of record for source, IR, and extracted references. That decision is already embodied in `BlobManifest`, `Store` (`cas/` + `ptr/`), `SourceArchive`, and `DiskCas`. External platforms strongly support the split: **docs.rs** shows catalog-vs-blob and optional ZIP+range for *non-dedupable* HTML; **Nix narinfo** shows signed manifests over CAS for sync; **Dash/Zeal** shows sqlite+payload for offline UX; **Sourcegraph SCIP** shows that semantic indexes are *not* CSTs; **tree-sitter maintainers** explicitly advise storing source (compressed) rather than serializing trees, and there is still **no stable Tree serialization API** in 2026 (query serialization work in PR #2594 is unrelated).

**Do not persist tree-sitter trees** in INDEX or REGISTRY. Re-parse is cheap at package and file grain; persist `ResolvedReference` (already) and optional ephemeral projections only if profiling demands. Prefer **zstd** with **trained dictionaries for IR**, plain zstd for source, and optional **seekable zstd transfer packs** for cold bulk sync—never as the only replica. Skip FastCDC until large binary blobs appear.

Local REGISTRY layout: **`registry.sqlite` + sharded `cas/` + tantivy + vectors + pin/LRU GC**. Remote INDEX: same logical objects on **S3 cas/** with Postgres catalog. Sync is **manifest-driven hash set difference**, the same pattern as Nix binary caches and IPLD roots.

Expected footprint for a 50k-LOC generation: roughly **0.6–0.9 MB** compressed without trees, with multi-version retention often **~5× smaller** than per-version archives.

---

## 11. References (inline URLs collected)

- docs.rs / Forge: https://forge.rust-lang.org/docs-rs/index.html  
- docs.rs archive issue: https://github.com/rust-lang/docs.rs/issues/1004  
- docs.rs compress issue: https://github.com/rust-lang/docs.rs/issues/379  
- S3 range reads: https://docs.aws.amazon.com/AmazonS3/latest/userguide/range-get-olap.html  
- MinIO S3 ZIP: https://docs.min.io/aistor/developers/s3-zip-extension/  
- tree-sitter discussion (no tree serialize): https://github.com/tree-sitter/tree-sitter/discussions/2293  
- TSQuery serialization issue: https://github.com/tree-sitter/tree-sitter/issues/1942  
- PR queries/highlights serialize: https://github.com/tree-sitter/tree-sitter/pull/2594  
- SCIP announce: https://sourcegraph.com/blog/announcing-scip  
- SCIP site: https://scip-code.org/  
- Future of SCIP: https://sourcegraph.com/blog/the-future-of-scip  
- Dash docsets: https://kapeli.com/docsets  
- Zeal: https://zealdocs.org/  
- zstd seekable format: https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md  
- zstd dictionaries: https://github.com/facebook/zstd  
- FB zstd blog: https://engineering.fb.com/2016/08/31/core-infra/smaller-and-faster-data-compression-with-zstandard/  
- Szorc zstd dict: https://gregoryszorc.com/blog/2017/03/07/better-compression-with-zstandard/  
- RocksDB dict: http://rocksdb.org/blog/2021/05/31/dictionary-compression.html  
- Nix binary cache: https://fzakaria.com/2021/08/12/a-nix-binary-cache-specification  
- nix-casync: https://flokli.de/posts/2021-12-10-nix-casync-intro  
- casync: https://github.com/systemd/casync  
- fastcdc crate: https://docs.rs/fastcdc  
- rkyv: https://rkyv.org/  
- object_store: https://docs.rs/object_store  
- GitHub archive hashes: https://github.blog/2023-02-21-update-on-the-future-stability-of-source-code-archives-and-hashes  
- zip crate: https://docs.rs/zip  

### Codebase (local)

- `workspace/heart/content.rs` — `ContentHash`, `JobKey`
- `workspace/heart/cache/disk.rs` — local CAS envelope
- `workspace/registry/store.rs` — `cas/` + `ptr/`
- `workspace/registry/blob/mod.rs` — `BlobManifest`, no CST storage
- `workspace/compiler/generate/source_archive.rs` — per-file digests
- `workspace/compiler/generate/cst.rs` — transient trees, serializable refs
- `workspace/server/save/blobs.rs` — audit/rebuild from blobs

---

## 12. Edge-tech decisions (materialization & CAS)

**Status:** decision pointers from `docs/research/edge-tech/` (2026-07-16). Does **not** rewrite §1–§11; anchors adopt/borrow/reject for materialization and content-addressed packaging.

**Unified map:** [edge-tech/00-DECISIONS.md](../../edge-tech/00-DECISIONS.md)

### 12.1 Verdict table

| Topic | Decision | Why (one line) | Source |
|---|---|---|---|
| **EdenFS** | **Reject** | Unsupported OSS, SCM-commit keyed, heavy daemon — wrong fit for generation/BLAKE3 CAS | [edge-tech/03-edenfs](../../edge-tech/03-edenfs/PLAN.md) |
| **Primary materialize path** | **CAS + selective tmpdir/hardlink views** | Producers need real `PathBuf` trees; no FUSE day-1 | 03 + this plan § materialize |
| **composefs / EROFS** | **Optional later (Linux pods)** | Immutable RO generation images for k8s compile nodes | 03, [05](../../edge-tech/05-surrounding-edge/PLAN.md) |
| **Public IPFS / Kubo swarm** | **Reject** as INDEX/desktop distribution | Legal, pin/GC, small-blob latency, ops; permanence ≠ free pin | [edge-tech/04-ipfs-and-cas](../../edge-tech/04-ipfs-and-cas/PLAN.md) |
| **BLAKE3 manifests + `cas/{hash}`** | **Keep (already planned)** | Shallow Merkle DAG without IPLD day-1 | 04 + this plan §2.8 |
| **CID / CAR export** | **Optional interop only** | CIDv1(raw,blake3) + CAR transfer packs at boundary | 04 |
| **UnixFS / IPLD codecs as SoT** | **Reject day-1** | Path→hash manifests beat UnixFS layout churn | 04 |
| **SOCI / eStargz-style archive indexes** | **Highest-ROI lazy materialize** | External TOC + range reads over generation archives — no Eden | 05 §P4 / §13.1 |
| **Bitswap wire** | **Reject; steal want/have pattern** | HTTP want/have + presign already planned (→ plan 14) | 04, 14 |

### 12.2 Materialization stance (locked)

```
INDEX:   manifest + cas/{blake3} (+ optional transfer packs)  — no VFS
Desktop: registry.sqlite + cas/ → views via hardlink/copy-on-read; GUI virtual tree from manifest
Compile: prefetch CAS → job-private tree (hardlink preferred) → sandbox RO bind
Later:   SOCI-like external archive index for range-lazy files;
         composefs/EROFS on Linux orch nodes only
```

### 12.3 Cross-links

| Plan / report | Role |
|---|---|
| [03-edenfs](../../edge-tech/03-edenfs/PLAN.md) | Eden reject + CAS/tmpdir + composefs path |
| [04-ipfs-and-cas](../../edge-tech/04-ipfs-and-cas/PLAN.md) | IPFS reject; CID/CAR optional; CAS DNA |
| [05-surrounding-edge](../../edge-tech/05-surrounding-edge/PLAN.md) | SOCI/eStargz, erofs/composefs ranking |
| [07-materializer-archive-index](../../edge-tech/07-materializer-archive-index/PLAN.md) | **Implementable** Materializer + `.ndpk`/`.ndix` design |
| [06-compiler-fleet-cache](../../edge-tech/06-compiler-fleet-cache/PLAN.md) | L5 = node-local hardlink source CAS |
| [14-client-sync](../14-client-sync/PLAN.md) | want/have transport; optional iroh Phase-2 |
| [22-crate-topology](../22-crate-topology/PLAN.md) | `blob` + optional Materializer / ArchiveIndex |

---

## 13. Materializer + SOCI-like archive index (pointer)

**Status:** design landed 2026-07-16 — **does not rewrite §1–§12**. Full ADR:

→ **[edge-tech/07-materializer-archive-index/PLAN.md](../../edge-tech/07-materializer-archive-index/PLAN.md)**  
→ twin: [edge-tech/07-materializer-archive-index.md](../../edge-tech/07-materializer-archive-index.md)

### 13.1 What this adds to storage plan 13

Plan 13 already chose **per-file BLAKE3 CAS as SoR** and sketched optional **transfer packs** + docs.rs-style range indexes. Edge-tech 03 rejected EdenFS; 05 ranked SOCI/eStargz external indexes as highest-ROI lazy materialization. **07 specifies the missing subsystem**: who builds real `PathBuf` trees for producers/GUI, from CAS and/or pack+index, with integrity and fleet L5 wiring.

### 13.2 Locked MVP decisions (from 07)

| Item | Decision |
|---|---|
| SoR | Unchanged: per-file CAS (`BlobManifest.files` / `SourceArchive`) |
| Materializer pair | **CasHardlink** + **LazyArchive**; **FullExtract** fallback |
| Pack / index | Optional `.ndpk` (NdPkV1 frame-per-file) + `.ndix` (`ArchiveIndex`) |
| Pack in Hash ① | **No** — catalog/sync pointers only |
| FUSE / EdenFS | **No** for MVP |
| composefs / EROFS | Phase-2 Linux orch only |
| Metrics | cold open p95; bytes hydrated / package size |

### 13.3 Layout addendum (packs)

```
# local REGISTRY (alongside §5.2)
packs/{generation_hash}.ndpk
packs/{generation_hash}.ndix
views/{view_id}/                 # hardlink / sparse session trees

# remote INDEX (alongside §5.6)
packs/{generation_hash}.ndpk
packs/{generation_hash}.ndix
# Range GET supported on pack objects
```

### 13.4 Live gap

Today `compiler_daemon` writes **all** request files into a tempdir before `generate_with`. Target: `Materializer::ensure_tree` → hardlink/lazy/full → same `PackageInput.root`. See 07 §6 / §10 (S-M.1 … S-M.12).

### 13.5 Implementation home

Prefer new workspace crate **`materialize`** (trait + NdPk + backends); adapters from `BlobManifest` / `SourceArchive`. Traits stay out of lean `heart` (only `ContentHash`).

---

*End of report.*

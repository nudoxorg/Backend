# Handoff: Research source+tree storage design
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a6d822257a1c80b19.jsonl`
- Agent id: `agent-a6d822257a1c80b19`
- Deliverable: `/Users/philocalyst/Projects/Backend/.research/librarification/13-storage.md`
- Existing report: False (0 lines)
- Tools used: 19 ({'Agent': 4, 'Bash': 2, 'ToolSearch': 1, 'WebSearch': 6, 'WebFetch': 6})
- Files/greps touched: 0
- Web queries: 12

## Web queries performed
- FETCH: https://github.com/rust-lang/docs.rs/blob/master/src/storage/mod.rs
- docs.rs archive storage S3 zip range read implementation 2024 2025
- docs.rs "archive storage" OR "zip archive" OR "archive_index" source code internals GitHub
- FETCH: https://github.com/rust-lang/docs.rs/blob/main/src/db/file.rs
- FETCH: https://raw.githubusercontent.com/rust-lang/docs.rs/main/src/storage/mod.rs
- FETCH: https://raw.githubusercontent.com/rust-lang/docs.rs/main/src/storage/archive_index.rs
- site:github.com rust-lang docs.rs storage archive_index.rs OR s3.rs OR mod.rs
- tree-sitter "serialize" OR "serialization" tree binary format 2025 2026 issue discussion
- FETCH: https://github.com/rust-lang/docs.rs/blob/main/src/db/file.rs
- FETCH: https://github.com/tree-sitter/tree-sitter/pull/2594
- tree-sitter tree persist serialize syntax tree disk cache 2024 2025 issue
- docs.rs storage migration "archive" zip S3 "range request" OR "byte range" crate documentation

## Files read / grepped (sample)

## Assistant synthesis fragments (inert history — verify)

### fragment

All four research agents are running in parallel. I'll now start direct web research on some of the key topics while waiting for them to complete.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent doing exhaustive external research for a major architecture plan. Today is 2026-07-16 — verify current details with WebSearch/WebFetch; cite URLs inline.

CONTEXT: nudox is a multi-language code-intelligence platform. Both its remote INDEX (sqlite + S3) and local REGISTRY (sqlite + disk, inside a desktop app) must store, for every package generation: (1) the typed IR blobs (already content-addressed, BLAKE3), (2) the SOURCE CODE, and (3) "resolved tree-sitter trees" — as efficiently as possible. Reference platforms should be studied for how they do this. Tree-sitter trees are used for CST-level snippet extraction and implementation-level resolution; sources are needed for rendering code views in the GUI and for re-parsing.

YOUR MISSION — exhaustive research:
1. docs.rs storage internals (study the actual implementation — github.com/rust-lang/docs.rs): the S3 layout, the archive format they moved to (zip-based archives with a separate index for range-read of single files? verify exactly: "archive storage" with remote zip + local index caching), compression (zstd dictionaries?), the database vs storage split, how source browsing is served, CDN/caching. Extract the reusable pattern.
2. Other reference platforms: Sourcegraph (SCIP index storage, blob storage for repos — gitserver), GitHub (git packfiles as the substrate + their code-nav artifacts), crates.io (source .crate files on S3 + CDN), deps.dev, Zeal/Dash docsets (sqlite-per-docset! — study the docset format: sqlite index + HTML files, tgz distribution — very relevant to our local REGISTRY), DevDocs (their offline storage format). For each: what they store per version, dedup strategy, single-file random access.
3. Tree-sitter tree persistence — settle this definitively: tree-sitter has NO stable public serialization of Tree (verify 2026 status — check tree-sitter repo issues/discussions on serialization; the WASM/binary format situation; ts_tree_copy is in-memory only). Options to evaluate: (a) DON'T persist trees — persist source + grammar version and re-parse on demand (tree-sitter parses ~MB/s scale — quantify: is re-parse cheap enough that persisting is pointless?), (b) persist a PROJECTION of the tree (the node types/ranges nudox actually uses — e.g., a flat postorder array of (kind_id, start_byte, end_byte, parent_idx) — compact, versioned by grammar; design this format), (c) memory-map-friendly formats (flatbuffers/rkyv zero-copy — rkyv 2026 state) for the projection. The `ir` crate already depends on arborium-tree-sitter (a tree-sitter distribution) and `yoke` (self-referential borrows) — factor that in. RECOMMEND one option with a concrete format sketch.
4. Source storage design: content-addressed per-file blobs (BLAKE3, already in `heart`) vs per-package archives with range access (docs.rs style) — dedup rates across versions of the same package (most files unchanged between versions — per-file CAS wins dedup; archives win syscall/latency? analyze); chunk-level dedup (FastCDC, casync/desync, restic/borg content-defined chunking) — overkill for source? zstd with trained dictionaries per-language (how much does a dictionary help on small source files — find benchmarks or reason quantitatively); seekable zstd format.
5. Local REGISTRY layout on disk: the full proposed directory/db scheme for the desktop app — sqlite catalog (packages, generations, symbols, lineage, trace digests) + CAS dir for blobs (sharded by hash prefix) + tantivy dir + vector dir; quota/GC (LRU by package generation, pinning the active project), mmap strategy; what the remote INDEX shares of this layout (same catalog schema on sqlite + S3 instead of dir).
6. Sync-friendliness: the storage format should make REMOTE→LOCAL sync trivial (content-addressed blobs = rsync-free dedup'd pull of missing hashes; manifest per generation listing blob hashes). Sketch the generation manifest format (what docs.rs's index-file-per-archive teaches; IPLD/Merkle-DAG manifests as prior art; Nix NAR/narinfo as prior art for signed content-addressed package artifacts).

SYNTHESIS: the recommended storage architecture for both sides — exact formats (manifest schema, tree-projection format, compression settings), the crates (rkyv? zstd? object_store already used), a size model (e.g., for a 100-file / 50k-LOC package: source X MB, trees Y MB, IR Z MB, compressed totals), and GC/pinning policy.

DELIVERABLE:
- Write the FULL report (dense markdown, 500-1000+ lines, URLs inline) to /Users/philocalyst/Projects/Backend/.research/librarification/13-storage.md
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) top concrete recommendations, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)

### WebFetch: https://github.com/rust-lang/docs.rs/blob/master/src/storage/mod.rs
```
The server returned HTTP 404 Not Found.

The response body was not retrieved. If this URL requires authentication, use an authenticated tool (e.g. `gh` for GitHub, or an MCP-provided fetch tool) instead of WebFetch.
```

### WebSearch: docs.rs archive storage S3 zip range read implementation 2024 2025
```
Web search results for query: "docs.rs archive storage S3 zip range read implementation 2024 2025"

Links: [{"title":"object_store - Rust","url":"https://docs.rs/object_store/latest/object_store/"},{"title":"How to Use S3 Glacier for Long-Term Archival Storage","url":"https://oneuptime.com/blog/post/2026-02-12-s3-glacier-long-term-archival-storage/view"},{"title":"Object Storage Classes – Amazon S3","url":"https://aws.amazon.com/s3/storage-classes/"},{"title":"Active Archive Alliance sees three archive tiers – Blocks and Files","url":"https://blocksandfiles.com/2025/11/04/active-archive-alliance-sees-three-archive-tiers/"},{"title":"Understanding and managing Amazon S3 storage classes - Amazon Simple Storage Service","url":"https://docs.amazonaws.cn/en_us/AmazonS3/latest/userguide/storage-class-intro.html"},{"title":"zip-old/src/read.rs at master · zip-rs/zip-old","url":"https://github.com/zip-rs/zip-old/blob/master/src/read.rs"},{"title":"Secure archive storage – Amazon S3 Glacier storage classes – AWS","url":"https://aws.amazon.com/s3/storage-classes/glacier/"},{"title":"Archiving Data in Amazon S3 Glacier Deep Archive — Northwestern IT Research Computing and Data Services Northwestern University","url":"https://rcdsdocs.it.northwestern.edu/tutorials/cloud/aws/archiving-data-amazon-S3.html"},{"title":"A complete guide to Amazon S3 Glacier for long-term data storage","url":"https://cloudtech.com/resources/understanding-aws-s3-glacier-storage-long-term-data"},{"title":"Archiving on Amazon S3: A Practical Guide - N2W Software","url":"https://n2ws.com/blog/archiving-on-amazon-s3-a-practical-guide"}]

Based on the web search results, I found some relevant information about S3 storage and archive solutions, but the results don't contain specific information about docs.rs archive storage implementation with S3 zip range read functionality for 2024-2025.

The search returned:

1. **Object Store Implementation**: Results showed information about the `object_store` Rust crate on docs.rs, which provides a high-performance async API for S3 and other cloud storage backends. However, this appears to be a general-purpose library rather than docs.rs-specific archival implementation.

2. **S3 Archive Storage**: Multiple results discussed S3 Glacier and archival storage strategies, but these are general AWS guidance rather than implementation-specific details.

3. **Zip File Implementation**: There was a reference to a Rust zip implementation, but without details on range
```

### WebSearch: docs.rs "archive storage" OR "zip archive" OR "archive_index" source code internals GitHub
```
Web search results for query: "docs.rs "archive storage" OR "zip archive" OR "archive_index" source code internals GitHub"

Links: [{"title":"Downloading source code archives - GitHub Docs","url":"https://docs.github.com/en/repositories/working-with-files/using-files/downloading-source-code-archives"},{"title":"docs.rs/src/db/file.rs at main · rust-lang/docs.rs","url":"https://github.com/rust-lang/docs.rs/blob/main/src/db/file.rs"},{"title":"GitHub - zarrs/zarrs_zip: A storage adapter for zip files for the zarrs Rust crate · GitHub","url":"https://github.com/zarrs/zarrs_zip"},{"title":"zip - Rust","url":"https://docs.rs/zip"},{"title":"zip 8.6.0 - Docs.rs","url":"https://docs.rs/crate/zip/latest"},{"title":"GitHub - archive-rs/archive-rs · GitHub","url":"https://github.com/archive-rs/archive-rs"},{"title":"RuneScape Archive Project","url":"https://rs-archive.github.io/"},{"title":"GitHub - mrkline/piz-rs: Parallelized zip archive reading","url":"https://github.com/mrkline/piz-rs"},{"title":"GitHub - Majored/rs-async-zip: An asynchronous ZIP archive reading/writing crate.","url":"https://github.com/Majored/rs-async-zip"},{"title":"zip-archive · GitHub Topics · GitHub","url":"https://github.com/topics/zip-archive"}]

I found a relevant result from the docs.rs repository. Here's what I found:

In the docs.rs source code, there's an async function `add_path_into_database` that uses storage to store files with compression, and `add_path_into_remote_archive` that calls `storage.store_all_in_archive` to handle archive operations.

The search returned results from the docs.rs GitHub repository at src/db/file.rs, which shows the file handling internals for both regular file storage and archive storage in the docs.rs documentation hosting system.

There are also several related Rust archive projects mentioned:
- archive-rs, a library for archive files like .tar and .zip
- A zip library for Rust which supports reading and writing of simple ZIP files
- Piz, a Zip archive reader where each file in a Zip archive is compressed independently, with a central directory telling us where to find it

If you need more specific information about docs.rs's internal archive storage implementation or a particular aspect of how it handles zip archives, please let me know and I can perform a more targeted search.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
```

### WebFetch: https://github.com/rust-lang/docs.rs/blob/main/src/db/file.rs
```
The server returned HTTP 404 Not Found.

The response body was not retrieved. If this URL requires authentication, use an authenticated tool (e.g. `gh` for GitHub, or an MCP-provided fetch tool) instead of WebFetch.
```

### WebFetch: https://raw.githubusercontent.com/rust-lang/docs.rs/main/src/storage/mod.rs
```
The server returned HTTP 404 Not Found.

The response body was not retrieved. If this URL requires authentication, use an authenticated tool (e.g. `gh` for GitHub, or an MCP-provided fetch tool) instead of WebFetch.
```

### WebFetch: https://raw.githubusercontent.com/rust-lang/docs.rs/main/src/storage/archive_index.rs
```
The server returned HTTP 404 Not Found.

The response body was not retrieved. If this URL requires authentication, use an authenticated tool (e.g. `gh` for GitHub, or an MCP-provided fetch tool) instead of WebFetch.
```

### WebSearch: tree-sitter "serialize" OR "serialization" tree binary format 2025 2026 issue discussion
```
Web search results for query: "tree-sitter "serialize" OR "serialization" tree binary format 2025 2026 issue discussion"

Links: [{"title":"Add serialization support for queries, highlight configs by SergioBenitez · Pull Request #2594 · tree-sitter/tree-sitter","url":"https://github.com/tree-sitter/tree-sitter/pull/2594"},{"title":"Serialize and Deserialize Binary Tree - LeetCode","url":"https://leetcode.com/problems/serialize-and-deserialize-binary-tree/"},{"title":"Serialize and Deserialize Binary Tree - NeetCode","url":"https://neetcode.io/problems/serialize-and-deserialize-binary-tree"},{"title":"Serialize and Deserialize a Binary Tree","url":"https://www.topcoder.com/thrive/articles/serialize-and-deserialize-a-binary-tree"},{"title":"Serialize and Deserialize a Binary Tree | Baeldung on Computer Science","url":"https://www.baeldung.com/cs/binary-tree-serialize-deserialize"},{"title":"Serializing and Deserializing Binary Tree","url":"https://algo.monster/problems/serializing_tree"},{"title":"Serialize And Deserialize a Binary Tree - Tutorial","url":"https://takeuforward.org/data-structure/serialize-and-deserialize-a-binary-tree"},{"title":"Serialize and Deserialize Binary Tree | DSA | AlgoMaster.io","url":"https://algomaster.io/learn/dsa/serialize-and-deserialize-binary-tree"},{"title":"297. Serialize and Deserialize Binary Tree - Solutions and Explanation | Vultr Docs","url":"https://docs.vultr.com/problem-set/serialize-and-deserialize-binary-tree"}]

Based on the search results, I found relevant information about tree-sitter serialization efforts:

Pull Request #2594 is a continuation of #2374 which fully resolves issue #1942, relating to serialization support in tree-sitter. The approach exposes serializable versions of queries and highlight configurations.

The most recent activity mentioned in the search results indicates that work on this feature was referenced as recently as February 18-19, 2026, showing ongoing efforts to address serialization concerns in the tree-sitter project.

The search results show that serialization support for tree-sitter queries and configurations has been a long-standing feature request, with PR #2594 representing efforts to provide practical serialization functionality for the parsing system.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
```

### WebSearch: site:github.com rust-lang docs.rs storage archive_index.rs OR s3.rs OR mod.rs
```
Web search results for query: "site:github.com rust-lang docs.rs storage archive_index.rs OR s3.rs OR mod.rs"

Links: [{"title":"docs.rs/src/db/file.rs at main · rust-lang/docs.rs","url":"https://github.com/rust-lang/docs.rs/blob/main/src/db/file.rs"},{"title":"GitHub - rust-lang/docs.rs: crates.io documentation generator · GitHub","url":"https://github.com/rust-lang/docs.rs"},{"title":"cargo/src/cargo/sources/registry/mod.rs at master · rust-lang/cargo","url":"https://github.com/rust-lang/cargo/blob/master/src/cargo/sources/registry/mod.rs"},{"title":"GitHub - minio/minio-rs: MinIO Rust SDK for Amazon S3 Compatible Cloud Storage · GitHub","url":"https://github.com/minio/minio-rs"},{"title":"Compress documentation uploaded to S3 · Issue #379 · rust-lang/docs.rs","url":"https://github.com/rust-lang/docs.rs/issues/379"},{"title":"docs.rs/README.md at main · rust-lang/docs.rs","url":"https://github.com/rust-lang/docs.rs/blob/main/README.md"},{"title":"GitHub - durch/rust-s3: Rust library for interfacing with S3 API compatible services · GitHub","url":"https://github.com/durch/rust-s3"},{"title":"rust-s3 - crates.io: Rust Package Registry","url":"https://crates.io/crates/rust-s3"},{"title":"Configurable index location · Issue #767 · rust-lang/docs.rs","url":"https://github.com/rust-lang/docs.rs/issues/767"}]

The web search has returned results related to the rust-lang/docs.rs repository and S3 storage operations. Here's a summary of what was found:

**Key Results:**

1. **docs.rs Storage Implementation**: The docs.rs project uses an S3 Client to store files into an S3 bucket, or otherwise stores files into the 'files' table of the local database.

2. **docs.rs Repository**: The main repository is located at `https://github.com/rust-lang/docs.rs`, which is the documentation generator for crates.io.

3. **Storage-related File**: Result #1 links to `https://github.com/rust-lang/docs.rs/blob/main/src/db/file.rs`, which contains storage-related functions like `add_path_into_database` and `add_path_into_remote_archive` that handle storing files using an `AsyncStorage` interface.

4. **S3 Configuration**: The project uses docker-compose to set up an S3 container, and includes configuration for S3 access through settings defined in .env files.

Unfortunately, the search didn't return direct results for the specific files you mentioned (`archive_index.rs`, `s3.rs`, or `mod.rs` in the storage module). You may want to access the repository directly at https://github.com
```

### WebFetch: https://github.com/rust-lang/docs.rs/blob/main/src/db/file.rs
```
The server returned HTTP 404 Not Found.

The response body was not retrieved. If this URL requires authentication, use an authenticated tool (e.g. `gh` for GitHub, or an MCP-provided fetch tool) instead of WebFetch.
```

### WebFetch: https://github.com/tree-sitter/tree-sitter/pull/2594
```
# Tree-Sitter Serialization PR #2594 Analysis

## What is Being Serialized

This PR focuses exclusively on **queries and highlight configurations**, not syntax trees. The PR introduces serializable versions of:
- `Query` objects
- `HighlightConfiguration` objects

## Proposed Format and Approach

Rather than directly serializing structures, the PR exposes "serializable versions" through new methods:
- `serializable()` methods on `Query` and `HighlightConfiguration`
- `deserialize()` methods for reconstruction
- New `SerializableQuery` and `SerializableHighlightConfiguration` structures
- These implement `serde::{Serialize, Deserialize}` when the `serde` feature is enabled

This approach allows consumers to choose their serialization format while respecting concerns like byte order.

## Performance Claims

The author reports "3 ms to deserialize over 100 highlight configurations" versus "~3000ms to generate configurations directly"—approximately **1000x improvement**.

## Identified Issues

The PR acknowledges two concerns:

1. **Insufficient size validation**: "No checking that the `const char *src` array's size is sufficient"
2. **Lack of versioning**: "Serialized bytes from a previous version can be deserialized by a new, incompatible version"

## Current Status

The PR remains **open and unmerged**. A maintainer indicated interest in "a more robust solution" covering additional use cases with improved deserialization performance, suggesting this interim solution may not progress without broader refactoring.
```

### WebSearch: tree-sitter tree persist serialize syntax tree disk cache 2024 2025 issue
```
Web search results for query: "tree-sitter tree persist serialize syntax tree disk cache 2024 2025 issue"

Links: [{"title":"Caching or Serializing a `TSQuery` · Issue #1942 · tree-sitter/tree-sitter","url":"https://github.com/tree-sitter/tree-sitter/issues/1942"},{"title":"tree-sitter · PyPI","url":"https://pypi.org/project/tree-sitter/"},{"title":"Tree — py-tree-sitter 0.25.2 documentation","url":"https://tree-sitter.github.io/py-tree-sitter/classes/tree_sitter.Tree.html"},{"title":"Tree-sitter (parser generator) — Grokipedia","url":"https://grokipedia.com/page/tree_sitter_parser_generator"},{"title":"tree-sitter-language-pack · PyPI","url":"https://pypi.org/project/tree-sitter-language-pack/"},{"title":"Jonas Hietala: Let's create a Tree-sitter grammar","url":"https://www.jonashietala.se/blog/2024/03/19/lets_create_a_tree-sitter_grammar/"},{"title":"tree-sitter · GitHub","url":"https://github.com/tree-sitter"},{"title":"tree-sitter download | SourceForge.net","url":"https://sourceforge.net/projects/tree-sitter.mirror/"},{"title":"Tree-sitter: Introduction","url":"https://tree-sitter.github.io/"},{"title":"GitHub - tree-sitter/tree-sitter: An incremental parsing system for programming tools · GitHub","url":"https://github.com/tree-sitter/tree-sitter"}]

Based on the search results, I found relevant information about tree-sitter's serialization and caching considerations:

There have been efforts to serialize and deserialize HighlightConfiguration structures to improve performance, with work exploring serialization of TSQuery objects to reduce HighlightConfiguration creation time. Creating a HighlightConfiguration is an expensive operation, with some languages like Ruby taking upwards of 500ms.

TSQuery appears to be serializable, but the structure exists only in C and isn't reproduced in the bindings, raising questions about whether its representation is stable enough for serialization.

Regarding tree storage and caching, trees are stored in an efficient internal binary representation optimized for incremental updates and fast loading, though public APIs primarily expose textual serializations like S-expressions for debugging and inspection.

The search results primarily reference discussions from 2022 and technical documentation, with limited information about specific 2024-2025 issues. For more recent developments or specific GitHub issues from 2024-2025, you may want to check the official tree-sitter repository's issue tracker directly.


REMINDER: Y
```

### WebSearch: docs.rs storage migration "archive" zip S3 "range request" OR "byte range" crate documentation
```
Web search results for query: "docs.rs storage migration "archive" zip S3 "range request" OR "byte range" crate documentation"

Links: [{"title":"Migrating files from local database to an S3 compliant provider · rust-lang/docs.rs Wiki · GitHub","url":"https://github.com/rust-lang/docs.rs/wiki/Migrating-files-from-local-database-to-an-S3-compliant-provider"},{"title":"Compress documentation per-crate, not per-file · Issue #1004 · rust-lang/docs.rs","url":"https://github.com/rust-lang/docs.rs/issues/1004"},{"title":"Working with Range and partNumber headers - Amazon Simple Storage Service","url":"https://docs.aws.amazon.com/AmazonS3/latest/userguide/range-get-olap.html"},{"title":"object_store - Rust","url":"https://docs.rs/object_store"},{"title":"Simple Storage Service (S3) | AWS Cert. Cheatsheet","url":"https://zhenye-na.github.io/aws-certs-cheatsheet/posts/s3/"},{"title":"Amazon Simple Storage Service","url":"https://docs.aws.amazon.com/whitepapers/latest/s3-optimizing-performance-best-practices/use-byte-range-fetches.html"},{"title":"object-store-rs","url":"https://developmentseed.org/obstore/v0.1.0/"},{"title":"zip 2.2.0 - Docs.rs","url":"https://docs.rs/crate/zip/latest/source/src/read.rs"},{"title":"s3 - Rust","url":"https://docs.rs/rust-s3/latest/s3/"},{"title":"ZIP to the rescue: saving on AWS S3 PUT requests | by Nikolay Dimitrov | Medium","url":"https://medium.com/@nikolay.dimitrov/zip-to-the-rescue-saving-on-aws-s3-put-requests-f4363ae14c9e"}]

I found relevant search results for your query. Here's what I discovered:

## docs.rs Storage Migration

Files currently in the local database need to be migrated out to S3, and an extra command is needed to properly transition to using the new file storage. The `cratesfyi database move-to-s3` command performs this migration, which can take a while depending on the database size.

## Archive and Compression Approach

Docs.rs should store a single archive per crate with compressed entire archives, which would decrease storage costs, upload times, and allow retrieving entire crates' documentation efficiently. Currently, docs.rs stores each generated HTML file individually on S3, which allows downloading a single page efficiently but is expensive to download all files for a crate since some crates have thousands of generated files.

## Range Requests and ZIP Files

For leveraging byte range requests with ZIP archives on S3, the solution is to read the last 64KB from S3 using the `Range: bytes=-65536` header 
```

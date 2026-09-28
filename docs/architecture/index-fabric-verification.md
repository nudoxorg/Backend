# Index fabric verification contract

Status: integration rubric, 2026-09-28. Passing a crate's own unit tests is not
evidence that the index, worker, store, and client agree on one generation. This
document names the independent observations required before the new fabric can
be called production ready. It is a test specification, not a claim that every
gate has passed.

## What must be observed

The oracle for a package is constructed from fixture source bytes, declared
package metadata, independent expected dependency edges, and an exact
toolchain/environment identity. Tests must not call the implementation's
manifest or ranking constructor to calculate the expected result. Fixtures
must be deliberately asymmetric: distinct file contents, object sizes,
versions, package URLs, targets, result planes, and query relevance labels.
Randomized tests generate *valid structured* histories—edits, missing imports
appearing, yanks, disconnects, retries, and restarts—so they traverse successful
and near-successful paths, not merely a common parse-error branch. This follows
the concrete failure pattern in [Dan Luu's agentic-testing analysis](https://danluu.com/agentic-testing/): agents often make their own tests pass while exercising trivial or self-derived properties.

Each test below should be challenged with the named mutation. If the mutation
does not make the test fail, the test is not an acceptance oracle for that law.

| Boundary | Independent observation | Mutation that must fail |
| --- | --- | --- |
| Workspace capture | Reopen every captured file/page from cold CAS and independently derive the canonical tree, positive/negative reads, recipe, platform, and toolchain root | Change or omit one valid file but retain the old input root |
| Package/target admission | Two valid package URLs in one lineage bind to distinct exact target identities | Swap package B into target A's otherwise valid offer |
| Encrypted dispatch | Two independent processes exchange mixed Bao and control streams on one endpoint under explicit peer allowlists | Route one ALPN to the wrong handler or accept an unlisted peer |
| Range resume | A new process reconstructs only durably verified sparse ranges; total transferred payload and proofs are counted | Acknowledge a truncated range or resend an already verified half as necessary work |
| Output budgets | Reject a worker's over-budget receipt before opening a sink or allocating grant pages; accept a nonuniform near-limit result | Remove the early byte/object cap |
| Result closure | Independently enumerate typed output objects and verify exact closure membership, manifest, binding, IR, and auxiliary planes | Drop one valid embedding plane or include an extra object |
| Selection | Turso selected head changes only after full durable admission and exact attempt/fence/source observation match | Accept an older attempt after a newer one, or treat an S3 ACK as head selection |
| Crash recovery | Kill at each object, pack, closure, observation, selection, projection, and ACK durability boundary; cold reopen yields old or new complete state | Skip one fsync or projection reconciliation |
| Client hydration | A client exposes a selected IR plane only after exact-root proof and can resume missing ranges after restart | Mix one segment from a newer generation into an older captured root |
| Local delta compile | Reuse requires an admitted positive/negative read witness and exact recipe/platform identity; otherwise fresh compile | Declare a no-op after a missing import appears or the embedding model changes |
| Search | Fixed cross-ecosystem query judgments include exact, alias, typo, description, adverse/yanked version, and forge-only cases | Remove one indexed metadata field or reorder a known relevant result below an irrelevant one |

## Process and storage matrix

Run the same canonical logical closure through a local FileStore-only index and
an index with S3-compatible artifact storage. The S3 route returns storage
receipts; both variants use the same index-owned closure verifier and Turso
selection transaction. Compare exact logical roots, selected generation,
client-visible IR/embedding plane IDs, and query results after cold reopen.
The physical layout and request counts may differ. Explicitly test a lost PUT
ACK, truncated range GET, stale presigned capability, local disk-full write,
corrupt pack manifest, process kill before directory fsync, and orphan cleanup.
No local test should require S3 credentials or an S3 process.

Run a separate worker process from persisted identity/trust configuration with
no environment-variable setup. A single endpoint must demultiplex control and
artifact ALPNs. Send a real compiler assignment, disconnect after durable
result storage but before the ACK, restart worker and index, replay the stored
result, then select the head once. The worker must reject an untrusted native
execution grant and a second offer when its pending-result quota is full.
Explicit abandonment is an operator action with an exact work/fence/closure
record; no age-based cleanup may silently discard an unacknowledged result.

## Benchmarks that can support a performance claim

Use a pinned corpus, fixed seeds, identical queries, a declared Rust profile,
and a quiescence report. Keep cold-process reopen separate from warm-cache
queries. Report sample count, median/p95/p99, first-byte time, end-to-end time,
bytes read/sent/resent, range/PUT/GET counts, disk footprint, peak RSS, and
parallel readers at 1/4/16. Include point lookup, nearby 1,000-object docs/IR
range, sparse random ranges, 1 KiB/64 KiB/1 MiB/16 MiB objects, and one-edit
100,000-file manifest diff. Report application/Bao bytes separately from QUIC
wire bytes; do not substitute one for the other. A one-sample debug test is a
correctness and byte-accounting observation, not a latency distribution.

Search uses a committed multi-ecosystem corpus and independent graded query
judgments. Report recall@k, MRR, nDCG@k, freshness/yank errors, index bytes,
incremental update cost, and cold/warm p50/p95/p99. A lib.rs comparison is
fair only on the same Cargo corpus and field coverage; cross-language coverage
is a separate capability measurement. Missing registry metadata remains a
typed unknown, never a zero download count or an invented quality score.

Known product gap: grouped package search returns at most 16 matching release
facets per lineage and marks additional matches with `more_releases`, but it
does not yet provide a lineage-scoped cursor to expand that release list. Exact
version-coordinate searches can still reach an older release directly. Before
calling broad release expansion complete, decide whether the package-version
view supplies the required full list or whether search needs a separate
lineage-scoped release cursor; the current lineage-page cursor only advances
between package groups.

The release gate is a reproducible command set plus the recorded results,
including failed experiments and their fixes. The checks must run through the
actual GUI/locald, CLI, MCP, index, worker, and client paths where those paths
claim the capability. A lower-level protocol test cannot substitute for a
production callsite.

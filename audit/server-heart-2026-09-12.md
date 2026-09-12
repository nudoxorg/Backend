# Server / heart audit — 12 September 2026

The code has a stronger vocabulary for correctness than its end-to-end behavior currently supports. There are useful immutable formats, explicit ownership, and careful validation kernels. But several boundaries turn matching labels into apparent evidence of completeness, residency, successful execution, or search relevance. Those gaps matter much more than the amount of boilerplate removed.

The largest simplification is to establish one application service, one search contract, one manifest that binds the published artifacts, and one real resource lifecycle. Then make local execution, remote execution, and individual search engines implementations of those contracts. At present, multiple partially connected paths each implement a different approximation of the product.

## Scope and evidence

Reviewed the checkout at `/Users/mileswirht/Downloads/backend`, where the requested `server` and `heart` directories live. The task's initial working directory, `/Users/mileswirht/Documents/ChatGPT/backend`, is a separate redesign checkout. Followed the relevant `interface/core`, `interface/library`, `interface/search`, and compiler host/publication boundaries as well. Excluded `server/index/turso`; references to `server/index/catalog` concern this project's wrapper, not the excluded dependency implementation.

This is a source audit with targeted executable counterexamples, not a claim that every test, generated file, platform implementation, or possible unsafe interleaving has been exhaustively verified. The broad review and the counterexamples were done by the main agent. After authorization, Luna made only the small routing fix in finding 14; the main agent reviewed it.

The working tree already contains extensive interface changes. Findings about that facade describe the current files, and are separated from the engine bugs. No broad refactor was applied. The production patch is restricted to two routing files.

The root workspace cannot currently load: `Cargo.toml` lists `GUI2`, but that directory is under `interface/GUI2`. Validation used a temporary source copy with a reduced workspace and compatible cached dependency resolution. This is not a full build against the original workspace lockfile, and no live Qdrant cluster was exercised. The Qdrant counterexample uses a local HTTP fixture through the real adapter.

Severity: **P1** means fix before relying on the affected behavior in a distributed product; **P2** means a substantial functional, performance, or architectural limitation; **P3** means cleanup or product-policy debt. These are engineering priorities, not CVSS scores. Internal API integrity problems are not automatically remotely exploitable vulnerabilities.

## Findings

### 01 · P1 · A safe payload owner can invalidate the immutable memory store

`MemoryStore` accepts arbitrary `PayloadOwner: AsRef<[u8]>`. It hashes the bytes on insertion, retains the owner, then calls `as_ref()` again on every lookup. `AsRef` makes no promise that successive calls return the same bytes.

The probe uses an entirely safe owner containing `Rc<Cell<bool>>`, returning either the static slice `b"good"` or `b"evil"`. It inserts the first, flips the cell, and reads the second under the first content identity. Both slices have the same length. No `unsafe`, hash collision, or mutable byte alias is needed. `VerifiedGeneration` compounds this: hydration checks that the stored descriptor agrees, relying on the store's broken immutability guarantee.

**Change:** accept a closed set of genuinely immutable byte owners, or a sealed immutable-storage abstraction whose implementations enforce stable content. An owned immutable byte handle with a retained backing owner is sufficient. Keep borrowed immutable slices where their lifetime really establishes the guarantee. Do not solve this by rehashing every read.

Evidence: [insertion and lookup](/Users/mileswirht/Downloads/backend/heart/memory/store.rs:207), [verification of arbitrary owners](/Users/mileswirht/Downloads/backend/heart/memory/store.rs:419), [publication's descriptor check](/Users/mileswirht/Downloads/backend/heart/hydration/publication.rs:98). **Reproduced.**

### 02 · P1 · Normal publication crash windows make reopen fail

Publication spans a journal commit, immutable fact file, temporary head, and head replacement. `load_existing()` requires all of them to be at the same final state. It rejects any `head_temp`, a complete fact without a head, or a fact receipt that is behind the journal's current receipt.

Two probes establish the immediate problem: a valid committed fact before its head returns `MissingHead`; a valid previous publication plus an interrupted successor head file returns `HeadTempPresent`. Later-update windows also encounter the exact-receipt/head-link checks. Corruption rejection is appropriate, but it does not provide crash recovery. The last good publication should remain usable while an unfinished successor is reconciled.

**Change:** define the durable commit point and recovery state machine. Reopen the last committed head, verify its reachable immutable facts, and reconcile or discard only identifiable unfinished successor state. Add fault injection at every persistence boundary. Preserve unknown outcomes where necessary without making unrelated valid state unavailable. Add checkpoints and reclamation: reopening currently also reads the entire chain of per-generation fact files.

Evidence: [owner persistence](/Users/mileswirht/Downloads/backend/server/journal/publication/owner.rs:371), [reopen checks](/Users/mileswirht/Downloads/backend/server/journal/publication/owner.rs:618). **Two crash-state probes reproduced.**

### 03 · P1 · The client overlay operation is insufficient to produce correct search results

`accept_remote_search()` iterates remote candidates, removes tombstoned keys, and substitutes a locally changed object when an upsert exists. It cannot check whether that replacement still satisfies the query, update its score, discover matches found only locally, or refill a page after deletions.

Example: the remote finds `parse_json`; the local edit renamed it to `encode_yaml`. The current merge substitutes the local object into the remote candidate list without any predicate evaluation. Conversely, a new local `parse_json` that has no remote candidate never enters this operation. The terminal calls these candidates, which is appropriate; the broader application still needs the missing effective-search stage before presenting them as matches.

**Change:** execute the same normalized query over a real local overlay index. Suppress remote versions superseded locally, union local matches with remaining remote candidates, deduplicate by stable declaration identity, and rerank. Fetch continuations when suppression prevents satisfying a requested page. Keep remote candidates useful immediately while source hydration proceeds independently.

Evidence: [candidate merge](/Users/mileswirht/Downloads/backend/interface/core/index_sync.rs:1549), [overlay representation](/Users/mileswirht/Downloads/backend/interface/core/index_sync.rs:610).

### 04 · P1 · Remote search admission is not bound to the query

`RemoteSearch` carries only a remote generation and candidate slice. The client verifies generation/snapshot equality, but has no query digest, scope, ranking recipe, page identity, or request correlation to verify. Two concurrent searches against the same snapshot are indistinguishable at this boundary. A delayed answer for one input can pass admission for another.

**Change:** bind a response to an immutable request identity containing normalized query, scope, snapshot, ranking recipe, and continuation. Pin the overlay revision used to assemble a displayed page, or explicitly restart that page when the revision changes. Reuse the routing layer's query-binding concept instead of inventing another disconnected identity.

Evidence: [remote response shape](/Users/mileswirht/Downloads/backend/interface/core/index_sync.rs:1084), [admission](/Users/mileswirht/Downloads/backend/interface/core/index_sync.rs:1549).

### 05 · P1 · Residency bookkeeping has no corresponding eviction or reclamation transition

Range receipts survive every manifest advance. Recording a receipt can coalesce overlapping ranges, but there is no operation to remove a receipt when bytes leave storage or to reclaim old, unreferenced segment receipts. `evict_disposable_projections()` clears only projection-kind bookkeeping. It does not solve canonical range residency.

The probe advances through 257 manifests, each with one distinct fully resident segment. The first 256 work; the next returns `ResidenceCapacity`, although the current manifest needs one slot. The local overlay similarly has 256 permanent key slots with no acknowledgment/rebase removal path.

More fundamentally, `ResidentRange::new()` accepts labels and extents without owning storage or verified bytes. That is an observation DTO, not a storage lease. A local query can rely on it only if an actual storage owner updates the controller coherently.

**Change:** make residency an indexed view of the cache manager's live entries, with leases for in-use data, explicit removal, byte accounting, and collection of unreferenced entries. Keep overlay acknowledgment separate from eviction. Offline local search also needs an independently persisted local base; `ClientIndex::new()` currently requires accepting a manifest before local selection.

Evidence: [manifest transition](/Users/mileswirht/Downloads/backend/interface/core/index_sync.rs:1243), [receipt recording](/Users/mileswirht/Downloads/backend/interface/core/index_sync.rs:1391), [receipt constructor](/Users/mileswirht/Downloads/backend/interface/core/index_sync.rs:856). **257-update exhaustion reproduced.**

### 06 · P1 · Capability acquisition reports execution without acquiring a capability

`LocalCapabilityExecution` changes from Armed to Ready, wakes itself, and returns `ExecutionRequest::completed()`. It performs no acquisition, loading, compatibility check, or activation. `apply_execution_terminal()` then stores an `ActiveBundle` consisting of a pin and bundle identity.

The pure `heart-adaptive` planner is not the problem: it correctly distinguishes a decision from execution. The interface adapter erases that distinction. Language-oracle support in the compiler host is local executable/module discovery and probing. I found no production bridge from a remote capability manifest to downloaded, verified, loaded oracles or embedding inference. There is also no implemented embedder in the reviewed server/interface paths.

**Change:** the executor must return an actual active resource handle. A capability manifest needs artifact identity, language/task support, target OS/architecture or portable runtime, ABI/protocol version, dependencies, sizes, and compatibility information. Model artifacts additionally need tokenizer and embedding-recipe identity. The host should verify origin policy and artifact bytes before loading executable capability code. Report Available, Loading, Active, Failed, or Unsupported based on real state.

Evidence: [simulated execution](/Users/mileswirht/Downloads/backend/interface/core/execution.rs:45), [active-bundle assignment](/Users/mileswirht/Downloads/backend/interface/core/service.rs:638), [local host discovery](/Users/mileswirht/Downloads/backend/compiler/application/host.rs:162).

### 07 · P1 · The current library facade advertises readiness for largely stubbed behavior

The current `Library` returns an empty shelf, returns an admission token without the documented durable admission work, cancels the admitted run, cannot reopen an image, and returns empty search lanes. Its health reports Shelf, Lexical, and Graph as ready. Several image accessors are also stubs. These are current interface-WIP blockers, not evidence that the underlying engines never existed.

`Library::open()` separately creates the requested workspace root and starts `LocalCompilerHost::production()`, which chooses its own data root from environment/platform defaults. The caller's workspace selection therefore does not define one coherent storage owner.

**Change:** wire one real service through the library, CLI, MCP, and GUI. Until then, unavailable operations must report their actual state. Admission should own a real reservation and durable admission identity. Pass an explicit host/storage configuration derived from the selected workspace into compiler startup. Do not expand the command vocabulary while the same commands have disconnected implementations.

Evidence: [open](/Users/mileswirht/Downloads/backend/interface/library/library.rs:82), [shelf/admission](/Users/mileswirht/Downloads/backend/interface/library/library.rs:144), [search](/Users/mileswirht/Downloads/backend/interface/library/library.rs:286), [health](/Users/mileswirht/Downloads/backend/interface/library/library.rs:332), [image facade](/Users/mileswirht/Downloads/backend/interface/library/image.rs).

### 08 · P1 · The working index explorer chooses eight directories by hash, not the published corpus

`index_search()` discovers projection directories, sorts their content identities, takes the first eight, and constructs a snapshot over a fixed shadow generation. There is no publication manifest determining which segments are active or their update order. A ninth segment can remain unsearchable regardless of its relevance. The response admits partial coverage, but supplies no mechanism to search the remaining segment selection. A directory's existence also does not prove current publication membership.

**Change:** select segments from the committed publication manifest. Use metadata routing and page/range budgets to bound each operation, with continuation across the full selection. Carry publication/snapshot identity into the response. Directory discovery belongs in recovery and diagnostics, not normal search semantics.

Evidence: [selection and coverage](/Users/mileswirht/Downloads/backend/interface/library/explore.rs:400), [directory discovery](/Users/mileswirht/Downloads/backend/interface/library/explore.rs:560), [shadow generation](/Users/mileswirht/Downloads/backend/interface/library/explore.rs:607).

### 09 · P1 · Production lexical weights destroy prefix ranking

The builder assigns every entity name a score of 1. Prefix discounting computes `stored * prefix_bytes / term_bytes` using integer division. Every proper prefix of a nonempty term therefore receives 0. `ma` against `map` and `map_or_else` ties at zero; document identity decides the order. Existing tests using larger artificial scores do not exercise the production combination.

**Change:** define one ranking recipe with sufficient resolution, or compare an explicit rank tuple such as match class, normalized edit/prefix measure, field priority, and stable identity. Cover production-generated rows in ranking tests. Raising the score constant alone fixes this arithmetic symptom but does not define identifier search quality.

Evidence: [builder score](/Users/mileswirht/Downloads/backend/server/index/build/construct.rs:33), [discount](/Users/mileswirht/Downloads/backend/server/index/core/lexical.rs:109). **Reproduced.**

### 10 · P1 · Prefix reconciliation confuses membership deletion with document deletion

`LexicalRowValue::Tombstone` means the document is no longer a member of a particular term. Manifest execution marks a document seen before deciding whether that row is live. It deduplicates by document across the entire matching prefix range, so the first matching term shadows all other matching terms for that document.

Counterexample: one segment contains tombstoned `(ma, A)` and live `(map, A)`. Exact `map` returns A; prefix `m` returns a complete empty result. Multiple live aliases likewise use the first matching row rather than the best matching contribution. The durable Tantivy composition follows the same document-first pattern.

**Change:** reconcile latest membership by `(term, document)` first, then combine surviving terms by document using the query's ranking rule. If document deletion is also needed, encode it explicitly as a separate fact. Do not overload a term-membership tombstone.

Evidence: [row semantics](/Users/mileswirht/Downloads/backend/server/index/core/lexical.rs:163), [manifest reconciliation](/Users/mileswirht/Downloads/backend/server/index/core/lib.rs:597), [durable query composition](/Users/mileswirht/Downloads/backend/server/index/tantivy/storage/query.rs). **Core counterexample reproduced.**

### 11 · P2 · “Lexical search” currently names several incompatible query languages

The core and durable projection use raw byte exact/prefix operations. In-memory Tantivy uses a tokenized `TEXT` field and `QueryParser`. The interface has separate exact, ASCII-insensitive, prefix, and substring scoring. These are materially different semantics for case, whitespace, punctuation, qualified names, and identifiers. The build path indexes entity names rather than a deliberate set of name, path, documentation, and signature fields.

Changing execution location or backend can consequently change which input is a literal, which is an expression, what matches, and how results rank. That violates the desired interchangeable local/remote execution model.

**Change:** parse once into a typed `QuerySpec`. Specify literal versus expression mode, identifier normalization, field selection, scope/version filters, and case behavior. Keep engine-specific syntax behind adapters. Preserve exact case-sensitive identity resolution as its own operation. Add a shared conformance corpus for all search implementations.

Evidence: [raw operation vocabulary](/Users/mileswirht/Downloads/backend/server/index/core/lexical.rs), [Tantivy parser](/Users/mileswirht/Downloads/backend/server/index/tantivy/lib.rs:359), [interface scoring](/Users/mileswirht/Downloads/backend/interface/search/score.rs:25), [semantic row construction](/Users/mileswirht/Downloads/backend/server/index/build/construct/semantic.rs:67).

### 12 · P2 · One Tantivy path discards relevance; the explorer discards result identity

The in-memory Tantivy adapter collects matches, ignores each backend score, and inserts documents in identity order. Its comments promise core ranking, but that search method does not invoke the core ranking recipe. This is useful only as a membership enumerator, not a ranked search API.

Separately, `IndexHitRow` discards `EntityDocumentId` and returns display text/matched term/score. Same-named declarations become indistinguishable. An artifact-aware identifier does not need a package display coordinate in order to remain valuable: keep it, and resolve its display metadata through the catalog.

**Change:** make membership enumeration explicit or feed it into the shared ranker. Retain stable document identity and provenance in every product result. Do not make opening a result depend on guessing an entity from a string.

Evidence: [discarded score](/Users/mileswirht/Downloads/backend/server/index/tantivy/lib.rs:387), [identity ordering](/Users/mileswirht/Downloads/backend/server/index/tantivy/lib.rs:425), [explorer result conversion](/Users/mileswirht/Downloads/backend/interface/library/explore.rs:633).

### 13 · P1 · Per-segment top-K cannot support the current newest-wins global merge

Workers return at most K observed hits. The merge then applies newest-segment deduplication. It cannot discover a newer version that fell outside that segment's K, or a tombstone, since `ObservedHit` has no deletion/membership state.

Reproduced with K=1: newer segment has A=1 and B=50; older segment has A=100. Canonical newest-wins search correctly returns B=50. The workers legitimately return B=50 and A=100. The coordinator returns stale A=100, with no missing assignments. This remains broken after the small routing fix below.

**Change:** reconcile version/membership authority before truncation, or provide an authoritative current-document filter plus refillable candidate streams and valid stopping bounds. Another viable layout assigns every version/tombstone of a semantic key to one reconciliation owner before distributed ranking. A larger arbitrary oversampling constant is not a correctness proof.

Evidence: [reply hit shape](/Users/mileswirht/Downloads/backend/server/index/routing/merge.rs:15), [reply cardinality contract](/Users/mileswirht/Downloads/backend/server/index/routing/merge.rs:493), [merge](/Users/mileswirht/Downloads/backend/server/index/routing/merge.rs:260). **Canonical-versus-distributed counterexample reproduced.**

### 14 · P1 · Successful replies could answer a different assignment — fixed

`Coordinator::execute()` validated successful replies against the whole plan, then stored the reply in the slot for the request it had just dispatched. A reply for another valid assignment could pass. Failure responses already required exact equality with the dispatched attempt.

**Applied:** check `reply.attempt == attempt` before the existing validation. Added a regression test returning another valid assignment's reply. The test fails without the guard, returning `Ok(MergeOutcome)`, and passes with it. All 12 routing tests passed. The fixture setup fails explicitly rather than silently returning from the test.

Evidence: [five-line guard](/Users/mileswirht/Downloads/backend/server/index/routing/planning.rs:320), [regression test](/Users/mileswirht/Downloads/backend/server/index/routing/tests.rs:618). This is the only production behavior changed in this audit.

### 15 · P1 · The Qdrant path is a tiny exact-vector fixture, not a scalable semantic retrieval pipeline

The shared vector structures allow at most 16 dimensions and 16 points per segment. Qdrant admits four selected segments, requests exactly 17 points with exact search and vectors included, then rejects a response containing more than 16 points. The remote candidate limit does not follow the caller's requested limit. Two legitimate selected segments totaling 17 matching points therefore break a request for even one result.

The pipeline accepts precomputed `i16` coordinates. It has no integrated text preparation, chunking, tokenization, inference, embedding cache, or model acquisition. `ModelId([u8; 16])` and a dimension/metric label do not specify how those coordinates were produced. The quantization, normalization, pooling, and task/query treatment need identities too.

**Change:** separate production vector storage/query configuration from bounded reference-test structures. Bind embeddings to source content and a complete embedding recipe. Select supported dimensions from the admitted model, use bounded chunks/batches independently of corpus size, and decide explicitly between exact search and approximate candidate generation. Keep the tiny exact implementation as an oracle for testing ANN recall and ranking correctness.

Evidence: [vector bounds](/Users/mileswirht/Downloads/backend/server/index/graph-vector/vector.rs:13), [Qdrant limits](/Users/mileswirht/Downloads/backend/server/index/qdrant/limits.rs), [request shape](/Users/mileswirht/Downloads/backend/server/index/qdrant/wire/request.rs), [response capacity rejection](/Users/mileswirht/Downloads/backend/server/index/qdrant/wire/response.rs:117), [query](/Users/mileswirht/Downloads/backend/server/index/qdrant/query.rs:19).

### 16 · P1 · Qdrant's “complete” result is inferred from intended coverage, not actual projection coverage

The adapter accepts fewer returned points than the selected descriptors declare, including zero points for a nonempty segment. The retrieval wrapper calls a healthy request Complete whenever the caller's `reachable` descriptors cover the selection. Those descriptors prove what was selected, not that Qdrant retained and searched the entire projection.

The HTTP probe selects a valid one-point segment and returns an empty points array. The real adapter returns `Ok(count=0)`. Combined with the wrapper's classification, projection loss can look like a complete no-match answer.

**Change:** publish/query a verified projection epoch or readiness receipt tied to the manifest. For the existing tiny exhaustive path, verify the expected complete segment contents/counts. For ANN, completeness must describe covered corpus/snapshot, while approximation and recall are separate quality properties. Never infer data coverage from successful transport alone.

Evidence: [response validation](/Users/mileswirht/Downloads/backend/server/index/qdrant/wire/response.rs:117), [coverage classification](/Users/mileswirht/Downloads/backend/server/index/retrieval/qdrant.rs:63). **Adapter counterexample reproduced; wrapper consequence established in code.**

### 17 · P1 · Remote vector labels do not authenticate the vector contents

A Qdrant response is checked for selected segment labels, authority, partition, and derived physical ID. The returned coordinates are then decoded and scored. The segment content hash is not checked against those coordinates, and decoding checks shape rather than the original `i16` coordinate representation. A changed vector can retain all identity labels and influence the score. The descriptor documentation explicitly says it does not prove remote candidate membership; consumers must honor that limit.

The physical ID is a 64-bit FNV hash of semantic labels. Pre-read compatibility checks are not an atomic collision guard against concurrent writers. No exploit or collision was demonstrated; the issue is an unnecessarily weak physical identity for a system otherwise built around cryptographic content IDs.

**Change:** treat remote rows as candidates, resolve them through canonical source/vector identity before making stronger claims, and use an explicit trusted projection boundary where full byte verification is impractical. Use a larger cryptographically derived physical identifier and an atomic publication protocol. Add typed authentication configuration: the current adapter owns its agent and supplies no API-key/header configuration, preventing ordinary authenticated deployments without another component.

Evidence: [coordinate admission and scoring](/Users/mileswirht/Downloads/backend/server/index/qdrant/wire/response.rs:117), [descriptor contract](/Users/mileswirht/Downloads/backend/server/index/graph-vector/vector.rs:274), [physical ID](/Users/mileswirht/Downloads/backend/server/index/qdrant/identity.rs:39), [transport headers](/Users/mileswirht/Downloads/backend/server/index/qdrant/transport.rs:68).

### 18 · P2 · Snapshot identity is included where it prevents immutable artifact reuse

Vector segment hashing includes `authority.snapshot`; Qdrant point IDs also include snapshot; worker affinity hashes snapshot as well. Publishing a new snapshot can change the identity and placement of unchanged vector contents and move unchanged lexical segments to different workers. That undermines the intended reuse of immutable artifacts and warm caches.

The current shared snapshot grammar binds only exact and lexical lanes. Simply adding vector IDs to that same grammar while keeping snapshot inside vector IDs would create a circular identity dependency.

**Change:** hash immutable vector artifacts from source/recipe/content facts independently of a publication snapshot. Let a manifest bind those artifacts into a published view. Bind requests to that view without copying the snapshot into every physical identity. Rendezvous placement should primarily use artifact/shard identity and worker topology; snapshot checks remain request-admission checks.

Evidence: [vector hash](/Users/mileswirht/Downloads/backend/server/index/graph-vector/vector.rs:427), [snapshot grammar](/Users/mileswirht/Downloads/backend/heart/identity/index_snapshot.rs:43), [worker affinity](/Users/mileswirht/Downloads/backend/server/index/routing/planning.rs:406).

### 19 · P1 · Graph completeness is accepted independently of the acquired graph view

The Trustfall retrieval boundary receives `GraphTerminal` and `ValidatedGraphView` separately. It compares their authority labels, then transfers Complete/Partial classification to the query result. `GraphTerminal::Complete` is publicly constructible, and the view validates a provided row set rather than proving that it is the complete set selected by that terminal.

An empty or unrelated same-authority view can therefore be paired with a Complete terminal. No Rust type binds the acquired selection, final coverage, and exact rows together. This is an internal trust-boundary problem; it is not evidence of an exposed network exploit.

**Change:** consume one acquired-batch owner carrying its exact selection and coverage together with the view/lease. The boundary that observes stream completion should be the only constructor of a completed acquisition. Keep untrusted observations distinct from verified content membership and from actual coverage.

Evidence: [separate inputs and authority comparison](/Users/mileswirht/Downloads/backend/server/index/retrieval/trustfall.rs:29), [Complete transfer](/Users/mileswirht/Downloads/backend/server/index/retrieval/trustfall.rs:127), [graph row/view validation](/Users/mileswirht/Downloads/backend/server/index/graph-vector/graph.rs).

### 20 · P2 · Trustfall scans the corpus to answer a keyed adjacency lookup

The generic adapter enumerates graph sources by scanning edges and checks earlier edges to suppress duplicates. That can be quadratic. The semantic adapter enumerates canonical entities and lets the interpreter filter the requested source instead of pushing the known identity into the starting-vertex resolver. The occurrence path walks the image's occurrence table to find one source's occurrences.

Cancellation around `next()` does not bound latency if one `next()` scans a large nonmatching region. Wrapping a ready iterator in an async stream does not make that work cooperative. Fixed small limits currently hide the impact; increasing them exposes it.

**Change:** use the existing entity/CSR indexes to seed the requested vertex and adjacency range directly. Index occurrences by source/link where repeated lookup warrants it. Sample cancellation and yield after bounded scanned work, including when no rows match. Retain Trustfall for compositional graph queries, and use the same indexed primitives for simple neighbor requests.

Evidence: [occurrence iteration](/Users/mileswirht/Downloads/backend/server/index/trustfall/graph.rs:341), [generic source enumeration](/Users/mileswirht/Downloads/backend/server/index/trustfall/graph.rs:834), [semantic starting vertices](/Users/mileswirht/Downloads/backend/server/index/trustfall/graph.rs:992), [stream polling](/Users/mileswirht/Downloads/backend/server/index/trustfall/graph.rs:236).

### 21 · P2 · The graph/vector result identity and traversal scope are narrower than the product needs

Lexical results can carry `EntityDocumentId` with artifact identity. Several graph/vector results carry only an image-local `EntityId`, plus varying partition/snapshot labels. A multi-package consumer still needs an authoritative mapping to the owning image. The semantic occurrence result does better by retaining its image owner; make that discipline consistent across the wire boundary.

The graph implementations traverse local outgoing targets and omit external targets. They do not themselves provide incoming references, cross-package resolution, or bounded multi-hop traversal. These are explicit implementation limits, not automatically bugs in a documented local-neighbors API. The weakness is treating that API as the full product graph service.

**Change:** use a globally meaningful entity reference, such as `(semantic image identity, entity ordinal)`, and retain exact relationship/source evidence. Preserve external or unresolved edges as explicit targets. Build incoming and cross-package resolution as real query capabilities, with frontier limits and coverage, instead of silently implying that a local neighborhood is the complete graph.

Evidence: [vector hit](/Users/mileswirht/Downloads/backend/server/index/graph-vector/vector.rs:452), [semantic graph paths](/Users/mileswirht/Downloads/backend/server/index/trustfall/graph.rs:293), [occurrence provenance](/Users/mileswirht/Downloads/backend/server/index/trustfall/graph.rs:98), [interface graph dispatch](/Users/mileswirht/Downloads/backend/interface/core/service.rs:218).

### 22 · P1 · Corpus size is coupled to scratch-array bounds

`build_semantic()` counts all canonical entities in an artifact and rejects more than `MAX_INDEX_ROWS`, currently 256. It does not split that artifact into multiple segments. Snapshot/route/client lane selections then cap selected segments at eight. These are not merely configurable concurrency limits: they constrain which corpus can be represented by the current operation.

The local generic graph/vector paths admit four partitions and small fixed rows. The interface's four-row reply container is another independent limit. None of these limits constitutes a scalable paging/partitioning strategy.

**Change:** distinguish corpus cardinality, immutable shard size, request fan-out, candidate batch size, output page size, and memory budget. Only the latter operational dimensions should bound a single execution step. Support a manifest directory larger than one request, indexed routing over it, continuation, and compaction. Do not raise the constants until the quadratic work described below is removed.

Evidence: [semantic preflight rejection](/Users/mileswirht/Downloads/backend/server/index/build/construct/semantic.rs:187), [core bounds](/Users/mileswirht/Downloads/backend/server/index/core/lib.rs), [client bounds](/Users/mileswirht/Downloads/backend/interface/core/index_sync.rs:16), [reply slots](/Users/mileswirht/Downloads/backend/interface/core/retrieval.rs).

### 23 · P2 · Search repeatedly does whole-selection work and allocates disposable intermediate storage

Core lexical execution preflights unique documents by scanning prior matching ranges/rows before using a hash table in the final pass. Durable Tantivy requires candidate scratch sized to all rows across selected segments and clears it each operation. The explorer reopens segments on every search; reopening validates stored documents against sidecars. Ranking uses repeated sorted insertion, and interface merge uses linear duplicate search plus `address.to_string()` allocations inside the sort comparator.

These costs are more consequential than whether a small result row is copied. The validations at first admission are useful; repeating them for unchanged immutable artifacts on every keystroke is avoidable.

**Change:** retain verified readers keyed by artifact and recipe, lease them across queries, reuse per-request scratch, and use indexes for predicate selection. Reconcile once and rank with a bounded heap/selection algorithm as appropriate. Precompute display/sort keys outside comparators. Keep a single scratch-output transaction if rejection must leave caller output unchanged; avoid separate quadratic counting passes to honor that contract.

Evidence: [lexical preflight](/Users/mileswirht/Downloads/backend/server/index/core/lib.rs:621), [durable search](/Users/mileswirht/Downloads/backend/server/index/tantivy/storage/query.rs), [reopen validation](/Users/mileswirht/Downloads/backend/server/index/tantivy/storage/publication.rs:232), [interface merging](/Users/mileswirht/Downloads/backend/interface/search/terminal.rs:64).

### 24 · P2 · Impossible runtime admissions become permanently pending futures

`admit_when_ready()` treats every ordinary rejection as something that can eventually succeed. A request larger than the runtime's entire byte budget is registered as a waiter. Its future remains pending even with an empty runtime, and no credit return can ever make it fit.

Pending futures also retain their work before those bytes enter the admitted-work accounting. The byte budget therefore describes admitted work, not total process memory including waiting payloads and retained result/error values.

**Change:** reject an individually impossible request immediately with a distinct error. Bound pending bytes separately, or queue compact work descriptors and acquire/materialize large payloads after reservation. Document and measure the separate budgets instead of describing the admitted-byte counter as a total memory bound.

Evidence: [wait registration](/Users/mileswirht/Downloads/backend/server/runtime/admission.rs:174), [future registration](/Users/mileswirht/Downloads/backend/server/runtime/async_admission.rs:200). **Empty-runtime counterexample reproduced.**

### 25 · P2 · The execution boundaries serialize remote work and have weak latency control

The coordinator dispatches assignments sequentially through a synchronous trait returning slices borrowed from immutable worker storage. That suits preexisting local rows; a real RPC needs an owned response/lease and an asynchronous lifecycle. The code has no production `WorkerBackend` implementation in the reviewed paths.

Qdrant uses blocking requests and immediate retries, including for 429. Per-request timeout/retry counts do not impose one query-wide deadline, and cancellation at the retrieval boundary only precedes the blocking work. Collection setup and mutation also incur multiple round trips. The library's custom `block_on()` loops with a no-op waker and `yield_now`, repeatedly polling pending futures.

**Change:** provide an async owned-batch backend with bounded concurrent fan-out, a query-wide deadline, cancellation, and backpressure. Use a real executor for asynchronous catalog work. Place unavoidable blocking work on a bounded host pool. Retry transient failures with bounded backoff/jitter and server guidance, within the same deadline. Move collection setup out of the request hot path.

Evidence: [sequential dispatch](/Users/mileswirht/Downloads/backend/server/index/routing/planning.rs:282), [worker lifetime](/Users/mileswirht/Downloads/backend/server/index/routing/worker.rs:95), [retry loop](/Users/mileswirht/Downloads/backend/server/index/qdrant/transport.rs:31), [busy polling](/Users/mileswirht/Downloads/backend/interface/library/explore.rs:668).

### 26 · P2 · Registry chunking repeats the entire source/history workload

The crates.io adapter fetches and parses the package's full sparse-index body for each bounded chunk, sorts it, then chooses the next slice. The pipeline also reloads all stored observations and builds an active-observation set each page. A full scan of V versions in 64-row chunks repeats approximately V work per chunk. The 512 KiB sparse-page cap rejects large histories outright.

`poll_and_apply()` is async, but acquisition and materialization are synchronous calls within it. It accumulates each page's materialized entity locators before the catalog commit; a 64-version count does not bound their total entity bytes. Only a test materializer implementation was found in this acquisition layer.

**Change:** acquire one immutable source snapshot per scan, retain or spool it with a cursor, and parse each record once. Query relevant observations rather than reloading all history. Budget bytes/entities as well as versions, and offload compilation/download work appropriately. Keep the transactional checkpoint-plus-page application: that is a useful invariant.

Evidence: [sparse polling](/Users/mileswirht/Downloads/backend/server/index/acquire/crates_io.rs:109), [pipeline](/Users/mileswirht/Downloads/backend/server/index/acquire/pipeline.rs:124), [atomic catalog apply](/Users/mileswirht/Downloads/backend/server/index/catalog/src/page.rs:93).

### 27 · P2 · Publication grouping conflates concurrent successors with conflicting duplicates

The owner takes a group, chooses one publication candidate, commits one event, and succeeds only commands with the same `PublicationInput`. Other noncancelled inputs in that group fail as conflicts. A group capacity of 64 therefore does not mean 64 distinct publications share a durable commit. Distinct valid work can fail merely because it arrived in the same owner batch.

**Change:** specify whether the queue represents compare-and-swap attempts against one parent or independently serializable publications. For compare-and-swap, carry the expected parent explicitly and surface a rebase outcome. For independent work, process the ordered publications rather than classifying the rest of a batch as conflicting duplicates. Group commit should amortize persistence while preserving that policy.

Evidence: [single event append](/Users/mileswirht/Downloads/backend/server/journal/publication/owner.rs:371), [group completion](/Users/mileswirht/Downloads/backend/server/journal/publication/owner.rs:557).

### 28 · P2 · Lane fusion and paging do not have stable, well-defined semantics

Interface fusion normalizes each lane relative to the maximum in the current candidate batch and adds a fixed contribution for every row. Duplicate rows from the same lane can contribute repeatedly. Changing a lane's candidate batch changes the scale even when a surviving result's own score has not changed. The cursor is only an offset, so updates to snapshot, overlay, or candidate composition can duplicate or skip results between pages.

**Change:** define one contribution per `(lane, declaration)`, choose a calibrated score or explicit rank-fusion recipe, and bind cursors to the full query context. Specify whether paging pins a candidate set or continues an ordered search. Add deterministic tie-breaking without allocating address strings inside comparisons.

Evidence: [fusion and cursor use](/Users/mileswirht/Downloads/backend/interface/search/terminal.rs:64).

### 29 · P2 · One unavailable source can fail an otherwise usable result batch

`resolve_tantivy_sources()` preflights and resolves the entire batch into scratch, but a missing canonical source/path on one hit returns an error for the batch. In contrast, occurrence-source transport already distinguishes captured and unavailable provenance. Search and navigation should preserve that distinction consistently.

**Change:** retain valid search matches and attach source availability per hit. A corrupt identity should fail admission; an intentionally unavailable span or a source not yet hydrated should not erase unrelated results. Reuse a verified-image session across the batch instead of the convenience path that re-verifies an image per hit.

Evidence: [batch resolution](/Users/mileswirht/Downloads/backend/server/index/retrieval/source.rs:510), [verified image session](/Users/mileswirht/Downloads/backend/server/index/retrieval/source.rs:639), [explicit occurrence availability](/Users/mileswirht/Downloads/backend/server/index/retrieval/source.rs:241).

## What to remove, consolidate, and keep

### Make the boundaries reflect deployment choices

The current crate graph pushes concrete engines into the interface library and compiler vocabulary into interface core. `compiler/application` then imports interface-owned package types. `heart/telemetry` depends on server runtime and workflow. This is conceptual coupling even where Cargo has no dependency cycle.

Use the following ownership split; it does not require immediately renaming every crate:

| Boundary | Owns | Must not require |
|---|---|---|
| Canonical data kernel | Content/artifact identity, descriptors, root/pack formats, validation | Qdrant, Tantivy, product UI, telemetry exporters |
| Index/query core | Query semantics, segment membership, logical entity refs, ranking/reconciliation | A particular network client or UI |
| Host adapters | Tantivy, Qdrant, Trustfall integration, catalog, registry, filesystem/runtime execution | Product rendering |
| Application service | Publication, search orchestration, local overlay, capability/resource lifecycle | One mandatory deployment topology |
| Protocol | Request context, pages, coverage, source refs, capability manifests | Compiler implementations or search engine crates |
| Interfaces | CLI/MCP/GUI parsing and rendering | Directly coordinating engine state |

Make local engines optional composition choices. A small remote-assisted client should not have to depend on the compiler application, Qdrant, Tantivy, and Trustfall merely to use the library's DTOs. Conversely, an offline client should be able to run with a persisted local base and selected installed capabilities.

Move telemetry exporters and runtime/workflow-specific probes out of the foundational `heart` boundary. Retain the tiny generic `heart-observe` hook there. Small pure crates can become modules where they share release policy, consumers, and invariants; do not merge genuinely independent byte-format or host-platform boundaries just to reduce the directory count.

Evidence: [library dependencies](/Users/mileswirht/Downloads/backend/interface/library/Cargo.toml), [interface core dependencies](/Users/mileswirht/Downloads/backend/interface/core/Cargo.toml), [compiler host's interface import](/Users/mileswirht/Downloads/backend/compiler/application/host/paths.rs:13), [telemetry dependencies](/Users/mileswirht/Downloads/backend/heart/telemetry/Cargo.toml).

### Stop creating proof-shaped types that only rename observations

Use a short, explicit progression:

1. **Observed:** remote labels, candidate rows, advertised manifests, storage reports.
2. **Verified:** bytes match an immutable artifact and parse under its schema/recipe.
3. **Selected:** that artifact belongs to the pinned published view.
4. **Resident/active:** an actual owner keeps the bytes or executable resource available.
5. **Complete:** the requested scope was covered under a stated retrieval method.

Not every stage needs a new generic wrapper. The crucial type should own the evidence or resource whose lifetime it promises. Plain observations should remain plain data. Fix the strong-sounding admission, active-bundle, graph terminal, and residency types at their constructors.

`PublishedIndexSnapshot::seal()` only checks the generation relation, while the newer compilation-index sealing path performs a stronger fragment/manifest validation and returns another owner. Unify the published view used by retrieval with the strongest existing publication proof instead of retaining parallel sealing stories. Graph/vector membership must enter that view explicitly.

Evidence: [legacy seal](/Users/mileswirht/Downloads/backend/server/index/publish/lib.rs:75), [compilation sealing](/Users/mileswirht/Downloads/backend/server/index/publish/compiler.rs).

### Spend types on distinctions that change behavior

Good candidates: image-local ordinal versus global entity reference; term deletion versus document deletion; candidate versus verified source; pending versus active capability; result page limit versus corpus/shard limit; permanent admission failure versus temporary pressure.

Low-value candidates: dozens of unrestricted numeric wrappers with nearly identical `Deref`, `Borrow`, `AsRef`, `From`, and `get` surfaces; hand-unrolled four-slot containers; repeated mirror enums used only to forward one dependency's error. Numeric newtypes are useful when they prevent mixing units or enforce a bound. They are not useful solely because every scalar can be given a noun.

Use straightforward typed slices/iterators for ordinary sequences. Prefer explicit value access for numeric wrappers. Retain private-field proof owners when construction actually validates an invariant. Reduce boilerplate comments that merely restate the filename; document the real preconditions, invalidation events, and complexity instead.

### Reuse memory at the resource-owner level

Keep verified immutable image/segment readers, pooled query scratch, bounded owned response batches, and immutable body ranges. Measure bytes across cache, in-flight work, waiters, and results separately. The current `MemoryStore` can be a bounded immutable staging owner, but it is not yet an evicting distributed cache; publication lifetimes borrowing the entire store also restrict mutation/replenishment.

`LeanMemoryStore` has a useful inline backing policy, yet hydration verification takes the heap `MemoryStore` alias. Generalize only that genuine backing-independent boundary or provide a compact verified-storage view. Do not expose extra storage generic parameters through the whole application just to share a small lookup operation.

Preserve the existing object-pack prefix/body split, checked borrowed layouts, domain-separated identities, exact output preflight where needed, verified source-image reuse, typed absence, and atomic catalog checkpointing. They are foundations worth connecting. Repeated whole-image verification, duplicated result mirrors, and bespoke ready-iterator async wrappers are the work to eliminate.

## The local-first flow to build

1. Open a durable local view and overlay immediately. Query local data without waiting for a remote manifest or full hydration.
2. When remote service is available, obtain a manifest/reference to a published view and issue the same normalized query remotely. A small response should contain stable entity refs, display fields, scores or rank information, query binding, and explicit coverage/provenance.
3. Reconcile remote candidates against the current local overlay. Include local-only matches. Display usable results immediately; source navigation can resolve or hydrate selected source artifacts on demand.
4. Hydrate reusable immutable ranges in the background under a byte budget. Cached data changes placement, not the meaning of the query. Leases and eviction keep the residency view accurate.
5. Acquire an oracle or embedding model only when needed and supported. Admit it through a real artifact/compatibility boundary; activate an actual handle and keep it leased while in use. Remote search remains useful before optional local inference is ready.
6. A publication update produces a new manifest referencing unchanged artifacts where possible. Query pages pin a coherent context. Rebase/acknowledge local edits explicitly and collect unreferenced cached artifacts.

An embedding recipe should bind source extraction/chunking, model artifact, tokenizer, pooling, normalization, query/document treatment, dimension, numeric representation, and metric. “Same model ID” alone is not enough to guarantee local and remote vectors occupy the same space.

## Order of work

**First: make existing answers trustworthy.** Repair immutable byte ownership, membership reconciliation, distributed newest-wins ranking, query-bound replies, and real coverage. Keep the small routing guard. Add interrupted-publication recovery. Replace fake success/readiness with real state.

**Second: connect one narrow usable product path.** Implement exact/name search from a real published manifest through the application service into every interface. Support a persisted local base, local edits, immediate remote candidates, source-on-demand, and stable pages. This should be an end-to-end slice, not another set of public enums.

**Third: remove corpus-sized limits from operation-sized storage.** Add manifest routing, proper segment construction, compaction, refillable ranking, cached readers, async bounded fan-out, and resource budgets. Benchmark after correcting semantics.

**Fourth: implement the semantic and graph capabilities.** Add real embedding production/model lifecycle, Qdrant projection readiness, global entity/source mapping, indexed Trustfall traversal, and explicit external/incoming graph support. Keep an exact reference implementation for differential tests.

**Then consolidate crates and vocabulary around the working ownership model.** Delete redundant paths and adapters after consumers migrate. Renaming folders before resolving the contracts would preserve the same problems under cleaner names.

## Verification and missing acceptance tests

The audit reproduced seven behavioral counterexamples in a separate probe executable: mutable `AsRef` owner, zero prefix scores, prefix tombstone suppression, empty Qdrant projection acceptance, 257-update residency exhaustion, impossible runtime admission, and canonical/distributed top-one disagreement. Two additional tests reproduced journal crash-state rejection. The routing regression fails with its guard removed and the full routing suite passes with the guard installed: 12 passed.

These probes intentionally assert the current bad behavior so they can demonstrate it. They are not regression tests asserting the desired repaired behavior. The production routing test does assert the repair. Sources and reproduction instructions are in [the evidence directory](/Users/mileswirht/Downloads/backend/audit/server-heart-2026-09-12-evidence/README.md).

Before calling the distribution complete, require the following tests:

| Area | Acceptance cases |
|---|---|
| Search conformance | Empty input, exact name, proper prefix, case variants, Unicode identifiers, `snake_case`, camel case, punctuation, qualified path, multiword literal, query-language operators, same name in multiple packages/versions |
| Update semantics | Rename, new local-only match, deletion, removed alias with live alias, rebase, duplicate delivery, concurrent searches, old reply after new query |
| Paging | Suppressed remote candidates, refill, overlay changes between pages, stale cursor, version-pinned continuation |
| Coverage | Empty versus missing projection, incomplete selected shard, stale worker, same-label wrong payload, source unavailable, external graph edge |
| Scale | Artifact above 256 entities; above eight segments; vector selection above 16 points; configured real model dimensions; repeated publication/cache turnover |
| Durability | Kill at each journal/fact/head sync and rename boundary; reopen last good state; recover successor; reject actual corruption |
| Resource use | Waiters with oversized payloads, cancellation during scans and network retry, slow remote, memory pressure with live leases, reader cache reuse |
| Semantic quality | Exact-versus-ANN recall on representative code queries, local/remote embedding equivalence, recipe migration, duplicate chunks, identifiers versus natural-language queries |

I would not claim a remote code-execution exploit, memory unsafety, measured production QPS, or ANN recall from this review. The demonstrated integrity, correctness, recovery, and scale failures are already sufficient to set the repair priority.

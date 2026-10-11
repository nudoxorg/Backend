# Runtime readiness: 2026-10-11 evening

This is a source and evidence checkpoint, not a production-readiness declaration. The [independent closed-tail audit](closed-runtime-tail-evidence-20261011.json), [current client audit](current-b3-client-evidence-20261011.json), [fresh installation audit](current60-first-flow-evidence-20261011.json), and [selected-read audit](current60-selected-read-evidence-20261011.json) distinguish the tested sources and retain earlier failures.

## Integrated source

The reviewed branch composes bounded MCP request cancellation with connection ownership and retained continuations. A canceled request cannot abandon an issued portable certificate during a fallible reconnect; its original request deadline survives preparation and reconnect. Stdio admission and progress queues are bounded, and HTTP request lifetime is distinct from shared owner lifetime. Native cancellation, half-close, HTTP socket-drop, and continuation gates are still required on this composed source.

Local semantic memory uses affine reservations for model provisioning, coordinate allocation, durable cache traffic, retained lookup metadata, and canonical embedding payloads. Immutable coordinate storage retains its accounting owner through all clones. Cache misses and failed reads release their credit; background cache actors retain credit until actual completion. These are source-reviewed changes; genuine model activation and the complete native memory suite remain open.

Folder admission in the GUI has one physically active filesystem task and one latest pending action per retained form. Edits and cover/uncover transitions retire observations without pretending to cancel non-preemptible filesystem work. Linux motion capture uses an explicit monotonic-clock anchor and pinned time-namespace identity. Current native interaction, resize, animation, and live-index captures remain open.

The installer validates an existing same-release Mac installation before downloading its archive, including inventory and startup rechecks. The one-line bootstrap calls that path through the already verified platform module. Download progress is bounded and network reads use a 20-second timeout. Root merged the independently tested seven-file installer subset through Forgejo PR #152 into canonical at `3082ee03c0379283383399ffd9b2e6c07e4e550f`. The unchanged public channel still selects historical binaries; this source merge has not promoted a new release.

## Actual native and installed observations

| Gate | Actual result | Scope |
| --- | --- | --- |
| Matched CLI/MCP/service build, `edd2c9735e` | Exit 0, 343.168 seconds | Diagnostic normal binaries; no tests or runtime acceptance |
| Matched CLI/MCP/service build, `6e9cc85def` | Exit 0, 133.591 seconds | Matched newer diagnostic binaries; no optimized-release benchmark |
| Full client gate, `6e9cc85def` | Compile exit 101; zero tests | Retained E0283 in a test's ambiguous revision conversion |
| Full client gate, `e961d16bff` | 132 passed, four failed, zero ignored | Three socket fixtures exceed Unix path capacity; one supposed foreign basis has unchanged canonical content |
| Full client gate, `b3f0d72131` | 144 passed, zero failed, zero ignored, zero filtered | Actual ILO build and test exit 0 in 214.711 seconds; test body 4.36 seconds; exact source/tree/locks and raw logs independently checked by Root |
| Installed HTTPie, earlier `71eef72724` | 133 Python sources genuinely Published | Fresh Ubuntu home; exact operation and view identities retained |
| Closed HTTPie selected reads, earlier `71eef72724` | 60 actual responses; first two searches fail with Preparing | Root independently rehashed every raw response, IDs, publication identity, and recorded budgets |
| Installer reuse regressions on Root's integrated source | 63 passed, zero failed | CPython 3.9.6; the later bootstrap integration has a separate rerun |
| Complete bootstrap/installer regressions on Root's integrated source | 67 passed, zero failed, 39.654 seconds | Includes the one-line launcher handoff, same-release no-download reuse, newer release, corruption and downgrade controls; synthetic transport, not a public release proof |

The client fixture repairs retain the original refusal and single-send assertions. The wrong-basis control now changes captured row content and proves that the revision differs. An additional control proves that unchanged canonical rows can share a revision while their producer certificates differ. Socket tests retain authenticated owner admission and deadlines while using a unique owner-only short directory. Their composed full-client native rerun passed; the MCP and runtime suites remain separate gates.

The closed HTTPie session returned 244,050 bytes and 77,132 measured cl100k tokens across 60 responses. Every recorded response fits its 49,152-byte and 12,000-token ceilings. Selected documentation, source, references, plain graph, limited graph and batch calls completed in under 0.3 seconds each; fleet admission latency is separate. The original first-search Preparing failures remain failures. A singleton graph without a cursor does not establish graph pagination or cold cursor replay. Seventeen invalid-argument refusals do not prove all seventeen positive tool capabilities. Semantic inference was not configured.

## Current fresh application work

The newer Linux package at `6e9cc85def` was assembled from the matched three binaries with its Node and TypeScript SDK. It precedes the newer composed `b3f0d72131` source and is a diagnostic, unoptimized Ubuntu 24/glibc 2.39 candidate. It does not prove Ubuntu 22 portability or current macOS installation.

Its fresh HTTPie installation passed in 5.436 seconds under a new UID-1001 home with no XDG overrides. `nudox add .` returned an active operation in 1.061 seconds. The real operation published all 133 Python sources with complete coverage, key `31ffd7cafed7217fea1f594bcfa20b3c8aee9ed5560f2fd52de4c86debfec914`, and view `117319afae459c769c87418ce2c196948f6e6adf9b75a7947eb306d868590148`. Publication was observed 42.898 seconds after add started, including admission waits; that is an observation bracket, not an exact indexing duration. The outer first-flow attempt still exited 1 because the disk guard refused its next status command before launch. Its owner was drained and the permit released.

A separate cold MCP session on that same published index returned Ready on its first search in 4.713 seconds. Root independently verified all 47 replies, IDs and response budgets: 246,559 bytes and 86,510 observer-measured cl100k tokens; the largest reply was 31,809 bytes/10,563 tokens. Source, references and batch reads returned actual selected operands. The real `setup_method` caller graph traversed five pages; every full row matched the plain graph union with no duplicates. Search traversed four pages and resolve three pages with their actual cursor chains.

One original source oracle check remains false: it compared the physical first line's four indentation bytes with a declaration-node excerpt that begins at `def`. The retained observer classifies the returned two-line declaration as the exact node slice; the public source result exposes a line but no starting column. Root preserves the original failed assertion and does not relabel the whole run green. After 47 replies, fleet admission refused the next malformed indexing request before mutation. Remaining mutation/refusal tests, CLI selected commands and cold issued-cursor replay are unrun. Semantic inference remains explicitly unconfigured.

Excalidraw product indexing, Go application first use and a larger Mealie replay are separate supervised jobs. Their preparation or observer controls do not establish successful product indexing.

Excalidraw's ordinary Node 24 dependency install, TypeScript check, and build passed at its pinned source. All 1,310 captured files were unchanged after setup. The previous Node 18 engine refusal is retained. The archive build emits caught Git-metadata diagnostics because it has no `.git`; no upstream Git revision embedding is claimed. This establishes normal application preparation, not Nudox indexing.

Fresh product observers now omit XDG overrides so the product chooses platform-default locations. Observer ownership parsing and cleanup must bind the actual startup mode, exact executable, UID, home and root filesystem. A setup pass, empty process group, or recorded fixture capture cannot replace actual publication and cold-restart evidence.

## Open release gates

The asynchronous semantic-worker proposal remains withheld pending Root review and native validation. Its successor checks selected lifetime and the original deadline inside preflight, persistent relation construction, lexical scanning, vector scoring and composition. A separate consuming lexical seed removes the worker's duplicate initial scan. Root/Luna found a further inherited allocation bug: inserting into a full retained prefix before popping its worst row can grow capacity before accounting. A separate correction and shared composition core are in preparation. The initial scan still occupies the serving mutex; HTTP disconnect propagation and hard wall-clock responsiveness are unproved. The proposal also requires a coherent wire-version cutover and genuine model/helper installation.

The composed client gate is green. The current source still needs runtime/MCP/GUI native gates, real HTTP disconnect and stdio cancellation, removal and re-add, offline/cold-restart lifecycle coverage, larger application tail handling, and current macOS installation. The real HTTPie paged graph union is now verified; that does not establish every ecosystem's graph quality. Warning counts remain substantial. No hundreds-of-projects, instant-all-tools, current investor bundle, or production-readiness claim is justified.

ILO is the reachable native execution host. The offline remote Mac is excluded under the user's explicit authorization. Fresh destination memory, disk and fleet checks remain mandatory; the canonical checkout's preexisting Nix edit is preserved.

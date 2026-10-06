# Historical regression audit: compiler, source, publication, and search

Audit date: 2026-10-06. Author: SOL. Scope: TypeScript, Python, Go, a small Rust
sample, and the shared compiler/IR, ingestion, durable search, CLI/MCP, and
workspace publication boundaries. GUI redesign and other language frontends are
outside this audit.

The canonical checkout is **`6ac9f5438b375f3dfc8985cc867046dbcf993abd`**.
The comparison integration checkpoint is
**`983fa8cd5c6ca30d2777a256eb214f5991c466cb`**, referred to below as **983**.
The observed remote canonical checkpoint was `6c98128cc5c09be49e011482495e3a845a94f659`.
These are pinned comparisons, not claims about the newest moving branch.
During the audit the integration branch advanced to `109335c5d31bb43be91a5774bf916440c60e9909`;
its unfinished runtime work is not silently counted as part of 983.

The largest practical finding is that three different conditions have been
mixed together: a capability actually removed in a migration, a current bug in
a retained capability, and a repair completed on a branch but absent from the
user's canonical binary. The repair sequence should preserve that distinction.
Canonical already contains the October 5 durable-cache control-entry and graph
identity repair. It still lacks several later TypeScript host, diagnostic, and
complete-source-facts changes present at 983. Both checkpoints retain concrete
risks in cache recovery and project TypeScript semantics.

## Evidence and coverage

The full reachable canonical graph contains **5,278 commits**, rather than the
2,720 commits on its first-parent chain. First-parent starts at an August 3
snapshot (`e3ca561b9f`); stopping there misses the January–July compiler history.
The accompanying `sol-history-commit-map-20261006.tsv` enumerates the full
reachable graph, parents, dates, first-parent membership, and subjects. This
provides an exhaustive history map. Deep source comparison followed the
capability paths below, including deleted code, actual old implementations,
replacement call sites, and selected introducing diffs. It does **not** claim
that every commit was built or every historical advertised feature was a
working product.

`sol-history-evidence-commits-20261006.tsv` resolves cited abbreviations to full
IDs and records ancestry separately for canonical and 983. Use that table to
distinguish equivalent sibling commits from integrated work.

| Author month | Reachable canonical commits | First-parent commits |
| --- | ---: | ---: |
| 2026-01 | 38 | 0 |
| 2026-02 | 57 | 0 |
| 2026-03 | 61 | 0 |
| 2026-04 | 64 | 0 |
| 2026-05 | 77 | 0 |
| 2026-06 | 118 | 0 |
| 2026-07 | 774 | 0 |
| 2026-08 | 551 | 392 |
| 2026-09 | 2,269 | 1,339 |
| 2026-10 | 1,269 | 989 |

Dates use the historical author date and may differ from integration order.
Large checkpoint commits, copied workspace trees, and cherry-picked equivalents
make path deletion and commit subject unreliable evidence on their own.
Historical citations below use `commit:path`; obtain the exact source with
`git show <commit>:<path>`. Current line numbers refer to canonical unless
explicitly marked 983. They are anchors, not claims that another checkpoint has
the same line numbers.

Evidence levels:

* **Observed product failure**: a concrete user/process report or runtime result
  exists. This audit does not convert a source acquisition or helper test into
  a product pass.
* **Confirmed code path**: the reachable code demonstrates the defect or missing
  behavior; the proposed product repro still needs execution.
* **Capability gap**: the old implementation exists, but a complete replacement
  and current product contract have not been established.
* **Fixed at checkpoint**: source repair is present. End-to-end, fresh-process,
  refresh, and restart evidence remain separate gates.

## Regression and checkpoint matrix

P1 means a core user operation can fail or return materially wrong semantic
facts under the stated trigger. P2 means a narrower portability, diagnosis,
selection, or retained-capability gap. The matrix deliberately does not label
every historical cutover a regression.

| ID | Severity / evidence | Concrete trigger and effect | Introduction / removal | Canonical 6ac9 | Integration 983 | Repair boundary |
| --- | --- | --- | --- | --- | --- | --- |
| S1 | P1, observed product failure | A durable search namespace contains its own `durable-cache.lock`; strict pruning rejects it and search fails after successful indexing. | Strict roots `b922f74637`; namespace fences `217efa7091` make the conflict reachable. | **Fixed** by `736eccd304`. | Fixed. | Verify the held namespace fence's exact control-file identity; retain strict validation for unrelated entries. |
| S2 | P1, confirmed code path; crash repro pending | An inactive 64-hex root has a zero/truncated `.last-used`, or a malformed cached file. Opening a valid selected root still traverses the inactive root and propagates its error. Cache maintenance poisons unrelated valid searches. | Strict retained-root validation `b922f74637`; strict stamp/error classification `d742d15f26`. | **Present.** | **Present.** | Atomic advisory stamp publication; classify and quarantine/remove inactive corrupt roots under their lease. Never discard a leased reader's root. |
| S3 | P2, confirmed code path | Durable Tantivy open/query error reaches `map_err(|_| QueryError::LexicalProvider)`; the client sees a generic provider failure instead of the actual IO/corruption cause. | V2 topology `e812201fd7`; durable route `0f3bd291e1` adds the failing-open conversion. | **Present.** | **Fixed in source** by `4cbce8ad98`, with later contract preservation `60c57e14a7`. | Typed operation phase plus complete cause chain across query, local service, CLI, and MCP. |
| T1 | P1, confirmed code path | Move the application away from the build checkout; TypeScript Node driver path still names the developer's `CARGO_MANIFEST_DIR`. The checker cannot start. | Explicit checker executable `b62e7fb002`; earlier Deno runner had the same pattern in `994d75a833`. | **Present.** | **Fixed in source** by `60efe7f258`, embedded bytes and private materialization. | Owned executable/module/driver admission, with no runtime dependency on a compiler checkout. |
| T2 | P1, confirmed code path; project repro pending | A project uses `tsconfig` paths, strict null checks, package conditions, NodeNext, or referenced projects. Driver creates a one-file program with hardcoded options and never reads the config. Resolutions/inferred types can disagree with the project. | New external driver at `2f82d74b49` (sibling equivalent `6e46f27c76`); first-class checker `88bd8e4b44` makes it production authority. | **Present.** | **Present in external driver.** New host config witnesses do not change `createProgram`. | One admitted project authority program with effective options, exact positive/negative resolutions, roots, and shared checker state. TSZ branch is a recovery candidate, not an ancestor of 983. |
| T3 | P2, confirmed platform-specific code path | On non-Unix platforms `link_dependency_directory` returns success while doing nothing. A staged package needing `node_modules` cannot resolve those dependencies as Unix does. | `53888897b4` introduces both Unix symlink and non-Unix no-op, confirmed by line blame. | **Present.** | **Present.** | Explicit platform dependency admission: junction, bounded copy, or typed unsupported result; success must establish the directory. |
| T4 | P1/P2, canonical observation plus checkpoint comparison | A normal project-local TypeScript installation or a `tsc` script with `#!/usr/bin/env node` is selected under an emptied environment. Node/module discovery can fail although the project toolchain works in its shell. | Closed native environment `281ec66868`, combined with older shell-script selection. | **Present in old host selection.** | **Source repairs present**, notably `e2997ad011`, `ebe8aae5d2`, `3a1a2740f6`, `4f0fdba059`. | Admit exact interpreter and module separately; witness local manifests and resolution paths; bound all discovery. Product shipping proof still required. |
| G1 | P1, confirmed toolchain-mode-only path | A shipped binary selects `GoToolchain`, with no configured oracle binary, after the build checkout disappears. It runs `go run .` in a build-time source directory. | Configured mode `b62e7fb002`; path spelling moved by `4298579e04`; older legacy invocation has the same pattern. | **Present.** | **Present.** | Ship an admitted compiled oracle, or embed/materialize its owned source and lock its toolchain/closure. Explicit `OracleBinary` mode avoids this particular defect. |
| I1 | P1/P2, confirmed producer behavior | A declaration-rich admitted source does not fit one product relation row. Excerpts, prose/signatures, then trailing declarations are discarded. Some structural source facts are unreachable even though syntax extraction found them. | Fixed one-row design in V2 seed `e812201fd7`; capacity-shedding ladder `e5d5d3e0e6` makes refusal survivable. | **Present**, with honest `DeclarationRetention`, not hidden complete coverage. | **Paged complete-facts source repair present**; registry omission caused `StoreCorrupt`, corrected by `0ee002b15d` and helper-path follow-up `983fa8cd5c`. | Complete facts as a paged authoritative relation; compact browse row as a view. Charge full facts on cold and warm paths. |
| I2 | P2, confirmed policy | A supported source exceeds 512 KiB, or aggregate selected source exceeds 64 MiB. Per-file refusal creates a typed unavailable row; aggregate bound can terminate ingestion. | V2 local ingestion `e812201fd7`. | Fixed constants. | Configurable source-admission work exists, with typed budgets and capture state. | Product-visible admitted policy, precise observed/permitted values, capture witness, and streaming handles. Raising constants alone is insufficient. |
| D1 | P2, selection-contract gap | Real source lies in a directory named `build`, `bin`, `vendor`, or another default generated directory. Default product scan omits it unless a policy reopens it. | Unified ignore-aware selection `c1a150f166`; explicit include reopening added `0068ab3f1d`. | Default product scanner uses default policy. | Same boundary unless a product route exposes selection. | Reuse the existing include-override API and persist the selected policy. Do not delete ignore support. |
| C1 | P1, observed graph failure in Oct 5 report | Human coordinate text is hashed as if it were the semantic symbol row's canonical address. The real row hashes package/semantic identity; paged graph looks up the wrong row. | Coordinate/projection split around `d52033b1b`, `d042ae5780`; label lookup `21184ead26` did not align every consumer. | **Fixed** by `736eccd304`. | Fixed. | Resolve once to canonical address, then retain that identity through graph and continuation paths. |
| C2 | P2, confirmed contract | MCP paged `graph` failure goes through `RpcError::tool` and becomes generic transport/internal error; neighboring graph-query route returns typed refusal. | Bound CLI/MCP projections `d10dd4d83a`. | **Present.** | Typed `Fault::from_client_error` cause preservation fixed by `11f4642773` / `5478ae398f`; ordinary tool-result/RPC refusal consistency still needs product comparison. | Preserve typed failure classification and unify each endpoint's documented protocol contract. |
| M1 | P1, confirmed actual Cargo/public shape failure on Oct 6 | TS/Python/Go and small Rust producers emit member declarations and containment but never prove the capture flag; owned image drops the list and public shape reader calls it Available(empty). Real `MorningSignal { pulse: u32 }` returned zero members. | Default flag `49fd2011ab`; exact owned-list loss `135bc43976`; false-empty public shape contract `76c7f7e3ea8`. | **Present.** | **Present in 983.** Later isolated member repair `43a1` + `77b` + `821d` passes five producer/common/native tests and the reopened-image reader gate. Actual Cargo warm member assertions and initial cold shape equality pass after Root selector-refusal classification fix. Same-add/refused-refresh/cold failed replay remain under test. | Explicit producer-complete direct-declaration inventories, independent of inherited/effective/runtime structure; reader authority gating and typed unavailable; preserve existing proven consumers. |
| M2 | P1, confirmed actual equal-generation re-add failure on Oct 6 | An admitted recompile selects the same publication and emits no delta. Capture completion wrongly records Failed(ProjectAuthority), while the operation reports Published. Repeats after view/journal repairs. | Delta suppression is intentional (`d042ae57807`, `4faee5661c4`); incompatible capture interpretation introduced by `f0a563dc7e5`. | Capture helper absent. | **Present in 983.** Narrow typed completion repair `f0cf` + `f2f8` passes exact source/admission/real terminal persistence controls. After explicit test-only Session reconnect, real equal-readd/capture/shape checks pass. Full lifecycle reaches M3 freshness failure before refused cold replay. | Carry admitted key/claim/coverage independently of row deltas; pair exact retained after-image with terminal capture in one intent; preserve source/store authority and reject missing or conflicting proof. |
| M3 | P1, confirmed actual config-only refused refresh on Oct 6 | Unproven scan omits bounded config contents from semantic_input_digest; separately, successful selected-generation observations masquerade as factual latest input after Pending/refused capture. A changed Cargo.toml refuses correctly while public freshness remains Current. | Config omission/digest `4faee5661c4`, async `7c8d1c57f0` (sibling `1f02a5d17d`). Selected-only fence `cc1251519f8`; incompatible separate capture cutover `44fdc36abc1` (sibling `3402cc7de1`) gathers but does not project capture freshness. | Config omission in common scan. | Present in 983. `8b144` passes six controls and changes the actual refused digest; the preserved second runtime FAIL establishes the missing projection. Shared exact-snapshot `a209` plus refused-view `e4a4808` PASS full real public prior-generation lifecycle, including original Historical assertion, retained shapes and refused cold reopen/replay; `adf` PASS first real refusal/cold absent authority. Exact checkpoint `c4f743`; separate typed-status slice not included. | Retain bounded config facts independently of reuse authority; compare retained generation input against admitted capture input from the same query snapshot. Missing proof stays Unverified and capture cannot manufacture a selected generation. |
| H1 | P2, capability gap | Need IR-native branch/tag replay, unrecord, symbol lineage across rename/resurrection, or advisory semver surface classification. Current snapshot diff and durable history do not establish equivalent Pijul channel semantics. | Old repository `c91c654aca`, `1ec859f8ae`, `a1c18b21d0`; removed by four-boundary migration `628a6698ca` (equivalent sibling `335e3732a`). | Borrowed snapshot diff and typed durable history survive. | Same foundational distinction. | Extend current canonical IR/history; salvage algorithms and tests selectively, not old blob/archive authority. |
| P1 | P1, observed integration failure, exact newer fix pending integration review | Recovered complete source-facts relation was persisted but omitted from serving registry, causing fresh workspace `StoreCorrupt`; later prepared operation identity work exposed another failure before publication. | New facts relation `c4fd275d8b` / `f0a563dc7e`; registry fix `0ee002b15d`, path correction `983fa8cd5c`; prepared identity fix `1121d895d7` on ingest lane. | Does not contain new facts relation. | Registry omission fixed; prepared-identity fix is **outside 983**. | Final immutable intent identity must be identical at prepare, submit, journal, replay, and selected publication. Only real product publication closes the gate. |

`T2` is a demonstrated current semantic limitation, not proof that the old OXC
resolver honored arbitrary `tsconfig` either. `H1` is a lost implementation
surface whose current user requirements must be specified; it is not a blanket
claim that all current versioned history is absent. `S2` remains distinct from
the already-fixed lock-file incident.

## Epoch 1: January–March, the original compiler and IR

The first commit, `4ddb4771a9`, records compiler and JSON-LD submodule gitlinks.
`880cd0077e` adds the IR submodule/workspace. Their source history is not fully
contained in those superproject trees. This audit does not invent analysis of
the original submodule contents. A TerminusDB schema experiment appears at
`830dba968d` / `754fdf78bf`, is deleted at `e4d55551dc`, and another submodule
arrives at `45e81dfd96`. Those experiments do not establish a durable compiler
product that should be revived wholesale.

`16073f5f1c` materializes the monorepo compiler and IR. The original
`compiler/src/core/rust.rs` has a real rustdoc JSON parser but an incomplete
package layer. `generate_ir(version)` ignores its version argument, invokes
Cargo from ambient current directory, and unwraps `parse_crate`. Available
versions returns `NotImplemented`; dependencies and dependents are `todo!`;
features are a mock `default/std/alloc` list. This is a useful baseline for
parser vocabulary, not evidence that arbitrary registry/version compilation
was complete in February.

The IR already models entries, callable parameters, generics, protocols,
records, types, and primitives. March evolves it through typed IR
`e69a21ede5`, versioned pipeline stages `acbca4b44f`, commit-resolution work
`25fb84dce9`, complete revision walking `8a393a356a`, and the fuller package
pipeline `f916d172bc`. `545bb1ffd0` repairs Cargo's current directory. In the
actual `f916d172bc:compiler/src/core/rust.rs`, rustdoc runs under the checked-out
package and reads its `target/doc` JSON. Version lookup then checks out the
resolved commit before parsing. This is an actual repair to input binding,
although the old package API still carries incomplete registry operations.
`bcee44c456` separates library and CLI.

The recurring lesson starts here: a type named `Versioned` or a version argument
is not evidence that compilation used that version's bytes. Modern input
frontiers, exact source binding, and selected publication proofs are stronger
than this prototype and should be preserved.

## Epoch 2: April–June, TypeScript documentation and first persistent search

`fd52d9f4ec` introduces TypeScript through Deno documentation; `756c3786e4`
temporarily ignores that lane while the IR changes at `07b4e56040`.
`6922d9226f` and `e4f7702f3f` migrate it. The large parser change `334cae3161`
adds overload groups, namespace merging, constructor/method groups, conditional
and mapped types, type operators, and predicates. Its important behavior is
also error handling: individual parse errors stop being silently swallowed.
Those node forms are valuable historical parity fixtures, rather than proof
the Deno producer should become today's compiler authority.

`346a2c0d1a` adds the TypeScript registry; `7a28943ada` uses gitoxide;
`8e131123e2` persists registry/cache state; `cc0ab47d61` stabilizes repository
paths; `255b2f1d40` indexes graph relationships. Old package/repository entry
fallbacks improve at `994d75a833` and `712e27a99c`.

The implementation at `994d75a833:compiler/src/core/ts.rs` is worth reading
because it is more than a package-log entry. It computes documentation roots,
builds a Deno `TypesOnly` module graph, and requests private documentation.
Local in-process `deno_doc` is preferred; remote/npm/jsr paths use a Deno CLI
fallback. The external runner already uses
`concat!(env!("CARGO_MANIFEST_DIR"), "/src/core/ts_doc_runner.ts")`. Build-machine
path dependence is therefore an old recurring defect, rather than an invention
of the latest migration. The Deno fallback also requires read/net/env access,
unlike the newer closed native input boundary.

Persistent full-text search begins at `2eb82ec045`. Actual
`compiler/src/text_index.rs` uses on-disk `MmapDirectory::open_or_create`, URI
upsert, explicit writer commit, a fresh reader for search, and Tantivy's query
parser over name/text/package fields. Errors retain a contextual source string.
`645cfc322f` adds a symbol-search HTTP endpoint; `a410f9a07f` introduces writer
actor/scoring work. These are real useful behaviors. They do **not** validate
current canonical row membership, stable ordinal coverage, exact source
postings, bounded setup, or multi-reader immutable roots.

`0ec8560374` and `d762d003ee` remove the old compiler/backend topology in favor
of workspace compiler implementations. Literal source deletion here is a
cutover. A regression claim must follow a capability into the replacement and
show that its behavior was lost.

## Epoch 3: July–August, OXC/TSZ, Python enrichment, and IR-native VCS

### TypeScript source graph and checker enrichment

`e80892bc00` vendors OXC. `e6379c3948`, `b474bb3537`, and `afc055fa39` evolve
TSZ integration; `ebd9c14c08` fixes declaration entry extensions;
`b3b457cba9` queries the TSZ checker directly; `4a19390b78` restores the TSZ
tier in a later producer. The last old tree before `628a6698ca` has actual
implementations in `workspace/compiler/languages/src/typescript/{entry,graph,producer}.rs`
and `oracle/tsz.rs`, not merely design notes.

The entry resolver prefers `.d.ts`, uses `types/import/node` conditions and
`types/typings/module/main`, maps JS suffixes to declaration/source variants,
and respects package exports. For packages without exports it deliberately
discovers deep imports, including declaration-rich CommonJS layouts. It also
falls back through conventional `mod`, `index`, and `src/index` roots. Those
algorithms can recover useful npm package coverage. Some resolution/read errors
are silently ignored, so that policy cannot become strict current authority
without typed outcomes.

The graph walker is breadth-first, deduplicates paths, follows static ESM and
literal `require` calls, and excludes Node builtins. It creates a separate OXC
allocator per module. Missing sources and caught OXC panics are skipped with a
warning. This is a useful graph-discovery reference, but a warning is not an
admitted negative resolution witness.

The old TSZ oracle is feature-gated and defaults to use when compiled in, unless
`NUDOX_TYPESCRIPT_ORACLE` disables it. The producer fills inferred holes from the
checker. `TypeData::Error` becomes `Any`; readonly/NoInfer wrappers are
transparent; some complex types format and reparse; any oracle construction
error falls back to OXC with a debug log. That last behavior means a successful
old package result was not proof of checker-backed semantic coverage. Restore
shared checker state and supported type mappings, not silent fallback or
Error-to-Any laundering.

### Python syntax plus monotone inference

The old `workspace/compiler/languages/src/python/{mod,context}.rs` combines Ruff
syntax with an in-process Pyrefly context. Written annotations win; inference
fills holes. Documentation, decorators, overloads, and syntax-authored shape
remain with Ruff. The old module's corpus measurements say 4,607 of 16,025
entries gained an inferred return/parameter type and 1,528 of 2,975 unannotated
entries gained information. Those are historical comments, not measurements
rerun in this audit.

`37de8eaac4` contains the August default Pyrefly checkpoint;
`a0228fbb6b` adds semantic Python references; `e1d2368357` adds TypeScript
resolved references; `c3e2b29137` extends foreign-reference metadata. The old
Python implementation also logs an error and returns syntax when inference is
empty, without an in-band coverage marker. That is an honesty defect to avoid.

The current Python lane still has Ruff-derived authored structure and an
explicit Pyrefly check/report boundary (`frontends/python/src/legacy/checker.rs`),
bounded child execution, exact source binding, and stronger occurrence/receiver
identity. It is not established by this audit that all old in-process project
context survives the external per-source report. Project configuration/import
closure and representative inferred type parity need a real package comparison.
This is a **parity gate**, not a fabricated proven regression for every Python
package.

### IR-native versioning was a real implementation

`c91c654aca` introduces a libpijul-backed IR repository; `95f5370344` changes
record/output toward delta work; `1ec859f8ae` adds historical replay;
`a1c18b21d0` adds branches/tags/versions; `7966f1cf99` adds demand checkpoints;
`771583f538` adds continuity/F1/substitution work; `bd65ee0c01` vendors a
field-aware libpijul fork. Body-plane evolution continues through
`b369d323a9`, `b95e983a32`, and `24b2c189d8`. `451180e5cf` deletes the old V1
blob grammar before the later whole repository cutover; `59c06a1711` moves
remaining consumers to newer IR.

At `5ef23738df:workspace/nudox-ir-vcs/lib.rs`, the repository API exposes
generation recording, per-IntroId symbol files, current-tip materialization,
unrecord, channel refs, checkpointing, replay/sealing, continuity, and serving.
Immediately before removal, `workspace/compiler/ir/vcs/repo/mod.rs` is still a
real libpijul implementation, using filesystem changes, Sanakirja pristine,
symbol working-copy paths, branch/tag/version refs, and archive serving cache.
The migration at `628a6698ca` deletes it; `335e3732a` is an equivalent migration
on another ancestry, as already distinguished by `compiler-ir-lineage.md`.

Current `crates/semantic/src/ir/vcs.rs` performs borrowed canonical entity/link
snapshot comparisons: generation identity, stable declaration families and
variant fingerprints, shape/docs/source/provenance/language facets, and links.
`crates/local-service/src/builtin/commands/snapshot.rs` activates complete
semantic publications, reopens images, and uses actual row identities to build
diffs. The current V2 store also owns durable commit/ref/closure history. These
are stronger foundations than resurrecting an old archive as a second truth.
They do not by themselves provide Pijul's arbitrary channel merge/unrecord or
persisted rename/resurrection continuity.

Old semver code is narrower than its title suggests. In
`628a6698ca^:workspace/compiler/ir/vcs/semver/classify.rs`, only Pack A runs;
Packs B/C/D are deferred, dependency provider is unused, closure status is
trivially complete, and configuration hashes proxy for state fingerprints.
Uncertain findings do not raise the required bump. Salvage pure surface/lint
algorithms as advisory analysis after binding real current generation identities.
Do not represent this as a removed complete cross-language semver product.

The scan-resistant `ServeCache` and incremental per-symbol materialization are
benchmark candidates for historical workloads. They must not replace current
admitted segment residency without measurements or revive the deleted NdIrSym
blob authority.

## Epoch 4: late August–September, boundaries and topology cutovers

`23530d79be` is another large checkpoint/move. `628a6698ca` collapses the
four-boundary workspace and removes the old producer/VCS implementations.
`f3dc01e76d` makes OXC syntax authority explicit; its own source says it is not
checker-derived and expects a later TSZ adapter. `51312dcf1b` recovers typed
frontends. Go authority image V4 arrives at `815b957793`; V5 is present in the
September 3 Go commits. Python uses an explicit external Pyrefly lane at
`2c0b26f864`. TypeScript first-class checker work is at `88bd8e4b44`;
`870afdf390` stages packages; `d75e293645`, `12e08b0618`, `20ff419932`, and
`a19e626288` fill computed type trees, mapped modifiers/remaps, typed causes,
and coordinate binding. `b62e7fb002` adds explicit executable selection and
`da162b0954` explicit module authority.

`e812201fd7` seeds/migrates V2 topology, `4298579e04` moves language crates into
`frontends/*/legacy`, and `c37daab79c` folds application/driver/publication into
the engine. The September 19 checkpoint `930aa72f97` named complete cutover
deletes code that later needs restoration, including compiler/application
support; `56e35d8188` restores a complete V2 graph. This is evidence of
topology drift, not evidence that a smaller compiling tree had retained product
journeys.

`docs/operations/complete-cutover.md` states the right historical rule: old
directories are capability oracles and should not disappear merely because V2
compiles. Its registry acquisition, real Tantivy, shipped helpers, and J1–J8
process journeys are useful gates. Several source migrations were tested at
internal layers while source discovery, native launching, durable publication,
or client querying still failed. Compile/check success closes a compile gate.
It does not close the product gate.

Current compiler/input fabric (`4faee5661c`, `281ec66868`) has exact roots,
source/configuration revision fences, closed native environments, canonical
semantic images, selected-head authority, hydration, CAS, and closure proof.
These should be extended rather than replaced. The current producer still
builds a full canonical image before cutting fixed-size byte chunks, as the
existing lineage audit explains. An early source edit can move later chunks;
stable family-based physical output and retained incremental analyzer state are
still new work. Fixture segment locality does not establish production locality.

### Go scope and portability

`9dc3d3a942` repairs a concrete scope error: the production Go authority must
serve the owning package, not expose the whole module as that package's facts.
Source paths are bound at `1a101e7d74`. Late September batches Go authority
execution (`66bba166e3`) and Python Pyrefly (`5e4c857490`). Their historical
performance subjects are not rerun numbers here.

The current host in `crates/engine/src/application/host/authority.rs:126–155`
selects `GoToolchain` when Go and its cache exist and no oracle override exists.
`frontends/go/src/legacy/oracle.rs:1210–1233` then constructs `go run .` with
`current_dir(env!("CARGO_MANIFEST_DIR")/src/legacy/oracle)`. This is the exact
reachable shipped portability problem in G1. The configured binary mode uses
the admitted executable and avoids this directory dependency. Current producer
proof hardening (`93f5447e3b` / equivalent `737b6d3264`, later `823e61c983`)
must be assessed separately: closure proof can make image authority honest but
does not materialize missing helper source.

### Source discovery and admission

`c1a150f166` unifies ignore-aware source selection, `0068ab3f1d` lets explicit
include rules reopen ignored paths, and `8a2f733802` excludes the workspace's
own owner lock. `crates/discovery/src/lib.rs` provides real include-override
semantics. Default generated names include `.git`, `target`, `bin`, `obj`,
`out`, `node_modules`, `dist`, `build`, `.venv`, `venv`, and `vendor` among
others. Those defaults can hide legitimate selected sources, but they are also
necessary to avoid recursively ingesting generated output and dependencies.
The gap is the product's explicit selection contract and persisted policy, not
an absence of include support in the discovery library.

Current extension classification covers TS/JS, `.mts`, `.cts`, `.mjs`, `.cjs`,
and JSX/TSX. This audit did not find a current missing-extension regression.
Unix symlink rejection in source admission is intentional exact-input policy;
do not silently follow aliases and call it recovery. Resolve admissible roots
or report the refusal with its source witness.

The canonical local ingest constants are at
`crates/local-service/src/builtin/ingest.rs:28–45`: 512 KiB per source, 64 MiB
aggregate selected source and encoded records, 100,000 directory entries,
500,000 discovery entries, bounded compiler configuration, and eight workers.
An oversized file is represented as unavailable and omitted from compiler
source, not passed to the compiler as an empty successful file. That honesty
must remain. A project limit and a one-row representation limit are different
resources and should not be conflated.

At 983, `crates/local-service/src/builtin/source_budget.rs` admits a validated,
identity-bearing policy with 8 MiB per file, 512 MiB aggregate project source,
256 MiB retained compiler source, 64 MiB in-flight source, 128 MiB encoded
records, and 500,000 records by default. Operator overrides are bounded and
invalid values refuse before scanning; the warm-cache key includes policy
identity. Those source-byte quotas do not bound decoded frontend/parser heap
expansion. Full fact pages must still be charged independently and consistently
on cold and warm paths.

`ProductSourceRecord::file_within_row_capacity`,
`crates/engine/src/builtin/relation.rs:1051–1105`, is deterministic but lossy.
It sheds excerpts, then prose/signatures, then trailing declarations, and
records retention. Its real `memchr 2.8.3` example is a 65,686-byte row; the
canonical node cannot fit it. This is a storage representation failure, not a
reason that the source file cannot have those declarations. Paged complete
facts should carry the full authoritative result while small product views
select what to show.

## Epoch 5: durable Tantivy and the October failures

The modern real Tantivy adapter begins at `2456420a7c`, followed by strict
manifest/source membership work. `1f907dab0b` retains postings across view
changes. `0f3bd291e1` makes roots durable. The September 30 sequence matters:

| Commit | Actual boundary added |
| --- | --- |
| `b922f74637` | Strict retention, root enumeration, pins, and file/name validation. |
| `9032c31801` | Persisted ordinal identity maps. |
| `d742d15f26` | Classified open errors and strict retained-root handling. |
| `dd7591ea8c` | Typed durable cache byte budget. |
| `9fbfccf2ac` | Active reader pins outside ordinary LRU slots. |
| `fc9089c7cd` | Immutable durable roots rather than in-place mutation. |
| `a0039b43f8` | Exact source binding. |
| `975c91551a` | Changed-segment/posting binding. |
| `b2335daf48` | Exact cold posting coverage verification. |
| `e2c49256ca` | Bounded cold work. |
| `ca0635f0cd` | Atomic/retry publication. |
| `217efa7091` | Namespace fences, staged builder leases, and cleanup. |
| `736eccd304` | Verify/skip only the held namespace control entry; graph identity repair. |

The October 5 attachment describes successful indexing of a TypeScript project
with 6,826 rows, working status/packages/outline/resolve/document/source/references,
and failing search/graph. That is strong evidence that indexing completion and
source browse did not establish working lexical and graph queries. The search
failure names `durable-cache.lock`; it is the S1 strict-pruner/fence conflict.
The graph failure concerns real semantic row identity. The known lock failure
must not be misattributed to `ca0635f0cd`, nor reported unfixed in 6ac9.

Current durable roots are derived from the admitted search fingerprint under
the workspace's `search-index-v2` owner. The implementation uses bounded
namespace locking, unique leased build stages, exact metadata/ordinal/posting
checks, immutable publication, root leases, and retention. Selected-root open
classifies definitive corruption separately from IO and budget failure. These
are valuable safety guarantees absent from the June implementation.

The new recovery problem is inside maintenance, not a reason to throw those
guarantees away. At `extensions/tantivy/src/engine.rs:3343–3353`,
`touch_durable_root` opens/truncates `.last-used`, writes 16 bytes, and fsyncs.
A crash can leave a present zero-length stamp. At 3436–3438,
`prune_durable_roots_entries` calculates **every** root's validated size and
last-used before deciding whether that root is selected or pinned. At
3525–3548, `durable_root_last_used` uses a fallback only when the file is
missing; a present non-16-byte file is fatal `InvalidData`. At 3571 onward,
`durable_root_size` rejects unexpected files/directories/symlinks, also before
inactive-root eviction. One corrupt, unused, application-owned cache entry can
therefore prevent reuse of a valid root indefinitely.

The selected-root classifier (`is_definitively_corrupt_root`, around 3151)
allows rebuild for corruption/staleness/schema/data incompatibility but does
not reinterpret every IO error as disposable corruption. Preserve that rule.
For S2, acquire the inactive root lease, classify corrupt owned data, then
remove or quarantine it without dereferencing symlinks. Publish the recency
stamp atomically or make invalid advisory recency fall back safely. A permission
failure, selected root, or active lease needs a different outcome. Blindly
ignoring every cache error would hide the original cause and violate ownership.

At `crates/local-service/src/query/local.rs:972–974`, canonical durable-open
conversion erases failures to the unit
`QueryError::LexicalProvider`. The 983 repair models phase and a cause chain;
it is essential to use the new typed diagnostic before guessing that all
current user reports are the old lock bug. S2 is confirmed statically and
requires the isolated product crash/restart repro below; this audit does not
claim it caused the latest uncollected report.

## Current TypeScript project authority: witnessed does not mean consumed

The 983 host improvements are substantial. It admits exact Node and TypeScript
installation identities, records positive/negative path observations, captures
manifests/configuration, bounds ancestor walks and file counts/bytes, and
materializes an embedded checker driver. In
`983:frontends/typescript/src/legacy/checker.rs:1462–1487`, Node receives that
materialized driver and the staged source path, with admitted `NODE_PATH`.
This repairs T1's compiler-checkout dependency.

But the driver at
`983:frontends/typescript/src/legacy/checker/main.cjs:51–65` constructs:

```javascript
const host = ts.createCompilerHost({}, true);
const program = ts.createProgram([absolute], {
  noEmit: true,
  target: ts.ScriptTarget.ES2022,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.NodeJs,
  jsx: isTsx ? ts.JsxEmit.Preserve : undefined,
  allowJs: isJavaScript,
  checkJs: isJavaScript,
}, host);
```

There is no `readConfigFile`, `parseJsonConfigFileContent`, effective option
argument, config root list, or project-reference input. Package lowering is
reachable through `Checker::run_package`, at 1339, then this explicit-file
driver. Capturing a `tsconfig` witness in the host cannot make this program use
`strictNullChecks`, `paths`, `baseUrl`, package-condition resolution, or the
selected project reference graph. Some imported files join transitively, but
their inclusion and inferred meaning use the fixed program policy.

The TSZ recovery branch is a better candidate than restoring the July adapter
verbatim. `3cab4b4adc` introduces the native project authority in
`frontends/typescript/tsz_authority.rs`; `e5b1e02868` extends shared full-context
checking in the vendored TSZ core. `788701f23a` translates effective compiler API
options, with exact resolution/session/budget work on that lane. It uses a merged program, shared checker definitions, admitted sources
and dependencies, option fingerprints, and typed module outcomes. It is **not
an ancestor of 983**, and helper fixtures do not prove the deployed product uses
it. Integrate through the current admitted package authority, compare against
the project's own pinned TypeScript, and verify every downstream plane.

TypeScript reference precision has materially improved in current ancestry:
cross-file method calls `84a43fb380`, property reads `cb2ca51019`, enum members
`23d69ed9b9`, bindings `017f3432cc`, and package source paths `221c5a2eee`.
Restore old aggregate context without regressing these exact declaration-site,
foreign module, overload-index, and UTF-16/source-byte binding guarantees.

## Protocol identity and durable publication

Canonical graph identity is repaired at `736eccd304`: a human label is resolved
to the actual canonical address before graph paging. Current library
`graph_page_for_address` retains the admitted address and continuation recipe;
the adapter resolves semantic label paths using the same kind of path as
document lookup. A row hash made from arbitrary coordinate text cannot stand
in for semantic symbol identity.

MCP still illustrates why endpoint tests must include failures. At
`apps/mcp/src/jsonrpc.rs:672–690`, canonical paged graph calls `RpcError::tool`
on client error. That helper at 1371 encodes internal/transport failure with no
typed structured cause. Immediately above, graph query returns a typed
`refused(Fault::from_client_error(...))` tool result. 983 uses the typed fault
mapping for paged graph, preserving details, but its RPC error/tool-result
semantics require comparison to the documented contract. Successful JSON shape
tests cannot establish refusal parity or retained continuation validity.

Current workspace publication is a strong foundation:
`crates/engine/src/workspace/publication.rs` represents prepared/durable/published
states, and `record.rs` binds selected publication to the exact prepared journal
sequence, offset, chain, and record identity. Post-linearization acknowledgement
failure is a typed published state rather than an ordinary retryable failure.
Do not flatten it into success/failure booleans or introduce a second source of
selected-head truth.

The October complete-facts recovery also demonstrates how easily a producer
repair can fail at another boundary. A fresh 805 runtime hit `StoreCorrupt`
because `ProductSourceFileFactsRelation` was missing from the serving registry;
`0ee002b15d` adds it and 983 fixes the helper paths. The ingest lane's `1121d895d7` then binds operation preparation to
the final intent identity. Root is validating the resulting real publication.
This audit records the exact checkpoints and does not count the fixes as a
completed product journey before that result exists.

## Historical salvage priorities

| Priority | Existing foundation to extend | Historical algorithm/fixture worth recovering | What must not return |
| --- | --- | --- | --- |
| 1 | Current search root ownership, leases, metadata/ordinal/posting verification | June search's clear operation-level errors; durable failure scenarios and reader freshness. | Mutable shared durable roots, unbounded query setup, accepting foreign cache entries, erased IO causes. |
| 1 | Current admitted package authority + newer TSZ session lane | OXC deep-entry discovery and literal import/require graph; TSZ shared program/inference; May conditional/mapped/overload cases. | Silent fallback, TSZ `Error` converted to `Any`, developer-checkout helpers, arbitrary ambient inputs. |
| 1 | Paged source-file facts relation and capture witness | Complete old syntax results, annotations/docs/decorators, actual dense-file examples. | Equating one compact row with full facts, dropping declarations to satisfy a node, uncharged warm reuse. |
| 1 | Current workspace prepared/durable/published tickets and selected closure | Process crash/restart/replay journeys at each state transition. | Recomputed request identity after preparation, serving registry drift, success counted before publication. |
| 2 | Ruff + explicit Pyrefly package report | Monotone fill-holes policy with written annotations winning; aggregate package context cases. | Logging empty inference and pretending authority coverage, ambient site-packages, inferred types overwriting written intent. |
| 2 | Current Go package-scoped image and closure proof | Method alias fixtures, module/package distinction, blank/doc-only identity cases. | Whole-module facts exposed as one package; `go run` from a vanished build directory. |
| 2 | Canonical borrowed semantic diff and V2 durable history | Pijul continuity matching, per-symbol delta output, rename/resurrection fixtures, advisory Pack A surface lints. | Second archive authority, old V1 blob format, complete-semver claims from deferred packs. |
| 3 | Current segment residency and strict versioned cursor | Scan-resistant ServeCache admission benchmark, repeated historical version scans. | Assuming an old whole-archive cache is better than current bounded segment residency without measurement. |

## Concrete product repro and acceptance plan

These are runnable acceptance designs, not pass claims. Each should retain
binary hash, checkpoint, admitted source/toolchain/config identity, exact
terminal publication, and error cause. Use isolated workspaces and cache roots.
Never mutate the user's live cache to demonstrate corruption.

1. **S1 fence regression guard:** fresh index of a nonempty TS/Python/Go fixture,
   successful selected publication, lexical search twice, refresh, fresh-process
   reopen. Confirm the namespace's held `durable-cache.lock` is admitted and an
   unrelated foreign control file is still refused. Run against old bad pair
   if a historical binary is available, then repaired checkpoint.
2. **S2 inactive-root crash recovery:** publish/search revision A, publish/search
   revision B, release A's readers, and stop the process. In the isolated cache,
   truncate only A's `.last-used` to zero (matching the truncation crash window).
   Reopen B and search. Repeat with an unrecognized regular file in inactive A,
   with A actively leased, and with a real permission error. Valid B should stay
   available when owned unleased A is disposable; diagnostics must distinguish
   other failures. Confirm no partial root is selected and no leased root is
   deleted. Also interrupt exactly between stamp truncate/write if a fault seam
   is available.
3. **T1/G1 shipped helper portability:** copy the built app/binary and admitted
   toolchains into a scratch install, then make the build checkout unavailable
   to the test process. Index a nonempty package through default Node/Go modes,
   then explicit oracle mode. Success must not read files under the original
   `CARGO_MANIFEST_DIR`. Do not delete a colleague's checkout to run this test.
4. **T2 project options:** use a pinned local TypeScript package and
   `tsconfig.json` with `strictNullChecks: true`, `baseUrl`, `paths` for `@lib/*`,
   and a NodeNext/project-reference case. Export inferred values whose null union
   and alias target differ under fixed defaults. Compare product declaration
   types and reference destinations with the same TypeScript compiler API
   loaded using effective project options. Then change only config and confirm
   invalidation/publication updates both declarations and references.
5. **T3 staged dependencies:** on Windows, index a package with a selected
   dependency declared under `node_modules` and verify the staged checker sees
   it. Include a bounded refusal case. Returning success from the link helper
   without creating any dependency path is a failure regardless of syntax rows.
6. **I1/I2 complete source facts:** exercise source sizes just below/above the
   admitted per-file boundary, many small files around aggregate charge, and a
   dense TSX or the cited `memchr` source whose complete facts exceed one row.
   Count compiler-extracted versus persisted complete declaration/site facts,
   read the last declaration through source/references, reopen fresh, then warm
   refresh unchanged input. Full facts and all charges must survive; compact
   excerpts may be a presentation policy, not authority loss.
7. **D1 selection:** index a project with legitimate `vendor`/`build` source,
   first using default policy then an explicit include override. Verify policy
   identity, expected omitted/admitted paths, negative witnesses, and stable
   refresh. Include generated output to avoid accidentally making all such
   directories unconditional source.
8. **C1/C2 identity and refusal:** resolve a real semantic declaration, document
   it, traverse graph, page with continuation, then mutate selected head and
   restart. Use the resolved canonical address. Repeat missing operand and
   lexical/provider failure through CLI and MCP; compare typed kind/cause and
   protocol envelope. A helper direct-row query is insufficient.
9. **Python package parity:** select explicit annotations, unannotated calls
   across modules, overloads/decorators, and receiver field uses. Compare old
   monotone enrichment's meaningful cases to current package authority under
   admitted project config. Capture unavailable inference separately; do not
   infer checker coverage from successful Ruff extraction.
10. **Go authority:** a module containing two packages with overlapping names,
    blank imports/identifiers and doc-only files. Product source/references must
    identify the exact package/source declaration; a proof for the whole module
    must not justify leaking unrelated package declarations.
11. **H1 history continuity:** publish symbol, change docs/body/shape separately,
    rename, delete, resurrect, and compare selected generations after restart.
    First establish current snapshot-diff/closure correctness. Then specify
    which old continuity/channel operations are actually required and add them
    over current canonical IR without reviving an old authority format.
12. **Publication:** crash before prepare, after prepare, after durable closure,
    at select, and after select before acknowledgement. Reopen through the real
    serving registry and query complete facts/search/graph. A published state
    with acknowledgement failure must not be retried as an uncommitted job.
    Verify prepare and submission use the identical final request identity.

## What this audit closes, and what remains

This audit closes the historical mapping and source-level lineage comparison
for the selected capabilities. It identifies exact code and commits for the
known Tantivy lock regression, generic cause erasure, fixed-option TypeScript
authority, build-checkout helper dependence, single-row fact loss, graph
identity mismatch, and removed IR-native repository implementation. It also
identifies strong surviving foundations and several misleading old completeness
claims that should not be restored.

It does not close current end-to-end production gates. In particular: S2 needs
a real isolated cold-open/restart repro; current user's newer Tantivy cause
needs the typed 983-or-later runtime; native TSZ project semantics need product
integration; complete facts need successful exact prepared-intent publication,
fresh-process serving, full-budget reuse, and last-declaration queries; Python
aggregate semantic parity needs measured comparison. Root's independent build
and runtime lanes supply those results. They should be appended with checkpoint
hashes rather than rewriting unexecuted proposals into passes.

Useful existing corroborating documents, read as historical evidence rather
than substitute runtime proof:

* `docs/architecture/compiler-ir-lineage.md`
* `docs/operations/complete-cutover.md`
* `docs/architecture/semantic-parity-audit-2026-09-12.md`
* `docs/reviews/semantic-frontier-audit-2026-09-29.md`

The user-supplied October 5 incident text was read from the attached
`Pasted text.txt` (attachment ID `b03587b3-c524-48f6-9dbc-9bd8c0b3f2b6`).
Its source/reference successes and search/graph failures are retained as
separate facts. No GUI or out-of-scope language success is used to claim this
audit's product gates pass.

Initial audit-artifact verification: all 139 cited commit abbreviations resolve to
commit objects; the accompanying ancestry manifest was generated from the
pinned canonical and 983 reachable sets. All 17 explicitly checked historical
source anchors exist, including the corrected native TSZ path. The complete
commit map has 5,278 unique rows and exactly 2,720 first-parent members; its TSV
column counts and the monthly census were checked. Markdown whitespace checks
pass. No compiler build or product test was performed solely to validate these
documentation artifacts.


## October 6 follow-up: real package member loss

The ingest handoff's exact public Cargo runtime confirmed M1 after initial Published, before any cold lifecycle phase. The test assertion was preserved. Historical `135bc43976` intentionally established a truthful Captured/Unavailable distinction, but left TypeScript, Python, Go and Rust producers without the required completion proof. That cutover is a concrete capability loss; restoring unconditional inverse parentage would undo its intended authority boundary. The repair must retain a producer-owned direct-declaration inventory and keep structural, inherited and effective membership separate.

Full execution custody, the 33-commit BPI9 dependency cohort, fixture repairs and the evolving matched repair gates are in `sol-ingest-handoff-verification-20261006.md`. The initial source-facts cold persistence proof passed. The first actual public package lifecycle failed at member projection; after the explicit member repair, actual Cargo warm member assertions pass. Root subsequently repaired the deliberate source-mismatch probe classification without weakening the owner-failure oracle. Matched public reruns then passed initial cold restart, status replay/conflict and shape equality, and exposed M2 during the distinct-key unchanged-package add: the operation was Published while its terminal capture was Failed(ProjectAuthority). The typed completion repair now passes focused admission and real terminal persistence controls. Two unchanged full public reruns initially failed with bare Disconnected(InvalidInput) after durable initial publication. An approved test-only explicit Session reconnect preserves the forced retirement gate and all assertions. The next matched run proves equal-generation re-add and actual failed compiler capture retention, then exposes M3 at the original config-only Historical freshness assertion. The bounded-config repair changes the actual failed capture digest but independently exposes a selected-only freshness projection. The coherent exact-snapshot projection and terminal refused-view repair then PASS both actual public lifecycles at `c4f743`: first-refusal cold absent authority and the original prior-generation gate through Historical retained shapes, failed cold status and exact replay. The execution ledger preserves every earlier failure and reached/unreached phase rather than replacing it with later helper passes.


## October 6 read-only extension: native relationship scope and cache claims

`sol-history-native-cache-followup-20261006.md` traces the pre-cutover Python all-module State and return/class-field enrichment into the current narrower report; distinguishes a non-ancestor batch successor from that lost semantic scope; identifies old Go package owners/docs and the non-ancestor September 5 package-namespace repair; and checks surviving TypeScript inherited-member resolution without equating it with complete effective membership. Its cache review reads both actual old benchmark bodies and current production residence/reader/segment owners, distinguishing latency/proof reuse from RSS. The 29 exact source anchors and explicit ancestry are committed beside the capture proof evidence. The pinned integrated checkpoint is 039c360d; this extension adds no build, benchmark or product pass.

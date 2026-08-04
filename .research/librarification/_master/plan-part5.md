
---

# Part VI — Program

## §20 Unified migration phases

Steps (`S<section>.<n>`) from Parts I–V, interleaved into waves. Each wave has an exit gate; waves overlap where dependencies allow. Within a wave, lanes are parallel workstreams.

### Wave 0 — Foundations, extractions, hygiene *(everything else stands on this)*

| Lane | Steps |
|---|---|
| Crate surgery | S1.1 compiler-wire · S1.2 meta-store · S1.3 text-search · S1.4 graph-cold move · S1.5 blob+ingest · S1.6 new crate shells + layering CI lint · S1.7 freeze legacy `compiler/` |
| Compiler injection | S13.1 OracleSet · S13.2 explicit ToolchainSet (env reads → binaries) |
| GUI hygiene | S19.1 Phase-0 bug fixes + fixture gating |

**Exit gate:** workspace builds with the new crate graph; forbidden-edge lint green; daemon + server byte-compatible with pre-split wire; zero behavior change.

### Wave 1 — Identity & wire contracts *(the vocabulary every later wave speaks)*

| Lane | Steps |
|---|---|
| Moniker/hashes | S6.1 moniker crate · S6.2 heart PackageStemId/part-hash types · S6.3 normalizers + determinism tests · S4.1–S4.3 IR roots/EntryContentHash/producer moniker emission |
| Wire v1.5→v2 | S2.2 `/v1/hello` + ErrorBody + aliases · S2.3 sync/plan + cas/has/presign · S2.4 compiler v2 full artifacts · S3.2 occurrences_ref |
| Storage | S3.1 zstd envelopes + dictionary training · S3.3 Store→Tiered routing |
| INDEX prep | S17.1 MetaStore trait dual-impl · S17.2 sqlite DDL + one-shot Pg→sqlite migrator (staged, not yet cut over) |

**Exit gate:** every sealed generation carries monikers + part hashes deterministically; daemon returns full artifacts under v2 negotiation; sync planning API live.

### Wave 2 — Incremental spine, commit gate, trust *(the product's central invariant, GD-28)*

| Lane | Steps |
|---|---|
| Spine | S7.1 Stage/run_stage · S7.2 trace tables · S7.3 sinks-as-Stages · S7.4 symbol_heads at seal · S7.5 SymbolDelta fan-out switch-on · S7.6 one-symbol E2E test |
| Commit gate | S8.1 CommitGate watcher · S8.2 generations schema · S8.3 tree-diff + CompositePackageMapper · S8.4 gate→seal wiring · S8.5 ephemeral previews · S8.6 multi-root matrix |
| Trust | S15.1 detectors · S15.2 Jail+BFS · S15.3 lockfile parsers · S15.4 typestates · S15.6 T1–T10 matrix |
| Lineage v1 | S6.4 matcher T0+T1 · S6.5 T2/T3 |

**Exit gate:** editing one symbol and committing re-embeds/re-indexes exactly that symbol; dirty trees provably trigger nothing; trusted/untrusted classification passes the T1–T10 matrix.

### Wave 3 — Remote planes: INDEX cutover + ORCH *(Postgres dies here)*

| Lane | Steps |
|---|---|
| INDEX | S17.3 single-writer outbox drain · S17.4 Litestream + DR drill · S17.5 index-service assembly + Postgres deletion · S17.6 sessions · S17.7 BackendKind |
| ORCH | S18.1 admission API · S18.2 warm path · S18.3 Kueue + Job templates (gVisor/userns/podFailurePolicy) · S18.4 reconciler + poison · S18.5 decommission jobs-table claims · S18.6 scale/chaos test · S2.5 Cas transport · S2.6 ORCH wire |
| Compiler fleet | S13.4 daemon v2 (from Wave 1) · S13.5 seal Rust producer · S13.6 seal/restrict Java+C# · S13.8 daemon Cas mode |
| Graph read path | S9.1–S9.4 cold PackageGraph + index-service always-cold reads (GD-25) |

**Exit gate:** production INDEX on sqlite+Litestream (restore drill < 5 min); queue fully in k8s; `grep -r sqlx-postgres` empty; graph reads served cold without Terminus.

### Wave 4 — Client, REGISTRY, embedded engines *(the desktop becomes real)*

| Lane | Steps |
|---|---|
| Client | S16.1 client-core · S16.2 HTTP backends · S16.3 SyncEngine v1 · S16.4 local backends · S16.5 Embedded/RemoteCompile (S13.3 TrustedForgeContext, S13.7 acquisition move) · S16.6 witnesses · S16.7 progress |
| REGISTRY | S3.4 disk layout + pin/GC · S9.5 occurrence parity · S9.6 stat emission |
| Text | S11.1 shared crate (from W0) · S11.2 tantivy 0.26.1 reindex · S11.3 schema v2 · S11.4 LanguageAnalyzers · S11.5 symbol fusion · S11.6 delta Stage |
| Vector | S12.1 vector-remote · S12.2 qdrant-edge · S12.3 fastembed worker · S12.4 EmbedStage · S12.5 gates+routing |
| Toolchains | S14.1 discovery · S14.2 `nudox-producer/3` epoch (coordinated with fleet) · S14.3 language packs · S14.4 feature CI · S14.5 settings UX |

**Exit gate:** §16's acceptance scenario — remote-serves-while-syncing, atomic local takeover, offline Ready-subset — passes; local search parity with remote on identical DepSets.

### Wave 5 — GUI product, hot tier, ambitious lineage *(the product ships)*

| Lane | Steps |
|---|---|
| GUI | S19.2 stores · S19.3 Dock/virtualization · S19.4 project open flow (S15.5 trust gate) · S19.5 omni-search · S19.6 RenderModel · S19.7 JobStore · S19.8 lineage timeline + type-search |
| Hot tier | S10.1 terminus-client · S10.2 leaky bucket + CMS · S10.3 TieredPackageGraph · S10.4 promote/demote + S9.7 Relation parity · S10.5 lineage publish · S10.6 rollups |
| Lineage v2 | S6.6 T4/T5 · S6.7 T6 embed-assist · S6.8 manual patches + guards |
| Storage tail | S3.5 INDEX GC |

**Exit gate:** P0 GUI demo on a cold machine; Terminus admits only under sustained load and demotes loss-free; lineage timeline shows real T0–T5 edges across ≥ 3 generations of a fixture package.

### Cross-wave rules

1. **CAS epoch discipline:** S14.2 (`nudox-producer/3`) is a coordinated flag-day for desktop + fleet JobKeys — schedule inside Wave 4 with a dual-read window.
2. **Derived stores are disposable** (GD-14): any reindex-requiring change (S11.2/S11.3) ships as rebuild-from-blobs, never in-place migration.
3. **No wave starts its exit-gated work before the previous gate is green**, but lanes may pre-land dark code behind features at any time.
4. **Legacy deletion points:** route aliases die at S2.7 (post-Wave 4); `GraphStore`/shims die when their last consumer migrates; root `compiler/` dies at Wave 1 start.

## §21 Risk register

| # | Risk | Sev | Mitigation |
|---|---|---|---|
| R1 | qdrant-edge is young (0.7.x, 2026-06) — API churn or perf gaps at 10⁶ vectors | H | Lance feature escape hatch (GD-4); VectorStore trait isolates; remote fallback via Routed is always live |
| R2 | Terminus Prolog core ops opacity; stale Rust embedding crate | M | HTTP-only boundary; cold path is the SoT; demotion loss-free by design (GD-13/25) |
| R3 | False lineage merges poison history (write-once read-many) | H | Frozen thresholds prefer false-split; auto ≥ 0.93; embed reuse ≥ 0.95; mass-delete guard; Manual patches override (GD-7) |
| R4 | SQLite single-writer INDEX becomes a write bottleneck at ecosystem-crawl scale | M | Batched IMMEDIATE writes measured ≥ 10k rows/s; queue already out of SQL; rqlite documented fallback (GD-2) |
| R5 | Litestream restore drill fails when actually needed | H | S17.4 makes DR a rehearsed, CI-scheduled drill, not a hope |
| R6 | `nudox-producer/3` epoch bump orphans existing CAS | M | Dual-read window; old epoch GC'd only after fleet+desktop cutover (S14.2) |
| R7 | Sealing Rust/Java/C# producers stalls (RA in-process complexity) | M | S13.6 allows fleet-restriction fallback; trusted desktop path (DevPassthrough) unaffected |
| R8 | Tantivy 0.26 mixed-version readers corrupt/misread segments | M | Marker file + forced rebuild; both planes pinned together (GD-21) |
| R9 | GUI refactor scope explosion (12 modules → product) | M | Store-by-store migration with shims; P0 scope frozen in §19.3 |
| R10 | Trust jail bypass (symlinks, config redirects, node_modules roots) | H | Canonicalize-before-jail; refusal defaults; diagnostic codes; T1–T10 matrix is release-blocking (GD-16) |
| R11 | Commit-gate watcher misses (packed-refs races, rebases) | M | 5 s poll net + debounce + quiet period; hooks as accelerators only (GD-15) |
| R12 | snix GPL contamination of GUI binary | H | Never linked (GD-18); optional GPL-isolated worker binary; license CI check |
| R13 | Kueue/k8s learning curve for a previously-SQL queue team | M | ORCH stays thin; warm path is plain HTTP; poison semantics are k8s-native GA features |
| R14 | Embedding model swap invalidates all vectors | M | tool_digest global invalidation is *designed*; versioned collection names for dual-index upgrade (GD-4) |
| R15 | Moniker grammar gaps for exotic language constructs (overloads, Nix attrs) | M | `moniker_grammar_version` field allows v2; method disambiguator reserved; per-language extractors tested against snapshot corpus |

## §22 Open questions & deferred work

1. **PackageStemId canonicalization for forks/renames** — registry rename events create new lineage roots pending an explicit package-rename patch mechanism (deferred to lineage v2).
2. **INDEX job-metadata retention** — how long poison/history rows live before archival to S3 (ops decision, Wave 3).
3. **ColdGraphBlob** (lean persisted edges for warm-not-hot packages) — only if §9 re-projection profiling demands it.
4. **Transfer packs** (seekable zstd) for bulk cold sync — deferred until sync telemetry shows manifest-pull tail pain (GD-12).
5. **Windows desktop** — post-MVP platform (§14).
6. **Multi-writer INDEX HA** (rqlite) — only if single-writer + Litestream RTO proves insufficient.
7. **T6 embed-assist thresholds** — frozen values (0.92/0.75/0.80) need recalibration against real corpus before Auto edges from T6 are enabled by default.
8. **Session store on INDEX** — keep, simplify, or drop multi-replica session merge (Wave 3 decision).
9. **NATS JetStream** for completion fan-out — off by default; revisit if warm-path 503 backpressure proves insufficient under GUI load.
10. **Docset import** (Dash/Zeal compatibility) — P3 GUI idea, unvalidated demand.
11. **Cross-language lineage** (e.g. generated bindings) — explicitly out of scope for auto matching (GD-7 scheme isolation); manual edges only.
12. **`ir` ↔ heart boundary pressure** — if producers need heart types inside ir, resolve by moving shared identity primitives down into a leaf crate rather than breaking GD-23.

---

*End of LIBRARIFICATION-PLAN. Research corpus: `.research/librarification/` (22 documents, ~23.6k lines). Assembled 2026-07-16.*

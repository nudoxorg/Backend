# Compiler Fleet Cache Topology for Untrusted K8s Compiles

**Research date:** 2026-07-16  
**Status:** Design ADR + implementation plan (streamlining candidate → recommended adopt for L0/L1/L3/L5)  
**Scope:** Whether and how compiler instances on the k8s fleet (warm Deployment + Kueue Jobs per plan 12) should share build caches; layer-by-layer analysis; security for untrusted packages; concrete config types and phased rollout.  
**Audience:** Architecture decision for ORCH + compiler-daemon ForgeRuntime CAS wiring.  
**Cross-links:**
- `docs/research/librarification/12-orchestration/` — warm Deployment + Kueue Jobs hybrid
- `docs/research/librarification/01-compiler-audit/` §9 — incrementality / JobKey / CAS
- `docs/research/librarification/13-storage/` — INDEX object_store CAS layout
- `docs/research/edge-tech/03-edenfs/` — materialize + node-local hardlink CAS
- `docs/research/edge-tech/04-ipfs-and-cas/` — reject public swarm; keep BLAKE3 CAS
- `docs/research/edge-tech/05-surrounding-edge/` — SOCI/eStargz, Nix store mental model
- Live code: `workspace/compiler/generate/mod.rs`, `compile/producer/runtime.rs`, `daemon/forge.rs`, `workspace/heart/cache/`

---

## 0. Executive summary (read this first)

### 0.1 One-paragraph answer

**Today, compiler pods do not share a fleet-wide build cache.** ORCH is designed to skip recompile at the **output** level when `input_hash` already has INDEX/S3 artifacts (L0). Inside a pod, `cache_get_or_build` stages (surface/CST/occurrences/archive) hit an **in-process MemoryCas + optional node-local DiskCas** via `ForgeContext` — there is **no L3 object-store tier on the daemon** (`DaemonL3::None` in `daemon/forge.rs`). Warm pods keep **in-process** toolchain state (RA analysis host, worker pools) that is **pod-local only**. Toolchain/sysroot layers are already shared via container images (L3 in this doc’s numbering). **They should share L0 + L1 stage CAS + L3 image layers + L5 source materialization cache.** They should **not** naively share language mutable build dirs (`target/`, GOCACHE, Gradle caches) across untrusted tenants without isolation keys. L4 warm state stays warm-pool only (no sticky multi-tenant analysis hosts).

### 0.2 Decision matrix (quick)

| Layer | Name | Share across fleet? | Today | Target |
|---|---|---|---|---|
| **L0** | ORCH/INDEX output CAS | **Yes** | Planned in plan 12; primary dedup | Required GA |
| **L1** | Stage CAS (JobKey postcard IR/CST/occ/archive) | **Yes** (read-mostly remote + node-local L2) | Per-pod Memory/Disk only; no Daemon L3 | Wire `Tiered` L3 → S3/object_store |
| **L2** | Language build caches (cargo/sccache/go/gradle/npm/…) | **Careful / mostly no** across untrusted tenants | Not designed | Opt-in hermetic only; never shared mutable `target/` |
| **L3** | Toolchain/sysroot image layers | **Yes** (already) | nix2container / per-lang images | Keep; pin digests |
| **L4** | In-process producer warm state | **Warm pool only** | Pod-local RA / workers | Sticky optional later; no cross-tenant RA |
| **L5** | Source materialization cache | **Yes** (node-local CAS hardlinks) | Per-job tempdir peel | Node-local DiskCas + hardlink farm |

### 0.3 Recommended stance in one line

**Share immutable content-addressed products fleet-wide (outputs, stages, images, source blobs); never share mutable toolchain scratch across tenants; warm only what is process-local and re-sealable.**

### 0.4 Savings intuition (quantified scenarios in §8)

| Scenario | Dominant win layer | Order-of-magnitude |
|---|---|---|
| Reindex popular crate already in INDEX | L0 | Skip **100%** of compile wall time |
| Second pod compiles same sealed input first time after peer miss | L1 | Skip producer lower; keep download/materialize cost |
| Docs-only version bump (same sources, new version label) | L0 partial / L1 if source_hash stable | High if JobKey source component unchanged |
| Mass reindex after `producer_version` bump | L0+L1 miss; L3+L5 still hit | Images + source blobs reuse; recompute IR only |
| Same node runs 50 versions sharing 80% files | L5 | Hardlink saves disk + extract CPU |
| Warm GUI request for serde after warm pod saw it | L1 L1-memory + L4 | Sub-second surface if stages still in L1 |

---

## 1. Problem framing

### 1.1 Fleet shape (from plan 12)

```
Client → Orchestration Server
           ├─ L0: INDEX input_hash → S3 outputs? return
           ├─ warm: Deployment compiler-server (long-lived)
           └─ cold: Kueue Job (ephemeral pod)
                    both write IR/blobs → S3; INDEX terminal status
```

Workload properties that make cache topology non-optional:

| Property | Cache implication |
|---|---|
| Untrusted third-party packages | Only **immutable, keyed** sharing is safe |
| Idempotent sealed inputs | Spot preemption + retry are free if CAS keys match |
| Bursty bulk + interactive GUI | L0 protects bulk; L1/L4 protect interactive |
| Outputs already content-addressed | L0 is free correctness; L1 is free correctness once JobKey is stable across nodes |
| Warm vs cold lifecycle | Cold pods start empty; need remote L1 L3 or pay full recompute |

### 1.2 What “build cache” means here (not CI jargon alone)

In CI, “build cache” often means `~/.cargo`, Gradle, npm. In nudox the **primary** product of compile is **IR + references + source digests**, not `.rlib` artifacts. Language build caches (L2) are secondary and often **unnecessary** if producers analyze without full link (RA, oxc, pyrefly, treesitter). L2 matters only for producers that still invoke `cargo`/`go`/`javac`/`dotnet` in ways that leave incremental state.

### 1.3 Codebase anchors (ground truth 2026-07-16)

| Component | Path | Behavior |
|---|---|---|
| Stage pipeline | `workspace/compiler/generate/mod.rs:96–138` | `JobKey` → `cache_get_or_build` for surface, cst, occ, archive |
| Cache client | `workspace/compiler/compile/producer/runtime.rs:124–153` | postcard get-or-build; first-write-wins put; invalidate on decode fail |
| `run_producer` | same file `:160–188` | CAS key = `SealedInput.key` |
| JobKey | `workspace/heart/content.rs:60–92` | BLAKE3 len-prefixed producer‖toolchain‖source‖dep_lock; `with_tag` children |
| Cas trait | `workspace/heart/cache/mod.rs` | put/get/put_keyed FWW; EvictableCas for L1/L2 only |
| Tiered | `workspace/heart/cache/tiered.rs` | L1 StampedeCache → L2 DiskCas → L3 Cas; promote on hit |
| DiskCas | `workspace/heart/cache/disk.rs` | `{root}/cas/{hex}`; envelope `blake3(value)‖value`; hardlink publish |
| Daemon forge | `workspace/compiler/daemon/forge.rs:34–48,141–146` | **DaemonL3::None always**; optional `NUDOX_CAS_ROOT` L2 |
| Local default | `LocalForgeContext` | memory-only Tiered, no L3 |

**Critical gap:** `Tiered::with_l3` is designed for `registry::StoreCas`-class L3, but the **compiler daemon deliberately stripped L3**. Fleet stage sharing is an unfinished wire of an existing design, not a greenfield invention.

### 1.4 Questions this document answers

1. Do compiler instances on k8s share a build cache **today**?  
2. Which layers **should** they share?  
3. What is the **security boundary** for untrusted multi-tenant code?  
4. What is the **recommended topology** for warm vs cold?  
5. How do we **roll out** without poisoning the fleet?  
6. What metrics prove “huge savings”?

---

## 2. Layer model (L0–L5)

Numbering is **nudox fleet-specific** (do not confuse with heart `Tiered` L1/L2/L3 memory/disk/object tiers — those nest **inside** fleet L1 stage CAS).

```
┌──────────────────────────────────────────────────────────────────────────┐
│ L0  ORCH/INDEX output CAS     input_hash → IR pack / blob pointers       │
├──────────────────────────────────────────────────────────────────────────┤
│ L1  Stage CAS                 JobKey → postcard surface/cst/occ/archive  │
│     ├─ heart L1: in-process StampedeCache                                │
│     ├─ heart L2: node-local DiskCas                                      │
│     └─ heart L3: remote object_store (S3)  ← fleet share                 │
├──────────────────────────────────────────────────────────────────────────┤
│ L2  Language build caches     cargo target, sccache, GOCACHE, gradle…    │
├──────────────────────────────────────────────────────────────────────────┤
│ L3  Toolchain image layers    nix2container / OCI layers / sysroots      │
├──────────────────────────────────────────────────────────────────────────┤
│ L4  In-process warm state     RA RootDatabase, worker pools, oxc/pyrefly │
├──────────────────────────────────────────────────────────────────────────┤
│ L5  Source materialization    node-local source blob CAS + hardlinks     │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## 3. L0 — ORCH/INDEX output CAS

### 3.1 What it is

Before any pod runs, ORCH checks INDEX for a **succeeded** job with the same content-addressed `input_hash` and returns S3 artifact URLs. Plan 12 §2.2 / §3.5:

- `input_hash = H(sealed_bundle ‖ toolchain_digest ‖ compiler_version)`  
- Outputs at `s3://…/outputs/{input_hash}/…` (immutable)  
- In-flight coalesce: second request attaches to existing job  

This is **whole-job** memoization: skip download, sandbox, produce, upload.

### 3.2 Hit rate expectations

| Workload | Expected L0 hit rate | Notes |
|---|---|---|
| GUI re-open same package version | **Very high (80–99%)** | Same sealed input |
| Crawler re-touch popular crates | **High** after first success | Serde/tokio class |
| “Reindex all after producer_version bump” | **0%** | input_hash includes compiler/producer version |
| Force recompile / poison clear | 0% by policy | Admin invalidation |
| Docs-only crates.io release (same tarball content) | High if seal uses source bytes not registry version metadata alone | Depends on seal definition |

### 3.3 Security isolation

| Concern | Assessment |
|---|---|
| Cross-tenant read | **Safe if authz gates who may read package artifacts**; payload is content-addressed IR of public packages typically |
| Poison | Bad compile must not mark succeeded without verification; prefer multi-check on upload |
| Tenant private packages | L0 keys must be **tenant-scoped** or authz-checked; never global anonymous get by hash alone for private repos |

**Recommendation:** For public registry packages, global L0 is fine. For private/enterprise packages, `input_hash` lookup is allowed only if caller has package ACL; optional salt: `H(tenant_id ‖ seal)` if multi-tenant isolation requires non-shared outputs even for identical source (rare; prefer ACL on read).

### 3.4 Implementation (already planned)

1. INDEX `jobs.input_hash UNIQUE` + status  
2. Orchestrator admission path: hit → return URLs  
3. S3 put-if-absent / immutable objects  
4. Metrics: `orch_l0_hit_total`, `orch_l0_miss_total`, `orch_compile_skipped_seconds_saved`

### 3.5 When NOT to share / skip L0

- Admin “rebuild from scratch” with force flag  
- Corruption investigation (verify path recompares hashes)  
- Partial output missing (treat as miss, recompile)  
- Producer_version / protocol epoch migration (new input_hash domain)

### 3.6 Relationship to L1

L0 is a **superset skip**. On L0 hit, L1 is never consulted. On L0 miss, a pod may still L1-hit stages if another pod already published stage blobs under JobKey (or if this pod’s local L2 has them). After success, L0 prevents future pods from even starting.

**Order of checks (admission):**

```
1. L0 INDEX/S3 outputs complete? → return
2. In-flight same input_hash? → coalesce
3. Dispatch warm/cold
4. Inside pod: L5 materialize → L1 stage CAS → produce → upload → L0 record
```

---

## 4. L1 — Stage CAS (JobKey postcard stages)

### 4.1 What it is

`generate_with` (`generate/mod.rs`):

```text
JobKey = BLAKE3(len‖producer_version ‖ len‖toolchain_digest ‖ len‖source_hash ‖ len‖dep_lock)
CAS[job]           = postcard(surface Index)     // via cache_get_or_build
CAS[job‖"cst"]     = postcard(CstSet)
CAS[job‖"occ-v1"]  = postcard(OccurrenceSet)
CAS[job‖"archive"] = postcard(SourceArchive)
```

`cache_get_or_build` (`runtime.rs:124–153`): get → decode postcard → on fail invalidate → else build → put_keyed FWW.

This is the **nudox-native remote build cache**: not sccache objects, but IR stages.

### 4.2 Today vs target

| Aspect | Today | Target |
|---|---|---|
| Heart L1 memory | Yes (StampedeCache capacity 256) | Keep; raise capacity on warm pods |
| Heart L2 disk | Optional `NUDOX_CAS_ROOT` / `NUDOX_PARSE_CACHE` | **Node-local PVC or hostPath** for warm; emptyDir for cold (or hostPath shared) |
| Heart L3 remote | **DaemonL3::None** | **S3 prefix `cas/stage/{blake3}`** via object_store Cas |
| Cross-pod share | **No** | **Yes** via L3 |
| Cross-node L2 share | Only if same node disk | Optional hostPath/PVC node-local |

### 4.3 Hit rate expectations

| Scenario | L1 remote hit | Notes |
|---|---|---|
| Two cold Jobs same package same producer | High after first finishes | Second skips lower |
| Warm pool after peer warm compiled same JobKey | High if L3 populated | L1 memory hit if same pod |
| Source one-file change | Miss whole JobKey | Package-granularity today (01 §9.3) |
| Resolver-only bump (`occ-v1` → `occ-v2`) | Surface/cst/archive hit; occ miss | `with_tag` isolation |
| Toolchain path-string difference across nodes | **False miss** | **Must fix** toolchain content fingerprints (`nudox-producer/3`) |
| Partial stage failure then retry | Per-stage FWW; good stages reusable | |

**Expected fleet steady-state (public bulk indexer):** L1 stage hit rate **30–70%** of CPU-heavy lower work on re-crawls and dependency fan-in (same versions many times), **near 0%** on pure first-seen long tail.

### 4.4 Security isolation

Stage blobs are **pure functions of JobKey inputs**. If JobKey is correct:

- Same key ⇒ same intended bytes (modulo nondeterminism bugs)  
- First-write-wins means a **poison first writer** sticks until invalidate  

| Threat | Mitigation |
|---|---|
| Malicious pod writes wrong postcard under valid JobKey | **Trust only fleet-signed identity** to put L3; untrusted *package code* cannot speak to L3 directly; only compiler process after seal |
| Compromised compiler image puts garbage | Content envelope + optional **recompute verify sampling**; epoch invalidate |
| Decode poison | Already: invalidate L1/L2 on postcard fail; L3 needs quarantine prefix or version epoch |
| Cross-tenant IR leak | Same as L0: public packages OK; private packages need ACL on stage get or tenant-scoped prefix |
| Timing channel “does this JobKey exist?” | Low value for public; for private use authz before exists check |

**Rule:** L3 stage CAS is written by **compiler service account** only. Sealed package code has **no network** to S3 in production cage (presigned URLs only for inputs/outputs via controlled env — never raw IAM for package process). Prefer: forge runtime uploads stages **outside** the cage after produce, or via host-side CAS put after worker returns bytes.

### 4.5 Implementation design

#### 4.5.1 Wire Daemon L3

Replace `DaemonL3::None` with a real backend:

```rust
// Conceptual — not landed
pub struct ObjectStoreCas {
    store: Arc<dyn object_store::ObjectStore>,
    prefix: ObjectPath, // e.g. "cas/stage/"
}

impl Cas for ObjectStoreCas {
    // get: GET prefix/{key.hex()} ; verify optional envelope
    // put_keyed: PUT if not exists (If-None-Match / head-then-put FWW)
}
```

ForgeConfig gains:

```rust
pub struct SharedCasConfig {
    pub scope: CacheScope,
    pub l3: Option<ObjectStoreCasConfig>,
    pub l2_root: Option<PathBuf>,
    pub l1_capacity: u64,
    pub write_l3: bool,          // cold may read-only if desired
    pub verify_sample_rate: f32, // recompute-and-compare
}

pub enum CacheScope {
    /// Fleet-shared stage keys (default for public IR stages)
    GlobalStage,
    /// Node-local DiskCas only (dev / airgap)
    NodeLocal,
    /// Explicitly disable stage cache (debug / forensics)
    Forbidden,
}
```

#### 4.5.2 Read/write path

```
get(key):
  L1 → L2 → L3
  promote upward on hit (existing Tiered)

put_keyed(key, bytes):
  write L2 then L3 (durable first) then L1 if novel
  FWW: if L3 exists, do not overwrite; promote existing into L1
```

Matches `heart/cache/tiered.rs` today.

#### 4.5.3 Poison / corruption handling

| Event | Action |
|---|---|
| Postcard decode fail | `cas_invalidate` L1+L2; for L3: mark `cas/stage-quarantine/{key}` or rely on epoch |
| Integrity envelope fail (DiskCas) | Delete local file; treat miss |
| Suspected wrong IR (QA) | Bump `PRODUCER_VERSION` or stage tag; never mutate L3 in place |
| Compromised writer window | Rotate `cas/stage-v{N}/` prefix; dual-read during migration |

**L3 is immutable by type** (`EvictableCas` does not touch L3). Poison repair = **new key domain** or **admin delete** via lifecycle tooling outside the Cas trait.

#### 4.5.4 Single-flight across pods

Heart `StampedeCache` coalesces **in-process** only. Cross-pod: two Jobs may both miss L3 and both build. Acceptable:

- First put wins; second put_keyed returns false  
- Wasted CPU is rare if L0 coalesce works for identical `input_hash`  

Optional later: ORCH-level “stage building” lease — **not** required for M1.

### 4.6 When NOT to share L1

- **Local untrusted desktop** mixing with fleet keys without matching toolchain fingerprint domain  
- **Debug force rebuild** (`CacheScope::Forbidden`)  
- **Nondeterministic producer** still under investigation (would poison FWW)  
- **Private tenant isolation policy** that forbids shared IR storage (use tenant prefix)

### 4.7 Prerequisite: toolchain content fingerprints

From 01-compiler-audit §9.2 / §11.12: `ToolchainSet.digest` hashes **path strings**. Different Nix store paths ⇒ different JobKeys ⇒ **L1 remote never hits across nodes**.

**Blocker for fleet L1:** migrate to semantic blake3 fingerprints (`nudox-producer/3` per librarification GD / 18-desktop-toolchains). Until then, L1 remote hits only within identical image digests (same path layout) — still valuable if all pods run the same OCI digest.

---

## 5. L2 — Language build caches

### 5.1 Inventory by language

| Ecosystem | Cache locations | Used by nudox producers? | Share? |
|---|---|---|---|
| **Rust** | `target/`, sccache, `CARGO_HOME` | RA primarily; may still build proc-macros | **No** shared `target/` across tenants |
| **Go** | `GOCACHE`, `GOMODCACHE` | Oracle/extractor may use modules | GOMODCACHE **RO content** maybe; GOCACHE careful |
| **Java** | `~/.m2`, Gradle caches | Oracle extractors | Dependency jars RO share OK; build dir no |
| **C#** | NuGet global packages | nupkg resolve | RO package cache OK |
| **TypeScript** | `node_modules`, `.turbo` | oxc/tsz path — often no full npm build | Prefer no node_modules share |
| **Python** | venv, `__pycache__`, pip cache | pyrefly analysis | No shared venv |
| **Nix** | `/nix/store` | snix / docs | Store is already CAS; image-provided |

### 5.2 Why mutable target dirs are dangerous

1. **Cross-tenant contamination:** package A’s build scripts write into shared `target/`; package B reads wrong rmeta.  
2. **Cache poisoning:** crafted crate influences sccache object for benign key if keying is wrong.  
3. **Nondeterminism:** incremental compiler state is a known source of heisenbugs.  
4. **Disk DoS:** unbounded growth on node.  
5. **Secret leakage:** build scripts may embed env into caches.

### 5.3 When L2 sharing can be considered

| Pattern | Allowed? | Conditions |
|---|---|---|
| RO registry of **fetched** crates/modules (content-addressed) | **Yes** | BLAKE3 verify; never execute from shared writeable tree |
| sccache with **strict key** (compiler hash + cmdline + inputs) + S3 | **Maybe later** | Only if producer still invokes rustc; multi-tenant **read** OK, write from trusted compiler only |
| Shared `target/` on hostPath | **Forbidden** | — |
| Per-job emptyDir target | **Yes** | Default |
| Per-tenant encrypted cache volume | Enterprise niche | Rarely worth it vs L1 |

### 5.4 Security policy (normative)

```
CacheScope for L2 language state:
  Forbidden  — default for untrusted sealed fleet
  NodeLocal  — only for TrustedThreatTier / first-party
  GlobalStage — never for mutable target/; only for content-addressed dep blobs
```

**Default: do not share L2 across untrusted tenants.** Prefer eliminating the need for L2 by analysis-only producers (RA without full cargo build where possible; oxc without npm install).

### 5.5 Hit rate if (wrongly) shared

Historically sccache hit rates of 50–90% in monorepos. For **heterogeneous untrusted packages**, hit rates collapse and risk rises. Expected safe RO dep cache hit (maven/go modules) **high** for popular deps; that is **dependency materialization**, better modeled as **L5** or image layers than classic L2.

### 5.6 Implementation guidance

1. Sealed jobs get **empty** `CARGO_TARGET_DIR` / `GOCACHE` under job scratch.  
2. Optional: node-local **read-only** `GOMODCACHE` populated by a trusted fetcher (not by package build scripts).  
3. Do **not** mount a shared RW Gradle home.  
4. If sccache ever enabled: separate S3 bucket, compiler-only credentials, disable for untrusted network-off builds that cannot populate cache legitimately anyway.

### 5.7 When NOT to use L2

- Default untrusted path  
- Any job with `ThreatTier` untrusted  
- Spot nodes with ephemeral disks (no benefit to large local L2)  
- When L1 already captures the product of analysis

---

## 6. L3 — Toolchain / sysroot image layers

### 6.1 What it is

Per plan 12 §4.5: **per-language images** via nix2container; shared base layers (glibc, certs, bwrap). Warm and cold pods pull the same digests. This is the **most successful shared cache in k8s** already: containerd/cri content store, optional eStargz/SOCI lazy pull (edge-tech 05).

### 6.2 Hit rate

| Event | Behavior |
|---|---|
| Second pod same node same image | **Layer cache hit** (near free) |
| New node | Pull once per layer; Karpenter churn costs money |
| Image digest bump | Full pull of changed layers only if nix-layered well |

### 6.3 Security

| Concern | Mitigation |
|---|---|
| Supply chain | Cosign sign; digest pins; SBOM |
| Writable sysroot | Images RO; never let package mutate `/nix/store` |
| Toolchain drift vs JobKey | JobKey must include **content** fingerprint of oracle + rustc, not only path |

### 6.4 Should they share?

**Yes — already.** Optimize with:

- Per-language images (smaller attack + pull surface)  
- nix2container layer dedup  
- Optional SOCI/eStargz for cold start  
- Karpenter consolidation to keep warm layer caches on nodes  

### 6.5 When NOT to share

- Do not use one fat “all languages” image for cold bulk if it inflates attack surface  
- Do not share **developer host** toolchains into untrusted pods  

---

## 7. L4 — In-process producer warm state

### 7.1 What it is

Long-lived warm Deployment pods keep:

| State | Source | Benefit |
|---|---|---|
| RA analysis host / Vfs / RootDatabase (if trusted keep-alive) | rust producer | Avoid cold RA start |
| Worker pools (nix, ts/py) | `ForgeRuntime` | Avoid process spawn |
| L1 MemoryCas hot stages | heart StampedeCache | Instant stage hit |
| Parsed grammar warm | treesitter | Minor |

Cold Kueue Jobs **start empty** every time (except L5 node CAS / image layers).

### 7.2 Today

Warm pool keeps toolchains and workers **pod-local**. Plan 01 §9.3: RA keep-alive is for **library/desktop form**, “incompatible with stateless k8s pod model” for untrusted — use package-level CAS there.

### 7.3 Should they share?

| Mechanism | Verdict |
|---|---|
| Share RA database across pods | **No** — memory, isolation, non-serializable |
| Sticky sessions (same package → same pod) | **Optional later** for GUI interactive only |
| Warm pool only (no stickiness) | **Yes — default** |
| Cold Jobs rely on L4 | **No** — rely on L0/L1/L3/L5 |

### 7.4 Security

- Never keep analysis state from package A when compiling package B without full reset if there is any cross-talk risk  
- Prefer **process-per-job** for untrusted adaptive producers even on warm pods (`require_worker`)  
- RA in-process on warm pool for **public packages** may be acceptable if Vfs is fully replaced per job; audit absolute path leaks  

### 7.5 Hit rate

Warm pod sequential packages: L4 helps **startup** (100–2000ms class for RA) more than end-to-end if L1 already hot. Sticky sessions help when user recompiles same package with tiny edits (desktop-like); fleet untrusted bulk rarely benefits.

### 7.6 When NOT to use L4 sharing

- Cross-tenant stickiness  
- Serializing RootDatabase to shared volume  
- Assuming cold Jobs have warm state  

---

## 8. L5 — Source materialization cache

### 8.1 What it is

From edge-tech 03 (EdenFS reject; CAS+tmpdir adopt):

> Prefetch declared producer inputs into RO scratch (or hardlink/erofs snapshot of CAS objects).

Today: peel tarball into job tempdir every time. Target: **node-local CAS of source file blobs** keyed by BLAKE3; materialize trees via **hardlinks** into per-job directories.

### 8.2 Architecture

```
S3 inputs/{input_hash}.tar.zst
        │
        ▼
  Node source CAS  (DiskCas or cas/{blake3} bodies)
        │ hardlink / composefs
        ▼
  /scratch/job-{id}/tree/   (RO bind into cage)
```

Shared across pods **on the same node** via hostPath or node-local volume. Cross-node share is L0/L1 S3, not L5 (L5 is intentionally node-local for speed).

### 8.3 Hit rate

| Workload | L5 hit | Savings |
|---|---|---|
| 50 versions of left-pad-class tiny packages | Medium | Small absolute |
| 50 versions of kubernetes/react with shared files | **High** | Extract CPU + disk |
| First package on fresh node | Miss | Full download |
| Mass reindex after producer bump | **High** | Sources unchanged |

### 8.4 Security

| Concern | Mitigation |
|---|---|
| Hardlink to wrong blob | Verify BLAKE3 of file body = manifest hash before link |
| Symlink escape | Reject symlinks or rewrite; O_NOFOLLOW |
| Cross-tenant presence oracle | Public CAS OK; private sources in tenant-scoped node cache or no L5 share |
| Leftover RW overlay | Wipe per job; never reuse upper |

Content-addressed source blobs are **naturally multi-tenant-safe for public packages** (edge-tech 03 §6.3).

### 8.5 Should they share?

**Yes, node-local.** Complements remote stage CAS: L5 saves **I/O and extract**; L1 saves **CPU lower**.

### 8.6 When NOT to share

- Private package sources on multi-tenant nodes without ACL  
- FUSE multi-tenant daemon (rejected in 03)  
- Mutable checkouts  

### 8.7 Relationship to L1 archive stage

`SourceArchive` stage is a **postcard of digests + layout**, not necessarily full file bodies. L5 holds **bodies**. Archive stage hit still needs bodies for producers that re-read sources unless producer output fully cached in L1 surface.

---

## 9. Prior art mapping (sealed untrusted multi-tenant fleet)

### 9.1–9.7 Lessons by system

| System | Steal | Reject / caution |
|---|---|---|
| **Bazel remote cache** | Action key = hermetic inputs; CAS remote + disk; auth writers; no non-hermetic cache | We cache **typed postcard stages + INDEX outputs**, not arbitrary action trees |
| **sccache** | Compiler hash in key; S3 backend | Multi-tenant L2 risk; only if rustc still dominates (else L1 supersedes) |
| **Gradle / Turbo / Nx Cloud** | Task I/O hashing; remote monorepo cache | Assume trusted monorepo — our untrusted seal is stricter |
| **GitHub Actions cache** | Branch/tenant scope vs poison; best-effort | Unpredictable eviction → explicit S3 lifecycle |
| **kaniko / OCI** | Layer cache = fleet L3 images; nix2container dedup | — |
| **Nix binary cache** | narinfo mental model for L0/L1; content-addressed store | Careful substituter trust → SA + ACL stage bucket |
| **Buck2 RE CAS** | Deferred materialize → L5; cattle workers → cold Jobs | — |

### 9.8 Synthesis for sealed untrusted multi-tenant

| Principle | From | Nudox rule |
|---|---|---|
| Immutable CAS only for cross-tenant share | Bazel/Nix/Buck | L0, L1, L5, images |
| Authenticated cache writers | Bazel | Compiler SA |
| Hermetic action keys | Bazel/sccache | JobKey components complete |
| No shared mutable workdirs | Multi-tenant CI incidents | L2 Forbidden |
| Best-effort cache | GHA | Never require L1 for correctness |
| Node-local materialize | Buck2/Eden ideas | L5 hardlinks |

---

## 10. Recommended cache topology

### 10.1 Warm path (Deployment compiler-server)

```
┌─ Warm Pod ─────────────────────────────────────────────────────────┐
│ L4: worker pools + optional RA reset-per-job                        │
│ L1 heart:                                                           │
│   L1 mem (large) → L2 DiskCas (PVC or hostPath) → L3 S3 stage CAS   │
│ L5: node-local source CAS hardlink → job scratch                    │
│ L3 images: already pulled                                           │
│ L2 language: emptyDir only (Forbidden share)                        │
└────────────────────────────────────────────────────────────────────┘
         ▲ L0 check before dispatch (ORCH)
```

**Config sketch:**

```yaml
env:
  NUDOX_CAS_SCOPE: GlobalStage
  NUDOX_CAS_ROOT: /var/lib/nudox/cas        # L2 node-local
  NUDOX_CAS_L3_URI: s3://nudox-cas/stage/  # L1 remote
  NUDOX_CAS_L1_CAPACITY: "4096"
  NUDOX_CAS_WRITE_L3: "true"
  NUDOX_SOURCE_CAS_ROOT: /var/lib/nudox/source-cas
```

Volume: `hostPath` or local PV for `/var/lib/nudox` (node affinity optional).

### 10.2 Cold path (Kueue Job)

```
┌─ Cold Job Pod ─────────────────────────────────────────────────────┐
│ L4: cold start (workers optional; no sticky)                        │
│ L1 heart:                                                           │
│   L1 mem (small) → L2 emptyDir OR hostPath source/stage → L3 S3     │
│ Prefer: L2 = hostPath node CAS if present, else emptyDir            │
│ L5: same node source CAS if hostPath; else peel to emptyDir         │
│ L2 language: emptyDir Forbidden                                     │
└────────────────────────────────────────────────────────────────────┘
```

**Cold may set `write_l3=true`** so bulk indexing populates stage CAS for later warm hits.  
**Cold may set `write_l3=false`** only if cost of S3 PUTs dominates and L0 is enough — not recommended for first rollout.

### 10.3 Topology diagram (fleet)

```
                    ┌──────────────┐
                    │ INDEX + S3   │  L0 outputs / inputs
                    │ outputs/…    │
                    └──────▲───────┘
                           │ success
              ┌────────────┴────────────┐
              │                         │
        ┌─────┴─────┐             ┌─────┴─────┐
        │ Warm pods │             │ Cold Jobs │
        └─────┬─────┘             └─────┬─────┘
              │                         │
              └────────────┬────────────┘
                           │ stage get/put
                    ┌──────▼───────┐
                    │ S3 cas/stage │  fleet L1 remote (heart L3)
                    └──────▲───────┘
                           │ promote
              ┌────────────┴────────────┐
              │  Node-local DiskCas     │  heart L2 + L5 bodies
              │  /var/lib/nudox/cas     │
              └─────────────────────────┘
```

### 10.4 Read-only remote + local write-through

Recommended default:

1. **Always read** L3 on miss of L1/L2  
2. **Always write** L3 on novel stage (first-write-wins)  
3. **Local L2** write-through for node reuse  
4. Never block compile success on L3 put failure (best-effort; log + metric)

Matches existing `cas_put` warn-on-error behavior.

### 10.5 First-write-wins + poison

| Layer | FWW? | Repair |
|---|---|---|
| heart L1 | Yes (memory) | invalidate |
| heart L2 DiskCas | Yes (hardlink publish) | invalidate file |
| heart L3 S3 | Yes (If-None-Match / head) | quarantine + epoch prefix |
| L0 S3 outputs | Immutable | new input_hash |
| L5 source | Content key = hash(bytes) | delete corrupt; re-fetch |

**Corruption handling algorithm:**

```
on_decode_error(key):
  invalidate L1, L2
  metrics.poison++
  rebuild from source
  put_keyed (if L3 still bad and FWW blocks, use key‖"repair-{epoch}" only after human/epoch bump)

on_sampled_verify_mismatch(key):
  alert high severity
  bump global stage epoch E → prefix cas/stage/e{E}/
  dual-read old+new during window
```

### 10.6 Scope enum (normative product types)

```rust
/// Where stage/output cache entries may be stored and read.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CacheScope {
    /// Fleet-shared JobKey stages + public L0 (default server).
    GlobalStage,
    /// Node-local DiskCas only; no remote stage put/get.
    NodeLocal,
    /// Disable stage cache entirely (forensics / correctness A/B).
    Forbidden,
}

pub struct SharedCasConfig {
    pub scope: CacheScope,
    /// heart L1 entry count (StampedeCache capacity).
    pub l1_capacity: u64,
    /// DiskCas root; None → no L2.
    pub l2_root: Option<PathBuf>,
    /// object_store URL + prefix for stage CAS; None → NoL3.
    pub l3: Option<RemoteCasConfig>,
    pub write_l3: bool,
    /// 0.0–1.0 fraction of hits to recompute and compare.
    pub verify_sample_rate: f32,
    /// Prefix epoch for poison rotation: "e0", "e1", …
    pub stage_epoch: String,
}

pub struct RemoteCasConfig {
    pub url: String,          // s3://bucket/cas/stage/
    pub region: Option<String>,
    /// Max concurrent L3 ops
    pub concurrency: usize,
}
```

Wire into `ForgeConfig` / ORCH admission (ORCH only needs L0 + maybe stage precheck later).

---

## 11. Warm vs cold interaction matrix

| Event | Warm | Cold |
|---|---|---|
| L0 hit at ORCH | Both skip | Both skip |
| L1 remote hit | Fast path | Fast path (still pay pod start) |
| L1 local L2 hit | Common on sticky node | If hostPath CAS present |
| L4 RA warm | Yes | No |
| Populate L3 for others | Yes | Yes (bulk seeder) |
| Spot preemption | N/A (PDB) | Safe; L0/L1 make retry cheap |
| Image pull | Amortized | Can dominate if L1 hit + small package |

**Implication:** For cold Jobs, **L0** is the only way to avoid pod start cost. L1 helps when Job must run (first success race, or outputs not yet recorded) or when producing intermediate stages for partial pipeline.

---

## 12. Metrics & SLOs

### 12.1 Metrics (Prometheus-style)

| Metric | Labels | Meaning |
|---|---|---|
| `nudox_orch_l0_hit_total` | lang, tenant | Full skip |
| `nudox_orch_l0_miss_total` | lang | Dispatch |
| `nudox_cas_get_total` | tier=`l1\|l2\|l3`, result=`hit\|miss\|error` | Stage CAS |
| `nudox_cas_put_total` | tier, result=`novel\|exists\|error` | FWW stats |
| `nudox_cas_poison_total` | stage | Decode/verify fail |
| `nudox_cas_bytes_saved` | tier | Size of hit payloads |
| `nudox_compile_seconds` | lang, path=`warm\|cold`, cache=`l0\|l1\|none` | Wall time |
| `nudox_compile_seconds_saved` | layer | Estimated |
| `nudox_source_cas_hardlink_total` | result | L5 |
| `nudox_l2_lang_cache_forbidden_total` | lang | Policy guard |

### 12.2 SLOs / dashboards

| Signal | Target (steady bulk) |
|---|---|
| L0 hit rate (reindex traffic) | > 60% after ramp |
| L1 L3 hit rate (on L0 miss) | > 20% after 1 week corpus |
| p99 warm compile (cacheable popular) | << cold p99 |
| Poison rate | < 0.01% of gets |
| L3 put error rate | < 0.1% (best-effort) |

### 12.3 Estimating seconds saved

```
seconds_saved_l0 ≈ mean_compile_time_lang * l0_hits
seconds_saved_l1 ≈ mean_stage_build_time * l1_hits  # not full compile
bytes_saved      ≈ sum(payload_bytes on hits)
```

Report both **CPU-hours** and **wall-hours** (cold pod start not saved by L1 alone).

---

## 13. Quantified scenarios (“huge savings”)

Assumptions for illustration (order-of-magnitude, not SLAs):

| Package class | Cold compile wall | Stage lower CPU | Source tarball |
|---|---|---|---|
| Small (left-pad class) | 5–15 s (mostly start) | 0.5–2 s | < 1 MB |
| Medium (common lib) | 30–90 s | 10–40 s | 1–20 MB |
| Large (tokio/serde/react) | 2–10 min | 1–5 min | 10–100 MB |
| Monster | 30–120 min | large | large |

### 13.1 Reindex popular crate already in INDEX

- **L0 hit:** save **100%** of wall (30s–10min → ~50ms INDEX lookup).  
- **Huge** for crawlers and GUI “refresh”.

### 13.2 Two cold Jobs race same input (no L0 yet)

- Without coalesce: 2× cost.  
- With ORCH in-flight coalesce: 1× cost.  
- With L1 only (no coalesce): second may still hit L3 mid-flight if first put stages early — partial win.  
- **Recommendation:** coalesce at L0/ORCH first.

### 13.3 Version bump docs-only (README change)

- source_hash changes → JobKey miss → full L1 miss.  
- L5: most files hardlink hit.  
- L0: miss.  
- Savings: extract + some parse if per-file incrementality exists (today: limited; 01 §9.3 gap).  
- **Future:** per-file CST keys → large savings; **today:** moderate via L5 only.

### 13.4 Mass reindex after `producer_version` bump

- L0/L1: **mass miss** (keys include producer).  
- L3 images: hit if image unchanged (or partial).  
- L5 sources: **mass hit**.  
- Savings: **avoid re-download of all source corpora**; still pay CPU lower.  
- **This is the killer app for L5 + good image caching**, not for L1.

### 13.5 GUI interactive: open serde 5 times

- First: L0 miss → warm compile → L0 fill + L1 fill.  
- Next four: **L0 hit** (or L1 if L0 disabled) → instant.  
- Warm L4: minor if L0 already wins.

### 13.6 Same node, 200 packages sharing dependency sources

- L5 hardlink farm: disk amplification → near **unique bytes only**.  
- Aligns with edge-tech 03 “CAS already wins” narrative.

### 13.7 Summary table

| Scenario | L0 | L1 | L2 | L3 img | L4 | L5 | Overall |
|---|---|---|---|---|---|---|---|
| Popular reindex | **★★★★★** | ★ | — | ★ | — | ★ | **Huge** |
| Docs-only bump | — | — | — | ★ | — | **★★★** | Medium today |
| Producer bump mass | — | — | — | **★★★** | — | **★★★★★** | Huge I/O |
| First-seen long tail | — | — | — | ★★ | — | ★ | Small |
| Warm repeat same pod | ★★★★★ | ★★★★ | — | ★ | ★★ | ★★ | Huge |
| Untrusted L2 share | — | — | **risk** | — | — | — | **Don’t** |

---

## 14. Security model (consolidated)

### 14.1 Trust boundaries

```
[Package source bytes]  untrusted
        │ seal / cage
[Compiler process]      trusted compute (fleet image, signed)
        │ SA creds
[S3 stage + outputs]    trusted store (IAM)
        │
[ORCH / INDEX]          trusted control plane
        │
[Client]                authenticated
```

### 14.2 Rules of engagement

1. **Untrusted code never holds write credentials** to L0/L1 L3.  
2. **Network-off** inside cage for sealed produce; fetches are orchestrated.  
3. **Share only content-addressed immutable objects.**  
4. **L2 mutable language caches default Forbidden.**  
5. **Private packages:** authz on get by hash; optional tenant prefix.  
6. **Verify sampling** for L1 remote hits in early rollout.  
7. **Epoch bump** beats in-place L3 delete for poison.  

### 14.3 Threat → layer matrix

| Threat | L0 | L1 | L2 | L3 img | L4 | L5 |
|---|---|---|---|---|---|---|
| Cache poison write | Authz SA | SA + FWW + sample | High if shared | Supply chain | Reset state | Hash verify |
| Cross-tenant IR read | ACL | ACL | N/A | Low | Isolation | ACL |
| Disk DoS | Lifecycle | Lifecycle | Severe if shared RW | Node pull | RSS limits | Quota GC |
| Side channel existence | Low | Low | Med | Low | Med | Low |

---

## 15. Implementation plan (phased M0–M4)

Aligned with plan 12 migration phases but **cache-specific**.

### M0 — Instrument & freeze keys (1 week)

- Metrics for current pod-local hit/miss (`ForgeObserver.cache_hit/miss` already).  
- Document JobKey components; add tests that two containers with same image produce identical JobKey for fixture.  
- Define S3 prefixes: `outputs/`, `inputs/`, `cas/stage/e0/`, `cas/source/` (optional).  
- **Exit:** dashboard empty but scraping; key golden tests green.

### M1 — L0 ORCH output CAS complete

- INDEX unique input_hash; immutable outputs; admission short-circuit.  
- In-flight coalesce.  
- **Exit:** recompile same package = no Job created; metric `l0_hit`.

### M2 — Node-local L2 + L5 hardlinks on warm

- Mount `/var/lib/nudox/cas` hostPath on warm pool.  
- Materialize via hardlink when source blobs present; else fill CAS from tarball.  
- Daemon already supports `NUDOX_CAS_ROOT` — turn on in Deployment.  
- **Exit:** second compile on same node shows DiskCas hits; disk growth bounded by GC job.

### M3 — Remote L1 stage CAS (heart L3)

- Implement `ObjectStoreCas` for stage postcard blobs.  
- Replace `DaemonL3::None` with configured L3 when `SharedCasConfig.l3` set.  
- `CacheScope::GlobalStage` default in prod.  
- write_l3 from warm + cold.  
- verify_sample_rate=0.01 initially.  
- **Prerequisite progress:** same OCI digest fleet-wide OR toolchain content fingerprints.  
- **Exit:** cold Job B hits stage CAS from Job A; p50 CPU drops on duplicates.

### M4 — Hardening & policy

- `CacheScope` enforcement tests (Forbidden disables puts).  
- Poison epoch tooling; quarantine metrics.  
- Private-tenant ACL on stage get.  
- Optional sticky warm routing experiment (feature flag).  
- Explicit **L2 Forbidden** admission check (env scrub of shared cache paths).  
- GC lifecycle rules on `cas/stage/` (LRU by last-access if available; else TTLs by age).  
- **Exit:** security review signed off; runbooks for epoch bump.

### Post-M4 (optional)

- Per-file stage keys (incrementality 01 §9.3)  
- sccache only if measured need  
- composefs/EROFS sealed trees (edge-tech 03)  
- Multi-cluster stage replicate  

---

## 16. Concrete code touchpoints

| Change | Path |
|---|---|
| SharedCasConfig / CacheScope | New in `compiler` or `heart::cache` |
| ObjectStoreCas | `heart/cache` or `registry` store adapter |
| DaemonL3 → real L3 | `workspace/compiler/daemon/forge.rs` |
| Env wiring | `compiler_daemon.rs` + k8s manifests |
| Materialize hardlink | compiler materialize path / acquisition |
| ORCH L0 | plan 12 orchestrator (not yet in tree as final binary) |
| Observer metrics | `sandbox::ForgeObserver` + OTEL bridge |
| Toolchain fingerprint | `sandbox` ToolchainSet + JobKey producer v3 |

### 16.1 Suggested API surface

```rust
// heart::cache or compiler::daemon
impl ForgeConfig {
    pub fn with_shared_cas(mut self, cfg: SharedCasConfig) -> Self { … }
}

pub fn open_tiered_cas(cfg: &SharedCasConfig) -> Result<Tiered<impl Cas>, CasError> {
    match cfg.scope {
        CacheScope::Forbidden => Ok(Tiered::memory_only(0)), // or always-miss shim
        CacheScope::NodeLocal => {
            let l2 = cfg.l2_root.as_ref().map(DiskCas::open).transpose()?;
            Ok(Tiered::new(cfg.l1_capacity, l2, NoL3))
        }
        CacheScope::GlobalStage => {
            let l2 = cfg.l2_root.as_ref().map(DiskCas::open).transpose()?;
            let l3 = build_object_store_cas(cfg.l3.as_ref().expect("l3 required"))?;
            Ok(Tiered::with_l3(cfg.l1_capacity, l2, l3))
        }
    }
}
```

### 16.2 Write path pseudocode (already mostly true)

```text
cache_get_or_build(ctx, key, build):
  if bytes = cas_get(key):
    if decode ok: return value
    else: cas_invalidate(key)  # L1/L2 only
  value = build()
  cas_put(key, postcard(value))  # FWW; ignore exists
  return value
```

No change to generate pipeline required for M3 — only Cas backend wiring.

---

## 17. GC, capacity, cost

### 17.1 Stage CAS size model

Average postcard stage set (surface+cst+occ+archive meta) — estimate **100 KB–50 MB** per package version depending on language.  
1M package-versions × 5 MB avg = **5 PB** worst case — **not** all retained.

Policy:

| Class | Retention |
|---|---|
| Hot popular (top N) | Pin |
| Recent (30–90 d) | Keep |
| Long tail | Evict L3; keep L0 outputs longer (product) |
| Poison epoch old | Delete after dual-read window |

L0 outputs are **product**; L1 stages are **acceleration** — prefer evicting L1 first.

### 17.2 Node-local DiskCas GC

- LRU by mtime/atime under `/var/lib/nudox/cas`  
- Cap e.g. 50–200 GB/node  
- Never delete blobs mid-job (refcount or “don’t GC open hardlink targets”)

### 17.3 S3 cost controls

- Intelligent-Tiering for `cas/stage/`  
- Lifecycle expire incomplete multipart  
- Metrics: monthly $ vs CPU-hours saved  

---

## 18. Failure modes & operability

| Failure | User impact | Mitigation |
|---|---|---|
| L3 S3 outage | Compiles still work; slower | Best-effort put/get; degrade to L2/L1 |
| L2 disk full | Misses + put errors | GC + emptyDir fallback |
| Poison FWW sticky bad entry | Wrong IR until epoch | Sampling + version bump |
| Toolchain path digest skew | False L1 misses | Content fingerprints |
| ORCH L0 wrong success | Serve bad artifacts | Upload checksums; canary recompute |
| hostPath multi-pod contention | Slow disk | Per-node SSD; limit concurrency |

### Runbooks (short)

1. **Disable remote stage cache:** `NUDOX_CAS_SCOPE=NodeLocal` or `Forbidden`.  
2. **Epoch bump:** set `NUDOX_CAS_STAGE_EPOCH=e1`; deploy; expire `e0` after 7d.  
3. **Force recompile package:** delete INDEX success row + outputs prefix (admin).  

---

## 19. Interaction with librarification incrementality

Plan 01 §9.3 / 08-incremental aim at **symbol-level** deltas. Fleet cache topology is complementary:

| Incrementality feature | Fleet cache effect |
|---|---|
| Per-file CST keys | Higher L1 hit on docs-only / single-file edits |
| Symbol content hash | Downstream INDEX sinks skip; not stage CAS |
| RA keep-alive desktop | L4 library form; not fleet share |
| producer_version domain | Mass L0/L1 invalidate; L5 survives |

**Do not wait** for symbol-level incrementality to ship L0/L1/L5 — package-granularity CAS already pays for bulk reindex patterns.

---

## 20. Anti-patterns (explicit)

1. **Shared RW `target/` across pods** — forbidden.  
2. **Redis as IR CAS without content verify** — prefer S3+hash.  
3. **Depending on L1 for correctness** — always recompute-capable.  
4. **FUSE multi-tenant source daemon** — rejected (edge-tech 03).  
5. **Public IPFS for stage blobs** — rejected (edge-tech 04).  
6. **Sticky RA across tenants** — isolation hazard.  
7. **CRD objects as cache** — etcd abuse (plan 12).  
8. **Hashing toolchain by path only in multi-node fleet** — silent miss storm.  
9. **Letting package build scripts populate sccache with network on** — seal violation.  
10. **Treating warm L4 as durable** — pod death loses it; L0/L1 are durable paths.

---

## 21. Open questions

| # | Question | Options / lean |
|---|---|---|
| Q1 | Tenant-scoped stage prefix vs global for all public packages? | Lean **global for public**, scoped for private |
| Q2 | hostPath vs local PV vs emptyDir for L2 on Karpenter nodes | Lean **hostPath + GC** on sticky node pools; emptyDir on pure spot cattle |
| Q3 | Should cold Jobs write L3 stages or only final L0 outputs? | Lean **yes write L3** — seeds warm |
| Q4 | verify_sample_rate in steady state? | 1% → 0.1% after trust |
| Q5 | Sticky sessions for GUI packages? | Defer; measure L0/L1 first |
| Q6 | When to implement per-file stage keys? | After M3; pairs with 08-incremental |
| Q7 | sccache ever? | Only if RA path still invokes heavy rustc and metrics show win |
| Q8 | Private package L5 on shared nodes? | Prefer no hardlink share; copy into job dir |
| Q9 | Stage CAS in same bucket as outputs or separate? | Separate prefix/bucket for lifecycle independence |
| Q10 | Dual-read during `nudox-producer/3` JobKey migration? | Yes — coordinated flag day with dual-write window |

---

## 22. Recommendations (normative)

### 22.1 Today

| Layer | Shared? |
|---|---|
| L0 | **Designed yes** in plan 12; implement if not complete |
| L1 | **No fleet share** — pod MemoryCas + optional DiskCas; DaemonL3 none |
| L2 | **No** (and should not) |
| L3 images | **Yes** |
| L4 | **Warm pod only** |
| L5 | **Not yet** as shared node CAS hardlinks |

### 22.2 Should they?

| Layer | Decision |
|---|---|
| L0 | **Yes — required** |
| L1 | **Yes — shared read-mostly remote stage CAS + node-local DiskCas** |
| L2 | **No across untrusted tenants**; RO content-addressed deps only as L5/image |
| L3 images | **Yes** |
| L4 | **Warm pool only**; no cross-pod share |
| L5 | **Yes — node-local** |

### 22.3 One-sentence strategy

**Make ORCH L0 and JobKey stage L3 the fleet’s remote build cache; keep language targets unshared; hardlink sources on-node; warm processes for latency, not for correctness.**

---

## 23. Appendix A: End-to-end sequence (warm, L0 miss, L1 hit)

```
1. GUI → ORCH CompileRequest(serde 1.0.210)
2. ORCH input_hash = H(seal)
3. INDEX: no success → miss L0
4. ORCH in-flight map: none → reserve
5. ORCH presign GET input, PUT outputs
6. ORCH POST warm Service /compile
7. Warm pod: fetch input if needed
8. L5: hardlink source files from node CAS (partial hit)
9. generate_with:
   JobKey K = derive(...)
   cas_get(K): L1 miss, L2 miss, L3 HIT → postcard surface
   cas_get(K‖cst): L3 HIT
   cas_get(K‖occ): L3 HIT
   cas_get(K‖archive): L3 HIT
10. Assemble blob_info; upload outputs
11. INDEX success; L0 filled for next caller
12. Metrics: l1_l3_hit×4, compile_seconds low
```

### Appendix B: End-to-end sequence (cold, full miss)

```
1. Crawler → ORCH bulk
2. L0 miss, create Job
3. Kueue admits → spot pod
4. Image layers hit on node (L3)
5. Download tarball; L5 fill node CAS
6. All stage L1 miss → run producers under cage
7. put stages → L2 + L3 FWW
8. put outputs → L0
9. Job complete TTL GC
```

### Appendix C: Producer version bump mass reindex

```
1. Deploy producer_version = nudox-producer/3 (new JobKey domain)
2. L0/L1 keys all miss
3. L5 source CAS retains bodies → hardlink materialize fast
4. CPU-bound re-lower all packages
5. New L0/L1 population under new keys
6. Old cas/stage/e0 lifecycle expire after dual-read window if any
```

### Appendix D: Mapping heart tiers ↔ fleet layers

| Fleet layer | heart Tiered | Storage |
|---|---|---|
| L1 stage (part) | L1 StampedeCache | Process memory |
| L1 stage (part) | L2 DiskCas | Node disk |
| L1 stage (part) | L3 ObjectStoreCas | S3 |
| L0 | Not heart Cas | INDEX + S3 outputs |
| L5 | Parallel DiskCas tree | Node source bodies |
| L2 language | Out of band | Forbidden shared |
| L3 images | containerd | Node + registry |
| L4 | Process heaps | Warm pod RAM |

### Appendix E: Production cutover checklist + glossary + refs

**Cutover:** L0 INDEX unique + immutable outputs · same image digest or content toolchain fp · `SharedCasConfig` in daemon · IAM compiler SA for `cas/stage/*` + `outputs/*` + `inputs/*` · NetworkPolicy cage no raw cloud API · poison metrics/alerts · node CAS GC · runbooks (Forbidden scope, epoch bump) · load test 1k duplicates · security review.

**Glossary:** JobKey = BLAKE3 produce key · FWW = first-write-wins · L0–L5 = fleet layers herein · heart L1/L2/L3 = memory/disk/remote inside `Tiered` · stage = surface/cst/occ/archive · warm pool = long-lived Deployment · cold Job = Kueue one-shot.

**In-repo anchors:** `workspace/compiler/generate/mod.rs`, `compile/producer/runtime.rs`, `daemon/forge.rs`, `workspace/heart/cache/{mod,tiered,disk}.rs`, `heart/content.rs`, plans `12-orchestration`, `01-compiler-audit` §9, edge-tech `03-edenfs`, `04-ipfs-and-cas`, `05-surrounding-edge`.

**External prior art:** Bazel remote cache · sccache · Buck2 RE/deferred materialize · Nix narinfo · Gradle build cache · Turbo/Nx remote cache · kaniko layer cache · GitHub Actions cache.

---

## 24. Executive close

**Do compiler instances on k8s share a build cache?**  
Only at the **design** level for L0 (ORCH/INDEX output dedup) and **OCI image layers**. Stage CAS is pod-local (memory/optional disk); daemon has no remote L3. Language caches and RA warm state are not fleet-shared.

**How should they?**  
Share **immutable content-addressed** layers: L0 outputs, L1 JobKey stages via S3 + node DiskCas, L3 images, L5 source hardlinks. Do **not** share mutable L2 tool state across untrusted tenants. Keep L4 warm-pool-local.

**Huge savings** come from: (1) L0 on popular reindex, (2) L1 on duplicate sealed work after first lower, (3) L5 on mass reindex after producer bumps, (4) images on every cold start. The implementation is mostly **wiring `Tiered` L3 into the daemon** and **finishing ORCH L0** — the cache client (`cache_get_or_build`) and key discipline already exist.

---

*End of report. Research date 2026-07-16. Grounded in live workspace compiler/heart cache code and librarification plans 01/12 + edge-tech 03/04/05.*

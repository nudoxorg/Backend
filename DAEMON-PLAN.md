# DAEMON-PLAN: Sealed Compute — the compiler as a horizontally scalable daemon

*2026-07-09. Synthesized from six deep research passes over `util/sandbox`, `compiler`,
`server`+`runtime`, `registry`+`heart`+`util/caching`, a workspace-wide type audit, and the
build/deploy substrate. Supersedes the prose notes on SealedJob/Cage/CAS; incorporates and
extends them.*

---

## 0. Thesis

> Given a content-addressed input set, run a pure-ish function under a capability budget,
> and store a content-addressed output.

Everything in the compile plane — bwrap, Nix, worker pools, snix seals, parse cache,
profiles, overrides, the queue itself — is a **backend or a policy** for that one sentence.
The refactor makes that sentence the only API surface:

```
Acquire  →  Seal  →  Cas.get(key)  →  Cage.run(Producer)  →  Cas.put
  net✓      CA key      hit? done         net✗, budgeted        CAS out
```

And the daemon extends the same sentence horizontally: **the postgres queue is the only
scheduler, the CAS is the only shared memory, and a fleet node is anything that can run a
Cage.** A second replica isn't a special deployment mode — it's a second consumer of the
same queue writing to the same CAS. The design goal is that nothing above `Cage` knows
about bwrap, nothing above `Cas` knows about foyer or S3, and nothing above the queue knows
how many nodes exist.

---

## 1. Where we actually are (evidence)

The research pass confirmed the original diagnosis — "features accreted around producers" —
and found both more rot and more existing strength than expected.

### 1.1 What is already right (keep, and build on)

| Asset | Where | Why it matters |
|---|---|---|
| `Env` has no ambient constructor | `sandbox/spec.rs:16` | The gold standard; the pattern to extend to FS/net/toolchains |
| `Network::Off` default, `Limits` all-`NonZero`-required | `sandbox/spec.rs:115`, `limits.rs:27` | No silent-unlimited states |
| Registry blob store is a real CAS | `registry/store.rs:98-115` — `cas/{blake3}` + `ptr/{uuid}`, integrity-verified, idempotent puts | The L3 of the unified CAS already exists |
| `heart::ContentHash` is the universal identity primitive | `heart/content.rs:10-56` | The one key language; already imported everywhere |
| Postgres queue is genuinely multi-replica safe | `registry/queue/mod.rs:169-311` — `FOR UPDATE SKIP LOCKED`, leases, reclaim sweeper, lease-guarded complete/fail, `ON CONFLICT DO NOTHING` enqueue | Horizontal scheduling is a solved problem in this codebase |
| Transactional outbox | `registry/coordination.rs:157-193` — state + facets + fan-out intents in one tx | Correct exactly-once *recording*; consumption is the gap |
| `Connect` Cold→Live typestate | `heart/connection.rs:20` | Template for `Server` phases and `SealedInput` |
| Sealed `Plane`/`Mutable` capability planes | `registry/catalog.rs:28-49` | Template for capability budgets |
| `render::Backend` trait (6 impls) | `compiler/render/backend.rs:144` | The one healthy per-language trait; model `Producer` on it |
| `Ir<Stage>` typestate | `ir/pipeline/pipeline.rs:8` | Collected→Indexed transition is already type-driven |
| `StampedeCache` (XFetch + single-flight + jitter) | `util/caching/stampede.rs:48-221` | Fully built L1 — **wired to nothing** |

### 1.2 The rot, by layer

**Isolation is four half-cages plus a fifth informal one.**
- `LinuxBwrap::run` (`backend/linux.rs:235`), `run_direct_hardened` (`linux.rs:347`, used by
  nothing), `NixDerivation` (`nix_derivation.rs:142`), `MacSeatbelt`, `Passthrough`, plus the
  `WorkerPool` path (`worker.rs:158`) which is a peer with its own protocol, its own limits
  resolution, and its own env story.
- `NixDerivation` is unsound as a cage: wall timeout checked *after* `nix build` returns
  (`nix_derivation.rs:198-200`), exit status synthesized via `Command::new("true")`
  (`:231-233`), no rlimits, `peak_mem: None` — yet it claims `production_grade: true`.
- `WorkerPool` is a serial queue with hot spares: one `Mutex<Vec<WorkerSlot>>` around the
  whole submit body (`worker.rs:177` — "serialised for simplicity"). Pool size buys restart
  redundancy, zero parallelism. A hung worker blocks the mutex forever (`read_line` blocks;
  deadline is checked only pre-read, `worker.rs:316-329`).
- The worker cgroup is deliberately leaked: `std::mem::forget(cg)` at `worker.rs:281-283`.
  Every restart strands a cgroup directory. Fatal for a long-lived daemon.
- Seccomp BPF is recompiled and written to a temp file on **every** run (`linux.rs:200-212`).
- Production gate (`NUDOX_SANDBOX_REQUIRE`/`NUDOX_ENV`) is re-read from the environment in
  three places (`probe.rs:52`, `passthrough.rs:31-37`, `isolate.rs:390-396`); `require_worker`
  duplicates `IsolationPolicy::from_env` word for word.
- Per-package limit overrides are dead code: `overrides::resolve(profile, package_key)` is
  always called with `package_key = None` (`isolate.rs:277`). Worker pools bake
  `ProducerProfile::limits()` at spawn and never see the override table (`worker.rs:132,143`).
- `IsolatedCommand.env` is `Vec<(OsString, OsString)>`, not `Env` (`isolate.rs:136`) — the
  allowlist invariant is enforced one layer too late.

**Producers have no shared shape.**
- There is no `Producer` trait. `surface::collect` (`generate/surface.rs:24`) is a bare
  match over six producers with incompatible signatures; three arms have a
  `worker_index(...) or in-process` fork, three don't.
- `oracle_hash()` and `materialize_oracle()` are byte-identical between Go
  (`go/package.rs:168-195`) and Java (`java/oracle.rs:150-182`). `wire_members` is
  implemented three times (Go `context.rs:82`, Python `context.rs:246`, Nix `context.rs:88`).
- Nix hermeticity mutates process-global env with `unsafe remove_var`
  (`nix/eval.rs:129-155`) — a scrub-eval-restore window that races every concurrent job in
  the same process.
- TypeScript builds and tears down a full tokio runtime per package
  (`typescript/package.rs:63-66`). Nix/Python acquisition uses `reqwest::blocking`
  (`nix/traversal.rs:469-548`).
- The parse cache gates only the IR surface; CST extraction and source-archive building
  re-read the whole tree on every call, even on cache hit (`generate/mod.rs:77-97` vs
  `cst.rs`/`source_archive.rs`). Every generate walks the tree at least twice.

**Process globals everywhere.** Eleven-plus singletons across the compile plane:
`BACKEND`, `OVERRIDES`, `OBSERVER`, cgroup `SEQ`, `ToolchainPaths`, two worker `POOL`s,
parse `CACHE`, `CUSTOM_REGISTRIES`, `PROMETHEUS`, plus every temp path keyed on
`process::id()` + nanosecond timestamps (collision-prone across restarts; the rustdoc
wrapper path `nudox-rustdoc-wrapper-{pid}` at `rust/package.rs:230` has **no** timestamp and
collides between concurrent Rust jobs in one process).

**The fan-out half of the pipeline does not exist.**
`materialize()` in `server/poll.rs:97-109` is a traced no-op. Blobs are written, state goes
`Stored`, outbox intents are recorded — and tantivy/qdrant/terminus are never populated
from the pipeline. The `runtime/text/poll.rs` `Poller` is implemented and correct but never
started. Because the consumer is a stub, the fact that outbox consumption is not
multi-replica-safe (watermark advance races; no claim on outbox rows) has been invisible.

**Storage is one CAS described four ways.**
`ParseCache::CacheKey` (`sandbox/cache.rs:22`) is a structural re-invention of
`heart::ContentHash` (same `[u8; 32]` BLAKE3). Four cache stacks exist independently:
`ParseCache` (L1 map + L2 disk, sandbox-only), `EmbeddingCache` (bare moka),
`StampedeCache` (built, unwired), and `registry::Store` (L3 with no cache in front). No GC
on `cas/`, no outbox row purge, tantivy watermarks are in-memory and reset to 0 on restart
(`search/tantivy.rs:22,64`).

**Type-system debt.** `Probeable` has zero impls while five hand-rolled probe closures
duplicate it (`server/coordination/health.rs:102-167`); `AccessPolicy` is an empty marker
behind an `Arc<dyn>`; `SandboxError::Backend(String)` is the catch-all for 5+ distinct
failure modes; `HashMap<String, LimitOverride>` keys mix profile wire-names and package
keys; `Output { status, killed: Option<KillReason> }` represents impossible states;
`ResolutionState::Unindexed { needed: bool }` hides a real `Queued` state inside a bool.

### 1.3 Deployment reality

Single `server` binary + `producer-worker` sidecar. Linux-only production (bwrap + cgroup
v2 delegation + landlock + seccomp; darwin dev falls back to `Passthrough`). External
services: postgres, terminusdb, qdrant, object store, embeddings endpoint; per-job network
to crates.io/npm/PyPI/FlakeHub/GitHub. No containers, no CI deploy. Embedding model is
monomorphized at compile time (`server/main.rs:13`). Buck2 + Nix devshell; toolchains
provisioned via `/nix/store` bind-mounts inside the cage — which is exactly the right
provisioning story for a fleet.

---

## 2. Target architecture

### 2.1 The one job model

```rust
// heart (vocabulary — no I/O, no deps)
pub struct JobKey(ContentHash);          // H(producer_id ‖ toolchain ‖ source ‖ lock ‖ budget-relevant policy)

// cage crate
pub struct SealedInput {
    pub key: JobKey,
    pub tree: MaterializedTree,          // hash-pinned root, already local
    pub toolchain: ToolchainRef,         // /nix/store paths only, never ambient
    pub budget: CapabilityBudget,        // fully resolved — no re-resolution downstream
}

pub struct CapabilityBudget {
    pub fs: FsGrant,                     // RO roots + exactly one RW scratch (typed)
    pub net: NetGrant,                   // see typestate below — Off in Sealed phase
    pub env: Env,                        // existing allowlist newtype, unchanged
    pub resources: Limits,               // existing all-NonZero type, unchanged
}
```

**Invariant:** once a `SealedInput` exists, nothing ambient can be read. Construction
(`Sealer::seal`) is the *only* code allowed to touch host `PATH`/`HOME`/env/toolchain
discovery, and it must project into the allowlist. This single rule deletes three
mechanisms that currently exist in parallel: the process-env scrub in `run_isolated`, the
`SecretEnvGuard` process-global scrub in Nix eval, and the worker-spawn `env_clear`.
Workers and cages receive a projection; they never subtract from ambient state.

**Phases as types, not comments.** The acquire/seal boundary is the security model
(fetch-with-net vs parse-without-net — the docs.rs generalization). Make it
unrepresentable to get wrong:

```rust
pub struct Acquiring;                    // net: FOD-only (hash-pinned fetches allowed)
pub struct Sealed;                       // net: statically Off

pub struct Job<P: Phase> { ... }

impl Job<Acquiring> {
    pub fn seal(self, sealer: &Sealer) -> Result<Job<Sealed>, SealError>;
}
// Job<Sealed> has no method that can set NetGrant::On. Not "checked" — absent.
```

The same pattern seals Nix hermeticity: `NixCaps::hermetic()` is a builder whose type
cannot express `enable_impure`; `DocsIO`, pure builtins, and the env story become one
projection at seal time rather than three documented disciplines.

### 2.2 One cage

```rust
pub trait Cage: Send + Sync {
    fn id(&self) -> CageId;
    fn capabilities(&self) -> CageCaps;
    fn run(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Captured, CageError>;
}
```

Implementations, in order of trust:

| Impl | Substrate | Notes |
|---|---|---|
| `LinuxNamespaces` | bwrap + cgroup v2 + seccomp + landlock | Production default. Absorbs today's `LinuxBwrap`; `run_direct_hardened` is deleted (dead) |
| `WorkerPool` | pooled long-lived children *under* `LinuxNamespaces` | No longer a peer of the backend enum — it is how the cage runs library-form producers. Protocol becomes its private implementation detail |
| `DevPassthrough` | rlimits only | Constructible only from `Policy::Development`; the type does not exist in a production-policy runtime, replacing the thrice-read env gate |
| `NixDaemon` | real per-producer derivations | **Deferred** (see §5, Phase 7). The current generic Spec→drv wrapper is deleted, not fixed — its supervision model is unsound and half-generic drv wrapping is the wrong long-term form |

`MacSeatbelt` survives as a dev-only cage behind the same `Policy::Development` gate.

Fixes folded into the `LinuxNamespaces`/`WorkerPool` rewrite (all located, all mechanical):
- Per-slot concurrency: `slots: Vec<Mutex<WorkerSlot>>` + semaphore, not one pool mutex.
  Real parallelism = pool size.
- Worker I/O with deadlines: non-blocking pipe reads or a reaper thread; a hung worker
  costs one slot for one wall-limit, not the whole pool forever.
- Kill the cgroup leak (`worker.rs:281-283`): the pool owns each slot's `Cgroup` and drops
  it on restart.
- Compile the seccomp BPF once per policy into a `memfd`, dup the fd per spawn.
- Scratch/temp naming: `NodeId + JobKey` prefix (see §2.5), created via `tempfile`, never
  `pid + nanos`.
- `Output` → `Captured { stdout, stderr, end: ProcessEnd, wall, peak_mem }` with
  `enum ProcessEnd { Exited(ExitStatus), Killed(KillReason) }` — the impossible
  `killed`+`status` pair goes away.
- `CageError` replaces `SandboxError::Backend(String)` with enumerated variants
  (`CgroupWrite`, `SeccompCompile`, `WorkerProtocol { .. }`, `MountMissing { path }`, …).

### 2.3 Producers as pure functions

```rust
pub trait Producer: Send + Sync {
    const ID: ProducerId;                        // versioned: "rustdoc/3", "snix/1" — part of JobKey
    fn language(&self) -> Language;
    fn tier(&self) -> ThreatTier;                // Hostile | Untrusted | Trusted — drives budget defaults
    fn plan(&self, input: &SealedInput) -> ExecPlan;
    fn decode(&self, captured: Captured) -> Result<ProducerOutput, ProducerError>;
}

pub enum ExecPlan {
    Commands(Vec<SealedCommand>),                // rust (metadata+rustdoc), go, java (javac+javadoc)
    Library(WorkerLang),                         // nix, typescript, python → WorkerPool inside the cage
}

pub struct ProducerOutput {
    pub index: Index,
    pub aux: AuxOutputs,                         // typed side-channel: rust source map, etc.
}
```

Two rules with teeth:

1. **In-process is not an architecture, it's a dev policy.** Library-form producers
   (snix/deno/pyrefly) run in the worker cage *always* under `Policy::Production`; the
   in-process fallback exists only inside `DevPassthrough`-land. `surface.rs` loses every
   `if let Some(worker)` branch — dispatch is `runtime.run_producer(lang, sealed)` for all
   six languages, one code path.
2. **Language hermeticity is part of the producer's budget, not a parallel project.**
   The snix `DocsIO` seal, the deno `file://`-only loader, and pyrefly's config isolation
   are each that producer's implementation of "honor `FsGrant`/`NetGrant`" — required *in
   addition to* the OS cage for `ThreatTier::Hostile` interpreters, expressed as the same
   budget type, tested by the same escape suite.

Shared producer substrate (deletes the duplication inventory):
- `oracle::materialize(files: &[(&str, &str)], label) -> OraclePath` — one FNV hash + one
  content-addressed materialization for Go and Java (and any future oracle).
- `Scratch` handle carved from `FsGrant` — the only way to get a writable dir; replaces
  eleven ad-hoc `temp_dir()` call sites.
- `wire::members<S: PathSplit>(entries)` — one member-wiring pass generic over the
  language's path convention (Go import paths, Python qualnames, Nix attrpaths).
- One `IsolatedFailure` → error mapping: `ProducerError` carries the `CageError` source
  intact instead of three per-language stringly re-wraps.
- A shared, long-lived current-thread tokio runtime handle for deno (owned by the
  `WorkerPool` worker — which also makes the per-call-runtime problem moot in production,
  since the worker process is long-lived).

### 2.4 One CAS

New `cas` crate (or `heart::cas` module — see open question §6.2):

```rust
pub trait Cas: Send + Sync {
    async fn get(&self, key: ContentHash) -> Result<Option<Bytes>, CasError>;
    async fn put(&self, bytes: Bytes) -> Result<ContentHash, CasError>;       // hashes
    async fn put_keyed(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError>; // pre-hashed, verified
}

pub struct Tiered {
    l1: StampedeCache<ContentHash, Bytes>,   // finally wired; single-flight kills recompile stampedes
    l2: Option<DiskCas>,                     // node-local; foyer when vendoring lands, plain dir CAS until then
    l3: Store<Live>,                         // existing registry object store, unchanged semantics
}
```

- `ParseCache` and `CacheKey` are **deleted**; `JobKey` is a `ContentHash` derived exactly
  the way `CacheKey::derive` does today (length-prefixed, domain-separated) but living next
  to `ContentHash` in heart. `run_producer` is the only cache client:

```rust
async fn run_producer(rt: &ForgeRuntime, p: &dyn Producer, input: SealedInput)
    -> Result<ProducerOutput, ForgeError>
{
    if let Some(bytes) = rt.cas.get(input.key.0).await? {
        rt.observer.cache_hit(&input.key);
        return p_decode_cached(bytes);           // postcard, not JSON (see below)
    }
    let out = execute_plan(rt, p, &input).await?; // Cage::run per ExecPlan
    rt.cas.put_keyed(input.key.0, encode(&out)).await?;
    Ok(out)
}
```

- **This is the biggest horizontal-scaling win in the plan**: with L3 shared, replica A's
  compile of `serde@1.0.219` is a cache hit on replicas B..N. Today every replica
  recompiles everything (parse cache is process-local).
- CST extraction and source-archive building become CAS entries under their own derived
  keys (`H(jobkey ‖ "cst")`, `H(jobkey ‖ "archive")`) so a hit skips *all* tree walking,
  not just IR.
- Cached-value encoding moves from `serde_json` to `postcard` (the manifest already uses
  it) — the IR round-trip is on the hot path of every hit.
- GC: a mark phase walking live `ptr/` manifests + a grace window; plus outbox row purge
  below `min(sink_watermarks)`. Both are cron-shaped daemon duties (see §2.5).

### 2.5 The daemon: roles, not new binaries

One binary, three roles, selected by config (`NUDOX_ROLE=gateway|forge|all`, default `all`
for dev parity):

```
                       ┌────────────────────────── postgres ──────────────────────────┐
                       │   packages / parse_status / jobs / outbox / sink_watermarks   │
                       └──────┬──────────────────────┬──────────────────────┬──────────┘
   POST /packages             │ enqueue              │ SKIP LOCKED claim    │ outbox claim
        ▼                     │                      ▼                      ▼
 ┌────────────┐        ┌────────────┐        ┌──────────────┐       ┌──────────────┐
 │  gateway   │  ...   │  gateway   │        │    forge     │  ...  │    forge     │
 │ (read+api) │        │ (read+api) │        │ acquire/seal │       │ acquire/seal │
 └─────┬──────┘        └─────┬──────┘        │ cage+produce │       │ cage+produce │
       │ search               │              └──────┬───────┘       └──────┬───────┘
       ▼                      ▼                     │ blobs + IR           │
  local tantivy         local tantivy               ▼                      ▼
  qdrant/terminus       qdrant/terminus     ┌──────────────────────────────────┐
                                            │        CAS  (L3 object store)    │
                                            └──────────────────────────────────┘
```

**`ForgeRuntime` replaces all compile-plane globals.**

```rust
pub struct ForgeRuntime {
    pub node: NodeId,                        // stable per-node identity (config or hostname+uuid)
    pub policy: Policy,                      // Production | Development — resolved ONCE at boot
    pub cage: Arc<dyn Cage>,
    pub cas: Arc<Tiered>,
    pub toolchains: ToolchainSet,            // resolved store paths, hashed → part of every JobKey
    pub overrides: OverrideTable,            // typed keys: Profile(ProducerProfile) | Package(PackageId)
    pub observer: Arc<dyn ForgeObserver>,    // metrics on run_producer boundaries only
}
```

Owned by the server, passed down (`Arc`). Deletes: `BACKEND`, `OVERRIDES`, `OBSERVER`,
`ToolchainPaths` OnceLock, both `POOL`s, parse `CACHE`, and the env-var re-reads — tests
construct a `ForgeRuntime` with a fake cage/cas instead of fighting process state.
`NodeId` replaces `process::id()` in every cgroup/scratch/temp name:
`nudox-{node}-{jobkey8}-{seq}`.

**Scheduling: keep the queue, it's already right.** `SELECT … FOR UPDATE SKIP LOCKED` +
leases + reclaim is the horizontal work-distribution primitive; N forges need zero new
coordination. Additions only:
- **Lease heartbeat**: forges extend `lease_until` every lease/3 while a job runs, so
  `JOB_LEASE` can drop from 15 min to ~2 min and crashed-node jobs get reclaimed fast.
- **Cancellation propagation**: `drive_job`'s timeout currently abandons the blocking
  thread (`indexing.rs:266-269`). `Cage::run` takes a `CancelToken`; cancel = write
  `cgroup.kill`, which actually stops the subprocess tree. Graceful drain = stop
  dequeuing, cancel or finish in-flight, release leases, exit.
- **Priority/affinity** columns already exist (`priority int`); wire priority into
  `dequeue_batch` ordering. No affinity until multi-arch nodes exist.

**Fan-out: make outbox consumption real and replica-safe.** Two changes:
1. Wire the existing `runtime/text/poll.rs::Poller` (and its qdrant/terminus siblings) into
   `materialize()` — this is required product work regardless of the daemon (derived
   stores are currently never populated).
2. Claim semantics: replace the shared watermark race with per-sink claims — either
   `pg_advisory_lock(sink, source)` held by one consumer per sink (simplest; consumers are
   idempotent upserts so failover double-work is safe), or a `claimed_by/claimed_until`
   pair on outbox rows mirroring the jobs table. Advisory lock first; row claims only if
   per-sink throughput demands fan-in.

**Replica-local read state, made honest:**
- Tantivy: persist the sync watermark next to the index dir; rebuild-from-postgres stays
  the recovery story (it is a fine one), but restarts stop re-folding all history.
- Sessions: move `MemorySessionStore` behind the existing `SessionStore` trait to a
  postgres-backed impl (the join-semilattice merge makes last-write-wins safe), or accept
  sticky sessions and document it. Postgres impl preferred — the trait already exists.
- Semantic quota: per-replica quota is *declared* intentional (config comment + metric
  label `per_replica`), or moved to a postgres counter. Declare-intentional first.

**Boot contract per forge node** (from the deploy research): bwrap on PATH, cgroup v2
write delegation, `/nix/store` (or FHS toolchains), `producer-worker` co-located, network
to postgres + object store + package registries. `boot_check()` grows into
`ForgeRuntime::assemble(policy)` — a Cold→Live typestate like every other store, refusing
to produce a `Production` runtime without a production-grade cage.

### 2.6 Type-system consolidation (workspace-wide)

Beyond the new core types, the audit's findings become part of this refactor rather than a
side quest:

**Delete/replace:**
- `Probeable`: implement it on the five store types and route
  `server/coordination/health.rs` through it — or delete it. Implement; the five closures
  are the duplication.
- `AccessPolicy`: delete the `Arc<dyn>`; reintroduce as a real trait when hosted
  multi-tenancy exists.
- `Pipe` (nix options), `assert_probe_future_send`: delete.
- `IsolatedCommand`, `Spec`, `JobRequest` (as public surface): collapse into
  `SealedCommand`/`SealedInput`. `Spec`'s good bones (Env, Mounts→FsGrant, Limits) carry
  over as `CapabilityBudget` fields.

**Newtype/enum the stringly:**
- `SandboxKey::{Profile(ProducerProfile), Package(PackageId)}` replaces
  `HashMap<String, LimitOverride>` — and the package arm gets *wired* (it is the
  per-package threat-tier hook).
- `ProducerId`, `NodeId`, `JobKey`, `NodeIri` (graph emit), typed worker-protocol error
  kinds, `ProbeResult::{Healthy, Unhealthy{reason}}`.
- `ResolutionState`: split `Unindexed{needed}` into `Unindexed | Queued` (schema-visible;
  needs a state-string migration).
- `Failure.cause: ErrorDetails::Message` duplication: make `Failure.message` derived,
  give `ErrorDetails` real variants (`CageKilled{reason}`, `ToolchainMissing{..}`, …) so
  the `failure` jsonb column becomes queryable.

**Typestate where invariants live:**
- `Job<Acquiring>/Job<Sealed>` (net), `NixCaps::hermetic()` (impurity),
  `BlobBuilder<NoIr>→<HasIr>→…` (section attachment — deletes four runtime error variants),
  `Server` assemble phases (Cold→Ready), `ForgeRuntime` likewise.

**Error architecture:** add `OutboxError::{Codec, Index}` variants (kills the
`format!("codec: {e}")` sqlx-wrapping), enumerate `CageError`, keep the
per-crate thiserror discipline that is already good.

**DRY sweep (mechanical, from audit §5):** one `to_io_error` helper, one temp-root
constant, one prod-gate resolution, WOQL query helpers, shared skip-list constant for
tree walks.

---

## 3. What gets deleted or merged

| Today | After |
|---|---|
| `Spec` + `IsolatedCommand` + `JobRequest` + `ToolchainPaths` + ad-hoc env scrub | `SealedInput`/`SealedCommand` + `Sealer` (one construction site for ambient access) |
| `run` / `run_isolated` / `try_worker_lower` / `lower_once` / `worker_index` | `ForgeRuntime::run_producer` (cache → cage → cas) |
| `surface.rs` language arms with worker forks | `Producer` trait, uniform dispatch |
| Secret scrub + `DocsIO` + pure builtins (three mechanisms, documented thrice) | One seal-time projection; `NixCaps::hermetic()` typestate |
| `ParseCache` + `CacheKey` + `EmbeddingCache`-pattern + unwired `StampedeCache` + bare `Store` | One `Cas` trait, `Tiered{L1,L2,L3}`; `heart::ContentHash` the only key |
| `Backend` selection env var + `Selected` OnceLock + `run_direct_hardened` | `Cage` trait on `ForgeRuntime`; dead path deleted |
| `WorkerPool` as backend-peer with own protocol/limits | `WorkerPool` as `Cage` implementation detail; per-slot locking; owned cgroups |
| `NixDerivation` generic Spec→drv wrapper | Deleted; revisit as real per-producer derivations (Phase 7) |
| 7 sandbox globals + 4 compiler globals + config-into-global installs | `ForgeRuntime` owned by server |
| `Output{status, killed}` | `Captured{end: ProcessEnd}` |
| `SandboxError::Backend(String)` | enumerated `CageError` |
| 3× prod-gate env reads, `require_worker` dup | `Policy` resolved once at assemble |
| Observer bolted onto supervise internals | `ForgeObserver` events at `run_producer` boundaries: hit/miss/kill/wall/restart |
| `materialize()` stub + unstarted `Poller` | Real sink consumers with per-sink claims |
| pid+nanos temp/cgroup names | `NodeId`+`JobKey` names via `tempfile` |

---

## 4. Crate topology after

```
heart        vocabulary: ContentHash + JobKey, identities, Language, ResolutionState,
             Connect, Retryable, budget-primitive types (Limits stays here or moves in)
cas          Cas trait, Tiered, DiskCas; registry::Store implements the L3 face
cage         (from util/sandbox) Cage trait, LinuxNamespaces, WorkerPool, DevPassthrough,
             CapabilityBudget, Sealer, SealedInput/SealedCommand, Policy, probes, escape tests
compiler     Producer trait + six producers (pure fns of SealedInput), IR, render, graph
             projection, treesitter — no isolation code, no env reads, no caches
registry     postgres (GlobalStore/Queue/Outbox/schema), blob manifest+emit, ingest, search
runtime      read plane (unchanged shape) + real sink materializers
server       roles (gateway|forge|all), ForgeRuntime assembly, HTTP, drive_job loop
util/caching absorbed into cas (StampedeCache moves; jitter/single_flight stay reusable)
```

Dependency direction stays strictly: `heart ← {cas, cage, compiler, registry, runtime} ← server`.
The compiler crate ends up with **zero** `std::env` reads (currently 14+ sites) — that is a
CI-enforceable lint (`disallowed-methods` in clippy.toml for `std::env::var*` outside
`cage::seal` and `server::config`).

---

## 5. Migration phases

Each phase is independently shippable and gated by the existing test suites
(compiler offline tests; `SERVER_TEST_BACKENDS=1` integration flow; `escape.rs` with
`NUDOX_ESCAPE_GATE` stays green throughout). Order follows the original streamline order,
with hygiene first and scale-out after the abstractions exist.

**Phase 0 — Hygiene (no behavior change).**
Merge `require_worker` into `IsolationPolicy::from_env`; single prod-gate resolution.
Implement `Probeable` ×5, delete `AccessPolicy` dyn, delete `Pipe`/`run_direct_hardened`.
`to_io_error` helper; shared skip-list; `OutboxError::{Codec,Index}`; `ProcessEnd` enum;
fix the cgroup leak and the pid-only rustdoc-wrapper collision; `tempfile` everywhere.
*Risk: nil. Value: immediate DRY + two real bugs fixed.*

**Phase 1 — One CAS.**
`cas` crate; `ContentHash` absorbs `CacheKey`; wire `StampedeCache` as L1, plain-dir L2,
registry `Store` as L3. `run_producer`-shaped cache wrapper around `generate()` (keep the
current call sites, change the storage). Add CST/archive as keyed CAS entries; switch
cached encoding to postcard. Persist tantivy watermarks.
*Gate: `indexing_flow.rs` twice-in-a-row = second run all-hit; cross-process hit test.*

**Phase 2 — One cage.**
`Cage` trait + `SealedCommand`/`SealedInput`/`CapabilityBudget`/`Sealer` in the `cage`
crate. Port `LinuxBwrap`→`LinuxNamespaces` (seccomp memfd cache, `Captured`, `CageError`).
Rebuild `WorkerPool` as cage-internal: per-slot locks, deadline-safe I/O, owned cgroups,
budget from the (typed, wired) override table. Delete `IsolatedCommand`; producers call
through a thin shim this phase.
*Gate: escape suite green; concurrent-jobs stress test (N producers in parallel, no
serialization, no cgroup residue).*

**Phase 3 — Producer trait.**
Introduce `Producer` + `ExecPlan` + `ProducerOutput`; port all six; collapse `surface.rs`;
shared oracle/materialize/wire_members/scratch helpers; delete the per-language error
re-wraps. Nix env projection moves to seal time (delete `SecretEnvGuard`'s process-global
mutation — the worker child gets a projected env instead). Deno runtime owned by worker.
*Gate: all per-language offline tests; `nix_env_leak` test rewritten against seal
projection.*

**Phase 4 — ForgeRuntime.**
Kill every remaining OnceLock; `ForgeRuntime::assemble(policy)` Cold→Ready typestate;
config → resolved `Policy` + typed `OverrideTable` injected; `NodeId`; observer moved to
`run_producer` boundaries. Server owns the runtime; tests construct fakes.
*Gate: full integration suite; a unit test proving two `ForgeRuntime`s coexist in one
process (the multi-tenant smoke test).*

**Phase 5 — Daemon roles + real fan-out.**
`NUDOX_ROLE`; lease heartbeat + shortened lease; `CancelToken` → cgroup.kill; graceful
drain. Wire real materializers (text Poller + qdrant + terminus sinks) behind per-sink
advisory-lock claims; outbox purge + CAS GC cron duties. Sessions → postgres-backed
`SessionStore` impl. Priority-ordered dequeue.
*Gate: two-replica integration test — N packages enqueued, two `forge` processes against
one postgres/CAS, assert disjoint claims, all `Stored`, derived stores populated,
kill-one-mid-job reclaim works.*

**Phase 6 — Hermeticity typestates + threat tiers.**
`Job<Acquiring>/Job<Sealed>`; `NixCaps::hermetic()`; `BlobBuilder` typestate;
`ThreatTier` per producer driving budget defaults (Hostile interpreters get
language-hermeticity assertions in addition to the cage); per-package `SandboxKey`
overrides live end-to-end.
*Gate: compile-fail tests (`trybuild`) proving net-on-in-Sealed and impure-hermetic don't
compile.*

**Phase 7 — Substrate options (only now).**
Real per-producer Nix derivations (crane/rustdoc) as a `NixDaemon` cage where the command
is naturally a derivation — CAS-integrated, not Spec-wrapped. Foyer L2 when vendoring
lands. Arena `&'a` IR (existing plan — the `Producer` trait's `decode` boundary is where
the arena gets rooted). MicroVM cage tier if hostile-code posture demands it — a third
`Cage` impl, no layer above changes.

---

## 6. Open questions (decide before their phase, not before starting)

1. **Queue vs. outbox for sink work at scale** (Phase 5): advisory-lock-per-sink is one
   consumer per sink kind fleet-wide. If a single sink can't keep up, promote outbox rows
   to claimable work items (jobs-table pattern). Start with the lock; the schema change is
   additive later.
2. **Where `Cas` lives** (Phase 1): separate `cas` crate vs `heart::cas`. Separate crate
   preferred — heart stays I/O-free (it currently has zero async and zero deps; keep that
   property).
3. **JobKey inputs** (Phase 1): does the budget hash into the key? Resource limits: no
   (a re-run with a bigger memory cap should hit). Capability-relevant policy (net mode,
   hermeticity level, producer version): yes. Draw the line as "anything that changes
   output bytes."
4. **Worker protocol evolution** (Phase 2): line-JSON is fine at current scale; add a
   version header when it becomes cage-internal so it can change freely later.
5. **Embedding-model monomorphization** (`Server<M>`) fights role-flexible fleets
   (gateway needs M, forge doesn't). Consider `dyn Embedder` at the gateway boundary in
   Phase 5 — measured, since the const-generic dimension safety is real.
6. **snix GPL** (standing): the worker-process boundary from Phase 3 is also the GPL
   boundary if a separately-licensed `nix-oracle` binary is ever needed. No action now;
   the architecture stops foreclosing it.

---

## 7. One-line summary

Isolation becomes *the* execution model — content-addressed pure compute under a typed
capability budget — with one cage, one CAS, one producer trait, and one runtime object;
the daemon is then just N consumers of the queue sharing the CAS, and every current
special case (workers, Nix seals, parse cache, profiles, overrides, dev passthrough)
is demoted to a backend or a policy of that model.

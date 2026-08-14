# K8s Orchestration Design for Untrusted Compiler Fleet

**Research date:** 2026-07-16  
**Scope:** Queueing, hybrid warm/cold paths, poison/retry without Postgres, sandbox layering, artifact flow, autoscaling, and a thin Rust orchestration server for nudox compile jobs.  
**Audience:** Architecture decision record for dropping the Postgres job queue and deploying the compiler as a stateless sandboxed fleet.

---

## 0. Problem framing

nudox today drives compile jobs through a **Postgres-backed job queue** (poison-pill-safe, exponential backoff) coordinated by an axum server. The librarification plan drops Postgres for job orchestration: the **compiler** becomes a nearly-stateless binary (sealed inputs → IR + blobs), deployed as a fleet; an **orchestration server** uses Kubernetes primitives for load-balancing and queueing instead of rolling a queue into the registry/index.

Workload characteristics that drive design:

| Property | Implication |
|---|---|
| Untrusted third-party package builds | Need microVM-class or better isolation; defense in depth |
| Bursty arrival (new versions + GUI on-demand) | Separate interactive warm path from cold bulk path |
| Outputs → S3-compatible object store | Don't tunnel large blobs through orchestrator |
| Job metadata → sqlite INDEX | INDEX is source of truth for clients; k8s objects are ephemeral execution |
| Idempotent content-addressed inputs | Spot preemption and retry are safe if keys are CAS |
| Existing sandbox: bwrap + landlock + cgroups | Must either nest inside containers or replace at pod runtime |

Recommended shape (evaluated in §2): **thin orchestration server (Rust / kube-rs) + Kueue-managed Jobs for cold bulk + warm Deployment of compiler-server pods for interactive builds**. No Postgres. Optional NATS only if warm-path backpressure or fan-out events need a durable bus.

---

## 1. Queueing decision space (2026)

### 1.1 Kubernetes Jobs + Indexed Jobs + Kueue

#### Kubernetes Jobs (built-in)

Core controls ([Jobs docs](https://kubernetes.io/docs/concepts/workloads/controllers/job/)):

- **`backoffLimit`** — max pod failures before Job fails. Failed pods recreated with exponential backoff (10s, 20s, 40s … capped at 6 minutes).
- **`activeDeadlineSeconds`** — wall-clock deadline for the whole Job; takes precedence over `backoffLimit`.
- **`podFailurePolicy`** — **GA in Kubernetes 1.31** ([blog](https://kubernetes.io/blog/2024/08/19/kubernetes-1-31-pod-failure-policy-for-jobs-goes-ga/)); still stable through 1.36. Classify failures by exit codes / pod conditions; fail Job immediately on non-retriable errors; ignore disruption-related failures so spot preemption does not burn retries.
- **`backoffLimitPerIndex`** — GA in 1.33 ([blog](https://kubernetes.io/blog/2025/05/13/kubernetes-v1-33-jobs-backoff-limit-per-index-goes-ga/)); useful for Indexed Jobs with independent work items.
- **Pod Replacement Policy** — GA in 1.34; controls when a failed pod is replaced (e.g. wait for full termination).
- **`successPolicy`** — GA in 1.33 for Jobs; declare success when a subset of indexes complete.
- **Indexed Jobs** — completion mode `Indexed` for fan-out of homogeneous work units; less natural for heterogeneous package builds (one Job per package is simpler).

**Fit for compile jobs:** Excellent unit of execution for *cold bulk* builds (one Job per sealed input hash). Not ideal alone for *interactive* latency (pod start + image pull + scheduler latency dominates).

#### Kueue (Kubernetes-native job queueing)

**Verified version (2026-07-16):** **v0.18.3** — install via  
`kubectl apply --server-side -f https://github.com/kubernetes-sigs/kueue/releases/download/v0.18.3/manifests.yaml`  
or Helm chart `0.18.3` ([installation docs](https://kueue.sigs.k8s.io/docs/getting-started/installation/), [releases](https://github.com/kubernetes-sigs/kueue/releases)).

**Requirements:** Kubernetes **≥ 1.29**.

**What Kueue is:** A quota-aware admission controller for batch workloads. It decides *when* a Job (or other workload) may create pods, *when* it waits, and *when* it is preempted. It does **not** replace kube-scheduler, cluster autoscaler, or Job lifecycle ([overview](https://kueue.sigs.k8s.io/docs/overview/)).

**Core primitives:**

| Primitive | Role |
|---|---|
| **ClusterQueue** | Cluster-scoped quotas, flavors, preemption, queueing strategy (`StrictFIFO` / `BestEffortFIFO`) |
| **LocalQueue** | Namespaced handle; Jobs select `kueue.x-k8s.io/queue-name` |
| **ResourceFlavor** | Heterogeneous capacity (spot vs on-demand, arch, GPU class) |
| **Cohort** | Share/borrow quota across ClusterQueues |
| **Workload** | Internal representation Kueue creates per admitted job |
| **WorkloadPriorityClass** | Priority for admission/preemption |
| **AdmissionCheck** | External gate (e.g. provisioning request to cluster-autoscaler/Karpenter) |

**Fair sharing & preemption** ([fair sharing](https://kueue.sigs.k8s.io/docs/concepts/fair_sharing/), [preemption](https://kueue.sigs.k8s.io/docs/concepts/preemption/)):

- Dominant Resource Share (DRS) based fair sharing among tenants in a Cohort.
- Preemption strategies under fair sharing include `LessThanOrEqualToFinalShare` and `LessThanInitialShare`.
- Borrow-within-cohort: pending work can preempt others using more than nominal quota.
- Flavor fungibility: if one flavor is full, admit on another (e.g. spot → on-demand for high priority).

**Integrations (native):** Batch Job, JobSet, Kubeflow training jobs, Ray Job/Cluster, AppWrapper, plain Pods/PodGroups, Deployments/StatefulSets (for mixing training + inference capacity), MultiKueue multi-cluster dispatch ([features overview](https://kueue.sigs.k8s.io/docs/overview/)).

**v0.17–v0.18 highlights (2026):** Workload Variants for parallel flavor probing ([Medium summary](https://medium.com/google-cloud/kueue-v0-18-whats-new-19fb199235d1)); MultiKueue batch job features promoted to stable; LocalQueue metrics beta.

**Is Kueue the right primitive for nudox cold path? YES** — with caveats:

| Need | Kueue covers? |
|---|---|
| Fair sharing between bulk indexing and interactive tenants | Yes (Cohorts + fair sharing) |
| Spot vs on-demand flavors | Yes (ResourceFlavors) |
| Priority: GUI-driven package > background crawl | Yes (WorkloadPriorityClass) |
| Preempt bulk when interactive needs CPU | Yes |
| Poison-pill / application-level retry semantics | Partial — use Job `podFailurePolicy` + INDEX dead-letter; Kueue handles quota re-admission |
| Sub-second interactive latency | No — use warm pool instead |
| Replace message queue for request/response | No |

**Anti-pattern to avoid:** Treating Kueue Workload CR as long-lived application state. Workload is admission state; durable job status lives in INDEX sqlite.

---

### 1.2 KEDA ScaledJobs / ScaledObjects

**Verified version:** **KEDA v2.20.1** (docs line: 2.20 latest; next estimated ~Sep 2026) ([releases](https://github.com/kedacore/keda/releases), [deploy](https://keda.sh/docs/2.20/deploy/)). Compatibility table lists v2.20 for k8s **v1.33–v1.35** — verify against your cluster version if on 1.36.

**ScaledObject** — scale Deployments/StatefulSets via HPA that KEDA manages; event sources include Prometheus, NATS JetStream, Redis Lists/Streams, RabbitMQ, AWS SQS, HTTP add-on (experimental), etc. ([scalers](https://keda.sh/docs/2.20/scalers/)).

**ScaledJob** — create Job instances from queue depth ([scaling jobs](https://keda.sh/docs/2.20/concepts/scaling-jobs/)).

**Critical constraint for nudox:** There is **no SQLite scaler**. Without Postgres/queue, pure KEDA-as-queue fails. Viable sources if you introduce a bus:

| Source | Role |
|---|---|
| Prometheus metrics (queue depth gauge from orchestrator) | Scale warm pool without a message broker |
| NATS JetStream scaler | Scale consumers if NATS is the warm backlog |
| HTTP add-on | Experimental; queue pending HTTP requests — useful for scale-to-zero APIs, immature for production compile admission |
| Kueue pending metrics | Prefer Kueue for batch; KEDA less necessary for cold Jobs |

**Verdict:** Use KEDA **optionally** for warm-pool HPA (Prometheus or concurrency metrics). Do **not** make KEDA ScaledJobs the primary cold-path queue when Kueue already admits Jobs against cluster quota. Overlapping Kueue + KEDA-on-Jobs double-controls admission and confuses operators.

---

### 1.3 Argo Workflows / Tekton

| Engine | Strength | Overkill for nudox? |
|---|---|---|
| **Tekton** | CI/CD CRDs (Task, Pipeline, PipelineRun), Triggers, Chains provenance | Yes for single-step compile — designed for multi-step pipelines |
| **Argo Workflows** | General DAG engine; each step a pod; strong for ETL/ML | Yes for single sealed-input → IR jobs |

([comparisons](https://kubernetes.ae/tekton-vs-argo-workflows/), [Platform9 context](https://platform9.com/blog/argo-cd-vs-tekton-vs-jenkins-x-finding-the-right-gitops-tooling/))

**Verdict: OVERKILL.** Compile jobs are not multi-stage DAGs. You pay for CRD surface, controllers, artifact sidecar patterns, and operational complexity without needing steps/DAG semantics. Keep regression/fuzz orchestration as separate CronJobs or a small JobSet later — do not adopt Argo/Tekton as the compile queue.

---

### 1.4 Deployment + HPA + lightweight in-cluster queue

#### Pattern

```
Client → Orchestrator (admission) → NATS JetStream work-queue
                                       ↓
                              Compiler Deployment consumers
                              (or HTTP warm pool with 503 backpressure)
```

#### NATS JetStream (preferred lightweight queue if needed)

**Verified versions (2026-07-16):**

| Component | Version | Source |
|---|---|---|
| NATS Server | **v2.14.3** (latest; 2.12/2.14 supported lines) | [download](https://nats.io/download/), [releases](https://github.com/nats-io/nats-server/releases) |
| async-nats (Rust) | **0.49.1** (2026-06-04) | [crates.io](https://crates.io/crates/async-nats), [docs.rs](https://docs.rs/async-nats) |
| Helm | `nats/nats` official chart | [k8s docs](https://docs.nats.io/running-a-nats-service/nats-kubernetes) |

**Work-queue semantics** ([NATS by Example Rust](https://natsbyexample.com/examples/jetstream/workqueue-stream/rust)):

- Retention: `WorkQueue` — message removed once acked; consumers with overlapping filters disallowed (error 10099).
- Ack policies: none / all / **explicit** (use explicit for work queues).
- **`MaxDeliver`** — poison-pill ceiling; after N deliveries message stops being offered ([consumers](https://docs.nats.io/using-nats/developer/develop_jetstream/consumers)).
- **`BackOff`** sequence on consumer — progressive redelivery delays (e.g. 1s, 5s, 1m, 5m).
- **`NakWithDelay`** — app-level exponential backoff on explicit NAK (distinct from auto-redelivery after AckWait).
- **DLQ pattern:** subscribe to advisories `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.<STREAM>.<CONSUMER>`; advisory carries `stream_seq` to fetch original message ([DLQ guide](https://streamtrace.io/articles/nats-jetstream-dead-letter-queue-implementation/)).
- Persistence: file storage + R3 cluster for HA; memory storage for pure cache (not recommended for job queue).
- At-least-once delivery with idempotent consumers → effective exactly-once *effects* when CAS outputs + INDEX unique on input hash.

**Vs Redis Streams:**

| Dimension | NATS JetStream | Redis Streams |
|---|---|---|
| k8s-native ops | Excellent; single binary + JetStream | Good if Redis already present |
| Work-queue + MaxDeliver | First-class | Manual (XPENDING + XCLAIM + custom max attempts) |
| Rust client maturity | async-nats JetStream complete | fred 10.x streams feature; redis-rs aio |
| Throughput (order of magnitude, 2026 blogs) | Higher producer rates in some benches | Lower latency p99 claimed |
| Footprint | Small | Small–medium; memory-bound |
| DLQ | Advisory stream pattern | DIY dead stream |

([2026 broker comparisons](https://dev.to/young_gao/real-time-event-streaming-kafka-vs-redis-streams-vs-nats-in-2026-34o1))

**RabbitMQ:** Heavier operator footprint; classic queues work but unnecessary vs NATS for this scale (100s–1000s jobs/burst).

**When to use NATS in nudox architecture:**

1. **Optional:** completion events to INDEX subscribers / GUI websockets.
2. **Optional:** warm-path overflow when all compiler pods saturated and you want durable backlog instead of client-side retry.
3. **Not required** if warm path is pure HTTP admission with 503 + Retry-After and cold path is pure Kueue Jobs.

**Recommendation:** Start **without** NATS. Add JetStream only when you need durable async backlog or multi-subscriber events. Prefer content-addressed HTTP request/response on the warm path.

#### Redis Streams (alternative)

Use only if Redis is already mandatory for caching/rate-limit. Pattern: `XADD` → consumer group `XREADGROUP` → `XACK`; reclaim stuck messages via `XCLAIM`/`XPENDING`. Poison detection requires app-level attempt counters. Rust: [fred](https://lib.rs/crates/fred) or redis-rs.

---

### 1.5 Custom operator / CRD-as-queue (kube-rs)

#### kube-rs 2026 state

**Verified version:** **kube 4.0.0** (2026-06-16), aligned with Kubernetes 1.36 / k8s-openapi 0.28 ([changelog](https://kube.rs/changelog/), [GitHub](https://github.com/kube-rs/kube)).

Prior majors: 3.0.0 (Jan 2026, k8s 1.35), 3.1.0 (Mar 2026), 2.0.0 (Sep 2025), 1.0.0 (May 2025 — first 1.x).

**Controller toolkit:**

- `Controller` reconciler, `watcher`, `reflector`, `Store`
- Server-side apply via client patch APIs
- Leader election (lease-based patterns in controller-rs examples)
- `#[derive(CustomResource)]` + `KubeSchema` / CEL client-side validation (`kube/cel` feature)
- `PredicateConfig` TTL cache (critical for high-churn Job watches)
- `RetryPolicy` default on client (4.0)
- Aggregated discovery API for faster startup

**Fit:** Ideal for a thin orchestrator that creates Jobs, watches completion, updates INDEX — **not** for storing every queue message as a CR.

#### etcd limits (CRD-as-queue breaks when…)

Authoritative limits ([etcd limits](https://etcd.io/docs/v3.3/dev-guide/limit/), operational guidance):

| Limit | Value | Impact |
|---|---|---|
| Request / object size | **~1.5 MiB** request; **~1 MiB** practical object | Sealed input blobs **must not** live in CRDs |
| etcd DB size | Soft ceiling **~8 GB** | High Job/CR churn bloats history |
| Object count pressure | Latency climbs past tens of thousands of objects | List/watch slowdowns |
| Write/churn rate | Creating thousands of short-lived Jobs/min stresses apiserver | Control plane saturation |

([Why etcd breaks at scale](https://learnkube.com/etcd-breaks-at-scale), [AKS large workloads](https://docs.azure.cn/en-us/aks/best-practices-performance-scale-large))

**CRD-as-queue anti-pattern:** Creating a `CompileJob` CR per request with full payload, never garbage-collecting, listing all CRs every reconcile. Breaks at:

- Payload size → 1 MiB wall (store S3 keys only).
- Sustained create/delete of thousands/min without TTL → etcd growth + watch storms.
- Using etcd as application database for retry counts, logs, build output.

**Safe use of CRDs:** Optional thin `CompileRequest` with **only** content hash, priority, S3 URI refs, status enum — GC after terminal state (TTL controller or owner finalizer). Prefer standard **Job** objects (already first-class) labeled with input hash; orchestrator watches Jobs, not a parallel CR universe.

**Mitigations if Jobs churn is high:**

- `ttlSecondsAfterFinished` on Jobs (e.g. 300–3600s)
- Low successful/failed history on any CronJob wrappers
- Namespace per tenant with ResourceQuota
- Avoid storing logs in Job status annotations

---

## 2. Recommended architecture

### 2.1 Hybrid: warm pool + Kueue cold Jobs

```
                         ┌──────────────────────────────────────┐
                         │           Clients (GUI / API)         │
                         └───────────────┬──────────────────────┘
                                         │ HTTPS
                                         ▼
                         ┌──────────────────────────────────────┐
                         │  Orchestration Server (Rust/axum)     │
                         │  - authn/authz, rate limit            │
                         │  - CAS dedup (INDEX + in-flight map)  │
                         │  - path select: warm vs cold          │
                         │  - presign S3 URLs                    │
                         │  - create Jobs / call warm Service    │
                         │  - HA: leader election for controllers│
                         └───────┬───────────────────┬──────────┘
                    interactive  │                   │  bulk / low priority
                    (latency)    │                   │  (throughput)
                                 ▼                   ▼
              ┌──────────────────────────┐   ┌────────────────────────────┐
              │ Warm pool                │   │ Cold path                  │
              │ Deployment:              │   │ Job + Kueue LocalQueue     │
              │  compiler-server pods    │   │ ResourceFlavor: spot/od    │
              │ Service + HPA/KEDA       │   │ podFailurePolicy           │
              │ concurrency gate/pod     │   │ runtimeClass: gvisor       │
              │ runtimeClass: gvisor     │   │ Karpenter NodePool spot    │
              └────────────┬─────────────┘   └─────────────┬──────────────┘
                           │                               │
                           └───────────────┬───────────────┘
                                           ▼
                         ┌──────────────────────────────────────┐
                         │ S3-compatible object store            │
                         │ inputs/sha256/...  outputs/sha256/... │
                         └───────────────────┬──────────────────┘
                                             │ completion
                                             ▼
                         ┌──────────────────────────────────────┐
                         │ INDEX (sqlite service)                │
                         │ job rows, poison pills, blob pointers │
                         └──────────────────────────────────────┘
```

### 2.2 Path selection rules

| Signal | Path |
|---|---|
| GUI interactive request, expected small/medium package | **Warm** HTTP to compiler-server |
| Background crawl / bulk registry import | **Cold** Kueue Job |
| Warm pool saturated (503 or concurrency full) | Spill to cold Job **or** client Retry-After (prefer Job for durability) |
| Known monster package (resource class XL) | Cold Job with large ResourceFlavor only |
| Same `input_hash` already Succeeded in INDEX | Return cached outputs (no compile) |
| Same `input_hash` in-flight | Coalesce: attach waiter to existing job id |

### 2.3 Warm path: queue vs direct HTTP

The compiler is "a server itself." Analysis:

| Approach | Pros | Cons |
|---|---|---|
| **Direct HTTP + Service LB + per-pod concurrency limit** | Lowest latency; simple; backpressure via 503/429 | No durable backlog; client must retry |
| **NATS work-queue consumers** | Durable backlog; MaxDeliver poison handling | Extra hop; ops surface; overkill if interactive |
| **k8s Service only (no concurrency gate)** | Trivial | Overload → OOM / latency death spiral |

**Recommendation: direct HTTP with admission layer**

1. Orchestrator (or sidecar) enforces global + per-tenant concurrency.
2. Compiler pods expose `/v1/compile` with max in-flight (semaphore); excess → **503 + Retry-After**.
3. Optional: queue depth Prometheus metric → HPA/KEDA scale warm Deployment.
4. Spill-to-Job if request exceeds warm SLA (e.g. estimated > 60s) or pool is full.

This beats a queue for interactive UX when the compiler is already a long-lived server with warm toolchain caches.

### 2.4 Hybrid vs pure alternatives

| Architecture | Verdict |
|---|---|
| **Hybrid warm + Kueue cold (recommended)** | Best latency/cost/ops balance |
| All Jobs, no warm pool | Simple; bad interactive latency |
| All Deployment + NATS, no Kueue | Harder fair sharing, preemption, spot flavors |
| Argo/Tekton | Overkill |
| Postgres queue (status quo) | Works but couples registry to queue; plan drops it |
| CRD-only queue | etcd abuse at scale |

---

## 3. Poison-pill, retry, idempotency (no Postgres)

### 3.1 Mapping old Postgres semantics → k8s + INDEX

| Old (Postgres) | New |
|---|---|
| Job row + status | INDEX sqlite row + Job/HTTP attempt |
| Exponential backoff | Job controller backoff **or** warm client Retry-After **or** NATS BackOff |
| Poison pill after N fails | `backoffLimit` + INDEX `status=poisoned` + package denylist |
| Visibility timeout / lease | Job pod exclusive; warm path in-process lease + INDEX `running` |
| Dead letter table | INDEX `poisoned` / `failed_permanent` + optional NATS DLQ advisories |

### 3.2 Job-level failure policy (cold path)

Example policy sketch:

```yaml
apiVersion: batch/v1
kind: Job
metadata:
  name: compile-<input_hash_short>
  labels:
    nudox.io/input-hash: "<full_sha256>"
    nudox.io/resource-class: medium
  annotations:
    kueue.x-k8s.io/queue-name: compile-cold
spec:
  backoffLimit: 3
  activeDeadlineSeconds: 3600
  ttlSecondsAfterFinished: 900
  template:
    spec:
      restartPolicy: Never   # required with podFailurePolicy
      hostUsers: false
      runtimeClassName: gvisor
      containers:
      - name: compiler
        image: registry.example/nudox/compiler-rust:sha-...
        # resources, env with INPUT_URI, OUTPUT_PREFIX, PRESIGNED...
  podFailurePolicy:
    rules:
    # Non-retriable: bad input / permanent sandbox reject
    - action: FailJob
      onExitCodes:
        containerName: compiler
        operator: In
        values: [10, 11, 12]   # app-defined permanent failure codes
    # Spot/node disruption should not burn backoff
    - action: Ignore
      onPodConditions:
      - type: DisruptionTarget
```

References: [pod failure policy task](https://kubernetes.io/docs/tasks/job/pod-failure-policy/), [Jobs concept](https://kubernetes.io/docs/concepts/workloads/controllers/job/).

**Exit code contract (compiler binary):**

| Code | Meaning | Orchestrator |
|---|---|---|
| 0 | Success; outputs in S3 | INDEX succeeded |
| 10 | Invalid invalid / unsupported | FailJob, no retry |
| 11 | Policy deny (malware heuristic, size limit) | FailJob, poison |
| 12 | Toolchain missing (config error) | FailJob, alert ops |
| 20–29 | Transient (network S3, mirror) | Count against backoffLimit |
| 137 / OOMKilled | Resource | Retry once on larger class, then poison or reclass |

### 3.3 Warm path retries

- Orchestrator retries **idempotent** POST only if no INDEX row and no output object yet.
- Use `Idempotency-Key: input_hash` header.
- Client (GUI) retries on 503 with jittered exponential backoff.
- After N orchestrator-level failures → write INDEX poison + optional cold Job with higher resource class once.

### 3.4 Dead-letter recording (INDEX sqlite)

INDEX columns (conceptual):

```
jobs(
  id TEXT PRIMARY KEY,
  input_hash TEXT NOT NULL UNIQUE,
  status TEXT,  -- queued|running|succeeded|failed|poisoned
  attempts INTEGER,
  last_error TEXT,
  resource_class TEXT,
  s3_input TEXT,
  s3_output_prefix TEXT,
  k8s_job_name TEXT NULL,
  created_at, updated_at, finished_at
)
```

Orchestrator (or Job completion watcher) is the only writer of terminal status. **Do not** rely on etcd Job objects for long-term history — GC them with TTL.

### 3.5 Idempotency & dedup

1. **Content-addressed inputs:** `input_hash = H(sealed_bundle || toolchain_digest || compiler_version)`.
2. **Output keys:** `s3://bucket/ir/{input_hash}/...` — put if not exists / immutable objects.
3. **In-flight dedup:** Orchestrator holds `input_hash → job_id` in memory + INDEX unique constraint; second request attaches as waiter.
4. **NATS-Msg-Id** (if used): set to `input_hash` for JetStream publisher dedup window.
5. **At-least-once effects:** Safe because CAS put + INDEX upsert are idempotent.

---

## 4. Sandboxing inside Kubernetes

### 4.1 Layers (defense in depth)

```
┌─────────────────────────────────────────────────────────┐
│ Node: Karpenter NodePool (spot/on-demand), CIS baseline │
├─────────────────────────────────────────────────────────┤
│ Pod: runtimeClass = gvisor (or kata for max isolation)  │
│      hostUsers: false (user namespaces GA k8s 1.36)     │
│      seccompProfile: RuntimeDefault / custom            │
│      drop ALL caps; no privileged; readOnlyRootFS       │
│      NetworkPolicy: egress only to S3 + package mirrors │
├─────────────────────────────────────────────────────────┤
│ Process: existing bwrap cage + landlock + cgroups       │
│          (workspace/compiler/sandbox/*)                 │
└─────────────────────────────────────────────────────────┘
```

### 4.2 User namespaces (GA)

**GA in Kubernetes v1.36** (2026-04-23) ([blog](https://kubernetes.io/blog/2026/04/23/kubernetes-v1-36-userns-ga/)).

```yaml
spec:
  hostUsers: false
```

Effects:

- Container root is not host root (UID mapping).
- Capabilities become **namespaced** — e.g. CAP_NET_ADMIN inside userns does not control host.
- ID-mapped mounts avoid expensive recursive chown (kernel 5.12+).
- **Enables nested user-namespace tools** (bwrap patterns) with far less need for privileged pods.

History: alpha (1.25+), beta (1.30), enabled-by-default progression through 1.32+, GA 1.36.

### 4.3 Nested bwrap / landlock / cgroups

Existing code under `workspace/compiler/sandbox/` uses **bwrap + landlock + cgroup v2 + seccomp**.

Inside a k8s container:

| Technique | Needs | With hostUsers:false + gVisor |
|---|---|---|
| **landlock** | Kernel LSM; no special cap for restrict_self | Works if host/gVisor exposes landlock ABI |
| **bwrap** | User namespaces, often CAP_SYS_ADMIN *in userns* | Prefer userns-enabled pods; avoid privileged |
| **cgroup v2** | cgroup namespace / delegated controllers | May need `cgroupfs` visibility; test on runtime |
| **seccomp** | seccomp profile on pod + optional bwrap seccomp FD | Compose carefully with gVisor (gVisor already intercepts syscalls) |

**Practical recommendation:**

1. **Primary isolation:** gVisor `runtimeClass` for untrusted compile pods (or Kata if microVM mandate is hard).
2. **Secondary:** keep bwrap+landlock inside when runtime allows; if gVisor blocks nested userns/bwrap, fall back to **landlock + seccomp + rlimits only** inside the sandbox and rely on gVisor for the rest.
3. Never run compile pods `privileged: true`.
4. Validate matrix: (runc + userns), (gVisor), (Kata) × sandbox features — CI gate.

### 4.4 gVisor vs Kata

| | gVisor (runsc) | Kata Containers |
|---|---|---|
| Isolation model | User-space kernel / syscall interception | Lightweight microVM per pod |
| CPU-bound compile | **Minimal overhead** — app code runs natively ([perf guide](https://gvisor.dev/docs/architecture_guide/performance/)) | Near-native CPU; VM overhead on start/I/O |
| I/O-heavy | Higher overhead (10–30% class for syscall-heavy) | Better for some I/O; startup cost |
| Managed k8s | GKE Sandbox mature; EKS/self-managed installable | More ops friction on some clouds |
| Startup | Hundreds of ms–low seconds | Often higher (VM boot) |
| Nesting bwrap | May be constrained | Separate guest kernel |

**For compile (CPU-heavy, untrusted):** **gVisor is the default recommendation** — excellent security/perf tradeoff for CPU-bound work. Escalate to **Kata** if threat model requires hardware virtualization boundary (hostile multi-tenant public compile-as-a-service).

Ant's production data: large fraction of apps <1–3% overhead on runsc after tuning ([blog](https://gvisor.dev/blog/2021/12/02/running-gvisor-in-production-at-scale-in-ant/)).

### 4.5 Image strategy (Nix)

Repo builds with Nix — leverage that:

| Strategy | Pros | Cons |
|---|---|---|
| **One fat image (all toolchains)** | Simple scheduling | Huge pulls; attack surface; cold start |
| **Per-language images** (`compiler-rust`, `compiler-go`, …) | Smaller; least privilege tools | More tags; Job must select image |
| **Nix-layered (nix2container)** | Content-addressed layers; dedup across images; reproducible | Pipeline complexity |

**Recommendation:** **Per-language images built with nix2container**, shared base layers (glibc, certs, bwrap). Orchestrator sets image from language field of sealed input. Pin by digest, not floating tag. Optional: lazy-pull / eStargz if cold start dominates.

Warm pool: keep N replicas per popular language **or** multi-language images only for warm path if memory allows.

---

## 5. Artifact flow

### 5.1 Lifecycle

```
1. Client or crawler → Orchestrator: CompileRequest{package, version, lang, priority}
2. Orchestrator seals inputs (or accepts pre-sealed), computes input_hash
3. INDEX lookup: if succeeded → return artifact URLs
4. Stage sealed bundle to S3: s3://.../inputs/{input_hash}.tar.zst  (if not present)
5. Path:
   Warm: POST compiler-server with input_hash + presigned GET/PUT
   Cold: create Job with env INPUT_HASH, GET_URL, PUT_PREFIX, INDEX_CALLBACK
6. Compiler downloads input, sandboxes build, uploads IR blobs (multipart, zstd)
7. Compiler or watcher notifies INDEX (success + blob keys) or failure
8. Client polls INDEX or receives push (optional NATS/WebSocket)
```

### 5.2 Presigned URLs vs in-cluster gateway

| Pattern | Use |
|---|---|
| **Presigned GET/PUT (recommended)** | Compiler talks to S3 directly; orchestrator never proxies multi-GB IR |
| In-cluster gateway (MinIO/nginx) | Only if private network policy requires single egress hop |
| Streaming through orchestrator | **Avoid** — memory and DoS risk |

Presign TTL short (e.g. 15–60 min) matching `activeDeadlineSeconds`. Scope keys to `inputs/{hash}/*` and `outputs/{hash}/*`.

### 5.3 Multipart / zstd

- **zstd** for sealed inputs and IR packs (ratio/speed balance).
- **S3 multipart** for objects > 8–16 MiB; part size 8–64 MiB.
- Content-Type + checksum headers (`x-amz-checksum-sha256` where supported).
- Immutable objects: never overwrite `outputs/{input_hash}/`; new compiler version ⇒ new hash dimension.

### 5.4 Result notification

| Method | Latency | Complexity | Recommendation |
|---|---|---|---|
| Orchestrator watches Job status (kube-rs) | Medium | Low | **Primary for cold path** |
| Compiler HTTP callback to INDEX/orchestrator | Fast | Medium (auth) | Warm path + cold success path |
| Client polls INDEX | Simple | Poll load | GUI default with backoff |
| NATS event `compile.completed` | Fan-out | Needs NATS | Optional for multi-subscribers |

**Recommended combo:** compiler writes success marker object + INDEX upsert (authenticated); orchestrator Job watcher is **backup** for crashes mid-notify; GUI polls INDEX with exponential backoff and optional WebSocket later.

---

## 6. Autoscaling & capacity

### 6.1 Warm pool scaling

- **HPA** on CPU **or** custom metric `compiler_in_flight` / `http_requests_inflight`.
- **KEDA ScaledObject** (v2.20.x) optional for Prometheus scaler or scale-near-zero off-hours ([KEDA](https://keda.sh/docs/2.20/concepts/)).
- Keep `minReplicas ≥ 1` (or per-AZ) for interactive languages to avoid cold start; scale-to-zero only for rare languages.

### 6.2 Cold path + Kueue + Karpenter

**Karpenter** ([NodePools](https://karpenter.sh/docs/concepts/nodepools/)):

- v1 APIs stable since Karpenter 1.0 (2024); **2026 line ~1.13–1.14**.
- Capacity types: `reserved` > `spot` > `on-demand` prioritization when multiple allowed.
- Separate **NodePools**:
  - `compile-spot` — bulk Jobs, toleration `nudox.io/capacity=spot`
  - `compile-ondemand` — interactive warm + high-priority Jobs
- Disruption budgets + consolidation for cost; Jobs must tolerate interruption (idempotent).

**Kueue ResourceFlavors** map to node labels/taints matching Karpenter requirements. AdmissionCheck + ProvisioningRequest integrates batch wait-for-capacity ([Kueue provisioning](https://kueue.sigs.k8s.io/docs/concepts/admission_check/provisioning_request/)).

### 6.3 Spot preemption safety

Because jobs are **idempotent CAS**:

- No checkpoint required.
- `podFailurePolicy` Ignore on `DisruptionTarget`.
- Karpenter interruption handling drains with notice; Job retries create new pod.
- Prefer spot for cold bulk only; warm pool on-demand or mixed with PDB.

### 6.4 Resource classes

| Class | CPU | Memory | Timeout | Path default |
|---|---|---|---|---|
| XS | 1 | 2 Gi | 5m | Warm |
| S | 2 | 4 Gi | 15m | Warm |
| M | 4 | 8 Gi | 30m | Warm→Cold spill |
| L | 8 | 16 Gi | 60m | Cold |
| XL | 16 | 32–64 Gi | 2h | Cold, on-demand flavor |

OOM → bump class once; second OOM → poison + human review flag in INDEX.

### 6.5 Cluster-autoscaler vs Karpenter

For bursty heterogeneous batch, **Karpenter is preferred** (faster, bin-pack aware, spot-native). Cluster-autoscaler remains fine for homogeneous node groups but is slower to react. Kueue works with both.

---

## 7. Orchestration server design

### 7.1 Responsibilities (thin)

| Does | Does NOT |
|---|---|
| Authenticate clients / service identity | Store large artifacts |
| Compute/validate `input_hash` | Run compiles in-process |
| Dedup / coalesce in-flight | Implement fair-share scheduler (delegate Kueue) |
| Stage/presign S3 | Own long-term job history (INDEX does) |
| Select warm vs cold | DAG workflow engine |
| Create Jobs with correct labels/flavors | Per-node scheduling (kube-scheduler) |
| Watch Jobs → INDEX terminal updates | Replace kube-proxy/Service LB |
| Emit metrics (queue depth, latency) | Multi-tenant billing ledger (maybe later) |
| Optional: warm pool target size hints | Full cluster autoscaler |

### 7.2 State

| State | Where |
|---|---|
| Terminal job metadata | INDEX sqlite |
| In-flight coalesce map | Orchestrator memory (rebuild from INDEX `running` + Job list on start) |
| Execution | Job objects / warm pods |
| Quotas | Kueue ClusterQueue |
| Blobs | S3 |
| Config (toolchain pins) | ConfigMap / dev-server pull as existing sketch |

**No Postgres. No durable orchestrator-local DB required** if INDEX + S3 + k8s are available. Optional small sqlite for orchestrator-local cache is fine but not source of truth.

### 7.3 kube-rs patterns

```text
Deployment replicas=2 (or 3)
├── axum HTTP API (all replicas)
└── controller workers (only leader)
    ├── watch batch/v1 Job label=nudox.io/managed=true
    ├── reconcile: map Job phase → INDEX status
    ├── finalizers optional for external cleanup
    └── SSA when patching annotations/status mirrors
```

- **Leader election:** Lease lock so only one watcher writes INDEX (avoid double transitions).
- **Server-side apply:** Own field manager `nudox-orchestrator` for Job create/patch.
- **Finalizers:** Only if you must delete S3 partials; prefer compiler cleanup + lifecycle rules on incomplete prefixes.
- **Predicates:** Use resourceVersion/generation filters + TTL predicate cache (kube 3.0+) under Job churn.

### 7.4 HA

- ≥2 orchestrator pods behind Service.
- Stateless API tier; sticky sessions unnecessary.
- Leader election for control loops only.
- INDEX sqlite: separate HA story (Litestream/rqlite/single writer service) — **out of band** of this report but required for multi-replica writers; prefer **one INDEX writer service** API.

---

## 8. End-to-end compile-job lifecycle

### 8.1 Interactive (GUI)

```
GUI → Orchestrator POST /v1/compile
  → INDEX get(input_hash): hit? return URLs
  → reserve in-flight
  → presign GET input (or upload client bundle)
  → try warm Service POST /compile
       success → INDEX succeeded → return
       503 → create Kueue Job (spill) OR Retry-After
  → GUI polls GET /v1/jobs/{id} until terminal
```

### 8.2 Bulk cold

```
Crawler → Orchestrator bulk API
  → for each package: dedup → create Job
       labels: queue-name, input-hash, class
       annotations: kueue queue
  → Kueue admits when quota free
  → Karpenter scales spot nodes if pending
  → Pod runs gVisor + compiler
  → upload outputs → INDEX
  → Job Complete → TTL GC
  → if Fail + poison rules → INDEX poisoned
```

### 8.3 Retry / poison / dedup story (summary)

1. **Dedup first** at admission (INDEX + in-flight).
2. **Retry** transient failures via Job backoff / warm Retry-After.
3. **Don't retry** permanent exit codes (`FailJob` in podFailurePolicy).
4. **Ignore** disruption failures for spot.
5. **Poison** after backoffLimit or permanent code → INDEX + optional package blocklist.
6. **Idempotent re-run** later: new Job same `input_hash` only if outputs missing and status not succeeded (admin requeue clears poison).

---

## 9. Exact component bill of materials (verified 2026-07-16)

| Component | Version / API | Role |
|---|---|---|
| Kubernetes | **1.36.x** preferred (userns GA); ≥1.31 for podFailurePolicy GA | Cluster |
| **Kueue** | **v0.18.3** | Cold Job admission, fair share, flavors |
| **KEDA** | **v2.20.x** (optional) | Warm pool event/metrics scaling |
| **Karpenter** | **v1.x** (~1.13–1.14), NodePool/NodeClaim v1 | Node burst + spot |
| **NATS Server** | **v2.14.3** (optional) | Events / overflow queue |
| **async-nats** | **0.49.1** | Rust NATS client if used |
| **kube** (kube-rs) | **4.0.0** | Orchestrator client + controllers |
| **k8s-openapi** | **0.28** (with kube 4) | Typed APIs |
| gVisor runsc | Node RuntimeClass | Pod sandbox |
| S3-compatible store | AWS S3 / GCS / MinIO | CAS artifacts |
| INDEX | sqlite service (existing plan) | Job metadata |

**Explicitly not required for v1:** Argo Workflows, Tekton, Postgres job queue, Kafka, RabbitMQ, CRD-as-queue.

### 9.1 Example Kueue resources (sketch)

```yaml
apiVersion: kueue.x-k8s.io/v1beta1
kind: ResourceFlavor
metadata:
  name: spot-cpu
spec:
  nodeLabels:
    karpenter.sh/capacity-type: spot
---
apiVersion: kueue.x-k8s.io/v1beta1
kind: ResourceFlavor
metadata:
  name: ondemand-cpu
spec:
  nodeLabels:
    karpenter.sh/capacity-type: on-demand
---
apiVersion: kueue.x-k8s.io/v1beta1
kind: ClusterQueue
metadata:
  name: compile-cq
spec:
  namespaceSelector: {}
  resourceGroups:
  - coveredResources: ["cpu", "memory"]
    flavors:
    - name: spot-cpu
      resources:
      - name: cpu
        nominalQuota: 200
      - name: memory
        nominalQuota: 400Gi
    - name: ondemand-cpu
      resources:
      - name: cpu
        nominalQuota: 50
      - name: memory
        nominalQuota: 100Gi
  preemption:
    withinClusterQueue: LowerPriority
  queueingStrategy: BestEffortFIFO
---
apiVersion: kueue.x-k8s.io/v1beta1
kind: LocalQueue
metadata:
  namespace: nudox-compile
  name: compile-cold
spec:
  clusterQueue: compile-cq
```

### 9.2 Warm Deployment sketch

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: compiler-server-rust
spec:
  replicas: 3
  template:
    spec:
      hostUsers: false
      runtimeClassName: gvisor
      containers:
      - name: compiler
        image: registry.example/nudox/compiler-rust@sha256:...
        resources:
          requests: { cpu: "2", memory: 4Gi }
          limits: { cpu: "2", memory: 4Gi }
        ports:
        - containerPort: 8080
        # env: max concurrency, S3 endpoint, etc.
---
apiVersion: v1
kind: Service
metadata:
  name: compiler-server-rust
spec:
  selector: { app: compiler-server-rust }
  ports:
  - port: 80
    targetPort: 8080
---
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: compiler-server-rust
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: compiler-server-rust
  minReplicas: 2
  maxReplicas: 40
  metrics:
  - type: Resource
    resource:
      name: cpu
      target:
        type: Utilization
        averageUtilization: 60
```

---

## 10. Comparison matrix (decision record)

| Option | Interactive latency | Fair share / quota | Spot bulk | Ops complexity | Poison handling | Rust fit |
|---|---|---|---|---|---|---|
| Postgres queue (current) | Good | DIY | DIY | Medium (DB) | Mature in-app | Good |
| Kueue + Jobs only | Poor | Excellent | Excellent | Medium | Job policy + INDEX | via kube-rs |
| Warm Deployment only | Excellent | Weak | Weak | Low | App-level | Excellent |
| **Hybrid (recommended)** | **Excellent** | **Excellent** | **Excellent** | **Medium** | **Strong** | **Excellent** |
| NATS + consumers | Good | DIY | DIY | Medium+ | MaxDeliver | async-nats |
| KEDA ScaledJob alone | OK | Weak | OK | Medium | Weak | N/A |
| Argo/Tekton | OK | Partial | OK | High | Workflow-level | Poor |
| CRD queue | OK | DIY | DIY | High risk etcd | DIY | kube-rs |

---

## 11. What the orchestration server does NOT do

Keep the binary thin. Explicit non-goals:

1. **Not a general workflow engine** — no DAGs, no multi-step pipelines.
2. **Not a cluster autoscaler** — Karpenter/CAS own nodes.
3. **Not a fair-share scheduler** — Kueue owns quotas/preemption.
4. **Not an object store** — S3 owns blobs; no proxy of large bodies.
5. **Not the source of truth for package intelligence** — INDEX/registry owns metadata.
6. **Not a sandbox implementation** — compiler binary owns bwrap/landlock; RuntimeClass owns pod isolation.
7. **Not multi-cluster federation** — MultiKueue later if needed; not v1.
8. **Not latency-based global load balancing** — Service LB + simple least-inflight later; no sophisticated RTT routing in v1 (sketch mentioned "not latency-aware" — keep it that way initially).

---

## 12. Migration plan (Postgres queue → hybrid)

| Phase | Work | Exit criteria |
|---|---|---|
| **M0** | INDEX job schema + CAS output keys | Idempotent recompile safe |
| **M1** | Compiler as HTTP server + Deployment (no k8s Jobs yet) | Interactive path works in cluster |
| **M2** | Orchestrator admission + S3 presign + dedup | Postgres still fallback |
| **M3** | Kueue install + cold Jobs + podFailurePolicy | Bulk path off Postgres |
| **M4** | gVisor RuntimeClass + hostUsers:false matrix | Sandbox CI green |
| **M5** | Karpenter spot NodePool for cold | Cost target met |
| **M6** | Drain Postgres queue; dual-write off | Postgres queue deleted |
| **M7** | Optional NATS events / KEDA tuning | Only if metrics demand |

---

## 13. Risks & open questions

| Risk / question | Mitigation / decision needed |
|---|---|
| gVisor blocks nested bwrap | Feature-detect; landlock-only inner; or Kata |
| Managed k8s without gVisor | GKE Sandbox / install runsc / fall back userns+seccomp |
| INDEX sqlite HA with multi orchestrator | Single-writer INDEX API; Litestream |
| etcd Job churn at huge bulk rates | ttlSecondsAfterFinished; rate-limit Job creates; batch slower |
| KEDA vs k8s 1.36 support matrix | Confirm KEDA version when cluster is 1.36 |
| Warm pool image size vs language count | Per-language Deployments vs fat image cost study |
| Monster packages starving fair share | Kueue priority + separate XL flavor with low nominalQuota |
| Supply chain of compiler images | Cosign sign; digest pins; Nix reproducibility |
| Presign clock skew | Short TTL + NTP; refresh mid-job for long builds |
| Multi-tenant noisy neighbor | NetworkPolicy, Kueue Cohorts, separate NodePools |
| Stage CAS poison / toolchain path digests break L1 hits | See § Compiler fleet cache; content fingerprints + epoch |

---

## Compiler fleet cache

**Full design:** [`docs/research/edge-tech/06-compiler-fleet-cache/PLAN.md`](../../edge-tech/06-compiler-fleet-cache/PLAN.md) (twin: `06-compiler-fleet-cache.md`).

ORCH path selection and artifact flow assume **content-addressed idempotency**, but that is only the top of a six-layer cache stack. This section keeps plan 12 coherent with the fleet-cache ADR.

### Do pods share a build cache today?

| Layer | Name | Shared on fleet today? |
|---|---|---|
| **L0** | ORCH/INDEX `input_hash` → S3 outputs | **Designed yes** (§2.2, §3.5, §5) — primary skip-compile path |
| **L1** | JobKey stage CAS (surface/CST/occ/archive postcard) | **No** fleet-wide — pod `MemoryCas` + optional `DiskCas`; daemon `DaemonL3::None` |
| **L2** | Language build dirs (`target/`, GOCACHE, Gradle, …) | **No** (and must not for untrusted) |
| **L3** | Toolchain/sysroot OCI/nix layers | **Yes** (containerd / image pull) |
| **L4** | In-process RA/workers warm state | **Warm pod only** |
| **L5** | Node-local source blob hardlink CAS | **Not yet** as shared node cache |

### Should they?

- **Yes:** L0 (required), L1 remote stage CAS + node-local DiskCas, L3 images, L5 node-local source hardlinks.  
- **No / careful:** L2 mutable tool caches across untrusted tenants (RO content-addressed dep blobs only, better as L5/image).  
- **Warm-pool only:** L4 — no cross-pod RA/database share; sticky sessions optional later for GUI only.

### Topology (warm vs cold)

```
ORCH L0 short-circuit
    → warm Deployment: large L1 mem + hostPath DiskCas + S3 stage L3 + L5 hardlinks + L4 workers
    → cold Kueue Job:  small L1 mem + optional hostPath / emptyDir + S3 stage L3 + L5 if present
Both write final outputs → S3; INDEX records success (L0 seed).
Stage puts: first-write-wins; poison → invalidate L1/L2 or stage-epoch prefix (L3 immutable).
```

Admission order: **L0 → in-flight coalesce → dispatch → (in pod) L5 materialize → L1 stages → produce → L0 record.**

### Types (from edge-tech 06)

- `SharedCasConfig` — `scope`, `l1_capacity`, `l2_root`, `l3` remote, `write_l3`, `verify_sample_rate`, `stage_epoch`  
- `CacheScope::{ GlobalStage, NodeLocal, Forbidden }`  
- Default prod: `GlobalStage` + best-effort L3; never require cache for correctness  

### ORCH responsibilities (cache-related)

| Does | Does NOT |
|---|---|
| L0 INDEX/S3 output dedup + coalesce | Implement heart `Tiered` / stage postcard cache |
| Presign inputs/outputs (and later stage URLs if needed) | Share language `target/` volumes across Jobs |
| Metrics: `orch_l0_hit/miss` | Own node-local DiskCas GC (node agent / daemon) |
| Force-rebuild / poison admin flags that bypass L0 | Sticky multi-tenant RA routing in v1 |

### Phased rollout (cache-specific; parallel to §12 M0–M7)

| Phase | Work | Depends on plan 12 |
|---|---|---|
| **C0** | Instrument pod-local hit/miss; JobKey golden tests | M0 keys |
| **C1** | L0 complete (already core to M0–M2) | M0–M2 |
| **C2** | Warm hostPath DiskCas + L5 hardlinks | M1 Deployment |
| **C3** | Wire daemon `ObjectStoreCas` as heart L3 for stages | M2 S3 + same image digest or content toolchain fp |
| **C4** | Scope policy, verify sampling, epoch runbooks, private ACL | M4 sandbox matrix |

### Savings (order-of-magnitude)

| Scenario | Dominant layer | Effect |
|---|---|---|
| Reindex popular crate already succeeded | **L0** | Skip 100% compile wall |
| Second sealed compile after peer stage put | **L1** | Skip producer lower CPU |
| Mass reindex after `producer_version` bump | **L5 + images** | Reuse sources/toolchains; recompute IR |
| Docs-only file change (today package JobKey) | **L5** (medium); L1 miss | Per-file keys later (01 §9.3) |

### Non-goals (align §11)

ORCH still does **not** become a general remote-cache service or blob store. Stage CAS is a **compiler ForgeRuntime** concern (`cache_get_or_build` already); ORCH only needs L0 + optional observability. Full threat model, prior art (Bazel/sccache/Nix/Buck2), and `SharedCasConfig` sketches live in edge-tech 06 — do not fork that design here.

---

## 14. Executive summary

Drop the Postgres job queue and split compile execution into two paths: a **warm Deployment** of long-lived `compiler-server` pods for interactive GUI/on-demand work, and **Kueue-admitted Kubernetes Jobs** for bursty bulk/third-party package builds. A thin **Rust orchestration server** (axum + **kube-rs 4.0**) admits work, content-address dedups against the sqlite **INDEX**, presigns S3 I/O, chooses path, creates Jobs, and reconciles Job outcomes into INDEX. It does **not** store blobs, run fair-share scheduling, or implement a DAG engine.

**Kueue v0.18.3** is the right cold-path primitive: ClusterQueues, ResourceFlavors (spot vs on-demand), fair sharing, preemption, and WorkloadPriorityClass match multi-tenant compile fleets without inventing a queue in etcd or Postgres. **Do not** adopt Argo Workflows or Tekton for single-step compiles. **Do not** use CRDs as a high-churn message queue (1 MiB object cap, 8 GB etcd soft ceiling). **NATS JetStream (server 2.14.x, async-nats 0.49.x)** is optional for completion fan-out or durable overflow—not required if warm path is HTTP with 503 backpressure and cold path is Jobs.

Poison and retry without Postgres: Job **`backoffLimit` + `podFailurePolicy` (GA 1.31+)** with permanent exit codes → FailJob; disruption → Ignore; terminal poison recorded in INDEX. Idempotency is content-addressed (`input_hash` keys in S3 + UNIQUE in INDEX). Sandboxing is layered: **gVisor RuntimeClass** (minimal CPU-bound overhead) + **`hostUsers: false` (userns GA in 1.36)** + retain **bwrap/landlock** inside when the runtime allows. Scale with **HPA/optional KEDA 2.20** on the warm pool and **Karpenter v1 NodePools** (spot for cold Jobs). Artifacts flow via **presigned S3** multipart/zstd—not through the orchestrator.

This hybrid matches the existing product sketch (orchestrator as load-balancer + instance manager, not a latency oracle) while eliminating Postgres queue coupling and giving production-grade multi-tenant batch controls out of the Kubernetes batch ecosystem.

---

## 15. References (inline URLs consolidated)

- Kueue overview: https://kueue.sigs.k8s.io/docs/overview/
- Kueue install v0.18.3: https://kueue.sigs.k8s.io/docs/getting-started/installation/
- Kueue releases: https://github.com/kubernetes-sigs/kueue/releases
- Kueue fair sharing: https://kueue.sigs.k8s.io/docs/concepts/fair_sharing/
- Kubernetes Jobs: https://kubernetes.io/docs/concepts/workloads/controllers/job/
- Pod failure policy GA: https://kubernetes.io/blog/2024/08/19/kubernetes-1-31-pod-failure-policy-for-jobs-goes-ga/
- User namespaces GA 1.36: https://kubernetes.io/blog/2026/04/23/kubernetes-v1-36-userns-ga/
- KEDA 2.20: https://keda.sh/docs/2.20/deploy/
- Karpenter NodePools: https://karpenter.sh/docs/concepts/nodepools/
- kube-rs changelog 4.0.0: https://kube.rs/changelog/
- async-nats: https://crates.io/crates/async-nats
- NATS download / 2.14.3: https://nats.io/download/
- JetStream work-queue Rust: https://natsbyexample.com/examples/jetstream/workqueue-stream/rust
- JetStream consumers: https://docs.nats.io/using-nats/developer/develop_jetstream/consumers
- etcd limits: https://etcd.io/docs/v3.3/dev-guide/limit/
- gVisor performance: https://gvisor.dev/docs/architecture_guide/performance/
- Kubernetes releases: https://kubernetes.io/releases/

---

*End of report. Research date 2026-07-16; versions re-verified via live WebSearch/WebFetch against vendor docs and release pages.*

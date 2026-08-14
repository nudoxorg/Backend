# Handoff: Research k8s orchestration design
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a9398fa60f24c5274.jsonl`
- Agent id: `agent-a9398fa60f24c5274`
- Deliverable: `/Users/philocalyst/Projects/Backend/docs/research/librarification/12-orchestration.md`
- Existing report: False (0 lines)
- Tools used: 19 ({'WebFetch': 3, 'ToolSearch': 1, 'Agent': 4, 'WebSearch': 11})
- Files/greps touched: 0
- Web queries: 14

## Web queries performed
- Argo Workflows vs Tekton kubernetes batch jobs compile 2026 overkill
- kubernetes warm pool deployment compiler server pods HPA ScaledObject 2026
- Kueue kubernetes 2026 stable version release changelog
- NATS JetStream work queue Rust async-nats 2026 version
- FETCH: https://github.com/nats-io/nats.rs/releases
- FETCH: https://kueue.sigs.k8s.io/docs/installation/
- async-nats rust crate latest version 2026 crates.io
- Kueue 0.18 features fair sharing preemption resource flavor 2026
- FETCH: https://crates.io/crates/async-nats
- kubernetes user namespaces GA 1.30 1.31 1.32 hostUsers false 2026
- gVisor runsc compile workload CPU overhead benchmark 2025 2026
- Karpenter v1 stable 2026 spot instance NodePool NodeClaim
- kube-rs 0.98 0.99 controller runtime 2026 latest version
- kubernetes podFailurePolicy GA stable k8s 1.31 1.32 1.33

## Files read / grepped (sample)

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll launch multiple parallel research agents to cover all the dimensions of this architecture plan simultaneously, then synthesize everything into the report.

### fragment

Four parallel research agents launched. I'll also do some direct research on the Argo Workflows / Tekton angle and the warm pool pattern while waiting.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent doing exhaustive external research for a major architecture plan. Today is 2026-07-16 — verify versions/status with WebSearch/WebFetch; cite URLs inline.

CONTEXT: nudox is a multi-language code-intelligence platform. Today a Postgres-backed job queue (poison-pill-safe, exponential backoff) drives compile jobs, and an axum server coordinates. THE PLAN: Postgres is dropped; the COMPILER becomes a stateless, sandboxed server binary ("takes sealed inputs, produces IR + blobs, as little state as possible") deployed as a fleet; an ORCHESTRATION SERVER uses Kubernetes primitives for load-balancing and queueing INSTEAD of rolling a queue into the registry. Compile jobs: untrusted third-party package builds (sandboxed via bwrap/landlock/cgroups today), bursty arrival (new package versions + on-demand requests from desktop clients), outputs go to S3-compatible object storage; job metadata lands in a sqlite-backed INDEX service. There's an existing sketch: orchestration server manages compiler instances (mainly load-balancer, NOT latency-aware) and production-server instances, pulls config from a dev server, runs regression/fuzz testing with spare compute.

YOUR MISSION — exhaustive research:
1. The queueing decision space on k8s (2026 state, verify each): (a) Kubernetes Jobs + Indexed Jobs + Job queues (kueue.sigs.k8s.io — research Kueue deeply: fair sharing, priorities, preemption, quotas — is it the right primitive?), (b) KEDA ScaledJobs/ScaledObjects (event-driven scaling from a queue source — but which queue if no postgres? KEDA sqlite scaler doesn't exist — what sources fit: HTTP add-on? Prometheus metric?), (c) Argo Workflows / Tekton (DAG engines — overkill?), (d) plain Deployment + HPA + an in-cluster lightweight queue (NATS JetStream! — research as the queue: persistence, work-queue semantics, redelivery/poison handling, Rust client maturity async-nats 2026; also Redis Streams, RabbitMQ as alternatives), (e) a custom operator/controller via kube-rs (2026 state of kube-rs controllers) where CRDs ARE the queue (CompileJob CRD; etcd limits! — object size 1.5MB, total etcd headroom, churn limits — quantify when CRD-as-queue breaks).
2. Recommend the architecture: likely shape is orchestration server (Rust, kube-rs) + {Kueue-managed Jobs for cold bulk builds} + {warm pool Deployment of compiler-server pods behind a Service for interactive/on-demand builds from GUI clients} — evaluate this hybrid vs alternatives; queueing for the warm path (NATS vs direct HTTP with 503-backpressure vs k8s Service load-balancing with per-pod concurrency limit — the compiler is "a server itself", so direct request/response with an admission layer may beat a queue: analyze).
3. Poison-pill & retry semantics WITHOUT postgres: Job backoffLimit + pod failure policy (podFailurePolicy — 2026 GA state), dead-letter handling (where do permanently-failing packages get recorded — INDEX sqlite), idempotency via content-addressed inputs (same sealed input hash → same output; dedup in-flight duplicates).
4. Sandboxing inside k8s: running bwrap/landlock/cgroup sandboxes INSIDE containers (privileges needed — user namespaces in k8s 2026 state, procMount, seccomp), vs gVisor/Kata as the pod-level sandbox (runtimeClass) — recommend the layering for untrusted code (defense in depth: microVM-class isolation was previously judged better than Docker for hostile code); image strategy for per-language toolchains (one fat image vs per-language images vs Nix-built layered images — the repo builds with Nix).
5. Artifact flow: sealed input bundles (source + toolchain refs) staged to S3, outputs (IR blobs) to S3 with content-addressed keys — presigned URLs vs in-cluster gateway; multipart/zstd; result notification back to INDEX (completion callback vs status poll vs NATS event).
6. Autoscaling & capacity: HPA on queue depth/custom metrics, cluster-autoscaler/Karpenter for burst, spot instances for cold bulk builds (preemption-safe because jobs are idempotent), per-job resource classes (small lib vs monster package), timeout/OOM policy.
7. The orchestration server itself: what it actually does in the recommended design (admission, priority, dedup, warm-pool sizing, zone routing later), kube-rs controller patterns (reconcile loops, finalizers, server-side apply), what state it keeps (none? all in k8s objects + INDEX?), HA (leader election).

SYNTHESIS: one recommended architecture diagram (ASCII), the exact k8s resources + third-party components with versions (Kueue vX, KEDA vX if used, NATS vX if used, kube-rs vX), the compile-job lifecycle end-to-end (GUI request → … → blobs in S3 → INDEX row), and the retry/poison/dedup story. Flag what the orchestration server does NOT do (keep it thin).

DELIVERABLE:
- Write the FULL report (dense markdown, 500-1000+ lines, URLs inline) to /Users/philocalyst/Projects/Backend/docs/research/librarification/12-orchestration.md
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) the recommended architecture bullets, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)

### WebSearch: Argo Workflows vs Tekton kubernetes batch jobs compile 2026 overkill
```
Web search results for query: "Argo Workflows vs Tekton kubernetes batch jobs compile 2026 overkill"

Links: [{"title":"Tekton vs Argo Workflows (2026): CI/CD vs Workflow Engine | NomadX","url":"https://kubernetes.ae/tekton-vs-argo-workflows/"},{"title":"Best Argo Workflows Alternatives And Competitors In 2026","url":"https://www.datastackhub.com/alternatives-to/argo-workflows-alternatives/"},{"title":"Tekton vs Argo Face-off: Unraveling the CI/CD Pipelines Mystery","url":"https://www.wallarm.com/cloud-native-products-101/cloud-native-ci-cd-pipelines-tekton-vs-argo"},{"title":"84-tekton-vs-argo-workflows.sh · GitHub","url":"https://gist.github.com/f11e690295311d8d7dd53b5128bd6d3e"},{"title":"Argo CD vs Tekton vs Jenkins X: Finding the Right GitOps Tooling • Platform9","url":"https://platform9.com/blog/argo-cd-vs-tekton-vs-jenkins-x-finding-the-right-gitops-tooling/"},{"title":"argo_workflows: FAQ | Orchestra","url":"https://www.getorchestra.io/guides/argoworkflows-faq-ec4e9"}]

Based on the search results, here's what I found about Argo Workflows vs Tekton for Kubernetes batch jobs and compilation tasks:

## Key Differences

If CI/CD is the whole job, choose Tekton; if pipelines are one of many job types, choose Argo Workflows.

**Tekton's Focus:** Tekton is a Kubernetes-native CI/CD framework made of CRDs - Task, Pipeline, and PipelineRun, providing cloud-native building blocks for assembling pipelines, with Tekton Triggers for event-driven execution and Tekton Chains for automatic, signed supply-chain provenance.

**Argo Workflows' Focus:** Argo Workflows is a Kubernetes-native, container-native workflow engine for general-purpose workflows where you define DAG-based or step-based pipelines with each step running as a pod, used for CI/CD, batch and data processing, and ML pipelines.

## When Each Excels

For batch jobs specifically, Argo Workflows handles heavy parallel batch jobs well because each step runs as a pod, and large parallel data jobs scale on Kubernetes capacity directly.

Argo Workflows is a strong alternative when the same engine also needs to run batch jobs, ETL, or ML training, because you avoid operating two systems.

## Is It Overkill?

Whether using either tool is "overkill" depends on your use case. If CI/CD is all you do, Tekton's building blocks fit more naturally; if you need one engine for many job types, Argo Workflows is the better generalist.


REMINDER: You MUST include the sources above in your response to the user using mar
```

### WebSearch: kubernetes warm pool deployment compiler server pods HPA ScaledObject 2026
```
Web search results for query: "kubernetes warm pool deployment compiler server pods HPA ScaledObject 2026"

Links: [{"title":"HorizontalPodAutoscaler Walkthrough | Kubernetes","url":"https://kubernetes.io/docs/tasks/run-application/horizontal-pod-autoscale-walkthrough/"},{"title":"Kubernetes HPA: How Horizontal Pod Autoscaling Saves Cost","url":"https://cast.ai/blog/what-is-kubernetes-hpa-and-how-can-it-help-you-save-on-the-cloud/"},{"title":"Horizontal Pod Autoscaling | Kubernetes","url":"https://kubernetes.io/docs/concepts/workloads/autoscaling/horizontal-pod-autoscale/"},{"title":"Kubernetes HPA: Use Cases, Limitations & Best Practices","url":"https://scaleops.com/blog/kubernetes-hpa/"},{"title":"Kubernetes HPA: 12 Best Practices For Production (2026)","url":"https://alexandre-vazquez.com/kubernetes-hpa-best-practices/"},{"title":"Scale to zero using KEDA | Kubernetes Engine | Google Cloud Documentation","url":"https://docs.cloud.google.com/kubernetes-engine/docs/tutorials/scale-to-zero-using-keda"},{"title":"How to Set Up Horizontal Pod Autoscaling (HPA) in Kubernetes","url":"https://oneuptime.com/blog/post/2026-01-06-kubernetes-hpa-setup/view"},{"title":"Scaling options for applications in Azure Kubernetes Service (AKS) - Azure Kubernetes Service | Microsoft Learn","url":"https://learn.microsoft.com/en-us/azure/aks/concepts-scale"},{"title":"GPU Inference Autoscaling with KEDA and Knative on Kubernetes: Cold-Start and Scale-to-Zero for LLM Serving (2026) | Spheron Blog","url":"https://www.spheron.network/blog/keda-knative-gpu-autoscaling-kubernetes-llm-cold-start/"},{"title":"Kubernetes Autoscaling: HPA vs KEDA — A Platform Engineer's Guide | Ade A.","url":"https://www.aade.me/blog/kubernetes-autoscaling-hpa-vs-keda"}]

Based on the search results, here's what I found regarding Kubernetes autoscaling with HPA and ScaledObject:

## Key Concepts

A HorizontalPodAutoscaler (HPA) automatically updates a workload resource such as a Deployment or StatefulSet, with the aim of automatically scaling the workload to match demand. The HPA is a built-in control-plane controller that automatically increases or decreases the number of pod replicas based on observed metrics.

## ScaledObject and KEDA

KEDA extends Kubernetes with a custom resource definition (CRD), referred to as a ScaledObject, to describe how applications should be scaled in response to specific traffic. KEDA will automatically create and manage the underlying HorizontalPodAutoscaler (HPA) object.
```

### WebSearch: Kueue kubernetes 2026 stable version release changelog
```
Web search results for query: "Kueue kubernetes 2026 stable version release changelog"

Links: [{"title":"Releases · kubernetes-sigs/kueue","url":"https://github.com/kubernetes-sigs/kueue/releases"},{"title":"GitHub - kubernetes-sigs/kueue: Kubernetes-native Job Queueing · GitHub","url":"https://github.com/kubernetes-sigs/kueue"},{"title":"Release v0.16.7 · Issue #10706 · kubernetes-sigs/kueue","url":"https://github.com/kubernetes-sigs/kueue/issues/10706"},{"title":"Releases | Kubernetes","url":"https://kubernetes.io/releases/"},{"title":"Release v0.14.8 · Issue #8461 · kubernetes-sigs/kueue","url":"https://github.com/kubernetes-sigs/kueue/issues/8461"},{"title":"vLLM, SGLang, Kubernetes, Kueue, and Helm ship runtime fixes","url":"https://rawkode.academy/news/2026-06-13-vllm-sglang-kubernetes-kueue-helm"},{"title":"Release v0.17.0 · Issue #9951 · kubernetes-sigs/kueue","url":"https://github.com/kubernetes-sigs/kueue/issues/9951"},{"title":"GKE release notes | Google Kubernetes Engine (GKE) | Google Cloud Documentation","url":"https://docs.cloud.google.com/kubernetes-engine/docs/release-notes"},{"title":"Kubernetes Releases - Nubenetes V2 | The AI's Cut","url":"https://nubenetes.com/kubernetes-releases/"},{"title":"Installation | Kueue - Kubernetes","url":"https://kueue.sigs.k8s.io/docs/installation/"}]

Based on the search results, I found information about Kueue's recent releases and changes in 2026:

## Latest Stable Versions

The latest release of Kueue available is v0.18.3, and Kueue published v0.18.1 and v0.17.5 on June 12, 2026.

## Release Cadence and Support

Kueue maintains a stable release cycle of 2-3 months, with Kubernetes 1.29 or newer required.

## Recent Changelog Highlights

Recent Kueue releases include several feature promotions and improvements:

**v0.17.0 (March 2026)** included:
- PropagateBatchJobLabelsToWorkload promoted to stable
- MultiKueueBatchJobWithManagedBy promoted to stable
- LocalQueueMetrics graduated to Beta

**Recent stable promotions** include:
- MultiKueueRedoAdmissionOnEvictionInWorker, MultiKueueWaitForWorkloadAdmitted, and SkipFinalizersForPodsSuspendedByParent promoted to stable

The search results show release information but don't provide a complete centralized changelog. For the most detailed release notes, you can visit the official GitHub releases page at kubernetes-sigs/kueue/releases.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
```

### WebSearch: NATS JetStream work queue Rust async-nats 2026 version
```
Web search results for query: "NATS JetStream work queue Rust async-nats 2026 version"

Links: [{"title":"NATS by Example - Work-queue Stream (Rust)","url":"https://natsbyexample.com/examples/jetstream/workqueue-stream/rust"},{"title":"ruststream-nats — async Rust library // Lib.rs","url":"https://lib.rs/crates/ruststream-nats"},{"title":"JetStream | NATS Docs","url":"https://docs.nats.io/nats-concepts/jetstream"},{"title":"async_nats::jetstream - Rust","url":"https://docs.rs/async-nats/latest/async_nats/jetstream/index.html"},{"title":"NATS by Example - Interest-based Stream (Rust)","url":"https://natsbyexample.com/examples/jetstream/interest-stream/rust"},{"title":"GitHub - nats-io/nats.rs: Rust client for NATS, the cloud native messaging system. · GitHub","url":"https://github.com/nats-io/nats.rs"},{"title":"NATS by Example - Work-queue Stream (Go)","url":"https://natsbyexample.com/examples/jetstream/workqueue-stream/go/"},{"title":"NATS by Example - Work-queue Stream (JavaScript)","url":"https://examples.nats.io/examples/jetstream/workqueue-stream/deno"},{"title":"Getting Started With NATS JetStream in .NET","url":"https://milanjovanovic.tech/blog/getting-started-with-nats-jetstream-in-dotnet"},{"title":"nats - Rust","url":"https://docs.rs/nats"}]

Based on the search results, here's what I found about NATS JetStream work queue implementation in Rust with async-nats:

## Work Queue Stream Implementation

A work-queue retention policy satisfies a very common use case of queuing up messages that are intended to be processed once and only once, and this retention policy supports queuing up messages from publishers independent of consumption.

## Using async-nats Library

NATS client functionality is split across two layers: the core client (async-nats) and Orbit, a separate set of crates with higher-level utilities, where the core can stay small, stable, and consistent across NATS clients in every language, while Orbit can iterate quickly on opinionated abstractions.

## Key Characteristics

Since each message is intended to be processed only once, this retention type allows for a set of consumers that have non-overlapping interest on subjects, meaning if multiple consumers are bound to a work-queue stream, they must have disjoint filter subjects.

## Practical Example

You can define the stream configuration, specifying WorkQueuePolicy for retention, and create the stream with async-nats by using `jetstream::stream::Config { name: "EVENTS".to_string(), r
```

### WebFetch: https://kueue.sigs.k8s.io/docs/installation/
```
I don't have access to browse the webpage at that URL. I can only work with content that's directly provided to me in our conversation.

To help you find this information, please:

1. **Copy and paste the relevant content** from the Kueue installation documentation into our chat, or
2. **Provide the key sections** that cover version information, system requirements, and supported integrations

Once you share that content, I'll be happy to give you a concise summary following the guidelines you've outlined (125-character quote limit, proper attribution, etc.).
```

### WebFetch: https://github.com/nats-io/nats.rs/releases
```
# Latest async-nats Releases

The most recent version is **async-nats/v0.49.1**, released on **June 4, 2024** at 16:42 UTC.

## Recent Release Timeline

1. **v0.49.1** (June 4, 2024) - "Release focusing on fixing behaviour around server connectivity," addressing ping interval issues and ordered consumer recreation on server restart.

2. **v0.49.0** (May 25, 2024) - A minor update "adding missing client-side max-payload validations for some methods" and TryFrom implementations for HeaderName.

3. **v0.48.0** (May 7, 2024) - Emphasized support for nats-server 2.14 features, including new error codes, server pool callbacks, and consumer reset APIs.

4. **v0.47.0** (March 31, 2024) - Introduced subject validation with opt-out capability and wrapped connection attempts in timeouts for improved robustness.

5. **v0.46.0** (January 20, 2024) - Featured feature-gating of modules to reduce binary size and dependencies.
```

### WebSearch: async-nats rust crate latest version 2026 crates.io
```
Web search results for query: "async-nats rust crate latest version 2026 crates.io"

Links: [{"title":"async-nats - crates.io: Rust Package Registry","url":"https://crates.io/crates/async-nats"},{"title":"nats - crates.io: Rust Package Registry","url":"https://crates.io/crates/nats"},{"title":"nats 0.26.0 - Docs.rs","url":"https://docs.rs/crate/nats/latest"},{"title":"async_nats - Rust","url":"https://docs.rs/async-nats"},{"title":"nightly-async-nats - crates.io: Rust Package Registry","url":"https://crates.io/crates/nightly-async-nats"},{"title":"async-nats-flyradar - crates.io: Rust Package Registry","url":"https://crates.io/crates/async-nats-flyradar"},{"title":"async-nats-wrpc - crates.io: Rust Package Registry","url":"https://crates.io/crates/async-nats-wrpc"},{"title":"nats-handling - crates.io: Rust Package Registry","url":"https://crates.io/crates/nats-handling"},{"title":"async-nats — Rust network library // Lib.rs","url":"https://lib.rs/crates/async-nats"},{"title":"async-nats-flyradar — Rust network library // Lib.rs","url":"https://lib.rs/crates/async-nats-flyradar"}]

Based on the search results, I found information about the async-nats Rust crate from crates.io:

The async-nats crate is an async Tokio-based NATS client with a stable API, however it remains on 0.x.x versioning, as the async ecosystem is still introducing a lot of ergonomic improvements.

The search results show that the crate page was last indexed on June 4, 2026 (the current date), but the specific version number wasn't displayed in the search results. However, the API is stable, but the crate remains on 0.x.x versioning.

To find the exact latest version number, you would need to visit the official crates.io page directly at https://crates.io/crates/async-nats, where you can see the current version listed at the top of the page.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
```

### WebSearch: Kueue 0.18 features fair sharing preemption resource flavor 2026
```
Web search results for query: "Kueue 0.18 features fair sharing preemption resource flavor 2026"

Links: [{"title":"Flavor-Aware Fair Sharing for Heterogeneous Resources (Weighted flavours) · Issue #8902 · kubernetes-sigs/kueue","url":"https://github.com/kubernetes-sigs/kueue/issues/8902"},{"title":"GitHub - kubernetes-sigs/kueue: Kubernetes-native Job Queueing · GitHub","url":"https://github.com/kubernetes-sigs/kueue"},{"title":"Cluster Queue | Kueue - Kubernetes","url":"https://kueue.sigs.k8s.io/docs/concepts/cluster_queue/"},{"title":"Features overview - Kueue - Kubernetes","url":"https://kueue.sigs.k8s.io/docs/overview/"},{"title":"Helm: Kueue Quota - AI Infrastructure Knowledge Base","url":"https://ai-infrastructure.net/helm-kueue-quota/"},{"title":"features package - sigs.k8s.io/kueue/pkg/features - Go Packages","url":"https://pkg.go.dev/sigs.k8s.io/kueue/pkg/features"},{"title":"Mastering Workloads in Kubernetes with Kueue — part 3: Embracing Fair Sharing | by Michał Żyliński | Google Cloud - Community | Medium","url":"https://medium.com/google-cloud/mastering-workloads-in-kubernetes-with-kueue-part-3-embracing-fair-sharing-8fef4d7fc13f"},{"title":"fairsharing package - sigs.k8s.io/kueue/pkg/scheduler/preemption/fairsharing - Go Packages","url":"https://pkg.go.dev/sigs.k8s.io/kueue/pkg/scheduler/preemption/fairsharing"},{"title":"Fair Sharing - Kueue - Kubernetes","url":"https://kueue.sigs.k8s.io/docs/concepts/fair_sharing/"}]

Based on the search results, here's what I found regarding Kueue 0.18 features related to fair sharing, preemption, and resource flavors:

## Kueue 0.18 Key Features

Reference templates are pinned to Kueue v0.18.1 against the kueue.x-k8s.io/v1beta2 API, indicating this is an active version in 2026.

### Fair Sharing and Preemption

Kueue includes advanced resource management comprising resource flavor fungibility, Fair Sharing, Cohorts and preemption with a variety of policies between different tenants. Fair Sharing can preempt Workloads in the cohort that satisfy the Fair Sharing preemption strategies.

Kueue offers several preemption strategies within its fair sharing implementation including LessThanOrEqualToFinalShare and LessThanInitialShare.

### Resource Flavor Features

A notable development in 2026 is Flavor-aware Fair Sharing calculation that accounts for the different values/costs of resource flavors when computing Dominant Resource Share (DRS). Currently, Fair Sharing aggregates resource usage across all flavors,
```

### WebFetch: https://crates.io/crates/async-nats
```
I apologize, but the web page content provided only shows the title "crates.io: Rust Package Registry" without any information about the async-nats package, its latest version, or recent releases.

To answer your question about async-nats, I would need access to the actual package page content from crates.io that displays version history and release information. Could you provide the full page content or a direct link to the async-nats package page?
```

### WebSearch: kubernetes user namespaces GA 1.30 1.31 1.32 hostUsers false 2026
```
Web search results for query: "kubernetes user namespaces GA 1.30 1.31 1.32 hostUsers false 2026"

Links: [{"title":"Kubernetes v1.36: User Namespaces in Kubernetes are finally GA | Kubernetes","url":"https://kubernetes.io/blog/2026/04/23/kubernetes-v1-36-userns-ga/"},{"title":"User Namespaces | Kubernetes","url":"https://kubernetes.io/docs/concepts/workloads/pods/user-namespaces/"},{"title":"Kubernetes 1.36: User Namespaces Are Finally GA – And Why","url":"https://www.vcluster.com/blog/kubernetes-1-36-user-namespaces-are-finally-ga-and-why-it-ma"},{"title":"Kubernetes User Namespaces Are GA in 1.36. Here’s How to Use Them | by Goutham Annem | Jun, 2026 | Medium","url":"https://medium.com/@annem.usedu/kubernetes-user-namespaces-are-ga-in-1-36-heres-how-to-use-them-5426770cdcc2"},{"title":"Kubernetes User Namespaces Explained: hostUsers, UID Mapping, and Rootless Isolation | Coding Protocols","url":"https://codingprotocols.com/blog/kubernetes-user-namespaces"},{"title":"Kubernetes 1.30: Beta Support For Pods With User Namespaces | Kubernetes","url":"https://kubernetes.io/blog/2024/04/22/userns-beta/"},{"title":"Use a User Namespace With a Pod | Kubernetes","url":"https://kubernetes.io/docs/tasks/configure-pod-container/user-namespaces/"},{"title":"Kubernetes v1.36: User Namespaces GA and What It Means for Container Security | webhani Blog","url":"https://www.webhani.com/blog/kubernetes-v136-user-namespaces-security-2026"},{"title":"Auto enable user namespaces in Kubernetes","url":"https://www.technowizardry.net/2025/09/auto-enable-userns-in-k8s-kyverno/"},{"title":"Kubernetes User Namespaces in 1.36 with hostUsers: false - DEV Community","url":"https://dev.to/indra_gustiprasetya_a80a/kubernetes-user-namespaces-in-136-with-hostusers-false-gpb"}]

Based on the search results, here's what I found regarding Kubernetes user namespaces and their GA status:

## GA Status Timeline

User Namespaces support in Kubernetes reached General Availability (GA) with the v1.36 release, which occurred on 22 April 2026.

Prior to GA:
- The hostUsers field in the pod spec was added as alpha in 1.25 (stateless pods only)
- Support for pods with persistent volumes was added in 1.28 (still alpha)
- In release v1.30, Kubernetes announced beta support for User namespaces

There's some conflicting information in the sources: one source states the full feature reached beta in 1.30 and is stable (GA) as of Kubernetes 1.33, while another notes in release v1.32, Kubernetes enabled it by 
```

### WebSearch: gVisor runsc compile workload CPU overhead benchmark 2025 2026
```
Web search results for query: "gVisor runsc compile workload CPU overhead benchmark 2025 2026"

Links: [{"title":"Performance Guide - gVisor","url":"https://gvisor.dev/docs/architecture_guide/performance/"},{"title":"Performance of gvisor","url":"https://groups.google.com/g/gvisor-users/c/NS6tsKFYhmk"},{"title":"gVisor Runtime Sandboxed Containers K8s | K8s Recipes","url":"https://kubernetes.recipes/recipes/security/gvisor-container-runtime/"},{"title":"Optimizing seccomp usage in gVisor - gVisor","url":"https://gvisor.dev/blog/2024/02/01/seccomp/"},{"title":"Running gVisor in Production at Scale in Ant - gVisor","url":"https://gvisor.dev/blog/2021/12/02/running-gvisor-in-production-at-scale-in-ant/"},{"title":"How Containerization Affects Database Performance: A Benchmark of runC, Kata, and gVisor | KubeBlocks","url":"https://kubeblocks.io/blog/does-containerization-affect-the-performance-of-databases"},{"title":"GitHub - bikramkgupta/container-runtime-benchmarks: Performance benchmarks: runc vs Kata Containers (Cloud Hypervisor) vs gVisor on self-managed Kubernetes · GitHub","url":"https://github.com/bikramkgupta/container-runtime-benchmarks"},{"title":"Improving gVisor Memory Subsystem Performance Xiaohan Fu, Bili Dong, John Hwang","url":"https://xhfu.me/files/ad5e3bbbdb2e7f4dbb5dc19c121e89a9/cse291_project_final_report.pdf"},{"title":"Benchmarks, performance metrics, and workload intensities used. | Download Scientific Diagram","url":"https://www.researchgate.net/figure/Benchmarks-performance-metrics-and-workload-intensities-used_tbl1_324239278"},{"title":"Questions about gVisor's performance","url":"https://groups.google.com/g/gvisor-users/c/G1pGQwu0ayc"}]

Based on the search results, here's what I found regarding gVisor runsc CPU overhead for compile workloads:

## CPU Overhead Findings

For CPU-bound workloads, runsc does not impose a performance penalty since the code executes natively, and runsc imposes minimal runtime overhead. This is particularly relevant for compilation tasks, which are CPU-intensive.

## General Performance Overhead

gVisor has approximately 10-30% performance overhead across various workloads. However, the overhead varies significantly by workload type:

In production at scale, 70% of applications running on runsc have less than 1% overhead, and another 25% have less than 3% overhead.

## Workload-Specific Results

For specific benchmarks, database workloads show RunC performs similarly to bare metal, Kata has a 17% increas
```

### WebSearch: Karpenter v1 stable 2026 spot instance NodePool NodeClaim
```
Web search results for query: "Karpenter v1 stable 2026 spot instance NodePool NodeClaim"

Links: [{"title":"Karpenter","url":"https://karpenter.sh/docs/concepts/nodepools/"},{"title":"Deploy Karpenter on EKS: Node Auto-Scaling Guide (2026)","url":"https://cast.ai/blog/deploy-karpenter-eks-node-autoscaling/"},{"title":"How to Create Kubernetes Karpenter Node Pools","url":"https://oneuptime.com/blog/post/2026-01-30-kubernetes-karpenter-node-pools/view"},{"title":"v1 Migration","url":"https://karpenter.sh/v1.0/upgrading/v1-migration/"},{"title":"EKS Karpenter nodeclaim error | AWS re:Post","url":"https://repost.aws/questions/QUfhmae96SSXK_4Zq_DPqz1g/eks-karpenter-nodeclaim-error"},{"title":"Karpenter - Amazon EKS","url":"https://docs.aws.amazon.com/eks/latest/best-practices/karpenter.html"},{"title":"AWS EKS Nodes Lifecycle Management with Karpenter","url":"https://careers.klika-tech.com/blog/aws-eks-nodes-lifecycle-management-with-karpenter/"},{"title":"Karpenter v1.12.0 Spec: NodeClaim","url":"https://kubespec.dev/karpenter/karpenter.sh/v1/NodeClaim"},{"title":"Create a Node Pool for EKS Auto Mode - Amazon EKS","url":"https://docs.aws.amazon.com/eks/latest/userguide/create-node-pool.html"},{"title":"Understanding Karpenter architecture for Kubernetes autoscaling | Datadog","url":"https://www.datadoghq.com/blog/karpenter-architecture/"}]

Here are the search results for Karpenter v1 stable 2026 with information about spot instances, NodePool, and NodeClaim:

## Key Concepts

**NodePool and NodeClaim Architecture:**
The v1 API replaced Provisioner and AWSNodeTemplate with NodePool and EC2NodeClass. Karpenter uses NodeClaims to manage the lifecycle of Kubernetes Nodes with the underlying cloud provider. Karpenter will create and delete NodeClaims in response to the demands of Pods in the cluster.

**Spot Instance Prioritization:**
If a NodePool is compatible with multiple capacity types, Karpenter will prioritize reserved capacity, followed by spot, then finally on-demand. Karpenter prioritizes Spot offerings if the NodePool allows Spot and on-demand instances. If the provider EC2 Fleet's API indicates Spot capacity is unavailable, Karpenter eventually will attempt to provision on-demand instances, generally within milliseconds.

**NodeClaim Details:**
The NodeClaim includes the exact requirements Karpenter chose for the new node, including instance type, zone, and capacity type.

**v1 Stable Release:**
The latest v1 API is available for deploying Karpenter o
```

### WebSearch: kube-rs 0.98 0.99 controller runtime 2026 latest version
```
Web search results for query: "kube-rs 0.98 0.99 controller runtime 2026 latest version"

Links: [{"title":"kube 3.1.0 - Docs.rs","url":"https://docs.rs/crate/kube/latest"},{"title":"GitHub - kube-rs/kube: Rust Kubernetes client and controller runtime · GitHub","url":"https://github.com/kube-rs/kube"},{"title":"kube 0.16.1 - Docs.rs","url":"https://docs.rs/crate/kube/0.16.1"},{"title":"Releases · kubernetes-sigs/controller-runtime","url":"https://github.com/kubernetes-sigs/controller-runtime/releases"},{"title":"Controller in kube::runtime - Rust","url":"https://docs.rs/kube/latest/kube/runtime/struct.Controller.html"},{"title":"Changelog - kube","url":"https://kube.rs/changelog/"},{"title":"controllerruntime package - sigs.k8s.io/controller-runtime - Go Packages","url":"https://pkg.go.dev/sigs.k8s.io/controller-runtime"},{"title":"GitHub - hollyburn/kube-rs: Rust Kubernetes client and controller runtime","url":"https://github.com/hollyburn/kube-rs"},{"title":"GitHub - EmbarkStudios/kube-rs: rust kubernetes client and controller runtime · GitHub","url":"https://github.com/EmbarkStudios/kube-rs"},{"title":"Releases · kube-rs/kube","url":"https://github.com/kube-rs/kube/releases?after=0.61.0"}]

Based on the search results, here's what I found about kube-rs versions:

The latest version of kube is 3.0.0, released on 2026-01-12. Version 0.99.0 was released on 2025-03-12, and version 0.98.0 was released on 2024-12-23.

The search results show a comprehensive version history for the kube-rs project, which is a Rust Kubernetes client and controller runtime. The versions you mentioned (0.98 and 0.99) are part of the development history leading up to the major version 3.0.0 release in January 2026.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
```

### WebSearch: kubernetes podFailurePolicy GA stable k8s 1.31 1.32 1.33
```
Web search results for query: "kubernetes podFailurePolicy GA stable k8s 1.31 1.32 1.33"

Links: [{"title":"Kubernetes 1.31: Pod Failure Policy for Jobs Goes GA | Kubernetes","url":"https://kubernetes.io/blog/2024/08/19/kubernetes-1-31-pod-failure-policy-for-jobs-goes-ga/"},{"title":"Handling retriable and non-retriable pod failures with Pod failure policy | Kubernetes","url":"https://kubernetes.io/docs/tasks/job/pod-failure-policy/"},{"title":"Kubernetes 1.31 Introduces Enhanced Pod Failure Policy for Job Efficiency – GDS","url":"https://gdservices.io/kubernetes-1-31-introduces-enhanced-pod-failure-policy-for-job-efficiency"},{"title":"Kubernetes 1.31 Release Notes_Kubernetes Version Release Notes_Cluster Versions_Product Release Notes_Product Bulletin_Cloud Container Engine_Autopilot-Huawei Cloud","url":"https://support.huaweicloud.com/intl/en-us/bulletin-cce-autopilot/cce_11_0099.html"},{"title":"Kubernetes v1.31: What’s New with Elli?","url":"https://www.vcluster.com/blog/kubernetes-v1-31-release"},{"title":"How to Use Pod Failure Policy in Kubernetes | techbeatly","url":"https://techbeatly.com/kubernetes-pod-failure-policy/"},{"title":"Kubernetes 1.33: Job's SuccessPolicy Goes GA | Kubernetes","url":"https://kubernetes.io/blog/2025/05/15/kubernetes-1-33-jobs-success-policy-goes-ga/"},{"title":"Handling Kubernetes Job Failures: Understanding Pod Failure Policy and Pod Replacement Policy | Bala's Blog","url":"https://dkbalachandar.wordpress.com/2026/04/05/handling-kubernetes-job-failures-understanding-pod-failure-policy-and-pod-replacement-policy/"},{"title":"Review release notes for Kubernetes versions on standard support - Amazon EKS","url":"https://docs.aws.amazon.com/eks/latest/userguide/kubernetes-versions-standard.html"},{"title":"How to Handle Job Pod Replacement Policy for Faster Failure Recovery","url":"https://oneuptime.com/blog/post/2026-02-09-job-pod-replacement-policy-failure/view"}]

Based on the search results, here's what I found regarding `podFailurePolicy` across Kubernetes versions 1.31-1.33:

## Pod Failure Policy Status

Pod failure policy reached stable (GA) status in Kubernetes v1.31 and is enabled by default. This feature graduates to stable in Kubernetes 1.31.

The search results do not contain specific information indicating whether `podFailurePolicy` was promoted to GA, moved to beta, or remained stable in Kubernetes 1.32 and 1.33. Based on the available data, it appears that the feature became GA in 1.31 and likely remains st
```

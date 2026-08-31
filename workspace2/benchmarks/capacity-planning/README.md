# NUDOX capacity planning

The executable target is `nudox-root`'s `capacity-planning` `[[bench]]` with `harness = false`.
The worksheet is its non-shipping `capacity-worksheet` example. These assets add no production
binary; raw runs, scripts, research, and worksheet inputs remain in this directory.

## Run the real API harness

```sh
cargo bench -p nudox-root --locked --bench capacity-planning -- \
  --corpus 8 --samples 3 --warmups 1 --mode warm \
  --output "$PWD/benchmarks/capacity-planning/.runs/warm-8"
```

Use `--mode cold --warmups 0 --samples 1` with a fresh `--output` directory for a
process/fresh-artifact sample. The runner rejects more than one cold sample: invoke it again for
another real process-cold point. “Cold” does not claim a flushed OS cache; it means a new process
and new durable artifact directory. “Warm” means an unrecorded complete journey ran in the same
process first. Compare each mode at corpus sizes 1, 8, 32, and 64 before sizing a server; one-row
Tantivy measurements are mostly setup, not a service capacity estimate.

Each `result.json` is machine-readable and contains the selected `rustc --version --verbose`,
architecture, profile, configuration, stage samples, wall time, logical input/output/durable
bytes, logical throughput, allocation-counter facts, and a best-effort post-stage RSS probe.
Allocation counting begins exactly around the stage closure: fixture creation, result JSON, `ps`,
and runner discovery are outside it. Native `rustc` is a child process, so its allocations are not
claimed as this process’s allocation count.

The standard library does not provide portable per-stage CPU time or peak RSS. The wrapper records
per-selected-stage process CPU and high-water RSS in adjacent `process-metrics.json` files:

```sh
benchmarks/capacity-planning/scripts/run-with-process-metrics.sh \
  benchmarks/capacity-planning/.runs/process-cold-32 \
  --corpus 32 --samples 1 --warmups 0 --mode cold
```

On macOS it uses `/usr/bin/time -l`; on GNU systems it uses `/usr/bin/time -v`. A selected stage
includes its real pipeline prerequisites, but skips later stages. That is an envelope around a
reproducible stage journey, not an invented in-process peak. The runner marks portable CPU/peak
fields unavailable rather than fabricating them.

| Stage | Real public operation | Important boundary |
| --- | --- | --- |
| `native-compile-lower-ir` | `nudox-compile-driver::compile` with native `rustc` | child compiler CPU/RSS is reported only by the process wrapper |
| `durable-compiler-publication` | compact fragments through `publish_compiled` and journal shutdown | durable regular-file bytes are counted after shutdown |
| `deterministic-index-build` | reopen publication, then `nudox-index-build::build` | consumes manifest-named compact fragments only |
| `exact-core-query` | exact rows, snapshot/manifest, and exact query | representative exact lookup |
| `tantivy-lexical-build` / `tantivy-lexical-query` | compiler-derived entity names through `TantivyLexical` | build and query are intentionally separate |
| `vector-ingress` / `vector-exact-query` | `compact_vector_facts`, validated segment, exact vector query | projection/oracle facts only; **not embedding inference** |

The current public graph-vector API bounds this fixture to 16 four-dimensional integer points.
The harness explicitly reports `local_embedding_inference` unavailable because no public
tokenizer/model runtime is linked. It separately reports `qdrant_vector_transport` unavailable:
the local vector oracle is not a Qdrant client or wire-transport benchmark. It never substitutes
sleeps or synthetic timing loops.

## Workload worksheet

The separate `capacity-worksheet` CLI recomputes every worker count from input facts rather than
fixed hardware magic:

```sh
cargo run -p nudox-root --locked --example capacity-worksheet -- \
  --input benchmarks/capacity-planning/examples/growth.json \
  --output "$PWD/benchmarks/capacity-planning/.runs/growth-plan.json"
```

Examples cover [starter](examples/starter.json), [growth](examples/growth.json), and
[horizontally scaled](examples/horizontally-scaled.json) deployments. They are deliberately
assumptions, not benchmark observations. Replace request rates and CPU milliseconds with p95/p99
measurements from the harness and service load tests.

The typed model uses these derivations:

- compiler cores = `ceil(rps × CPU-ms/request ÷ (1000 × target-utilization))`; compiler workers
  pack those cores into the supplied core count, so compilers scale vertically before queueing;
- replicated index bytes = primary durable bytes × replica count; index nodes are the maximum of
  durable capacity, measured hot working-set RAM, admitted CPU-equivalent query slots, and one
  independent node per immutable replica;
- embedding weight bytes = `ceil(parameters × quantization-bits ÷ 8)` and raw vector bytes/item
  = `ceil(dimensions × output-element-bits ÷ 8)`; one worker’s GPU admission is weights + runtime
  workspace + activation bytes/item × batch size; workers are demand divided by **measured**
  items/sec at that exact model, dimension, quantization, closed execution provider, and batch;
- both replica/failure and domain placement are validated. Every immutable replica needs its own
  domain, and replicas must be at least tolerated domain failures + 1.

This deliberately keeps tokenizer shards, execution-provider workspace, activation batching, and
model weights separate. A model that fits by weight bytes can still fail its requested batch.

## Operational shape

Keep native compiler/compact-IR publication workers vertical: native code generation has a
latency-sensitive single-core component, while independent build requests can fill separately
admitted cores. Keep immutable index servers horizontal: publish content-addressed immutable
artifacts, serve a closed snapshot, and place each replica in a different zone/rack. Index nodes
need enough RAM for active mmaps, query working set, and OS cache; the worksheet’s usable disk
number must already subtract that reserve. Keep embedding workers stateless behind a queue and
size them from observed batch throughput, not GPU marketing throughput.

Object storage is the recovery/distribution authority, not the synchronous hot query path. Store
immutable publications with generation/content identity, verify before serving, and retain the
last known good snapshot on download/activation failure. A compiler worker or embedding worker can
be replaced; an index node must never advertise a partially activated generation.

## Lean local client: <50 MiB base binary target

The base executable contains no toolchain adapter, tokenizer, model weight, execution provider,
or architecture kernel. The 50 MiB target is a binary-size budget, not a claim that optional local
capabilities are small. Proposed CI budgets are 25 MiB signed application/core, 10 MiB UX and
protocol assets, and 15 MiB update/verification headroom; measure the actual release binary before
declaring the target met.

```rust
enum ManifestFormat { V1 }
const MAX_CAPABILITIES: usize = 64;
const MAX_CAPABILITY_DEPENDENCIES: usize = 8;
struct ContentDigest([u8; 32]);
struct ManifestSignature([u8; 64]);

struct CapabilityManifest {
    format: ManifestFormat,
    root: ContentDigest,
    signature: ManifestSignature,
    offline: OfflinePolicy,
    capabilities: [Capability; MAX_CAPABILITIES],
}

enum CapabilityKind {
    NativeToolchainAdapter,
    Tokenizer,
    ModelShard,
    ExecutionProvider,
    ArchitectureKernel,
}

enum OfflinePolicy { Strict, VerifiedCacheOnly, NetworkAllowed }

struct Capability {
    digest: ContentDigest,
    kind: CapabilityKind,
    target: CapabilityTarget,
    bytes: u64,
    dependencies: [ContentDigest; MAX_CAPABILITY_DEPENDENCIES],
}

enum CapabilityTarget {
    MacOsAarch64,
    MacOsX86_64,
    LinuxX86_64,
    LinuxAarch64,
    WindowsX86_64,
}
```

Each capability carries a content digest, byte count, target architecture/OS, dependency digests,
and activation constraints. Activation is: verify the signed manifest; resolve a compatible
closed capability set; resumably range-download to a temporary content-addressed path with an
`If-Range` validator; reject a resumed response whose content range/version disagrees; verify
every digest and signature; mmap only verified artifacts; then atomically move the active lease
pointer. A failed download or hash never becomes activatable.

Use LRU eviction only for artifacts with no active lease and no recovery pin; leases are renewed by
live users and an expired lease is reclaimable. `Strict` never starts network I/O, `VerifiedCacheOnly`
uses only already-verified content, and `NetworkAllowed` may fetch a manifest-permitted artifact.
The CPU/SIMD implementation is the universal fallback. Add a Metal/Core ML, CUDA, DirectML, or
ONNX Runtime sidecar only after its model/provider/batch benchmark improves the chosen metric;
the sidecar and downloaded model footprint remain outside the base-binary budget.

A native-toolchain adapter binds a discovered executable’s canonical path, version output, and
manifest-allowed digest before exposing it to compilation; an arbitrary `PATH` executable is not a
capability. Model shards are independently content-addressed and activated only as a complete,
dependency-verified model set. Architecture kernels remain separate capabilities, so an x86-64
AVX kernel never bloats an Apple-silicon client and a failed GPU provider simply falls back to the
verified CPU/SIMD path when policy permits it.

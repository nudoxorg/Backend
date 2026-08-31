# Capacity and hardware research

Research date: 2026-08-31. All factual specifications below are linked to first-party project,
manufacturer, cloud-provider, or service documentation. Recommendations are engineering
inferences from those sources and must be recomputed with the worksheet and current quotes.

## What to measure and why

- Rust: `rustc` documents that more codegen units increase parallelism but can weaken optimized
  code; Cargo defaults build jobs to logical CPUs. Treat a compiler machine’s fast cores, RAM, and
  NVMe latency as latency capacity, then schedule independent compile requests across a bounded
  vertical pool. Cargo’s own performance guide documents diminishing returns in parallel frontend
  work and recommends measuring thread count rather than assuming all cores help equally.
  Sources: [rustc codegen options](https://doc.rust-lang.org/rustc/codegen-options/index.html),
  [Cargo build jobs](https://doc.rust-lang.org/cargo/commands/cargo-build.html), and
  [Cargo build performance](https://doc.rust-lang.org/cargo/guide/build-performance.html).
- Tantivy: segments are independent immutable units and its architecture describes atomic metadata
  commits and mmap/RAM directory tradeoffs. This supports immutable closed-snapshot serving, but
  does not justify a fixed RAM number: measure hot mmap/page-cache plus lexical query concurrency.
  Source: [Tantivy architecture](https://github.com/quickwit-oss/tantivy/blob/main/ARCHITECTURE.md).
- Vector: Qdrant’s capacity guide provides a concrete planning method: data plus HNSW graph plus
  quantization/rescore copies, then about 20% headroom; its graph is random-read sensitive and
  should remain on low-latency storage. Its cited formula is a planning input, not a NUDOX
  measurement. Source: [Qdrant capacity planning](https://qdrant.tech/documentation/capacity-planning/).
- Storage: Samsung specifies PM9D3a sequential 12 GB/s read, 6.8 GB/s write, up to 2M/400k random
  read/write IOPS, power-loss protection, and 1 DWPD over five years for selected capacities.
  Endurance must instead be calculated as measured immutable publish bytes/day × observed write
  amplification × retention; never infer it from sequential bandwidth alone. Source:
  [Samsung datacenter SSD](https://semiconductor.samsung.com/ssd/datacenter-ssd/).
- Object storage: AWS documents per-prefix request guidance and notes that first-byte latency can
  be 100–200 ms. Use parallel content-addressed recovery distribution, not request-per-object
  synchronous search. Source: [Amazon S3 performance](https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance.html).
- Accelerators: AWS documents 22 GiB per L4 GPU on G6 and 44 GiB per L40S GPU on G6e. ONNX Runtime
  documents CPU, CUDA, TensorRT, DirectML, and Core ML execution-provider choices and fallback
  ordering. Treat provider compatibility, driver version, model dimension, quantization, batch,
  and measured items/sec as worksheet inputs. Sources: [AWS accelerated instances](https://docs.aws.amazon.com/ec2/latest/instancetypes/ac.html),
  [ONNX Runtime execution providers](https://onnxruntime.ai/docs/execution-providers/), and
  [NVIDIA CUDA compatibility](https://docs.nvidia.com/deploy/cuda-compatibility/minor-version-compatibility.html).

## Purchase recommendations — dated 2026-08-31

1. For the vertical native compiler tier, solicit a one-socket system around an
   [AMD EPYC 9565](https://www.amd.com/en/products/processors/server/epyc/9005-series/amd-epyc-9565.html):
   AMD lists 72 cores/144 threads, 4.3 GHz max boost, 384 MiB L3, 12 DDR5 channels, and 128 PCIe
   5.0 lanes. Its page listed a US$8,233 1KU price when checked on this date. Pair with ECC memory
   sized from concurrent compiler working sets, not only source bytes; the 1KU figure is a vendor
   reference, not an integration quote.
2. For compiler journals and hot immutable index copies, procure two independently replaceable
   [Samsung PM9D3a](https://semiconductor.samsung.com/ssd/datacenter-ssd/) enterprise NVMe drives
   per node with PLP. Size usable capacity after replica, page-cache, and endurance reservation;
   do not buy a consumer SSD based on a peak sequential score.
3. For an embedding pilot, first reserve an
   [AWS G6e L40S 44 GiB GPU](https://docs.aws.amazon.com/ec2/latest/instancetypes/ac.html), or a
   G6 L4 22 GiB when the worksheet says the loaded model + workspace + batch fits. Keep it
   separate from compiler/index hosts, benchmark the actual provider/model, then decide whether a
   purchased accelerator is justified. Regional price and availability need a fresh quote.

## Tier recommendations

| Tier | Compiler/publishing | Immutable index | Embeddings | Failure domain |
| --- | --- | --- | --- | --- |
| Starter | one vertical worker plus queue admission | minimum two independent replica nodes if one domain failure is tolerated | one measured provider worker; CPU/SIMD fallback | recovery copy and two independently replaceable domains |
| Growth | one or more large-memory vertical workers; route retries idempotently | three immutable replicas across three zones; scale nodes from the maximum worksheet constraint | stateless queue-fed pool | no node activation without verified complete generation |
| Horizontally scaled | separate worker pools by tenant/priority; scale on queue delay and CPU-ms | add nodes horizontally; object storage distributes immutable recovery artifacts | provider-specific pools by model shape | zone-aware replicas, independent queues, and last-known-good snapshot rollback |

The worksheet examples are intentionally not hardware orders. Re-run them after replacing their
assumed p95 CPU-ms, durable bytes, query slots, model parameter count, bits, workspace, batch, and
measured embedding throughput.

## Calibration record

The early smoke is discarded. These fresh samples are observations on one development machine,
not a server-sizing conclusion.

- macOS 15.7.4; Apple M3 Pro; 11 physical cores; 36 GiB RAM; `aarch64-apple-darwin`.
- `/etc/profiles/per-user/mileswirht/bin/rustc`: Rust 1.97.1, commit
  `8bab26f4f68e0e26f0bb7960be334d5b520ea452`, LLVM 22.1.6; release profile; Tantivy 0.26.1.
- `HEAD` was `bb3de90f3dd59bcfb0ea63ee3b3de34b2348e226`; the measured benchmark source-set digest
  was `166bbf8784a86cb17179c8b1684ca9cda8596539bc0c9dc8278bab730a2334ea`; executable was
  `target/release/deps/capacity_planning-24a7aef1a16e9beb` with SHA-256
  `b1a4bc33e37e571ec1df3cdc215f423bdb5b3299e6030f1e8bff8418e7feeb61`.
- Cold used one fresh process/artifact invocation at each corpus 1/8/32/64. Warm used one full
  unrecorded journey then three measured samples. Cold does not assert an OS-cache flush.

Every wall/allocation cell is `mean wall ms / allocation calls / requested KiB`; allocations wrap
only the stage closure. Native `rustc` allocations remain child-process work and are not claimed by
the harness.

| Stage | cold 1 | cold 8 | cold 32 | cold 64 |
| --- | ---: | ---: | ---: | ---: |
| native compile → compact IR | 41.578 / 35 / 6.0 | 225.877 / 280 / 48.2 | 874.999 / 1,120 / 193.2 | 1,718.738 / 2,243 / 386.5 |
| durable publication | 133.770 / 94 / 16.7 | 144.311 / 150 / 29.8 | 382.485 / 342 / 74.8 | 655.967 / 597 / 134.7 |
| deterministic index build | 0.578 / 45 / 7.7 | 0.394 / 59 / 12.3 | 1.035 / 107 / 28.1 | 2.608 / 171 / 49.1 |
| exact query | 0.005 / 0 / 0 | 0.010 / 0 / 0 | 0.026 / 0 / 0 | 0.075 / 0 / 0 |
| Tantivy lexical build | 5.264 / 347 / 4,913.9 | 2.573 / 368 / 4,921.3 | 2.527 / 440 / 4,946.8 | 2.542 / 536 / 4,980.8 |
| Tantivy lexical query | 2.680 / 390 / 4,920.4 | 2.219 / 411 / 4,928.5 | 2.334 / 483 / 4,956.0 | 2.479 / 579 / 4,992.7 |
| vector projection ingress | 0.003 / 0 / 0 | 0.009 / 0 / 0 | 0.006 / 0 / 0 | 0.005 / 0 / 0 |
| vector exact query | 0.011 / 0 / 0 | 0.005 / 0 / 0 | 0.004 / 0 / 0 | 0.004 / 0 / 0 |

| Stage | warm 1 | warm 8 | warm 32 | warm 64 |
| --- | ---: | ---: | ---: | ---: |
| native compile → compact IR | 28.751 / 35 / 6.0 | 235.043 / 281 / 48.3 | 1,241.300 / 1,125 / 193.5 | 2,514.797 / 2,255 / 387.5 |
| durable publication | 82.259 / 93 / 16.7 | 149.212 / 149 / 29.7 | 370.774 / 341 / 74.8 | 689.683 / 597 / 134.7 |
| deterministic index build | 0.443 / 45 / 7.7 | 0.586 / 59 / 12.3 | 2.167 / 106 / 28.0 | 6.149 / 171 / 49.1 |
| exact query | 0.007 / 0 / 0 | 0.008 / 0 / 0 | 0.045 / 0 / 0 | 0.081 / 0 / 0 |
| Tantivy lexical build | 5.061 / 345 / 4,913.7 | 5.829 / 366 / 4,921.2 | 5.226 / 438 / 4,946.7 | 3.350 / 534 / 4,980.7 |
| Tantivy lexical query | 3.207 / 390 / 4,920.4 | 3.216 / 411 / 4,928.5 | 5.393 / 483 / 4,956.0 | 4.492 / 580 / 4,992.8 |
| vector projection ingress | 0.005 / 0 / 0 | 0.006 / 0 / 0 | 0.009 / 0 / 0 | 0.007 / 0 / 0 |
| vector exact query | 0.009 / 0 / 0 | 0.005 / 0 / 0 | 0.010 / 0 / 0 | 0.013 / 0 / 0 |

The 64-item cold logical I/O, directly counted rather than inferred from device blocks, is:
native `1,782/15,808/0`, durable `15,808/42,144/42,144`, deterministic build
`15,808/2,496/0`, exact `7/0/0`, Tantivy build `448/4,544/0`, Tantivy query `7/40/0`, vector
ingress `128/384/0`, and vector query `8/288/0` (read/write/durable bytes).

`/usr/bin/time -l` supplied user+system CPU and peak RSS. Each is an external process envelope:
it includes real prerequisites; every warm envelope also includes the unrecorded warmup. Thus it
is deliberately not attributed as exclusive in-process stage CPU.

| Stage envelope (CPU ms / peak MiB) | cold 1 | cold 8 | cold 32 | cold 64 |
| --- | ---: | ---: | ---: | ---: |
| native compile | 60 / 56.73 | 220 / 56.84 | 710 / 56.80 | 1,580 / 56.73 |
| durable publication | 60 / 56.61 | 220 / 56.83 | 700 / 56.78 | 1,580 / 56.86 |
| deterministic build | 70 / 56.53 | 240 / 56.78 | 680 / 56.70 | 1,600 / 56.75 |
| exact query | 70 / 56.47 | 250 / 56.72 | 730 / 56.84 | 1,850 / 56.86 |
| Tantivy build | 70 / 56.73 | 220 / 56.73 | 720 / 56.81 | 1,860 / 56.88 |
| Tantivy query | 70 / 56.63 | 230 / 56.70 | 670 / 56.81 | 1,780 / 56.88 |
| vector ingress | 60 / 56.34 | 230 / 56.70 | 700 / 56.81 | 1,680 / 56.83 |
| vector query | 70 / 56.47 | 220 / 56.69 | 840 / 56.91 | 1,690 / 56.80 |

| Stage envelope (CPU ms / peak MiB) | warm 1 | warm 8 | warm 32 | warm 64 |
| --- | ---: | ---: | ---: | ---: |
| native compile | 140 / 56.63 | 840 / 56.78 | 2,990 / 56.86 | 6,120 / 56.81 |
| durable publication | 150 / 56.52 | 820 / 56.78 | 3,170 / 56.86 | 6,390 / 56.84 |
| deterministic build | 150 / 56.72 | 820 / 56.80 | 3,210 / 56.83 | 6,270 / 56.88 |
| exact query | 160 / 56.70 | 860 / 56.75 | 3,160 / 56.77 | 5,970 / 56.88 |
| Tantivy build | 160 / 60.92 | 860 / 60.36 | 3,230 / 59.80 | 5,140 / 68.39 |
| Tantivy query | 170 / 87.94 | 890 / 80.52 | 3,260 / 89.63 | 6,380 / 84.83 |
| vector ingress | 190 / 85.17 | 850 / 87.02 | 3,130 / 92.81 | 6,330 / 95.20 |
| vector query | 190 / 94.56 | 850 / 84.86 | 3,360 / 100.94 | 6,300 / 88.13 |

The direct regular target command was `cargo bench -p nudox-root --locked --bench
capacity-planning -- --corpus N --samples 1 --warmups 0 --mode cold --output "$PWD/..."`, with
warm `--samples 3 --warmups 1`. During the final process-envelope continuation, the shared
workspace lockfile changed independently and rejected a new locked rebuild. The already-built
executable and source/executable digests above were used for those remaining wrapper captures; no
lockfile was changed. Raw machine-readable samples remain under ignored `.runs/`.

Qdrant vector transport is **not measured**: no public Qdrant client is linked. Embedding
inference is **not measured**: no tokenizer/model runtime is linked. Vector rows are local
projection/oracle measurements only, never embeddings or Qdrant claims.

## Unmeasured sizing recommendations

These are workload formulas, not local benchmark conclusions. For a compiler-heavy server, choose
fast cores and enough ECC RAM/NVMe for the measured concurrent native working set, then admit at
70% target CPU utilization: workers = ceil(ceil(rps × p95_cpu_ms / 700) / cores_per_worker).
The EPYC 9565 recommendation above is a concrete candidate whose price/specification still needs
a system quote and a native compile regression.

For example, the horizontally-scaled worksheet's assumed 50 requests/s × 300 CPU-ms needs 22
admitted CPU cores at 70%. A 72-core EPYC 9565 worker admits 50.4 core-equivalents at that target,
leaving 28.4 core-equivalents (189% of the assumed demand) for queueing, burst, or loss of
single-core efficiency. That is a vertical compiler recommendation, not a claim that one compile
uses 72 cores; the remaining cores serve independent requests.

For an index/vector-heavy tier, compute max(replicated durable bytes / usable NVMe, hot working
set / usable RAM, CPU-equivalent query slots, replica-domain count), then retain the configured
reserve before declaring capacity. Qdrant's documented graph/data/quantization calculation and
20% headroom are an external planning input; a future Qdrant transport benchmark must replace
that planning estimate with observed transport/ingestion/query measurements.

The same worksheet's illustrative 100 TiB primary × three replicas, 6 TiB usable NVMe/node, 16
TiB hot set, 512 GiB usable RAM/node, and query/failure facts derive
max(50 storage, 32 RAM, 6 query, 3 domain) = 50 index nodes. Provisioning 60 such node-equivalents
leaves ten nodes (20%) beyond that formula and distributes cleanly as 20 per three zones. Its 7B
parameter, four-bit embedding assumption uses 3.5 GB weights plus 8 GiB workspace and 3 GiB batch
activation: 14.3 GiB/worker, so an 80 GiB accelerator fits the configured batch; 2,000 / 250 = 8
measured-throughput embedding workers. Those values are worksheet assumptions, not a measured
model or Qdrant result.

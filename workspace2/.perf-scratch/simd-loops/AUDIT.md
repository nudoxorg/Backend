# SIMD / hot-loop audit (read-only production review)

Scope: `workspace2/crates`, with scratch-only recommendations. Production files were not edited.

## Ranked findings

### 1. Keep and extend the existing strict-row SIMD kernel (high confidence)

`nudox-root/src/locality/artifact/rows.rs:31-124` is the only measured long contiguous kernel. It
does one scalar seed read, then compares adjacent big-endian `u32` lanes using `fearless_simd`,
returns the first bad lane from a bit mask, and scalar-drains the tail. The current 32-row cutoff
and alignment 0..15/differential tests are exactly the right shape. Existing `LAYOUT_AUDIT.md`
reports M3 Pro full-artifact medians of 24,815 us scalar vs 8,392 us SIMD for 250,000 parses and
18,338 us vs 1,801 us at 16,384 rows (but these are parse-loop totals, not hardware counters).

Candidate experiments, in order: (a) benchmark cutoffs 16/24/32/48/64; (b) compare the current
two-slice `chunks_exact` kernel with a single cursor over one byte slice; (c) compare a safe
unaligned typed `[U32<BigEndian>]` view only for scalar/random lookup paths. Do not add unsafe
loads until Miri/sanitizer and first-error differential tests pass. Report ARM NEON and x86 SSE/
AVX2/AVX-512 separately; never make wire bytes host-endian. A possible second-level SIMD scan
must prove the current `u32x4` fallback does not duplicate work on targets where `u32s` is already
four lanes (`rows.rs:68-73`).

### 2. Rank bit-word reads are the likely random-lookup bottleneck (high confidence)

`nudox-root/src/locality/artifact/rank.rs:25-42,85-99,120-125` loops over at most three 64-bit
words per rank and materializes `[u8; 8]` with `copy_from_slice` for every word. This is excellent
for safe unaligned portability but can dominate sparse random lookup after binary search. Measure
rank work by density and query distribution before changing it. Safe candidates are a validated
fixed-width endian view and a local 256-row cursor that carries current block/population for
sequential scans. Unsafe `read_unaligned` is only admissible as a lab candidate with exact lane
geometry, over-aligned/short-tail tests, Miri, and sanitizer. SIMD popcount is not portable by
default (NEON has `cnt`; x86 needs POPCNT/AVX-512 variants), and rank's <=4-word workload likely
does not amortize dispatch.

### 3. Binary-search byte decoding needs a profile, not speculative SIMD (medium-high)

`nudox-root/src/locality/artifact/view.rs:126-149,328-347,357-365` performs a branchy binary
search over sorted 32-bit rows, with four byte indexing operations per comparison. The search is
pointer-chasing and only log2(exception_count) comparisons; SIMD/gather is unlikely to win. Measure
miss/hit/first/middle/last and densities. A safe typed endian slice or cached decoded row block can
remove byte assembly, but increases retained witness/state or alignment assumptions. A branchless
search may reduce mispredicts but can do extra reads; only accept if end-to-end lookup wins and
first-error/identity behavior is unchanged.

### 4. Overlay propagation repeats a merge three times (high confidence for large overlays)

`nudox-root/src/overlay.rs:186-199,227-250,257-275` walks `view.root_rows()` three times and
advances a `Peekable` base merge in each pass; `mark_ancestors` at `202-213` adds a parent-chain
walk. This is ordered data with semantic side effects, so parallelizing the pass is unsafe and
likely loses to cache locality. Benchmark disjoint vs deep shared ancestry. A safe candidate is a
single retained scratch sidecar of base-object presence/descriptor indices, or a fused count/write
state machine after exact preflight; both add memory and must preserve transactional output and
error priority. Do not use a lock-free queue: the API is `&mut` output/scratch and the merge has
ordered canonical semantics.

### 5. Overlay marks are byte-per-row and have a bitset opportunity (medium confidence)

`overlay.rs:134-179` uses `&mut [bool]`, so a 100k-row mark scratch is ~100 KiB and clears every
row. A safe packed bitset could reduce scratch and clear touched words only, but random ancestry
writes may cost extra masks; benchmark sparse/deep and dense cases. Reuse H2's epoch/bitset trial
law and retain exact capacity errors. This is a memory experiment, not a production suggestion.

### 6. Ready bitmap is already the right lock-free granularity (retain baseline)

`nudox-runtime/src/ready_bitmap.rs:56-92` uses one atomic `u64`, `trailing_zeros`, and a weak CAS
loop. It has a clear linearization point and no scan. Sharding or per-slot atomics would add cache
traffic/false sharing and lose the word-level batching. Benchmark contention/retry counts at 1/2/8
producers and 1/2/8 consumers; do not call it lock-free without a Loom ordering/reuse proof.

## Benchmark card

Question: which already validated contiguous or ordered kernel dominates end-to-end latency?
Baseline: production scalar path and current cached-level SIMD path. Candidates: only one axis per
trial (cutoff, rank read representation, or overlay mark representation). Profiles: Apple ARM
client plus x86-64 server. Workloads: rows 0/1/16/31/32/33/256/16k/250k; all-valid, first/middle/
last invalid, every alignment 0..15; rank random/sequential; overlay sparse/dense/deep/shared.
Measures: wall time, cycles/instructions/branches/cache misses where available, allocations,
retained/peak bytes, binary text, first-error equivalence. Adoption threshold: at least 10% end-to-
end win in one declared profile with no >3% regression in the other and no semantic/safety gap.

## Safety and parallelization conclusion

No new unsafe or unstable feature is justified by source inspection. `portable_simd` is not a
drop-in replacement for the existing `fearless_simd` dispatch and would add toolchain/ISA risk.
Hashing is delegated to BLAKE3 and should not be hand-vectorized. Sparse lookup, pointer chasing,
short fixed records, and journaling should remain scalar. Parallel work is only plausible across
independent batches at the caller boundary; the current `&mut` APIs and ordered errors make
intra-operation lock-free parallelism a semantic regression.

## Requested prototype status / BLAKE3 evidence

No new prototype timings were produced in this handoff (the requested expansion was stopped before
running another long benchmark). The only measured SIMD data is the checked-in M3 Pro evidence
quoted above; rank/provider/fixed-decoding/SoA candidates remain unmeasured and are explicitly
inconclusive, not wins. This distinction matters: there is no honest basis for claiming a second
SIMD kernel or a portable-`simd` replacement.

The installed BLAKE3 1.8.6 source exposes architecture-specialized internal `platform::hash_many`
(portable, SSE2/SSE4.1, AVX2/AVX512, NEON, wasm) and public `Hasher::update_rayon` behind its
optional `rayon` feature. `hash_many` is an internal unsafe/platform seam, not an API available to
this workspace. `update_rayon` parallelizes one sufficiently large contiguous stream; it cannot
hash independent object records into independent content IDs while preserving each record's
domain personalization. A batch API would require a separate hasher per object (parallel across
objects), ordering the output IDs by input ordinal, and a measured threshold to amortize Rayon
setup. It must not replace the canonical sequential `ContentHasher` without byte-for-byte tests.

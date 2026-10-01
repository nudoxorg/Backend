# IR lineage and edit locality: source-only experiment

**Base:** `debb60e14` (`codex/index-compiler-tentpole`). **Scope:** source inspection and deterministic Python models only. No Cargo build or test ran. The committed source-only fixtures are `crates/semantic/benches/ir_delta_locality.py` and `crates/semantic/benches/ir_delta_locality_8192.json`; this slice adds `crates/semantic/benches/lineage_edit_locality.py` and its 8,192-row output.

## Finding

Stable-key segmentation can make object transfer and storage highly local, while the current producer still has to traverse and encode the complete semantic family. The current evidence does not support claiming less compiler work. Persistent IR lineage needs a trusted producer frontier that proves the exact compiler inputs and recipe for each reusable result; a stable declaration key, matching payload hash, or source-only package digest is insufficient.

The narrow opportunity is to retain an incremental native compiler session within one `CompilerSessionLineage`, update it from a complete exact input frontier, and let the native dependency graph report which queries/declarations changed. Reuse of canonical output rows then feeds the existing stable-key segmenter and content-addressed object store. Where a language authority cannot prove that frontier, it must keep compiling the complete unit and receive only physical object reuse.

## Source contract

`docs/architecture/compiler-ir-lineage.md` distinguishes package-wide analyzer reuse during one Rust compilation from retaining an analyzer database across edits. It also describes `SemanticDiff` as a borrowed snapshot comparison, not persistent commit/replay history. Stable IDs, segment roots, generation roots, and physical layout IDs have different jobs:

- `PackageLineage` names the logical package; `CompilerSessionLineage` commits compatibility inputs such as profile/toolchain but deliberately excludes source revisions.
- `ExactInputWitness` advances with a source/config revision. A compile attempt also needs its own attempt fence.
- Stable declaration keys preserve identity while row payloads change. Typed segment IDs commit family, stable-key range, and exact bytes.
- `GenerationRoot` commits the typed semantic planes and coverage. `LayoutId` can change during repacking without changing that logical root. A selected branch/frontier is a separate authoritative binding.

The current `stream_canonical_plane_family` in `crates/semantic/src/ir/versioned_records.rs` builds a complete family plan, collects compact stable-key/reader-handle pairs, sorts the full key index, then encodes every row into one bounded output segment at a time. The bounded output buffer limits scratch; it does not mean unchanged rows were skipped. The producer sink in sibling commit `c7a5dd784` commits stable SPIR and jumbo objects into FileStore, reads them back for exact admission, and reports new versus reused objects. Its metric documentation explicitly charges segment validation bytes and jumbo leaf hashes. The associated fixture asserts unchanged jumbo values are still streamed and rehashed.

That producer commit is **not an ancestor** of this experiment base. Its worktree is separate context, not integrated behavior. The commit's test measures physical object locality from a real `SemanticReader` fixture, but explicitly disclaims complete seven-family V2 admission and end-to-end compiler speedup.

For correctness, output reuse must be gated by a complete compiler frontier. The semantic `SemanticInputWitness` carries input root, read-manifest scope, and coverage. A claimed root does not authorize reuse; admission checks the exact scope and complete-coverage witness. The current package-source fallback in `crates/engine/src/application/compiler.rs` hashes ordered paths and source bytes but marks its witness `Coverage::Partial`; it excludes config, lockfile, toolchain behavior, ambient reads, and negative reads. The exact read-set verifier already models positive, negative, and directory-listing reads, but its contract is deliberately stronger than a source inventory. This is the frontier gap an incremental compiler must close.

## Deterministic locality measurements

The 8,192-row source-independent fixture is 6,889,505 bytes before the early-edit scenarios. The figures below are fetched target payload bytes; they exclude old object deletion and closure writes. `CDC` is the fixture's row-boundary content-defined plan. `Prefix` groups the same records by their first stable-key byte. These are model comparisons, not Rust or network timings.

| Edit | 1 MiB ordinal | Row CDC | Stable-key prefix |
| --- | ---: | ---: | ---: |
| 4 KiB attribute growth at head | 6,893,601 | 423,755 | 31,849 |
| 4 KiB attribute growth at middle | 3,747,873 | 833,322 | 26,803 |
| 4 KiB attribute growth at tail | 602,145 | 1,011,614 | 30,167 |
| Insert one declaration at head | 6,890,346 | 2,505,339 | 52,983 |
| Delete one declaration at head | 6,888,664 | 1,426,336 | 52,983 |

The position results matter: CDC is much better than ordinal chunks for an early edit and a head insertion, but it loses to the ordinal layout for the tail edit. Prefix buckets keep this fixture's changed payload lower in all five cases, at the cost of one bucket rewrite and a bucket-size distribution tied to the key function.

SIMD is orthogonal to this result. It can lower the cost of a full scan or hash pass, but it cannot avoid encoding unchanged rows or prove that compiler queries are reusable. First measure and remove redundant passes; then profile the remaining row encoding, boundary scan, and digest kernels. Any vectorized kernel still needs scalar-equivalence checks and the same domain-separated identity bytes.

Physical locality does not equal work avoided. A no-op pair of complete planners reads 13,778,944 bytes and hashes 13,778,944 segment bytes in this fixture. The early attribute edit increases those totals only slightly, to 13,783,040 bytes each; it still materializes and hashes both complete sides. The optimistic persistent row-hash-index model reports 4,937 changed-row bytes hashed for that edit, but derives the changed-key list by comparing both full maps and explicitly does not charge those 16,384 discovery-row visits. That is a lower bound conditional on a trusted compiler change stream, not a measured compile saving.

The added companion fixture exercises two requested cases absent from the earlier row matrix:

- Reversing declaration encounter order while preserving all rows leaves the modeled canonical image and stable-key plans unchanged. A synthetic encounter-order ordinal byte stream fetches all seven chunks (6,889,513 bytes). That stream is only a sensitivity control; it does not claim production NXFI serializes declarations in encounter order. Even the no-op stable-key plans read and hash both complete inputs (about 27.56 MB total).
- Replacing the valid UTF-8 docs value `cafe` (4 bytes) with `café` (5 bytes) preserves the row's stable key. Fetched bytes are 3,743,805 ordinal, 829,254 CDC, and 22,735 prefix. Each planner still reads and hashes both full sides, about 27.56 MB combined.

The existing jumbo model includes random, repeated-byte, and periodic low-entropy data. For three clustered 48-byte edits to a 1.5 MiB random value, the content-defined rope emits 114,354 new target leaf bytes versus 524,288 bytes fetched by 512 KiB ordinal chunks, while scanning and hashing the target value twice costs 3,145,728 bytes. On repeated and periodic data, 144 changed bytes produce 524,288 new target leaf bytes; 1,048,576 bytes of leaf occurrences refer to reused content, but only 262,144 distinct reused-object bytes exist. Occurrence reuse and distinct object reuse must remain separate counters.

No saved NXFI/SPIR corpus was found in this checkout; the measurements above use deterministic generated fixtures. Existing checked-in `.irfrag` files are not an NXFI compiler corpus.

## Experiment needed before claiming compiler-work reduction

Use the old complete compile as an independent oracle for each revision. Keep one native session per exact `CompilerSessionLineage`; advance it only after verifying a captured frontier that includes source bytes, configuration, lockfile/dependency resolution, toolchain/profile/environment, positive reads, missing-path reads, and directory enumeration reads. Fence completion with the exact `CompileAttempt` input root. Then compare the updated session's canonical `Ir` row map and typed plane roots with a clean full compile, including deletions and stable-key re-pairing ambiguity.

Report separate counters for source capture, native queries invalidated/recomputed, declarations lowered, rows encoded, segment/leaf hashes, CAS hits/new writes, bytes transferred, and closure/root work. Run front/middle/tail edits; declaration insert/delete/reorder; docs/visibility/occurrence-only edits; valid multibyte UTF-8; repeated and random content; missing-import creation; config/lockfile/toolchain changes; dependency cycles; and stale completion after a newer edit. A mutation that treats a partial source root as a complete read frontier must make the oracle comparison fail. Keep hidden holdout edits separate from the implementation workload, consistent with `docs/reviews/testing-cutover-research-v10.md` and `docs/research/danluu-testing-agents.md`.

This slice adds only a reproducible locality artifact. No production code changed because the safe reuse boundary depends on compiler authority/read-frontier proof and overlaps the active producer, jumbo, and history cutovers.

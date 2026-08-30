# P1 C1a canonical-root closure packet

## Scope and terminal

This packet closes only the portable, checked-borrowed-root C1a experiment. The accepted code
starts from caller-retained canonical root bytes, validates them once into `ValidatedRoot`, pairs
that witness with the already checked `ValidatedLocality`, and exposes `BorrowedGenerationView`
for exact lookup and one borrowed locality scan. The facts visible through these witnesses are
readable but immutable: private facts plus `Deref`, with no `DerefMut`.

It deliberately does **not** construct `Published`, a receipt surrogate, or a
verified-to-published transition. It also does not claim the later C1b caller-scratch
selection/hydration/complete-closure vertical. Existing `VerifiedGeneration` facts are unchanged.
This is an experiment closure and an integration-review candidate, not product closure.

## Reproducibility and custody

| Item | Exact value |
|---|---|
| Clean source control | `/private/tmp/nudox-orchestra` at `44c22154fd5238e4769562590420371979306050` |
| Manager worktree | `/private/tmp/nudox-prototype-canonical-root-hydration` |
| Manager branch | `codex/prototype-canonical-root-hydration` |
| Accepted code candidate | `316abe63` (`59459baf^..316abe63`) |
| Worker source range retained separately | `6bc8c383^..e850b1ca` on `codex/prototype-canonical-root-hydration-c1a-repair` |
| Baseline control | `Box<[RootRow<DomainTag>]>`, untouched in `packed.rs` |
| Historical rejected builder | `3626021f`, `a4681ec7`, `f8eac9c0` on `codex/prototype-canonical-root-hydration-c1a-luna-builder` |
| Historical invalid C1 falsifier | `9e584d` on `codex/prototype-canonical-root-hydration-c1-falsifier` |

The manager branch was clean before the closure-document commit. Nothing was merged, rebased,
or cherry-picked into `orchestra-shared`.

## Candidate comparison and decision

| Candidate | Retained representation | Temporary validation state | Result |
|---|---|---|---|
| C0 control | one `Box<[RootRow]>`; 64 bytes per root row | existing builder scratch | retained safe control |
| C1 allocation-free Floyd | canonical bytes plus metadata proposal | no retained state, repeated parent search | rejected: a deep 100k chain has quadratic parent follows |
| C1a split parent/state draft | canonical bytes plus borrowed typed rows | parent coordinates plus packed state | rejected before adoption: more state/text than necessary |
| C1a accepted | caller-owned `8 + 63*N` canonical bytes, `&[RootWireRecord]`, O(1) view metadata | one transient `Vec<u32>` of exactly `4*N` requested bytes for nonempty roots | accepted for future integration review |

The accepted validator performs exact geometry first, then a raw descriptor/schema pass with
ordinal/key/source provenance, then retains the checked typed slice. It checks key order and
parent rules, resolves each parent with binary search, and uses destructive memoized Floyd with
`u32::MAX` as root/done sentinel. Parent resolution is O(N log N); the resolved functional-graph
walk is O(N), with one vector allocation that is dropped before `ValidatedRoot` returns.

## Changed surface and formatted LOC

Production changes are confined to `nudox-root`: canonical wire record derivations,
`root_view.rs`, public reexports, the immutable locality-facts witness, and crate-private locality
cursor access. No dependency or manifest changed; no runtime, I/O, mmap, `Arc`, dynamic dispatch,
unsafe code, or per-row owner was added.

| Surface | Baseline-relative formatted LOC | Guard |
|---|---:|---:|
| Production root source | +599 (638 added, 39 removed) | <=620 |
| `root_view.rs` | 576 | <=576 |
| Root behavioral integration test | 335 | <=345 |
| Root/view compile-fail sources | 29 privacy + 15 immutable + 15 escape | 394 total C1a Rust test LOC, <=420 |
| Layout-lab control binary | 283 | <=300 |
| Raw TSV | 25 rows including header | committed evidence, not production code |

The root behavioral matrix includes byte-for-byte permutation canonicality, pointer containment,
header/count/extent truncation, structural and descriptor mutations, complete priority collisions,
absent/missing/invalid parent forms, branch-to-done and incoming-to-cycle witnesses, zero/one and
100k shallow/deep validation, lookup/scan/locality identity stability and mismatch rejection.
The three UI fixtures separately demonstrate private root/view literals (`E0451`), immutable
facts (`E0594`), and root/view lifetime escape (`E0515`). Splitting these fixtures is necessary:
rustc suppresses the private-literal diagnostics when reassignment errors share a file.

## Measured raw evidence

`layout-lab/raw/p1-canonical-root-control.tsv` is a release build on
`aarch64-macos-64`, `rustc 1.97.1 (8bab26f4f 2026-07-14)`, with three repetitions each.
Its stable fields were replayed locally against a fresh 25-line run; excluding `elapsed_ns`, the
diff is empty.

| Workload | Validate-and-drop allocation/deallocation/requested/peak | Warmed get+scan |
|---|---|---|
| zero | 0 / 0 / 0 / 0 | 0 / 0 / 0 / 0 |
| one | 1 / 1 / 4 / 4 | 0 / 0 / 0 / 0 |
| 100k shallow | 1 / 1 / 400000 / 400000 | 0 / 0 / 0 / 0 |
| 100k deep | 1 / 1 / 400000 / 400000 | 0 / 0 / 0 / 0 |

The lab measures exactly validation + pairing + get + scan + drop with locality validated before
the measured scope; the warmed scope measures only get + scan. It does not invent algorithmic
counters. Copy and pointer claims come from source and public pointer-containment assertions,
not allocator arithmetic. The raw timings are retained as context only: shallow validation spans
4.94–5.82 ms, deep 8.78–24.78 ms, and warmed 100k scans about 0.68–0.75 ms on this host.

## Gates and reviewer record

The final independent, read-only Terra review passed the range `9cab67e5..e850b1ca` before
integration. The manager then reproduced two clean gates on the integrated candidate:

1. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
   `cargo test --workspace` in `workspace2`.
2. `cargo fmt --all -- --check`, targeted release `clippy -D warnings` for
   `p1-canonical-root-control`, and three-repeat stable-field TSV replay in `layout-lab`.

All passed. The root test suite includes the four expected compile-fail fixtures.

Every delegated role was explicit and non-inheriting. Luna implementation/mechanical task IDs
were `p1_luna_c1a10_memo_builder`, `p1_luna_c1a11_repair_builder`,
`p1_luna_c1a12a_quality`, `p1_luna_c1a12b_behavior`, `p1_luna_c1a12b2_proof`,
`p1_luna_c1a12b3_assertions`, `p1_luna_c1a12b4_compaction`,
`p1_luna_c1a12c_real_lab`, `p1_luna_c1a12c1_format_replay`,
`p1_luna_c1a12c2_closure`, `p1_luna_c1a12b5_final_compaction`, and
`p1_luna_c1a13_no_panic`, all `gpt-5.6-luna`. Independent read-only reviewer/calibrator IDs
were `p1_terra_c1a8_preedit_review`, `p1_terra_c1a10_artifact_review`,
`p1_terra_c1a11_repair_review`, `p1_terra_c1a12a_quality_review`,
`p1_terra_c1a12b_behavior_review`, `p1_terra_c1a12b2_proof_review`,
`p1_terra_c1a12b3_proof_loc_review`, `p1_terra_c1a12b4_final_behavior_review`,
`p1_terra_c1a12c_lab_review`, `p1_terra_c1a_closure_review`,
`p1_terra_c1a_final_closure_recheck`, `p1_terra_c1a_ultimate_closure_review`, and
`p1_terra_c1a_terminal_review`, all `gpt-5.6-terra`. Cold Luna reader
`p1_luna_c1a8_cold_reader` was also `gpt-5.6-luna`. Earlier BLOCK findings drove committed
cards and narrow repairs; the terminal reviewer PASS is the only acceptance of this C1a range.

## Counterexamples, rejected evidence, and rollback

The strongest falsifiers were retained rather than hidden:

- A first C1 conclusion that safe Rust could not retain an infallible checked typed slice was
  retracted after the accepted `nudox-object-pack` typed-slice precedent was inspected.
- Allocation-free Floyd was rejected before implementation because the deep-chain operation bound
  is quadratic.
- The first builder's synthetic `allocations=1` lab row, byte-sum “scan” metric, tiny test sample,
  and strict-clippy failures remain committed only on the rejected worker branch.
- A wrong global pass priority and a test/LOC accounting mistake were blocked and repaired.
- The compiler diagnostic suppression in a combined forge/reassignment fixture was reproduced;
  the final split has independent source and stderr proof.

Rollback is simple and preserves C0: do not advance a shared branch, or revert
`59459baf^..316abe63` from a future integration branch. The manager branch and all rejected
worker branches remain independent evidence; no shared branch was modified.

## Remaining UNVERIFIED and future integration card

UNVERIFIED: injected allocator-OOM behavior and its exact error source; Linux, Windows, other
macOS, and 32-bit targets; performance outside the recorded local host; C1b caller-reused
selection/hydration scratch through complete closure; and every P2 publication/durable receipt
behavior. These are not silently covered by C1a.

Minimal future shared-integration card: prerequisite is this C1a range plus a fresh source-neutral
consumer audit and independent design review. Its disjoint implementation paths must be confined
to the new C1b consumer seam and the already-owned selection/hydration call sites; it must not
modify P2 publication types or treat C1a facts as publishable. The first public terminal is a
caller that pairs checked root/locality witnesses, supplies reusable selection and hydration
scratch, reaches the existing non-forgeable `VerifiedGeneration` fact after complete closure, and
demonstrates allocation-free post-setup traversal. This is a card for future review only—no merge
or promotion into a shared branch is authorized here.

## Proposed skill findings (at most three)

1. Calibration cards should label allocator-observable facts separately from source/test proofs
   of copies, pointers, and algorithmic work.
2. Compile-fail cards should require one error class per fixture when rustc diagnostic recovery can
   suppress a second claim.
3. LOC cards should use baseline-relative and per-file counts from the start, with a hard
   re-budget checkpoint for review-discovered evidence.

**Verdict: PROMOTE FOR FUTURE INTEGRATION REVIEW.**

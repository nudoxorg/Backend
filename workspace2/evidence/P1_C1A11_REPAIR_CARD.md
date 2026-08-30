# P1 C1a11 proof-and-quality repair card

Base artifact: isolated `c4b8941a` on
`codex/prototype-canonical-root-hydration-c1a-memo-builder`. The C1a10 card at manager commit
`9b73173a` remains the full design authority. This repair only closes findings in
`P1_C1A10_ARTIFACT_REVIEW.md`; it does not authorize C1b, production hydration changes, a
publication fact, new ownership forms, a dependency, or unsafe code.

## Exact repair obligations

1. Split decoder validation after key order into three complete passes: every row's
   `ParentPresent`; every row's `AbsentParentKey`; then every present-parent binary search for
   `MissingParent`. Preserve all earlier phases and exact operand/source values. Add collision
   mutations proving the complete global order.
2. Replace direct hierarchy casts/indexing with short checked, documented private coordinate and
   parent-step helpers. A helper may convert only a non-DONE parent coordinate whose origin is
   the exact root's successful binary search; it must expose the sentinel guard before the array
   access. Floyd must immediately return on equality, and destructive marking must save-next,
   write-DONE, then test-DONE before conversion. Keep one `Vec<u32>` only and drop it before the
   witness returns.
3. Bring `nudox-root` all-target strict clippy to zero diagnostics without broad lint allowance
   or reducing lint scope. Add public-item documentation and narrowly justified existing-project
   safety annotations only where the proof is exact.
4. Expand `crates/nudox-root/tests/canonical_root_view.rs` as ordinary public owning-crate tests
   to every C1a10 row: header/count/geometry/short/trailing/schema/order/parent/missing/cycle
   mutations and priority collisions; canonical permutation/identity; pointer containment;
   zero, one root, one self-cycle, 100k shallow/deep; branch-to-destroyed-tail and incoming-cycle;
   exact lookup and full scan; local-only identity stability and mismatch; immutable fact-read
   source compatibility; scratch/drop and sentinel/overflow source theorem. No test-only
   production APIs and no fabricated counters.
5. Strengthen the two card-permitted hydration UI cases so diagnostics demonstrate a literal
   root/borrowed-view forge attempt, root/locality fact reassignment impossibility, and both
   `ValidatedRoot` and `BorrowedGenerationView` lifetime escapes.
6. Add only `layout-lab/src/bin/p1-canonical-root-control.rs` and its actual emitted
   `layout-lab/raw/p1-canonical-root-control.tsv` (plus any minimal existing-lab export needed).
   Use its installed global `TrackingAllocator`. Build C0 root/canonical bytes and empty C0
   locality artifact outside scopes. Scope A starts before `ValidatedRoot::try_from`, builds the
   paired view from the prevalidated locality, drops view then root, and reports real allocation
   count/deallocation count/requested/peak and elapsed: zero row `0/0,0/0`; nonzero rows
   `1/1,4*N`. Scope B surrounds warmed borrowed `get` plus full scan only and reports `0/0`.
   Execute real 0/1/100k shallow/deep rows at least three times. TSV must state workload, host
   target/profile, scope, observed facts, elapsed, and `allocator_failure_source=UNVERIFIED`;
   it must contain no fields inferred from N, fake checksum, or claimed ordinary-copy metric.

Allowed implementation paths are the C1a10 frozen paths plus the named lab binary/raw TSV and
this repair card's evidence updates. The repair worker must start from the isolated base artifact,
commit each proof-bearing checkpoint on a fresh isolated branch, run formatting, focused strict
clippy, root/hydration tests and UI compile fail, execute the release lab, and return exact raw
results. It must not merge or alter the manager/source/shared branches.

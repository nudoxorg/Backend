# P1 C1a12C real allocator-lab card

Base: isolated repair branch `codex/prototype-canonical-root-hydration-c1a-repair` at
`ef723bec`. Behavior/UI/LOC has independently passed at 416 added test lines under the 420
ceiling. This final C1a checkpoint owns actual lab evidence only: add
`layout-lab/src/bin/p1-canonical-root-control.rs` and the result generated from it at
`layout-lab/raw/p1-canonical-root-control.tsv`. No production/root/test/UI/C1b/P2 change,
dependency, unsafe code, or fabricated counter is authorized.

The binary installs the already-public `TrackingAllocator` globally. For each 0, 1, 100k-shallow,
and 100k-deep workload and three real release repetitions, it builds a valid public C0 root,
writes canonical bytes, and prepares/writes/validates an empty C0 locality artifact *outside*
all scopes. It reports the compiler/target/release profile and the workload/repetition/scope.

Scope `validate_view_drop` begins immediately before `ValidatedRoot::try_from`. Inside it, build
the coherent `BorrowedGenerationView` against the already-validated empty locality, verify a
representative public lookup/scan without allocation-producing collection, drop view, then drop
root before the scope closes. Its observed allocator facts must be exact: N=0 allocations /
deallocations / requested / peak = `0/0/0/0`; N>0 = `1/1/(4*N)/(4*N)`. Record elapsed from an
actual monotonic clock and `allocator_failure_source=UNVERIFIED`.

Scope `warmed_get_scan` starts only after another valid root/view pair exists outside the scope.
Inside it, make a public `get` and fully iterate public `closure` without collection, then close;
observed allocations/deallocations must be `0/0`. Record requested/peak/elapsed only as observed
facts. Do not name a byte sum or checksum a work/copy metric, derive a metric from N, claim an
ordinary copy count, or include fixture allocations in either scope. A mismatch with the exact
expected allocator facts fails the binary rather than writing a favorable synthetic row.

Execute the release command in the card and commit the actual TSV exactly as emitted. The worker
reports host/toolchain, command, raw rows, all scope facts, exit status, and formatted lab LOC.
A fresh independent Terra review verifies source scopes against TSV before any manager integration.

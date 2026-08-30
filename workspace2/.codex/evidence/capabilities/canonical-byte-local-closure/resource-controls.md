# Terminal resource and public-surface controls

## Allocation and lifetime ledger

The raw allocation authority is
`layout-lab/raw/canonical-closure-resources-aarch64-apple-darwin.tsv` (SHA-256
`a1b6aad0f843b262a3a497e789f679d24ec379af8171d940e9fef411e43d2d72`). Values exclude allocator
bookkeeping and unrelated fixture construction.

| Lifetime/site | 1 row | 100,000 rows | Allocation law |
| --- | ---: | ---: | --- |
| caller canonical root bytes | 71 B | 6,300,008 B | caller owner; exact `8 + 63*N` |
| all-resident canonical locality bytes | 49 B | 49 B | caller owner; sparse representation |
| root validation parent lane | 4 B peak | 400,000 B peak | exactly one allocation, zero retained after return |
| returned `ValidatedRoot` heap | 0 B | 0 B | no retained allocation; inline witness is 80 B |
| reusable `ClosureScratch` | 8 B | 800,000 B | two allocations, exact logical `8*N` payload |
| reusable `PlanScratch` | 4 B | 400,000 B | one allocation, actual retained capacity bytes exposed |
| warmed full plan | 0 B | 0 B | zero allocations/peak after scratch construction |
| complete verification | 0 B | 0 B | zero allocations |
| bind + no-request run | 0 B | 0 B | zero allocations |

At 100,000 rows, validation logical peak over an already-owned canonical root is 400,000 extra
bytes. Planning reuses 1,200,000 caller-scratch bytes. Counting caller canonical/locality backing,
the two non-overlapping phase payload highs are 6,700,008 B for root ingress and 7,500,057 B for
planning; stack witnesses and allocator bookkeeping are excluded. The current owned-root control is
13,600,000 B at construction and 6,400,000 B retained, so the borrowed validator is a win only when
canonical bytes already exist; building those bytes from the owned root does not erase the prior
owner peak.

## Work, copies, branches, atomics

- Root ingress performs one typed row-slice cast, `N` authority checks, `N-1` key-order checks, `N`
  parent-row checks, up to `N` parent binary searches, and destructive cycle traversal bounded by
  `4*N` steps in the 100,000-row adversarial oracle. The constant-memory forward-chain control is
  5,000,050,000 parent reads and is rejected.
- The full-plan executable projects exactly `N` rows, follows zero ancestor edges for its all-root
  fixture, invokes the presence predicate exactly `N` times, and final verification exactly `N`
  times. A ranged projection records its explicit projected rows and ancestor edges in caller
  scratch.
- Root/locality/object descriptors are projected as typed by-value stack/register values but no
  retained descriptor/body copy exists. Exact machine copy and dynamic branch counts are compiler
  dependent and remain `unknown` in the atlas rather than being invented.
- The public journey executes no atomics. The separate runtime atomic-ordering controls remain lab
  candidates and do not change production ordering.

## Public layout ledger on aarch64 Apple Darwin

| Public type | size / align | 64-byte span | Owner/lifetime |
| --- | ---: | ---: | --- |
| `BorrowedRootFacts` | 56 / 8 | 1 | canonical root caller |
| `ValidatedRoot` | 80 / 8 | 2 | canonical root caller |
| `BorrowedGenerationView` | 88 / 8 | 2 | root + locality callers |
| `BorrowedGenerationScan` | 48 / 8 | 1 | scan borrow |
| `BoundBorrowedNeed` | 32 / 8 | 1 | view borrow |
| `BorrowedHydrationPlanView` | 176 / 8 | 3 | view + caller scratch |
| `BorrowedStagedGeneration` | 8 / 8 | 1 | plan borrow |
| `VerifiedGeneration` | 64 / 1 | 1 | owned sealed facts |
| `BoundLocalObjectProvider` | 88 / 8 | 2 | generation-proof lifetime |
| `ObjectPackView` | 32 / 8 | 1 | caller pack bytes |
| `ObjectPackObject` | 64 / 8 | 1 | caller pack bytes |
| `PreparedObjectPack` | 32 / 8 | 1 | caller inputs/output boundary |

The complete 1,114-row atlas covers all 19 shipping crates and explicitly records unknown resource
facts. Its release binary is 587,776 B with 434,608 B Mach-O `__text`. The closure-resource binary
is 439,808 B with 291,144 B `__text`; those are whole binaries, not per-type attribution.

## Public surface

| Item | Current consumer and falsifier |
| --- | --- |
| `ValidatedRoot` | chief/root tests; malformed bytes cannot construct it and output borrows input |
| `BorrowedGenerationView` | hydration/chief; unrelated root/locality fails with both facts |
| `Need::bind_borrowed`, `plan_borrowed` | hydration/chief; mismatch, partial, and undersized scratch fail exactly |
| `VerifiedGeneration` | operation/chief; construction is sealed and partial/missing closure cannot issue it |
| `LocalObjectProvider::bind_verified` | operation/chief; stale generation and missing key fail before run |
| `BoundLocalObjectProvider::start()` | chief; no request is accepted after binding; batch/terminal/fusion are exact |

No public helper without a current consumer was retained.

# `TESTING.md` digest and clause mapping

Source: `TESTING.md`, SHA-256 `c29ae328a26117dd347c9b4952b24774c0cd7a5c6b8cc0b28ae2d7f22fda8e7b` at the baseline tree.

| Applicable `TESTING.md` clause | Matrix row | Required local evidence / precise exclusion |
| --- | --- | --- |
| Exact size, alignment, field offsets; isolated allocator evidence | CBC-08 | borrowed witness, generation view, binding witness; 1/100k owner+backing and transient scratch |
| Structured errors preserve source and operands | CBC-01, CBC-02, CBC-04, CBC-06 | root/locality/pack/store/bind faults match variants and source chains |
| Typed wire records borrow supplied input; semantic access does not reparse | CBC-01, CBC-03 | pointer containment and warmed trusted traversal work counters |
| Illegal adjacent state fails exactly and leaves accounting/output unchanged | CBC-01–CBC-06 | truncation, mismatch, partial, failed verification, rejected admission, stale binding |
| Work observable rather than only wall-clock | CBC-03, CBC-08 | scan/lookup/plan/operation counters; constant-body/input-removal mutants |
| Actual public values across crate boundaries; no shadow domain | CBC-04, CBC-09 | chief journey uses root/pack/store/hydration values in ordinary `tests/` |
| Compile-fail forged witness/cross-domain substitution where runtime cannot express law | CBC-05, CBC-07 | retain/extend sealed-witness compile-fail only if current public boundary needs it |
| Foundation truncation/mutation and no repeated validated scan | CBC-01, CBC-02 | every root/locality grammar boundary and representative mutation; no admission on malformed bytes |
| Root/locality build and locality-only changes preserve semantic facts | CBC-02 | paired identity and locality-only mutation controls; complete versus partial authority |
| Store collision/capacity/rejection ownership controls | CBC-04 | existing read-only store controls replayed through verified borrowed body |
| Planner coverage and missing/mixed generation cannot become ready | CBC-03, CBC-05 | complete/partial/missing descriptor planning and verification faults |
| Cursor fusion and unique operation terminal | CBC-06 | bound operation emits one resident batch, one terminal, then fused finish |
| End-to-end actual artifacts; corrupt/truncate/substitute stale generation | CBC-01–CBC-06, CBC-09 | chief journey plus Terra-owned focused faults |
| Async/runtime/durable cancellation and remote transport | exclusion | explicitly forbidden; no such surface belongs to this capability |
| SIMD/cache/power/binary platform comparison | exclusion | no new SIMD/kernel authorized; ordinary layout/allocation/work measurements still apply |

Every exclusion is contractual, not a claim that the general test law is unimportant.

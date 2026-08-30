# Terminal proof matrix

All rows below are controller reproductions, not independent-review transitions. The corrected
review path is `EVIDENCE_BLOCKED`; no row is labelled `REPRODUCED BY TERRA` or approved.

| ID | Law and falsifier | Terminal evidence | State |
| --- | --- | --- | --- |
| CBC-01 | Canonical root bytes validate once into a borrowed witness; malformed grammar, authority, ordering, parent, cycle, and 100k forward-chain faults must preserve exact causes | owning root tests; one typed row cast; one transient `u32` lane; input-pointer containment; `MissingParent` retains insertion coordinate | CONTROLLER REPRODUCED; REVIEW BLOCKED |
| CBC-02 | Root and locality must have one generation identity/count | mismatched ID and row-count tests preserve both operands before selection | CONTROLLER REPRODUCED; REVIEW BLOCKED |
| CBC-03 | Complete borrowed demand/planning cannot accept a partial/mismatched view or allocate after caller scratch exists | borrowed hydration tests; ordered required/present/promised/missing routes; exact scratch faults; warmed allocation control | CONTROLLER REPRODUCED; REVIEW BLOCKED |
| CBC-04 | Selected pack verification precedes borrowed store transfer | pack corruption/digest tests; chief pointer oracle proves stored body is the selected `pack_bytes` slice; zero-allocation lookup/verify control | CONTROLLER REPRODUCED; REVIEW BLOCKED |
| CBC-05 | Only full required-store presence creates the sealed witness | compile-fail construction test; partial and first-missing descriptor tests | CONTROLLER REPRODUCED; REVIEW BLOCKED |
| CBC-06 | Stale identity is checked at bind; bound progress has no request recheck | stale/missing/key/locality owning tests; reusable witness; no-argument `start()`; batch/terminal/fused chief journey | CONTROLLER REPRODUCED; REVIEW BLOCKED |
| CBC-07 | Canonical grammar has one owner and every public item has a current consumer | source scan, semantic lints, dependency/unsafe custody gate, shipping atlas, chief consumer; no new escape-hatch owner | CONTROLLER REPRODUCED; REVIEW BLOCKED |
| CBC-08 | Owner, scratch, layout, passes, allocations, code size, and candidate comparisons are executable at 1/100k | `canonical-closure-resources` raw TSV, all-crate shipping atlas, root 100k work oracle, preserved SIMD/SoA/atomic/Swiss/root controls | CONTROLLER REPRODUCED; REVIEW BLOCKED |
| CBC-09 | Named journey/fault/resource gates are mutation-sensitive and all affected workspaces remain green | chief cfg 1/1; full quality/release/Miri/Loom gates; exact error-source lints found and killed two source-erasure mutants | CONTROLLER REPRODUCED; REVIEW BLOCKED |

The strongest remaining falsifier is semantic, not a compile failure: a caller may pass
`BorrowedStagedGeneration::verify(|_| true)`. The sealed result is predicate-bound rather than
store-identity-bound. See `closure.md` and `terminal-report.md`.

# Fresh direct Terra pre-edit review — findings and disposition

Reviewer task: `/root/canonical_byte_local_closure_terra/pre_edit_reviewer_refresh`; direct dispatch requested `gpt-5.6-terra` / `xhigh`, `fork_turns="none"`. The reviewer observed the registered snapshot config but no independent runtime model/sandbox receipt. Snapshot `/private/tmp/canonical-byte-local-closure-review4.piGBE0/snapshot/workspace2` was exported from `3b11fdbf`; packet SHA-256 was `493734e07f8f6cb51dd4058c108406475cc9b998007b96b5e1368d5a8a7e82c3`.

Manager custody check after return reran the frozen command `(cd snapshot && find . -type f -print0 | sort -z | xargs -0 shasum -a 256 | shasum -a 256)` twice and obtained `44fb2fddfc529be3efda555191101cc6598ea0fdf8f3dcf8d344f400c9204e63` both times. The reviewer’s different reported aggregate is retained as a method discrepancy, not evidence of a mutation.

| Finding | Review result | Sol / Terra disposition |
| --- | --- | --- |
| F-01 snapshot hash mismatch | BLOCKER under reviewer’s undocumented alternate aggregation | manager’s frozen aggregation reproduced before/after; no source mutation; retain method discrepancy |
| F-02 operation cannot name hydration sealed witness under dev-only edge | BLOCKER | Sol `98e4e56e` authorizes promotion of the existing internal hydration edge only; no external dependency, trait/generic adapter, or witness relocation |
| F-03 fault and 1/100k rows lack fixed detailed oracles | BLOCKER | Sol made named faults/resources mandatory; cards must supply exact owning-crate/top-level oracles and raw measurements |
| F-04 old `Provider::start(request)` has rechecks after desired bind | BLOCKER | Luna card must return a bound no-argument runner from `bind_verified`; stale rejects at binding, not during bound progress; legacy request route is not reused by the chief terminal |
| leaf-only pointer coverage | MAJOR | later CBC-04/CBC-09 fault card must prove every required transferred body and substitution failure |

The review’s tripwire scan found no candidate unsafe/SIMD/dependency additions, no panic/unwrap/expect/unreachable, and no candidate public item; the chief test had only fixture `Vec` and typed error mappings. Existing pack `verify` and borrowed memory-store transfer were cleared controls. The strongest surviving concern is hidden native-root reconstruction/reparse, assigned to CBC-01/CBC-08 measurements.

# P1 C1a10 isolated Luna artifact review

Reviewed candidate: `codex/prototype-canonical-root-hydration-c1a-memo-builder`, range
`9b73173a961e12010afa78d9340590b22dd04c09..c4b8941a`. This branch is an isolated
artifact only; no code from it is in the manager branch.

Independent reviewer: explicit non-inheriting `gpt-5.6-terra`, task
`/root/p1_canonical_root_manager/p1_terra_c1a10_artifact_review`, read-only. Verdict:
**BLOCK**.

## Preserved positives

- The raw fixed-width descriptor pass precedes retained `RootWireRecord` typed rows.
- Trusted borrowed `get` and scan use `ObjectRef::from(&row.descriptor)` without a trusted
  `TryFrom`.
- Root and locality facts are private behind immutable `Deref`; the cursor addition remains
  crate-private.
- The encoder edit is within the frozen writable surface, and C0's direct API was not changed.

## Blocking findings

1. P0 — `root_view.rs:182` combines parent-presence, absent-parent-key, and missing-parent
   checks in one per-row pass. C1a10 requires three complete passes in precisely that order.
   A missing parent early and an invalid parent-presence later therefore produces the wrong
   error. Repair must make all three phases explicit and test their collision priority.
2. P0 — no `layout-lab/src/bin/p1-canonical-root-control.rs` or
   `layout-lab/raw/p1-canonical-root-control.tsv` exists. The card's actual allocator/drop,
   warmed access, and 0/1/100k evidence is absent.
3. P1 — `cargo clippy -p nudox-root --all-targets -- -D warnings` fails with 99 diagnostics,
   including unchecked conversions/indexing and missing public docs in the candidate surface.
4. P1 — the public test has two basic tests only. It lacks the card mutation/error matrix,
   scale/deep chain, branch-tail/incoming-cycle, coherent locality, pointer, scan/get, and
   sentinel evidence.
5. P1 — UI fixtures exercise fact assignment and a root escape but not a literal borrowed-view
   forge or borrowed-view escape.
6. P1 — destructive traversal uses raw casts/indexing instead of the frozen, mechanically
   obvious sentinel/coordinate helper. It is plausibly bounded but has not supplied its required
   proof surface or direct tests.

The review also ran: `cargo fmt --check` PASS; `cargo test -p nudox-root --test
canonical_root_view` PASS (2 tests); `cargo test -p nudox-hydration --test compile_fail` PASS
(3 fixtures); and `cargo test -p nudox-root -p nudox-hydration` PASS, all subject to the listed
coverage and lint gaps. No review changes were made.

The reviewer did not dispute the card-permitted O(N log N) total parent-search bound or the
card-permitted hydration UI fixtures. This report is repair input, not acceptance or promotion.

## C1a11 repair rereview

Reviewed range: `c4b8941a..c90519a7` on the distinct isolated repair branch. Independent
explicit non-inheriting `gpt-5.6-terra` reviewer task:
`/root/p1_canonical_root_manager/p1_terra_c1a11_repair_review`. Verdict: **BLOCK**.

The reviewer verified the new complete decoder passes (presence, absent-parent key, then
missing-parent search) and the guarded destructive memoization source invariants. Formatting
passes. The branch still contains no lab binary/raw TSV; strict root all-target clippy fails 70
diagnostics; the root integration test remains 45 lines/2 tests; and the UI fixtures still lack
actual root/view literal and borrowed-view escape cases. These are outstanding proof/quality
gates, not a reason to discard the corrected C1a mechanism. The next repairs are deliberately
sequential: C1a12A quality, C1a12B behavior/UI, then C1a12C measured lab; each requires a fresh
read-only review before the next card.

## C1a12A quality review

Independent explicit non-inheriting `gpt-5.6-terra` task
`/root/p1_canonical_root_manager/p1_terra_c1a12a_quality_review` reviewed
`c90519a7..83631f94` and returned **PASS for its production-only card**. Formatting and strict
library clippy passed, public docs were added, and the C1a11 decoder/sentinel invariants were
preserved. The reviewer found the remaining 12 all-target clippy diagnostics only in the
intentionally incomplete public integration test; C1a12B owns those diagnostics together with
the missing behavior/UI proof. No production regression or unapproved path was found.

## C1a12B behavioral review

Independent explicit non-inheriting `gpt-5.6-terra` task
`/root/p1_canonical_root_manager/p1_terra_c1a12b_behavior_review` returned **BLOCK** despite
passing formatting, all-target clippy, root tests, and compile fail. It found seven exact proof
gaps: byte equality under permutation; global collision priority and geometry; pointer range;
branch-tail/incoming-cycle; exact scan/locality stability/mismatch; semantic sentinel theorem;
and stderr proof that literal fields remain private. Allocation/drop remains intentionally absent
from this card and belongs only to C1a12C. C1a12B2 is a seven-item targeted repair, not a
mechanism rewrite.

## C1a12B2 proof rereview

Independent explicit non-inheriting `gpt-5.6-terra` task
`/root/p1_canonical_root_manager/p1_terra_c1a12b2_proof_review` still returned **BLOCK**, with
five exact test-assertion gaps only: the pointer range was tautological rather than relative to
the input; both required cross-row phase directions were reversed; scan assertions compared keys
but not all entry fields; the two localities were semantically identical; and no literal grammar
geometry theorem was asserted. All checks passed and root/view literal diagnostics are now
specific. C1a12B3 owns only those five assertions.

## C1a12B3 semantic/LOC rereview

Independent explicit non-inheriting `gpt-5.6-terra` task
`/root/p1_canonical_root_manager/p1_terra_c1a12b3_proof_loc_review` verified the B3 semantics
and all checks, then returned **BLOCK** solely on the frozen test LOC stop rule. New root test
is 414 lines; two UI source fixtures add 34, for 448 versus forecast/ceiling 373/420. The
individual root file also exceeds its 320 forecast by more than 20%/25 lines. C1a12B4 forbids a
re-budget and requires readable compaction to root <=384 and total <=420 before lab work.

## C1a12B4 final behavioral/LOC review

Independent explicit non-inheriting `gpt-5.6-terra` task
`/root/p1_canonical_root_manager/p1_terra_c1a12b4_final_behavior_review` initially included the
unchanged 7-line compile-fail harness in the test total. Its read-only recalibration against the
card's baseline-relative LOC rule corrected that error: root test 382, UI 15+19, harness delta 0,
combined 416/420. It returned **PASS**: B3 semantics survived compaction and formatting, strict
clippy, root all-target tests, and compile-fail checks passed. C1a12C now owns only real lab
evidence.

## C1a12C lab review

Independent explicit non-inheriting `gpt-5.6-terra` task
`/root/p1_canonical_root_manager/p1_terra_c1a12c_lab_review` verified the literal scope law,
actual observation rows, 24-row release replay, strict lab clippy, and 227/300 lab LOC. It
returned **BLOCK** only because rustfmt reports diffs in the committed lab source. C1a12C1 is a
two-file format/replay repair. The reviewer also records a direct inherited-environment release
failure in `libm` caused by a non-UTF-8 environment value; a sanitized-environment replay passed
with the committed semantic facts. Allocator-OOM injection remains explicitly UNVERIFIED.

## Initial C1a closure review

Independent explicit non-inheriting `gpt-5.6-terra` task
`/root/p1_canonical_root_manager/p1_terra_c1a_closure_review` returned **BLOCK** on two final
lab-evidence conditions only: format must be run from the independent `layout-lab` workspace,
and the raw TSV must name `schema_provenance`, `typed_slice`, and `hierarchy` setup categories
required by the master card. All other C1a gates passed: strict root/lab clippy, behavior/UI,
C0 preservation, source scope, replay semantics, and LOC (production 596/620, tests 416/420,
lab 277/300). C1a12C2 authorizes only those two final artifact corrections and a fresh replay.

## Final C1a closure recheck

Independent explicit non-inheriting `gpt-5.6-terra` task
`/root/p1_canonical_root_manager/p1_terra_c1a_final_closure_recheck` passed every executable and
source/replay gate after C1a12C2, including lab-workspace formatting and setup metadata, but
returned **BLOCK** on the master card's per-delta LOC stop rule. The root test is 382 against a
320 forecast (+62), although aggregate tests are 416/420, production is 596/620, and lab is
283/300. C1a12B5 is a no-rebudget test-only compaction to <=345; all platform findings remain
UNVERIFIED exactly as the closure report recorded.

## Ultimate C1a closure recheck

Independent explicit non-inheriting `gpt-5.6-terra` task
`/root/p1_canonical_root_manager/p1_terra_c1a_ultimate_closure_review` found all scope, replay,
behavior, UI, C0, and baseline-relative aggregate caps sound, but returned **BLOCK** on three
repairable gates: root-workspace formatting of the compact test; nine production `expect` calls
and two trusted `get`/scan omission fallbacks in `root_view.rs`; and a forge UI source that
formats to 37 versus a 28-line forecast. It also confirmed full lab all-target clippy fails only
in unchanged baseline `layout-lab/src/main.rs`; the authorized control binary lint passes.
C1a13 is a four-file final no-panic/format/UI repair with no P2 or public behavior expansion.

## C1a14 diagnostic split exception

An external safe compiler probe, confirmed by parent review, established that a fixture with both
private struct literals and mutable reassignment errors emits only the latter `E0594` diagnostics.
The required literal (`E0451`) and immutable-facts evidence must therefore use separate safe UI
fixtures. C1a14 records the one necessary new non-production fixture and a 407/420 explicit test
budget exception; it does not alter production, C1b, P2, or public APIs.

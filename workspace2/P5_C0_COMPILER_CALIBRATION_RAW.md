# P5 C0-COMPILER calibration raw index

## Custody

This index is intentionally separate from the closed C0-IR calibration deck.  It will be completed
only with raw task/model transport results for the digest of `P5_C0_COMPILER_MANAGER_CARD.md` committed
alongside it.  No C0-IR result authorizes this capability.

```text
worktree: /private/tmp/nudox-prototype-real-compiler-ir
branch: codex/prototype-real-compiler-ir
committed card path: workspace2/P5_C0_COMPILER_MANAGER_CARD.md
R15 pre-calibration card/skeleton commit: 0741bebef3479f1a5e701e5d2246a85f10c82931
complete-card SHA-256: 933b72bcd4bce8ccc2bbfb48817816225e9fbdfecff7464efad94652cb8b6dd3
frozen skeleton SHA-256: lib.rs bbe0152ce639ee7f92a9b72e26dd6b040133a28f3700f04aee302abd1c4fe867;
  dispatch.rs 588ee2b4c5ec3ad7847970ff316640a7ca05343ab66e75aea890b8880acdd9ab;
  subset.rs 05bc6f455335106bac3812f4cde299d97890592fddd1c07a6ec1043213c77b8a;
  release_consumer.rs b86ecd9c1c15f855e8523883250b89ab96f042708b54e4ccd21f01edfd4a76c8
seed runner SHA-256: run-valid-cell-mutant.sh 4d30f95619e37c64fc5fa9d94fc9175b3a9f1b344220f0396a5e51e2494564d4;
  run-typescript-lower-mutant.sh bc692fc9e0aca831280ce3eb2cd0b625637665891b52e2978215de03fef89801;
  run-same-source-control.sh 6cc31be5688e9381bc3138babba6da6d06394ab763e6f4bc9fa152218eb12715
verification: git show 0741bebef3479f1a5e701e5d2246a85f10c82931:workspace2/P5_C0_COMPILER_MANAGER_CARD.md | shasum -a 256
```

## Required deck

| role | explicit model | permitted inputs | required result | status |
| --- | --- | --- | --- | --- |
| cold reader one | `gpt-5.6-luna` | card and governing skills only | exact terminal, paths, evidence rows, caps, reserve, negative space | R15 CLEAR; `calibration/R15_ROLE_RESULTS.md` |
| cold reader two | `gpt-5.6-luna` | card and governing skills only | independent exact restatement | R15 CLEAR; `calibration/R15_ROLE_RESULTS.md` |
| plausible misreader | `gpt-5.6-luna` | card and governing skills only | cheapest seemingly compliant wrong patch and card gap | R15 CLEAR; `calibration/R15_ROLE_RESULTS.md` |
| reviewer calibration | `gpt-5.6-terra` | card, governing skills, skeletons, seeds, prior consumer/owner-body artifacts | literal tripwire table and defect severities | R15 CLEAR; `calibration/R15_ROLE_RESULTS.md` |

The separate non-inheriting R15 hostile pre-edit Terra review is CLEAR at
`calibration/R15_HOSTILE_PREEDIT.md`. It is the sole authority for the following one-path Luna
checkpoint on this exact digest.

The prior R9 hostile/pre-edit, build, postbuild, closure, and final gates are **superseded evidence**,
not authority: Root reproduced the subset test's inherited binary metadata stdout. The R10 skeleton
explicitly sent rustc stdout to `Stdio::null()`, but the first R10 cold reader found a separate
diagnostic-cardinality weakness. The R11 skeleton additionally rejects an uncoded primary error while
permitting only rustc's literal one-previous-error summary. R11 Terra then correctly found that the card
needed to state its intended lifecycle: pre-edit review calibrates the frozen detached skeleton while
production remains untouched, and all R9 codegen must be regenerated post-repair. R12 cold reader one
then found the lifecycle's singular destination wording did not explicitly bind it to the one named path
of a given checkpoint. R13 resolved that scope, then R13 cold reader one found an orphaned review-table
actor and “production” wording that could exclude test/example destinations. R14 named all three Terra
review outputs and every authorized destination path, but R14 cold reader two found the required table
schema and reviewer-object mapping still implicit. R15 pins both; it requires every R15 role and fresh
hostile pre-edit review before one exact-path Luna repair checkpoint.

The R10 skeleton-observability observation is superseded with the R10 deck. R11 executed the exact new
frozen subset test with `--nocapture`; its clean textual terminal and new primary/summary predicate are
retained at `evidence/p5-c0/compiler/calibration/R11_SKELETON_OBSERVABILITY.md` before reader authority.

## Staleness rule

Any semantic card/skeleton change, model mismatch, blocker, or major ambiguity stales every result.
The raw result directory is keyed by the complete-card SHA-256; the committed card SHA, card digest,
task identity, explicit model request, and returned artifact are retained there before a pre-edit
reviewer or builder is commissioned.

## Rejected R10 deck

R10 is churn. Root reproduced inherited raw binary metadata on the normal subset-test terminal, so its
prior C0 closure was correctly reopened. The stdout-null repair changed the skeleton/card digest, then
R10 cold reader two found that the text diagnostic predicate could accept an additional uncoded primary
error. The R10 misreader was interrupted and no R10 Terra reviewer, hostile review, or builder was
authorized. R11's exact primary/summary parser, fresh digest, fresh clean-terminal execution, and full
four-role deck replace it; no R10 return carries authority.

## Rejected R11 deck

R11 is churn. Both R11 cold readers and its misreader cleared the improved stdout and diagnostic rule;
R11 Terra confirmed the frozen detached skeleton passes its exact local semantics and has a clean textual
terminal. It stopped build authority because production correctly remains untouched before the authorized
checkpoint, but the card had not said that lifecycle explicitly and R9 control/codegen evidence had not
yet been declared superseded for the new digest. The R12 card resolves both points. This card-only
semantic clarification restarts the full deck; R11 results do not authorize a builder.

## Rejected R12 deck

R12 is churn. R12 cold reader one confirmed the repaired skeleton lifecycle but found the phrase “that one
destination” could mean all four authorized paths instead of only the path in a particular builder grant.
Cold reader two was interrupted and the remaining roles were not commissioned. R13 states that every
checkpoint names one path, changes only that path, and leaves all other production paths unchanged until
their own grants. This card clarification restarts the full deck; no R12 return authorizes a builder.

## Rejected R13 deck

R13 is churn. R13 cold reader two cleared the one-path lifecycle and the exact stdout/diagnostic contract.
R13 cold reader one then found an orphaned “It” before the review-table requirement and production wording
that could omit the test/example destinations. The R13 misreader was interrupted; no Terra reviewer,
hostile review, or builder was commissioned. R14 explicitly names every Terra table and every authorized
destination path. This card clarification restarts the full deck; no R13 return authorizes a builder.

## Rejected R14 deck

R14 is churn. R14 cold reader one cleared the named reviewers and destination scope, but R14 cold reader
two found the literal tripwire table lacked required columns and a reviewer-to-object mapping. No R14
misreader, Terra reviewer, hostile review, or builder was commissioned. R15 fixes the table schema and
object mapping. This card clarification restarts the full deck; no R14 return authorizes a builder.

## Rejected partial deck

The two source-blind reader returns for card digest
`ca5ba77bf7b0648130d2d130cbb10ed4ef9146076cc27d62e241abf42693986f` are churn only.  A hostile
pre-edit read found three material gaps before the remaining roles completed: the dedicated target was
not cleaned before actual-rlib discovery; the codegen law could accept an uninspected direct call one
frame outside a wrapper; and the retained control was not mechanically same-source/same-callable.
The in-flight plausible-misreader was interrupted.  The card must be recaptured with a new digest and
all four roles restarted; no reader output carries forward as authority.

## Rejected R2 deck

The complete R2 deck at card digest
`1a8d3f98962a839cfe397fb3033ca9d7d5c284e5a2dfff2d62b587a58d8c46e4` is churn.  The plausible
misreader identified missing per-skeleton hashes, total coded-error wording, mechanical target-freshness
custody, residual-call closure semantics, and literal scan rules.  The separate Terra reviewer found a
major ambiguity: the card's unqualified macro/tag ban could reject standard test syntax and the private
example-only diagnostic result in its own frozen skeleton.  The narrowed card must digest-pin every
skeleton, count one coded error total, bind zero-after-clean/one-after-build rlib cardinality and hashes,
state the no-regression/residual-call verdict, and scope the macro/tag ban to dispatch/capability
mechanisms before a fresh full deck.  None of R2 grants edit authority.

## Rejected R3 deck

The complete R3 deck at card digest
`7f42cd019286c7170cb53c79b8e14eb90a18ae0707d8271375b804d5c2837da4` is churn.  The plausible
misreader challenged nominal shared forwarding, while the independent Terra reviewer found `RustSubset`
had no direct public consumer/falsifier.  The resolved specimen deletes that redundant Rust public
subset.  It keeps the exact Rust-success/TypeScript-error and TypeScript-member-absence falsifiers as
the genuine-row proof, and permits a helper only when it preserves those differences and removes rather
than adds representation.  No R3 result grants edit authority.

## Rejected R4 deck

The two R4 cold reads at card digest
`391197759418c09621da72deaff2eafc5561810f82b824a579194f57b75eb7b1` are churn.  One reader correctly
found that the forecast table called its limits “protected cap” without saying they were net-delta rather
than total-file limits, making the frozen totals appear impossible.  The card now gives each baseline,
forecast delta, net-delta cap, skeleton total, total-file cap, and per-file/aggregate unused capacity.
No R4 result grants edit authority.

## Rejected R5 deck

The R5 readings are churn.  Their Terra calibration reviewer correctly stopped the deck because the
promised executable mutation was not initially supplied, then the first supplied rejected-cell patch did
not mutate a valid forwarding cell.  The repaired R6 custody has an explicit runner and separately
retained raw `101` red output for Rust Parse, Rust LowerIr, TypeScript Parse, and the exact TypeScript
LowerIr error contract.  It also records its codegen limitation: successful release-example LLVM
emission is not a before/mutant callable comparison, so no input-removal or zero-cost conclusion carries
forward.  No R5 result grants edit authority.

## Rejected R6 deck

R6 completed its four-role calibration on card digest
`26501daa96a0cbcd065d55a617ed1cbd396ad081a0a27bb139ae3cc6c662e145`; its successful spawn/model/task
record remains at `evidence/p5-c0/compiler/calibration/R6_ROLE_RESULTS.md`.  The separate hostile Terra
pre-edit review found a blocker: the frozen release consumer formatted a `FrontendError` payload as a
constant message without reading it, so warnings-denied Clippy could not pass while the consumer also had
to be byte-identical to the skeleton.  The R7 skeleton consumes and presents that exact typed error,
passes the detached frozen fmt/test/Clippy gate recorded in `calibration/R7_SKELETON_GATE.md`, and changes
the card/skeleton digest.  Therefore all R6 results are churn and grant no authority.

## Rejected R7 deck

R7 completed its four-role deck on card digest
`d30b53c129552e37e3baec11cd4b156a3b5833f7ed4cbd787ef37dfacacaa553`; its role record remains at
`evidence/p5-c0/compiler/calibration/R7_ROLE_RESULTS.md`.  The separate hostile Terra pre-edit review
found a MAJOR: direct same-source `rustc` release control/candidate compilation named only the registry
rlib even though the consumer directly imports `nudox_compile_vocab`.  That made the claimed direct
artifact comparison non-executable and Cargo-example fallback is forbidden.  R8 pairs exactly one fresh
registry rlib and exactly one fresh compile-vocab rlib, hashes both, passes both explicit `--extern`
flags with one dependency search path, and retains its successful replay in
`evidence/p5-c0/compiler/control/R8_PREEDIT_CONTROL.md`.  The card digest changed, so all R7 results are
churn and grant no authority.

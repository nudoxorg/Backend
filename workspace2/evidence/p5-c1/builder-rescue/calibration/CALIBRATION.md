# C1 builder rescue calibration custody

## R9 rejected deck

Frozen R9 card commit: `949a37880ccc7dd2b5242867ba3440d15fbb68c1`.

| role | explicit model/fork | result |
| --- | --- | --- |
| independent reader one | `gpt-5.6-luna`, `fork_turns=none` | card/control boundary restated |
| independent reader two | `gpt-5.6-luna`, `fork_turns=none` | no product-authority ambiguity |
| plausible misreader | `gpt-5.6-luna`, `fork_turns=none` | proposed copy-backed prepared state with lifetime marker |
| reviewer calibration | `gpt-5.6-terra`, `fork_turns=none` | REJECT: provenance, 256 conversion, consumer coverage, unnamed offsets |

R9 grants no edit authority. The next card digest must include the root-directed repairs and run a fresh
four-role deck plus a separate hostile pre-edit Terra review.

## Inadmissible transport churn

The R11 reader reported `/private/tmp/nudox-orchestra`; it is inadmissible because this prototype's
only repository is `/private/tmp/nudox-prototype-real-compiler-ir`.

The R12 reader passed the correct checkout preflight at `ebfefc02`, but read
`workspace2/evidence/p5-c1-rescue/C1_FORMAT_RESCUE_MANAGER_CARD.md` rather than the canonical
`workspace2/evidence/p5-c1/builder-rescue/C1_BUILDER_RESCUE_CARD.md`. It is also inadmissible.
Neither report is a calibration result for the builder-rescue card.

## R17 circular-gate counterexample

At `29395af02daf4493f3ff5c0258b81cc3a51e7209`, a Luna reader correctly reported that the then-current
receipt only contained historical R9 material, but the role prompt incorrectly allowed it to treat the
absence of the very fresh receipt it was supposed to supply as a card defect. This is retained solely
as a role-prompt counterexample. It is not a calibration finding and grants no authority.

## R18 fresh four-role deck

Frozen card commit: `cf16f5ad68ff938f3a10e38bf02f793783681e1c`.

Every role reported this preflight before its read-only review:

| field | required and observed value |
| --- | --- |
| `pwd` / git top level | `/private/tmp/nudox-prototype-real-compiler-ir` |
| branch | `codex/prototype-real-compiler-ir` |
| HEAD | `cf16f5ad68ff938f3a10e38bf02f793783681e1c` |
| worktree | clean (`git status --short` empty) |

The old R9 calibration/hostile receipts and PRE-EDIT pending mutation/codegen/closure receipts were
explicitly identified as historical or expected non-evidence, not as prerequisites for this deck.

| task | explicit model / fork | result and material scope |
| --- | --- | --- |
| `r18_reader_one` | `gpt-5.6-luna`, `fork_turns=none` | APPROVE: no internal inconsistency, authority escape, or missing requirement in the card and named custody. |
| `r18_reader_two` | `gpt-5.6-luna`, `fork_turns=none` | APPROVE: ledger matched; checked API, provenance, counts, sentinels, rlib, mutations, codegen, caps. |
| `r18_plausible_misreader` | `gpt-5.6-luna`, `fork_turns=none` | APPROVE: copy/marker, wrapping, partial-write, magic/body/lane, derived-view, validation, and codegen evasions have named falsifiers. |
| `r18_terra_reviewer` | `gpt-5.6-terra`, `fork_turns=none` | APPROVE: public fence, write/view order, target-bounded codegen, caps, and closure gates have no material ambiguity. |

This deck authorizes only the separate hostile pre-edit Terra review of this exact card and its frozen
control. It does not authorize a production builder until that review is clear.

## R19 final fresh four-role deck

Frozen card/control commit: `427867ed6a6b3716a57d1855c1be50f3008f13f0`.

Every admissible role supplied exact raw preflight values: `pwd` and git top level
`/private/tmp/nudox-prototype-real-compiler-ir`; branch `codex/prototype-real-compiler-ir`; HEAD
`427867ed6a6b3716a57d1855c1be50f3008f13f0`; empty `git status --short`. Each was read-only and
limited to the canonical card plus its named skeleton, executable-control, mutation, and codegen
artifacts. R9/R18 are historical rejected findings; the absence of this R19 receipt before these roles
was expected.

| task | explicit model / fork | result and material scope |
| --- | --- | --- |
| `r19_reader_one` | `gpt-5.6-luna`, `fork_turns=none` | APPROVE: exact-five-field provenance, direct-validation, causal-mutation, and feasibility-only boundaries are complete. |
| `r19_reader_two` | `gpt-5.6-luna`, `fork_turns=none` | APPROVE: all nine ledger digests match; public, range, writer/view, rlib, lexical-consumer, codegen, and cap rules are complete. |
| `r19_plausible_misreader` | inadmissible predecessor, then `gpt-5.6-luna`, `fork_turns=none` retry | The predecessor failed to report git top level and is discarded. The retry APPROVEd: no cached-state, public-field, indirect-validation, count/write, rlib, mutant, or codegen bypass remains. |
| `r19_terra_reviewer` | `gpt-5.6-terra`, `fork_turns=none` | APPROVE: R18 hostile repairs are operational and no material ambiguity remains. |

This final deck authorizes only a separate hostile pre-edit Terra review of the exact card, frozen
control, and this receipt. It still does not authorize production editing until that hostile review is
clear.

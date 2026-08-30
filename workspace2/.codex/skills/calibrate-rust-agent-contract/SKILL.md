---
name: calibrate-rust-agent-contract
description: Forward-test one demonstrated ambiguity in a workspace2 agent card, skill, reviewer rule, or rubric. Use after recurring misinterpretation; never as Phase-0 authorization, a required pre-edit ceremony, or a substitute for implementation.
---

# Calibrate one agent-contract ambiguity

Calibration is a regression test for instructions. It is not part of every capability cycle and it
never blocks reversible implementation. Use it only when a real transcript shows that two capable
agents interpreted the same rule differently or repeatedly gamed the same rubric row.

## Input

Supply only:

```text
the exact ambiguous rule and current digest
one realistic task/card that triggered it
the observed wrong interpretation
the observable decision that must become deterministic
```

Do not ask the evaluator to read the full roadmap, every skill, historical evidence, or the intended
implementation. Preserve the user's public intent; calibrate role behavior, not product architecture.

## Minimal blind trial

Use one fresh agent that has not seen the prior failure. Give it the revised rule plus the realistic
card and ask it to state its first actions, stop conditions, and evidence. Add a plausible
misinterpretation fixture only when the original failure involved rubric gaming or reviewer severity.

For a reviewer rule, provide a small concrete diff with one seeded defect and one legal near-neighbor.
The reviewer must reject the defect and clear the legal case for the right semantic reason. For a
worker rule, the trial must choose an executable red/code action rather than packet construction or
broad research.

Do not require a fixed three-agent deck, source-isolated custody, runtime-model receipts, or repeated
full rereads. Those mechanisms previously consumed implementation capacity without testing the
specific ambiguity.

## Rewrite rule

Change the narrowest owning instruction. Prefer a do/don't example, executable lint/test, or explicit
stop rule. Delete conflicting older language instead of appending an amendment. A changed role rule
does not invalidate unrelated production evidence or require all agents to restart.

If the fresh trial still misinterprets the same decision, revise once more. After two failures,
replace prose with a tool/type/test where possible or return the unresolved decision to Sol. Never
loop through new cards, digests, reserve arithmetic, or wording-only evidence commits.

## Passing result

Calibration passes when the fresh agent makes the intended observable decision, rejects the seeded
defect without rejecting the legal neighbor, and the official skill validator passes. Return the raw
trial, the exact deleted/added rule, and the remaining known boundary. Passing calibration improves
future dispatch; it does not authorize or score product code.

---
name: write-evidence-rubric
description: Create or refine a workspace2 executable capability rubric that Luna can implement continuously, Terra can strengthen through research and hostile review, and Sol can use for final integration. Use for living capability rubrics and stable 0–10 closure rubrics; never use as pre-edit authorization or self-scoring ceremony.
---

# Write an executable Rust rubric

A rubric is a collection of falsifiable engineering laws, not prose goals or a reason to delay code.
Start from the public terminal, current red, direct consumer, and resource/authority boundaries. Load
the shared craft/domain sections relevant to those rows; do not require full-plan rereads.

Compose from one executable vertical outward. The first mandatory row must traverse the real public
input to the real public terminal and fail for a constant or fake adapter. Remaining rows strengthen
that same vertical at distinct invariant boundaries: authority, failure/restart, resource/work,
concurrency, and diagnostics only where applicable. Do not create one row per file, test, command,
type, language, backend, or stylistic preference. Merge rows that have the same owner and falsifier;
split a row only when its parts can progress independently without inventing a second terminal.

## Row schema

Every row is independently executable:

```text
ID and observable law
why user/system behavior depends on it
exact falsifier or command
exact expected evidence
anti-cheat mutant that must fail
resource/error/ownership bound
mandatory or same-direction stretch
dependencies and non-overlap
state: RED | IMPLEMENTING | GREEN | ATTACKED | SUPERSEDED
research decision and remaining uncertainty
```

Each row names its proof tier: focused edit, capability closure, or chief integration. A command is
owned by exactly one tier. Do not attach cold Nix, full-workspace Dylint, live services, Miri/Loom,
and end-to-end corpus runs to every row; attach each expensive proof once where its inputs are fully
composed. A Luna card should normally receive one or a few adjacent rows with one focused red command.

“Elegant,” “fast,” “robust,” “well tested,” “low allocation,” or “production ready” are invalid
without an observable definition. Compilation and test totals prove no row. A row must distinguish
the desired capability from a constant body, ignored input, mixed authority, weak error, unbounded
owner, or other plausible substitute.

## Living refinement

Terra owns the rubric during implementation. Research may add a missing row, strengthen a falsifier,
or supersede one representation-specific row. It never makes unrelated green evidence stale. A
change to the public terminal, permanent wire semantics, authority owner, or cross-capability
dependency returns to Sol; implementation choices remain Terra's job.

Luna receives only assigned rows and continues until they are green and self-attacked. Luna does not
edit anchors or score itself. The Terra reviewer tries to falsify rows and finds missing rows,
especially simplification, file-boundary, negative-API, and resource cases. Terra adjudicates and
repairs; Sol performs the final system-level pass.

## 0–10 semantics

- **0:** absent, inverted, or unverifiable.
- **2:** local happy path or weak test that a plausible mutant passes.
- **4:** coherent component behavior; boundary/failure/resource evidence materially incomplete.
- **6:** integrated primary path with hostile cases; named mandatory gaps remain.
- **8:** the complete approved capability: every mandatory row is `ATTACKED`, exact public/fault/
  resource evidence reproduces, and no blocker/major finding survives.
- **9:** the same capability wins under one predeclared harder workload, fault, platform, or resource
  target without weakening eight.
- **10:** independently reproduced stretch evidence plus materially tighter predeclared measures and
  breakage-resistant enforcement. Ten is never more features or more abstraction.

No weighted average can compensate for a missing mandatory row. Full plan completion is exactly
eight; stretch begins only after eight.

## Hard caps

- correctness, source/owner loss, unsound authority, unsafe uncertainty, or false durability: below 4;
- missing public integration, restart/fault, or cancellation proof where applicable: below 6;
- unbounded memory/work/export or remote machinery in the portable client: below 6;
- performance claim without raw end-to-end control: that row at most 4;
- tests that assert only success/no panic or survive the anti-cheat mutant: testing at most 2;
- unresolved blocker/major: no closure score.

Do not score source length, test count, generic count, dependency count, allocation count, SIMD,
unsafe, agent activity, or documentation volume. Score the observable consequence and charge the
mechanism through its resource/coupling rows.

## Calibration without ceremony

Before calling a reusable rubric stable, blind one fresh reviewer on three small concrete artifacts:
a compiling weak mutant, a complete-to-plan candidate, and a legitimate same-direction stretch. The
rubric must place them below eight, at eight, and above eight for the correct reasons. Also try a
source-compression patch, duplicated easy tests, and benchmark-only optimization; none may improve
the result without capability evidence.

Calibrate only ambiguous rows. Do not require two rubric writers, multiple cold readers, source
custody, model receipts, or full-deck reruns for every edit. If two attempts still disagree by more
than one anchor, split or rewrite that row and prefer a tool/test over more prose.

## Output

Return the living row table, current states, hard caps, raw evidence links, anti-cheat results,
research-driven changes, reviewer disagreements, and unverified boundaries. The writer does not
score its own implementation; Terra maintains capability state and an independent reviewer/Sol uses
the stable rubric for acceptance.

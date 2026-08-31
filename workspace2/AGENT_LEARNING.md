# Agent learning registry

This registry turns repeated agent friction into system improvements. It is not product evidence,
progress, or a required pre-edit read. Every capability has one shared working journal at
`.codex/learning/capabilities/<capability-id>/insights.md`. Luna, Terra, the Terra reviewer, and Sol
append to that same file from their own altitude; nobody creates a private competing narrative.
Terra triages it continuously. Sol periodically promotes recurring patterns here and closes them
with an enforceable mechanism.

## Capability journal contract

Create the journal with the first red test or implementation commit, never as a pre-edit gate. Keep
it short enough to scan in one sitting. It has four append-only sections:

```text
Observed       raw recurring behavior with artifact/commit and role
Explained      Terra root-cause or alternative analysis
Corrected      local code/test/rubric repair and its result
Promoted       Sol-level type, lint, gate, skill, or explicit task-local disposition
```

- Luna records a concrete surprise, weak instruction, recurring temptation, or failed mutant with
  its next technical commit. It does not diagnose architecture or pause implementation to journal.
- Terra reads new observations while reviewing the same commit, merges duplicates by fingerprint,
  researches the cause, and changes the live rubric or next dispatch before the pattern repeats.
- The Terra reviewer records only findings that escaped Luna and Terra, especially simplification,
  representation, testing, and boundary failures. It never rewrites builder history.
- Sol samples the journal and representative raw diffs at every candidate return. It promotes an
  urgent safety/correctness issue immediately and runs a deeper cross-capability review after every
  three returns or any repeated fingerprint.

Every journal edit accompanies code, test, lint, rubric, or review work. A journal-only commit is a
process smell unless it closes a scheduled cross-capability trend review. Insight count is never a
score; silence is not success, and volume is not progress.

## Observation schema

One issue is one terse record:

```text
fingerprint: stable kebab-case behavior name
role: luna | terra | reviewer | sol
capability and commit
observed behavior and concrete artifact
why the current rubric/skill/tool allowed it
local correction attempted and result
suggested enforcement: type | dylint | clippy | test | repository gate | skill | task-local
occurrences: capability IDs, not repeated turns in one loop
state: observed | watching | promoted | closed | task-local
owner and closing artifact
```

Record behavior, not blame or personality judgments. Multiple turns in the same failure loop count as
one occurrence. Luna never stops implementation to polish this record; it adds the observation with
its next code/test commit. Terra deduplicates fingerprints and records the disposition at candidate
return.

## Promotion rules

- Promote immediately when one occurrence can silently violate correctness, authority, ownership,
  source preservation, durability, boundedness, or safety.
- Promote after two independent capabilities when a pattern creates avoidable complexity, weak tests,
  allocation/copy, file-boundary drift, or orchestration churn.
- Prefer type/visibility, then semantic lint, compile-fail/property/fault test, repository layout gate,
  and finally prose. A promoted prose rule must state why stronger enforcement is not sound.
- A lint needs a positive and negative fixture, explicit exclusions, and a full shipping-workspace
  inventory. A skill change needs one realistic blind forward test only for the ambiguity it changes.
- Close an issue only with the commit/test/lint that prevents recurrence. Moving text between skills
  is not closure.

Sol reviews new capability-local observations after each Terra return and performs a deeper trend
review after three capability returns or whenever the same fingerprint appears twice. The deeper
review samples raw Luna diffs and reviewer findings, not only summaries. Its output is a small set of
promotions or explicit task-local dispositions, never another general advice document.

## Research basis

The loop deliberately combines four externally proven ideas without copying their ceremony:

- Anthropic's orchestrator-workers pattern supplies dynamic delegation, while its
  evaluator-optimizer pattern supplies the separate Terra reviewer and measurable feedback loop:
  <https://www.anthropic.com/engineering/building-effective-agents>.
- Anthropic's agent-evaluation guidance supplies executable graders, anti-cheat mutants, and review
  of the whole work trajectory rather than only a plausible final answer:
  <https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents>.
- SWE-agent's agent-computer-interface result motivates the small role-specific context packet and
  exact commands instead of indiscriminate transcript/document loading:
  <https://arxiv.org/abs/2405.15793>.
- Google SRE's postmortem culture motivates a blameless shared incident record whose action items
  close only when recurrence is mechanically prevented:
  <https://sre.google/sre-book/postmortem-culture/>.

These sources justify mechanisms, not permanent process. Keep a mechanism only while repository
evidence shows that it improves executable progress, defect escape rate, or integration cost.

## Seeded findings from the first orchestration audit

| Fingerprint | Evidence | System cause | Enforcement | State |
| --- | --- | --- | --- | --- |
| pre-edit-evidence-gate | P5 stopped after two calibration dispatch failures; older waves returned custody-only blockers | conflicting Phase-0 and action-first rules | rewritten manager/calibration skills; product blockers exclude topology | promoted |
| full-context-frontload | P5 read thousands of lines before its first implementation attempt | unconditional “read completely” cascades | progressive role-specific context packets | promoted |
| evidence-commit-dominance | closure history contained 25 evidence/docs commits versus 16 code/test commits | evidence treated as authorization and progress | one live rubric plus raw artifacts and final receipt | promoted |
| late-structure-review | first root implementation combined parsing, hierarchy, selection, scanning, and tests in one large module | review began after broad implementation | early structure review mode and Terra-owned simplification | promoted |
| accessor-ceremony | public one-line field getters survived existing lint | lint only recognized already-public fields | expand semantic analysis and add legal authority getter fixture | promoted |
| stringly-adapter-schema | Qdrant used nested dynamic JSON and string diagnostic details | rubric named error fidelity but no adapter schema gate | typed DTO/error lints plus adapter review inventory | promoted |
| vector-copy-during-score | Qdrant cloned response vectors before immediate scoring | ownership ledger omitted response-borrow lifetime | borrowed decode/scoring; allocate only for escaping readback | closed by `964bb666` |
| lint-registered-not-enforced | two semantic lints and UI fixtures existed but the shipping runner omitted their deny flags | lint registration and enforcement maintained separate name inventories | shipping Dylint runs with `-Dwarnings`; focused UI fixtures retain per-lint diagnostics | promoted |
| fixture-present-not-executed | the lint crate compiled while four compiletest goldens still failed and a fake auxiliary crate collided with the driver sysroot | implementation evidence substituted for executing the consumer harness | real macro provenance fixture plus the pinned UI gate and exact golden output | promoted |
| dependency-lock-gate-drift | the full Dylint inventory reached the adaptive plane and stopped before analysis because its lockfile lagged its manifest | focused checks did not execute the same locked, offline shipping path | regenerate the lock deterministically and keep the full repository gate terminal | corrected |
| capability-fanout-explosion | one cross-crate Sol spawned lease, capacity, witness, Qdrant, application, IR, durability, parsing, and adaptive managers | every valid adjacent finding became a new active lane instead of a chief backlog item | three top-level tasks by default; two child slots per task; one public vertical per manager | promoted |
| stale-baseline-integration | long-running branches began far behind canonical and later overlapped unified crate moves and shared manifests | branch activity outlived its integration assumptions | new delivery tasks start from current canonical; old branches are mechanism corpora only | promoted |
| repeated-closure-gates | agents repeatedly rebuilt cold Nix, Dylint UI, full workspaces, and duplicate clean targets after narrow edits | verification had no risk-tier ownership or invalidation rule | `quality.sh` focused/capability/closure tiers and the shared proof ladder | promoted |
| transcript-context-flood | multi-hour raw outputs and exhaustive initial prompts consumed context before a live decision | role packets mixed product history, every quality law, and broad research goals | sample current diff, mutant, finding, and repair; progressively load one historical artifact | promoted |
| adjacent-research-dispatch | stronger reviews repeatedly opened new capability managers instead of repairing or returning the owned vertical | discovery and scheduling authority were conflated | Terra records out-of-scope counterexamples for Sol; research continues only while the current vertical advances | promoted |

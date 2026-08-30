# Nudox orchestration system

This document defines custody of architecture, implementation, review, and evidence. Rust craft laws
live once in `.codex/skills/deliver-reviewed-rust-slice/SKILL.md`; role skills reference that source
instead of copying it.

## Truth boundaries

| Role | Owns | Must not own |
| --- | --- | --- |
| Sol chief steward | capability graph, public red integrations, cross-crate architecture, semantic policy lints, final integration, shared-skill learning | a Terra's worker queue, repeated unit-gate reproduction, scoring its own merge |
| Terra capability orchestrator | proposal space, research journal, evolving proof matrix, Luna dispatch, unit/fault oracles, measurements, candidate integration and verification | product-wide architecture, final shared merge, independent review of its own work |
| Luna proof implementer | one frozen card, proof-bearing code, focused tests/measurements, small commits, relentless self-check against the matrix | changing the contract, choosing product architecture, declaring a capability complete |
| Terra hostile reviewer | counterexamples, semantic tripwires, deletion opportunities, evidence gaps, performance trade-offs | edits, repair design, acceptance, scores without an adopted rubric |
| Terra rubric writer | observable anchors, caps, calibration artifacts, anti-gaming mutations | implementation, architecture invention, scoring the artifact used to author the rubric |

Role resolution is useful provenance, not product authority. Every dispatch names the registered role
and expected model/effort from `.codex/config.toml`, but missing router or sandbox telemetry cannot
make otherwise reviewable code inadmissible. Record the runtime identity once when available and keep
moving against executable product evidence.

## Capability cycle

1. **Chief boundary.** Sol names one public terminal and writes a concise red integration/fault test
   or executable oracle for it. The brief states product laws, negative space, shared baseline, and
   paths owned by concurrent work. Sol does not select the internal representation.
2. **First executable checkpoint.** Terra traces the owning consumer, writes one red falsifier, and
   immediately starts the narrowest reversible implementation. A brief or matrix may clarify the
   contract, but neither is a gate and neither needs a pre-edit review.
3. **Parallel proposal and research.** Terra launches workers on independent rows when useful and
   studies primary sources, production code, and measured controls concurrently. Every research
   tranche must alter the next test, representation, or implementation decision and be bracketed by
   a commit, red test, reproduced failure, or integration verdict. Two tranches without such progress
   stop research and return the manager to implementation.
4. **Luna convergence.** Luna workers receive disjoint paths or isolated branches, one or more red rows, exact
   commands, and a stopping boundary. They commit each proof-bearing checkpoint. They may simplify
   repeatedly, but cannot broaden the public contract or turn a failed design into a weaker claim.
5. **Terra verification.** Terra reproduces worker evidence, compares alternatives, integrates only
   the strongest mechanisms, and updates the journal and proof matrix. Losing prototypes remain
   inspectable until a symbol-level salvage ledger accounts for their invariants, algorithms,
   diagnostics, tests, and measurements.
6. **Independent attack.** At the first material checkpoint and closure candidate, a separate Terra
   attacks concrete code without write access when the role is available. Terra converts accepted
   findings into falsifiers and bounded repairs. Reviewer routing or source-isolation telemetry may
   qualify an independence claim, but failure to obtain it never stops implementation, local review,
   or integration of a candidate whose product evidence is otherwise reproducible.
7. **Candidate return.** The primary Terra returns one committed candidate with zero unresolved
   blocker/major findings, the research journal, proof matrix, reviewer output, exact gates, and the
   strongest surviving counterexample. It returns early only for a genuine product-authority fork or
   a bounded `EVIDENCE_BLOCKED` receipt for a reproducible product, authority, external-system, or
   toolchain blocker after two distinct implementation attempts. Missing agents and receipt-format
   failures are orchestration incidents, not product blockers.
8. **Chief integration.** Sol reviews the candidate in the context of every affected crate. It may
   transplant a few mechanisms, refactor surrounding owners, or reject most of the surface. It does
   not repeat Terra's whole unit-test campaign; it runs the public red journey, affected workspace
   gates, semantic lints, and novel cross-cutting attacks. A fresh blind Terra review strengthens a
   risky shipping repair when available; its absence does not suspend the repair behind ceremony.

## Durable artifacts

Each capability has a stable lowercase kebab-case ID and lives under
`.codex/evidence/capabilities/<capability-id>/`. Its `index.toml` binds artifact digests, baseline and
candidate identities, every worker/reviewer runtime receipt, and retention/supersession state. The
schema lives in `.codex/skills/manage-rust-swarm/references/capability-evidence.md`.

The directory may keep four small committed artifacts when they reduce ambiguity. They are updated
in place rather than through amendment chains:

- **brief:** public terminal, laws, negative space, baseline, concurrent path ownership, `TESTING.md`
  digest and applicable-clause mapping;
- **proof matrix:** one falsifiable row per law and its current state;
- **research journal:** source, mechanism, experiment, decision changed, saturation signal;
- **closure receipt:** candidate identity, exact evidence, reviewer custody, remaining uncertainty.

Every research row names the unresolved matrix or representation decision it informs and the code or
test changed by the finding. Once a tranche changes no decision, research on that question stops;
remaining uncertainty stays explicit.

## Evidence, not volume

Source/document volume, test totals, generic totals, parameter totals, dependency totals, allocation
totals, and agent turns are never objectives or caps. They can locate change but cannot establish
quality. A larger coherent function may be preferable to callback or parameter-object coupling.

Track properties that survive refactoring:

- number and ownership of representable invalid states;
- duplicated invariant owners and cross-crate dependency direction;
- cognitive/control-flow complexity at semantic transitions;
- allocations, peak live bytes, copies, scans, branches, atomic traffic, and queue bounds;
- codegen/text cost for monomorphized or SIMD paths;
- exact error/source/owner preservation;
- mutation sensitivity of public, fault, concurrency, and restart tests;
- current consumers for public surface and generic dimensions.

An abstraction earns itself by deleting a measured cost, invalid state, repeated proof, or unstable
dependency. It is not rewarded for compressing source text.

## Enforcement ladder

Use the strongest mechanism that can express a law without false positives:

1. type or visibility boundary;
2. compiler/Clippy/Dylint semantic lint with pass/fail UI fixtures;
3. compile-fail, property, Loom, Miri, or mutation test;
4. public integration/fault/performance oracle;
5. hostile review checklist;
6. prose only when the law is contextual and cannot be mechanized.

Repository regex remains appropriate for dependency inventory, generated-file custody, and exact
forbidden tokens. It must not infer Rust semantics. Every custom lint documents its semantic match,
intentional exclusions, diagnostic, allow mechanism, and a nearby legal fixture.

## Merge law

Never merge a large prototype merely because it is complete, and never delete one merely because a
replacement is smaller. Partition it into representation, invariant, algorithm, diagnostic, oracle,
and measurement mechanisms. For each useful mechanism, name the old symbol, new owner, replayed
falsifier, and mutation that kills the replacement. Whole-block removal is valid only when every row
is absorbed or an executable counterexample proves that the claimed capability was false or harmful.

The shared branch advances by observable capabilities, not activity. A merge report names the public
terminal, strongest attack, retained mechanisms, affected gates, and unresolved uncertainty.

## Primary references

- [OpenAI Codex subagents and custom agents](https://learn.chatgpt.com/docs/agent-configuration/subagents.md)
- [OpenAI Codex skills](https://developers.openai.com/codex/skills)
- [Dylint workspace libraries and custom linting](https://github.com/trailofbits/dylint)
- [Rust compiler diagnostics and lint development](https://rustc-dev-guide.rust-lang.org/diagnostics.html)

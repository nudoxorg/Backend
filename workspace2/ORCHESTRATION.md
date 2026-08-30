# Nudox orchestration system

This document defines custody of architecture, implementation, review, and evidence. Rust craft laws
live once in `.codex/skills/deliver-reviewed-rust-slice/SKILL.md`; role skills reference that source
instead of copying it.

## Truth boundaries

| Role | Owns | Must not own |
| --- | --- | --- |
| Sol chief steward | capability graph, public red integrations, cross-crate architecture, semantic policy lints, final integration, shared-skill learning | a Terra's worker queue, repeated unit-gate reproduction, scoring its own merge |
| Terra capability orchestrator | focused research, living executable rubric, Luna dispatch, unit/fault oracles, alternative comparison, ruthless simplification, candidate integration and verification | product-wide architecture, final shared merge, independent review of its own work |
| Luna proof implementer | assigned rubric rows, proof-bearing code, focused tests/mutations/measurements, coherent commits, relentless self-check and post-green cleanup | changing the public contract, choosing product architecture, declaring a capability complete |
| Terra hostile reviewer | early boundary attack, closure counterexamples, deletion/simplification opportunities, evidence gaps, performance trade-offs | edits, acceptance, scores without an adopted rubric |
| Terra rubric writer | observable anchors, caps, calibration artifacts, anti-gaming mutations | implementation, architecture invention, scoring the artifact used to author the rubric |

Role resolution is useful provenance, not product authority. Every dispatch names the registered role
and expected model/effort from `.codex/config.toml`, but missing router or sandbox telemetry cannot
make otherwise reviewable code inadmissible. Record the runtime identity once when available and keep
moving against executable product evidence.

## Capability cycle

1. **Chief boundary.** Sol names one public terminal and writes a concise red integration/fault test
   or executable oracle for it. The brief states product laws, negative space, shared baseline, and
   paths owned by concurrent work. Sol does not select the internal representation.
2. **Living rubric and immediate dispatch.** Terra traces the owning consumer, writes the first red,
   creates executable mandatory rows, and immediately dispatches or implements them. The rubric
   evolves row by row and never authorizes work; unchanged green rows survive clarification.
3. **Terra foresight.** Terra stays ahead of Luna by researching the next hard representation,
   storage, concurrency, allocation, protocol, or library decisions. Each result becomes a
   discriminating experiment, stronger row, or dispatch. Two consecutive searches that change
   nothing saturate that question.
4. **Luna convergence.** Luna continuously implements assigned rows, tests anti-cheat mutants,
   simplifies the complete owned diff after green, commits coherent recovery points, and continues
   without requesting approval. It stops only at the assigned boundary or an exact authority fork.
5. **Terra verification and compaction.** Terra observes real diffs and raw gates, compares
   alternatives, integrates the strongest mechanisms, and performs the hard simplification Luna is
   least suited to discover. One return receives one accept, falsifier-bound repair, or rejection;
   it never triggers a new packet/calibration cycle.
6. **Independent attack.** At the first material vertical candidate and closure candidate, a separate Terra
   attacks concrete code without write access when the role is available. Terra converts accepted
   findings into falsifiers and bounded repairs. Reviewer routing or source-isolation telemetry may
   qualify an independence claim, but failure to obtain it never stops implementation, local review,
   or integration of a candidate whose product evidence is otherwise reproducible.
7. **Candidate return.** The primary Terra returns one committed candidate with zero unresolved
   blocker/major findings, the living rubric, research decisions, reviewer output, exact gates, and the
   strongest surviving counterexample. It returns early only for a genuine product-authority fork or
   a bounded `PRODUCT_BLOCKED` receipt for a reproducible product, authority, external-system, or
   toolchain blocker after two distinct implementation attempts. Missing agents and receipt-format
   failures are orchestration incidents, not product blockers.
8. **Chief integration.** Sol reviews the candidate in the context of every affected crate. It may
   transplant a few mechanisms, refactor surrounding owners, or reject most of the surface. It does
   not repeat Terra's whole unit-test campaign; it runs the public red journey, affected workspace
   gates, semantic lints, and novel cross-cutting attacks. A fresh blind Terra review strengthens a
   risky shipping repair when available; its absence does not suspend the repair behind ceremony.

## Durable artifacts

Keep only artifacts that help the next technical decision: one compact manager packet, one living
executable rubric, concise research decisions, raw measurements, and one final closure receipt.
Worker/reviewer runtime metadata is optional provenance recorded once at return, not an admission
gate. Do not commit Phase-0 authorization, dispatch receipts, digest rebinding chains, amendment
stacks, or evidence-only progress between code/test commits. Git already retains history.

Progressive context is mandatory: Sol holds the capability graph and system history; Terra receives
one capability packet plus shared/domain craft; Luna receives assigned rubric rows and direct source;
the reviewer receives rubric, diff, consumer, and tests without builder rationale. More advanced
context is loaded only when a live decision needs it.

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

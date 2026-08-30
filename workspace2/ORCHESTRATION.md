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

Role resolution is part of evidence. Every dispatch names the registered role and expected model/
effort from `.codex/config.toml`; neither a task label nor a model string alone proves that the role's
instructions, sandbox, checkout, and skill contract were loaded. The capability index records the
runtime task ID, resolved config path, model, effort, sandbox, baseline, and checkout.

## Capability cycle

1. **Chief boundary.** Sol names one public terminal and writes a concise red integration/fault test
   or executable oracle for it. The brief states product laws, negative space, shared baseline, and
   paths owned by concurrent work. Sol does not select the internal representation.
2. **Terra Phase 0—sequential and pre-edit.** Terra traces current consumers, commits the brief,
   coupling skeleton, proof matrix, resource controls, and red falsifiers, maps every applicable
   `TESTING.md` clause to a matrix row or evidenced exclusion, and passes the frozen packet through a
   source-isolated pre-edit Terra review launched by the distinct review sidecar below. No
   production-writing Luna starts before Phase 0 closes.
3. **Parallel proposal and research.** Immediately after Phase 0, Terra launches registered Luna
   workers on independent rows and spends the same interval studying primary sources, upstream
   production code, and measured local controls. Every useful finding enters the committed journal;
   changed laws version the matrix and invalidate stale worker cards.
4. **Luna convergence.** Luna workers receive disjoint paths or isolated branches, one or more red rows, exact
   commands, and a stopping boundary. They commit each proof-bearing checkpoint. They may simplify
   repeatedly, but cannot broaden the public contract or turn a failed design into a weaker claim.
5. **Terra verification.** Terra reproduces worker evidence, compares alternatives, integrates only
   the strongest mechanisms, and updates the journal and proof matrix. Losing prototypes remain
   inspectable until a symbol-level salvage ledger accounts for their invariants, algorithms,
   diagnostics, tests, and measurements.
6. **Independent attack.** At the pre-edit design, first material candidate, and closure candidate,
   a separate Terra receives the frozen artifacts without the builder's rationale and with no write
   access to the source snapshot. Because
   live parent permissions override child profile defaults, the writable manager invokes a distinct
   Codex parent process whose `workspace-write` root is a disposable build directory, with implicit
   `$TMPDIR` and `/tmp` writes disabled, while the source snapshot is a separate readable path and is
   not added as writable. That parent only spawns the
   registered reviewer and returns its raw JSON receipt. Terra
   converts accepted findings into new falsifiers; Luna receives the bounded repair, never the
   reviewer's prose as architectural authority.
7. **Candidate return.** The primary Terra returns one committed candidate with zero unresolved
   blocker/major findings, the research journal, proof matrix, reviewer output, exact gates, and the
   strongest surviving counterexample. It returns early only for a genuine product-authority fork or
   a bounded `EVIDENCE_BLOCKED` receipt containing raw operational failures and an external owner.
8. **Chief integration.** Sol reviews the candidate in the context of every affected crate. It may
   transplant a few mechanisms, refactor surrounding owners, or reject most of the surface. It does
   not repeat Terra's whole unit-test campaign; it runs the public red journey, affected workspace
   gates, semantic lints, and novel cross-cutting attacks. Any Sol shipping repair gets a fresh blind
   Terra review before merge.

## Durable artifacts

Each capability has a stable lowercase kebab-case ID and lives under
`.codex/evidence/capabilities/<capability-id>/`. Its `index.toml` binds artifact digests, baseline and
candidate identities, every worker/reviewer runtime receipt, and retention/supersession state. The
schema lives in `.codex/skills/manage-rust-swarm/references/capability-evidence.md`.

The directory keeps four small committed artifacts, updated in place rather than through amendment
chains:

- **brief:** public terminal, laws, negative space, baseline, concurrent path ownership, `TESTING.md`
  digest and applicable-clause mapping;
- **proof matrix:** one falsifiable row per law and its current state;
- **research journal:** source, mechanism, experiment, decision changed, saturation signal;
- **closure receipt:** candidate identity, exact evidence, reviewer custody, remaining uncertainty.

Every research row names the unresolved matrix or representation decision it informs. Research on
that decision stops only after two independent relevant sources or experiments add no falsifier,
candidate, resource bound, or decision change; remaining uncertainty stays explicit. Two nearby or
irrelevant sources cannot manufacture saturation.

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

# Cold reader one — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_legal_cold_reader_one`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

Calibration-only findings

Custody tuple (verified exactly):

- worktree: `/private/tmp/nudox-prototype-real-compiler-ir`
- branch: `codex/prototype-real-compiler-ir`
- card commit: `17af65fcda9ed091a2a75f6dfd2d96d0d60aa76d`
- card path: `workspace2/P5_C0_MANAGER_CARD.md`
- digest: `003193151bf359b44c06d03cca7651e53664ec7a36a4521ebaa1f6ee94eb707d`

The tempting misreading is to treat the compiler fixture as merely a local type-check demonstration—possibly with shadow `EntityId`/`TypeId` types or without compiling the legal `TypeId` mutant. Literal card law rejects this: the fixture must import the actual exported `nudox_ir_vocab` rlib, resolve exactly one artifact, produce exactly one causal `E0308`, retain stderr, and first prove the legal mutant compiles.

Findings:

- Card scope and C0-IR boundary are unambiguous.
- The pinned 34-line production source and sole writable test path are explicit and enforceable.
- The 95-line skeleton, 110-line hard cap, and 15-line unused reserve are explicit.
- Actual-artifact causality, exact-one rlib resolution, and legal-mutant requirements are sufficiently precise.
- Raw evidence obligations name the required commands, toolchain, source/rlib digests, command, stderr, and evidence paths.
- The local `Option` is an explicit narrow exception for zero-or-one artifact discovery; it is not semantic state or a terminal. Review should not misclassify that use as the forbidden fallback/terminal pattern.

Hard blockers/major ambiguities: none.

Remaining unverified: implementation, source-pinned comparison path, skeleton execution, model-runtime identity, and all builder/reviewer evidence. No implementation approval is implied.

Verdict: CALIBRATION PASS — sufficiently unambiguous for the fresh four-role deck.

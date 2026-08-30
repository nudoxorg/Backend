# Cold reader two — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_typed_cold_reader_two`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

Frozen tuple verified:

- Worktree: `/private/tmp/nudox-prototype-real-compiler-ir`
- Branch: `codex/prototype-real-compiler-ir`
- Card: `workspace2/P5_C0_MANAGER_CARD.md`
- Commit: `91f7fe86e93f934ff639dd74cd2a313f4abddc03`
- SHA-256: `3977db393490c5e64d6bbcb75f0ef2ab5a9d311e6a2a62e912f2d97cf6ba0e90`

No source, candidate, history, evidence, skeleton, or working tree was inspected. No edits were made.

## Strongest false proof

The strongest false proof is a compile-fail fixture that appears to prove “kind impossibility” while only proving a local argument mismatch—or worse, passes because the diagnostic predicate was mutated rather than the forbidden `EntityId` expression.

The card correctly limits the claim to direct typed use and explicitly preserves the public `raw`/`new(u32)` rebrand counterexample. Therefore the representation is not falsely claimed to provide global construction safety. The proof is valid only if the legal mutant changes exactly the forbidden expression to a legal `TypeId` expression, the mutant compiles, and the original predicate then fails.

Literal disposition: retain the narrow direct-use claim; reject any broader “cannot rebrand” interpretation.

## Findings and blockers

1. **Current-executable dependency discovery is internally uneven — MAJOR ambiguity.**
   The fixture derives its dependency directory from `current_exe().parent()`, but the manager’s preparation command hardcodes `domains/ir/target/debug/deps`. A configured `CARGO_TARGET_DIR` or workspace target override can make the manager’s “exactly one” check inspect a different directory from the fixture. The clean-preparation command must bind to the active target directory or explicitly prove that the configured target directory is the hardcoded one.

2. **Legal-mutant causality is under-specified — MAJOR ambiguity.**
   “Legal `TypeId` mutation” must identify the exact source token/expression being mutated and retain the mutation diff or raw input. Otherwise a test could mutate the predicate, diagnostic matcher, or error-count logic and falsely appear causal. Require the original and mutant source-bearing expression plus successful mutant compilation.

3. **Fresh rlib binding is an evidence obligation, not fully enforced by the command — QUESTION/UNVERIFIED.**
   The command counts one rlib and prints source/rlib hashes, but does not itself assert the source digest equals the recorded baseline or that the rlib is from the same active target/toolchain used by the fixture. The manager must retain those bindings explicitly; until then this row is unverified.

4. **“Exactly one coded compiler error” needs a literal counting rule — QUESTION.**
   The card requires one `E0308`, source-visible `EntityId` and `TypeId`, and exactly one compiler error, but does not define the stderr grammar for counting errors. The skeleton may resolve this; the Terra review must verify it does not accept warnings, secondary spans, or a second diagnostic hidden by formatting.

## Literal dispositions

- Four-byte size/alignment assertions: **accept as host-only layout proof**; not a cross-target ABI claim.
- Direct typed-use safety: **accept narrowly**, with raw rebrand limitation preserved.
- One `E0308` plus `EntityId`/`TypeId`: **accept only with causal mutant and exact error-count predicate**.
- Actual public rlib: **accept in principle; unverified until target-directory and digest binding are retained**.
- Sole writable test path and unchanged production source: **clear and literal**.
- 100-line skeleton / 110-line hard cap / 10-line reserve: **clear**, though the percentage threshold should state its reference explicitly.
- Retained raw stderr and mutant command: **required and appropriate; unverified until artifacts exist**.
- No broadened scope: **clear**. Compiler dispatch, fragments, frontends, registries, and C1 are explicitly excluded.
- Source-pinned comparison path: **intentionally unverified**, as required by the card.

## Calibration-only verdict

**NOT YET AGREED / REWRITE BEFORE EDIT AUTHORITY.**

The card’s central contract is sound and appropriately limits the raw-rebrand claim, but the target-directory binding and legal-mutant causality need literal clarification. Any semantic rewrite invalidates this deck and requires all four fresh calibration roles to rerun.

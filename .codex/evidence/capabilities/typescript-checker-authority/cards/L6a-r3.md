# Card L6a-r3: collect must tell the truth — instrument the fold, retain the fault, make the real file lower

registered role: nudox_luna_implementer (expected `luna`/max; L6a session,
third card)
baseline: 067751a15 (your L6a-r2 result). Same mandatory environment:
  export PATH=/opt/homebrew/bin:$PATH
  export NODE_PATH=/Users/mileswirht/Downloads/backend/node_modules
  export CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-typescript/.local/target

## Terra diagnosis (reproduced in the worktree; verify first)

- The REAL npm file @babel/parser@7.26.8 typings/babel-parser.d.ts
  (8183 bytes; pinned sha256 d35d24bb0029bdd4932b1705b936b60002f9faa1ac0f166c3cf0ff4b23a7156e)
  reaches the public terminal ONLY as
  `LoweringUnsupported::NoSupportedDeclaration` on ALL three routes:
  compile_ir(None), compile_ir(Some(checker report)), and the native route.
  Your 40-member union falsifier passes, so the Plugin union per se is
  fixed; some OTHER pooled-lane fault fires and is erased by the fold at
  typescript.rs ~line 57-68 (`fn fault(_cause: FactFault) -> lane_rejection()`,
  self-described as "a recorded lane criticism").
- Eliminated by Terra bisection as the trigger: JSDoc density (17-JSDoc
  member interface lowers OK), the Plugin union alone, tokTypes index
  signature, the export list, plain alias cycles. The fault is somewhere in
  the remaining construct mix — your first job is to NAME it, not guess.
- The file lowers the declared plane from OXC alone (compile_ir(None)
  fails identically), so the trigger is reachable without the checker.

owned paths: compiler/driver/lower/typescript.rs, compiler/driver/lower.rs
(TypeScript surface only), compiler/driver/types/lowering.rs (ONLY if the
decision table below requires it — FactFault is `pub` in the shared
driver types; additive changes only, every sibling lane's forced match
gains its arm by rustc custody; run each sibling lane's lib check to
prove no breakage), compiler/driver/tests/typescript_lower.rs.
NOT owned: compiler/ir/**, compiler/vocabulary/**, sibling lanes' lower/
files, checker.rs, main.cjs, render/lifecycle tests.

## Required behavior

1. **Instrument, name, then fix.** Temporarily log the exact FactFault
   variant + operands at the fold site while running the real file's bytes
   through compile_ir (commit the bytes as a test fixture ONLY if size
   permits a slimmed reproducer; otherwise keep the diagnostic log as your
   evidence receipt and craft the minimal source fixture that triggers the
   SAME FactFault variant). Report the variant in your return.
2. **Decision table.**
   - If the fault is a CAPACITY fault (TypeRowCapacity/ChildCapacity/
     Capacity/ComputedRowCapacity/TypeChildCapacity): the lane's geometry
     is too small for real packages. Raise the involved constant(s) to fit
     the corpus reality with the new memory bound documented at each
     constant (arrays are lane-side; state the per-compile high-water).
     Then the real file must LOWER with zero faults.
   - If the fault is a LAW fault (ChildRole/ChildTarget/TypeChild/
     TypeRecord/OccurrenceOwner/...): that is a lowering bug — fix the
     lowering so the file lowers honestly; the fault stays reachable for
     genuine bugs.
   - In BOTH cases: no public vocabulary change. Add a lane-internal
     `TypeScriptCollectError` arm retaining the FactFault, and at the
     public conversion write the exact fault Debug into the bounded
     diagnostic_output scratch (it already exists on the emit path) while
     the public variant stays coarse. A future rejection thus names its
     exact cause in the diagnostic lane instead of erasing it.
3. **Falsifiers.**
   - The real file's slimmed reproducer (committed fixture) lowers OK; its
     decoded fact counts are asserted >= the checker's declaration census.
   - A forced-fault falsifier: construct a source that overflows the
     chosen capacity genuinely (e.g. > pool rows) and assert the public
     failure is the coarse variant while the diagnostic output contains the
     exact FactFault name — the fold no longer erases, it delegates.
4. **Regression watch.** typescript_lower (31), render (14), authority (9),
   package (1), crate (1+17+5), lib check clean. PLUS the two sibling-lane
   lib checks IF you touched types/lowering.rs: `cargo check -p
   compiler-driver --lib` covers the shared binary; run it with the
   lane-local target.

## Checkpoint protocol

One commit expected, prefix `fix(typescript):`. Return: commit sha, the
diagnosed FactFault variant + operands, the decision-table branch taken,
the constant/bug change with its memory bound, focused outputs, and the
smallest remaining red row. If the true fix requires compiler/ir or
vocabulary changes, STOP and return ESCALATE with the exact surface.

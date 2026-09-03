# TRUNK shared-surface capability brief

## Public terminal
Six Sol-final decisions landed on trunk (compiler/ir, compiler/driver, compiler/publication,
compiler/application, compiler/vocabulary, server/journal publication), with every existing
schema-1 fragment still validating and all six language lanes unblocked:

1. ESC-1/ESC-2 computed type-fact segment; TS golden D1 ignore killed.
2. D2 emission geometry raise + re-derived EMISSION_BUDGET; six.py ignore killed.
3. Journal chained generations with parent linkage; conflict only on same-chain-position divergence.
4. Cause retention: push_fact/shared terminals keep FactFault/ProjectionFault operands; exact
   CompileFailure arms incl. ExtensionAtomUnbound projection.
5. Render truth: python int/None render, fn signature tails render, C-pure headers drop the
   `pub `-family vocabulary.
6. Shortcut hunt on every touched file.

## Non-negotiable laws
- Schema-1 fragments decode byte-identically (additive decode; validator accepts schema in {1,2}).
  The type-fact header change (declared_count+computed_count) is exactly the authorized version
  bump; FRAGMENT_SCHEMA moves to 2.
- Declared type-fact segments (anonymous rows then fact rows) keep strictly-backward children.
- Computed rows trail declared rows; a computed child may target any declared row (both
  directions) or a strictly-earlier computed row; no declared row may target a computed row.
- NOMINAL cells everywhere validate against ENTITY space (< entity_count); the diagonal
  self-nominal stays legal. Schema-1 keeps the historical nominal law during its decode.
- Computed rows' owners are entities.
- Generation chain: identical content resubmission stays idempotent; divergent content claiming
  the same chain position stays a typed Conflict with retained facts.
- RejectedFact operands (ordinal, name bytes, FactFault) survive to CompileFailure; the
  interface cause stays bounded (compiler-vocabulary cannot depend on ir-vocabulary).
- Render: Visibility::Unknown renders no prefix; "pub " remains Rust Public truth only.
- Every existing test suite stays green or is updated with an exact falsifier named in the matrix.

## Baseline (frozen independent of git status)
Commit 4f66e3fbe. Digests (sha256-12, LOC):
2fa517587f3b 676 compiler/ir/type_facts.rs; 12d2db9173b9 474 compiler/ir/wire.rs;
af64c87f2618 137 compiler/ir/lib.rs; 0777f0b63e3f 830 compiler/ir/view/validate.rs;
7dcbc8ee98ba 821 compiler/ir/render.rs; 054dcef37190 4246 compiler/ir/semantic.rs;
fa9129879d7e 1013 compiler/ir/prepared.rs; f3b9ed809dcc 2285 compiler/driver/lower.rs;
21274413a6c6 433 compiler/driver/types/terminal.rs; e66b6cc8a00b 800 compiler/driver/types/compile.rs;
02f177e8461d 358 compiler/application/terminal/native.rs; 20fa8b4f2381 411 compiler/vocabulary/lib.rs;
dd08c10a2a48 451 compiler/publication/generation.rs; a860d056b6c1 589 server/journal/publication/owner.rs;
29c845c4353f 475 server/journal/publication/format.rs.
Known reds at baseline: compiler-application E0004 (ExtensionAtomUnbound arm);
#[ignore] python_purl_lifecycle.rs:202 (six.py capacity), typescript_lower.rs:506 (golden
forward reference). No root TESTING.md/ORCHESTRATION.md exists in this layout; clause binding is
to the in-repo per-crate test trees named per row.

## Concurrent-path ownership
Other lanes actively commit to this worktree (languages/*, driver lower slimming). Luna cards
commit only owned paths; Terra re-checks `git log`/`git status` before each integration and gate.

## Authority notes
- No new dependency, no unsafe, no SIMD anywhere in this capability.
- server/journal/publication/** is in-scope for decision 3 as the durable publication adapter the
  decision explicitly re-semantics; no workflow/journal changes beyond the publication plane.
- interface/core CompilerCause stays structurally as-is; compiler-vocabulary gains closed
  additive LoweringUnsupported variants (bounded interface cause, full operands on CompileFailure).

## Product-authority questions open
None. All six forks were decided by Sol; interpretation forks are recorded per row in the matrix.

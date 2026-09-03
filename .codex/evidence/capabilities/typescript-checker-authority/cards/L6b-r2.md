# Card L6b-r2: land the achievable lattice slice; keep honest operands pinned; name the fork

registered role: nudox_luna_implementer (expected `luna`/max; resume of the
L6b session)
baseline: dc92ea8fc + YOUR UNCOMMITTED WORKING SET (lower.rs,
lower/typescript.rs, checker.rs, main.cjs, lib.rs) — it is good work; do
not revert it. Delete compiler/driver/tests/terraform_probe.rs (your stray
diagnostic) before committing.

MANDATORY environment (unchanged):
  export PATH=/opt/homebrew/bin:$PATH
  export NODE_PATH=/Users/mileswirht/Downloads/backend/node_modules
  export CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-typescript/.local/target

## Terra verification of your stop (your evidence accepted, scope narrowed)

Terra reproduced your renders: conditional `[T] extends str ? "s" : "n"`,
mapped `{ [K in ?unsupported]-?: ?unsupported }`, template
`` `${str}` ``, literals `"ok" | "42" | false | true`. Two distinct
defect classes remain:

A. **Fork-dependent (document, do not fix):** template TEXT parts and
   mapped `keyof`/indexed-access key operands cannot be carried by the
   declared-plane record today. Terra escalates the declared/computed
   segment wire question to Sol. Keep the render goldens pinned to the
   CURRENT honest structural output — `?unsupported` for a genuinely
   unrepresentable operand is the honest Other record rendering, not a
   silent fallback — and mark in each golden's test comment which operand
   class is fork-pending.

B. **Fork-independent (fix now):**
   1. `${string}` renders as `${str}` — a truncated atom/spelling for a
      Builtin `string` placeholder. `string` is fully representable; this
      is a lane bug (suspect the span slice for the placeholder type inside
      the template). Fix and pin `` type Greet = `hi ${string}` `` →
      `` `${string}` ``-style goldens with the text-part absence documented
      per (A).
   2. The conditional render `[T] extends str ? "s" : "n"` — `str` again:
      same slice bug on the check type `T extends string`. Fix so the
      operand spells `string`.
   3. Decoded-IR falsifiers per construct in typescript_lower.rs: conditional
      (4 operands: check/extends/true/false rows), mapped (modifier
      add/remove/preserve + target), template (placeholder children count),
      literal (bases) — assert exact record kinds on the plane they land in.
   4. R12 legs 3-4 (card L6b addendum): forward-nominal publish->reopen
      equality + render names `B`. Heap buffers; default 2 MiB thread.
   5. Vocabulary renames (R4): the grep receipt moved — backlog at
      lower/typescript.rs ~960/~1211, degradation ~2297. Rename to
      TypeReason vocabulary naming the record; no behavior change.
   6. Goldens: update the four render tests to the current honest outputs
      (literals to the full `"ok" | "42" | false | true`; conditional to
      the arm-true rendering post-slice-fix; mapped/template with fork-pending
      operand comments).

owned paths unchanged from L6b. NOT owned: compiler/ir/**, vocabulary/**,
publication/**, sibling lanes, lifecycle files, no new dependencies.

## Exact focused commands (with the lane-local CARGO_TARGET_DIR)

- cargo test -p compiler-driver --test typescript_render   (14/14 after goldens)
- cargo test -p compiler-driver --test typescript_lower
- cargo test -p compiler-languages-typescript
- cargo test -p compiler-driver --test typescript_authority
- cargo test -p compiler-driver --test typescript_package
- cargo check -p compiler-driver --lib
- grep receipt for backlog/degradation over owned production files

## Checkpoint protocol

One commit, prefix `feat(typescript):`. Return: commit sha, focused outputs
verbatim, the two slice-bug fixes' diffs summarized, the golden texts you
pinned for mapped/template (with fork-pending comments), the grep receipt,
and the smallest remaining red row.

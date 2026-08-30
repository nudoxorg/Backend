# C1 builder rescue hostile pre-edit custody

R9 hostile pre-edit review at `949a37880ccc7dd2b5242867ba3440d15fbb68c1` is rejected.

- A padded private copy plus `PhantomData` can pass E0451/E0597/layout without retaining source facts.
- Three-item overflow does not kill cast-before-check wrapping at 256.
- The paired consumer covered only valid 2/2 and used cursor length rather than consumption.
- Writer header offsets were unnamed.

These findings are frozen falsifiers for the R10 recard; no production edit is authorized by this receipt.

## R18 hostile pre-edit rejection

At `d5cee9ef485edc68eb81ae38a4622fd3d1377dcc`, the separate `gpt-5.6-terra`,
`fork_turns=none` reviewer passed the exact prototype checkout/branch/HEAD/clean preflight and rejected
the card on three in-scope bypasses:

- required borrowed fields could coexist with differently named private typed caches that the writer
  used instead;
- the E0451 child named only `entities`, `types`, and `output_len`, leaving `entity_count` and
  `type_count` unproved private; and
- output/result equivalence could not detect a discarded second validation.

The R19 recard must bind the struct tripwire to exactly the five legitimate stored fields, make the
rlib E0451 child prove every field private, and add per-callable lexical direct-validation counting.
This rejection grants no production authority and invalidates the R18 calibration deck for the repaired
card.

## R19 final hostile pre-edit review

The first R19 hostile invocation is inadmissible transport churn: it remained in the desktop task
directory rather than `/private/tmp/nudox-prototype-real-compiler-ir` and therefore had no git identity.
It made no repository read or change.

The explicit retry used `gpt-5.6-terra`, `fork_turns=none`, changed to the required prototype checkout,
and passed raw preflight at `776460d7dd824f564feccfb1098fd1e76477b7eb`: exact prototype path and git
top level, branch `codex/prototype-real-compiler-ir`, that HEAD, and empty `git status --short`.

Result: CLEAR. The reviewer found material falsifiers for exact cache-free five-field provenance,
all-field E0451 privacy, entity-first counts, capacity-atomic direct prefix writing, view containment,
per-callable lexical single validation, actual-rlib typed/lifetime proof, causal mutations, and
target-bounded pending codegen. Historical/PRE-EDIT placeholders were correctly treated as
non-authorizing. This clear hostile gate authorizes one Luna exact-path builder only; it is neither a
closure verdict nor a product/merge decision.

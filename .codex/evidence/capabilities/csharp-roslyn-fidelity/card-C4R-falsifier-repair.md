# Card C4-R — falsifier repair (registered role: nudox_luna_implementer)

## Context
Checkpoint 50369c350's `wire_saturation_gaps_stay_image_retained_pending_trunk_cells`
was REJECTED by hostile review: its asserts are vacuous.

```rust
let reopened_variance = Variance::Invariant;
assert_eq!(reopened_variance, Variance::Invariant);      // a constant against itself
let reopened_params_cell = pools.len() != 28;            // magic 28, unrelated to params
assert!(!reopened_params_cell);
```

The test would stay green if trunk flipped the behavior — it proves nothing.
The rust.rs import/signature unblock in the same commit is ACCEPTED and
stands; do not redo it.

## Owned paths (nothing else)
- compiler/driver/lower/csharp.rs — ONLY the one falsifier test
- compiler/driver/lower.rs — only if a strictly mechanical test-support
  change is required for decoding reopened pools in the test

## Repair contract
Rewrite the falsifier so its assertions are computed FROM the reopened
fragment, and die if the behavior changes:
1. Keep the fixture (variance=1 patched TypeParameters row byte 10, params
   bit 0x1 patched Parameters row byte 9, digest recomputed).
2. collect + admit + FragmentView::validate as today.
3. Decode the REOPENED extension type-parameter lane from the validated
   view (the APIs already used in this test module for extension pool
   payloads / reopened extension sections; find the typed accessor that
   yields the pooled `TypeParameter` rows or their wire rows). Assert the
   decoded row for T carries `Variance::Invariant` — computed from the
   bytes, never from a local constant.
4. For params: assert the reopened C# extension facts of the `rest`
   parameter carrier carry reference_kind Value and that the reopened
   record layout contains no params cell — again computed from the decoded
   reopened row (name the exact accessor; if the reopened wire layout has
   no such cell, assert the decoded CSharpFacts fields exactly and state
   in the comment that absence is structural: `CSharpFacts` at
   compiler/ir/semantic.rs:1296 owns no params field).
5. Keep the doc comment citing the escalation packet and drop sites, and
   the flip-when-trunk-lands instruction.
6. Sanity guard so the fixture cannot silently regress: first assert the
   PRE-admission image facts (parse the image with
   compiler-languages-csharp's CSharpImage — it is already a dependency —
   and assert generic.variance == VarianceTag::Out and
   parameter.is_params), so the falsifier fails loudly if the fixture
   stops carrying the wire cells.

## Gate
CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-csharp/.local/target-csharp
cargo test -p compiler-driver --offline --lib lower::csharp — 19 green.
Mutation check (in your head, then report): flipping the assertion targets
must make the test fail — describe in the return how each assert dies.

## Bounds
One commit, prefix fix(driver-tests):. No semantic changes beyond the
falsifier. No unwrap/expect in the test body (typed test error).

## Return
Commit hash; exact assert lines; the accessor names you decoded through;
command tails.

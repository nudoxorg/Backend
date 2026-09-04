# Card d6-repair — deep spot checks, not re-labeled entity pins

registered role: nudox_luna_implementer
baseline: commit 9e47e6716 (branch codex/fidelity-python)
- compiler/driver/tests/python_packages.rs (post-9e47e6716; run shasum for your return)

## Owned path (no other writer)

- compiler/driver/tests/python_packages.rs

## Rejection reason (Terra, from the diff)

The card demanded DEEP decoded-lane evidence (exact parameter-kind sequences,
in-order decorator atom lists, or exact docstring text prefixes). The
delivered `SpotCheck` enum has exactly one variant — `Entity { symbol, kind }`
— which re-asserts what the existing four-symbol pins already prove, and the
mutant analysis described the OLD behavior. This checkpoint is rejected as an
under-delivery of law 3. The machinery deletion and LayoutClass work are
accepted.

## Law (repair)

1. `SpotCheck` must gain deep variants that decode REAL lane content and
   compare exact bytes/kinds against source truth, minimally:
   - ParameterKinds: one named function whose decoded parameter-kind sequence
     (e.g. positional-only, positional, vararg, keyword-only, kwarg as the
     wire's PythonParameterKind spells them) equals the written signature's
     kinds in order;
   - DecoratorSequence: one named declaration whose decoded decorator atom
     list equals the source's decorator spellings in source order;
   - DocstringPrefix: one named module or member whose decoded docstring text
     starts with the source's exact docstring opening bytes.
2. At least TEN of the twenty packages must pin a deep variant (ParameterKinds,
   DecoratorSequence, or DocstringPrefix). The `Entity` variant stays legal for
   the rest. Derive every pinned sequence from the actual sdist source (fetch
   the exact pinned versions; the host has network; reuse the file's
   established download/unpack machinery).
3. Do not duplicate the existing four-symbol loop's assertion through `spot`;
   if a spot duplicates a pinned symbol, deepen it instead.
4. Keep forbid(unsafe)/deny(unwrap,expect,panic), typed network skip,
   LayoutClass lines, rustfmt clean.

## Must prove (falsifiers)

1. `CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-python/.local/target cargo test
   -p compiler-driver --test python_packages -- --nocapture` → 10/10, 20
   corpus lines, zero typed-terminal lines.
2. Mutation analysis IN THE RETURN: for one DecoratorSequence and one
   ParameterKinds pin, state the exact failure message observed when the
   source order is deliberately mis-pinned (do the mis-pin locally, observe
   the failure, then restore; do not commit the mis-pin).
3. `rg -n "expect:|ExpectedTerminal|lane_full_terminal|child_lane_full_terminal"
   compiler/driver/tests/python_packages.rs` → zero matches (must remain true).

## Commit and return

One coherent checkpoint: `test(python): pin decoded parameter, decorator, and
docstring evidence per corpus package`. Stage ONLY the owned file.
Return: commit hash; the deep-pin table (package → variant → exact pinned
fact); both mutation failure messages; gate tails; smallest remaining red.

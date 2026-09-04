# Card d6-repair-2 — substantive evidence only, no dead variants

registered role: nudox_luna_implementer
baseline: commit 66d50740 (branch codex/fidelity-python)
- compiler/driver/tests/python_packages.rs (post-66d50740; shasum in your return)

## Owned path (no other writer)

- compiler/driver/tests/python_packages.rs

## Rejection reason (Terra, from the diff)

Checkpoint 66d50740 delivered 10 deep pins, but:
1. `DocstringPrefix` is constructed by ZERO packages — an unconstructed enum
   variant is dead code and emits a warning. Dead variants are forbidden.
2. Three pins are EMPTY sequences (certifi `where` ParameterKinds [],
   packaging `__title__` DecoratorSequence [], click `Command`
   DecoratorSequence []). An empty-sequence pin proves lane presence, never
   order or content, and passes even if the evidence class is dropped.
3. No package pins a NON-EMPTY decorator sequence, so decorator ORDER is
   unproven corpus-wide.

## Law (final repair — concrete targets, derive each from the real sdist source)

1. DocstringPrefix: pin it on at least TWO packages whose pinned primary has
   a real module docstring (pyparsing 3.2.3 core.py opens with a long
   docstring; check tomli/_parser.py, werkzeug, jinja2 — derive the exact
   opening bytes from the fetched source and pin a prefix of >= 24 bytes).
   The variant must be constructed or deleted.
2. Replace the three empty-sequence pins with substantive evidence:
   - certifi: `contents()` — derive its real parameter kinds from the source.
   - packaging: pick a genuinely decorated declaration in the pinned primary
     (packaging 25.0 __init__.py is tiny — if it has no decorated
     declaration, pin DecoratorSequence on a package that does, e.g. pluggy
     `_hooks.py` `HookImpl` class or itsmethods, and keep packaging on a
     ParameterKinds pin of a real function such as `_);
     derive from source).
   - click: `Command` is undecorated in 8.2.1 core.py — instead pin a
     DecoratorSequence on a genuinely decorated declaration (several
     `@t.overload`-decorated method groups exist; pick one and pin the exact
     source-order spellings) or switch click's pin to a ParameterKinds of a
     function with >= 2 parameters.
   Every final pin: non-empty decoded evidence compared byte-for-byte /
   kind-for-kind against source truth.
3. Keep everything else accepted in 66d50740 unchanged (machinery deletion,
   LayoutClass lines, the 7 substantive ParameterKinds pins).
4. Zero warnings from this file; forbid(unsafe)/deny(unwrap,expect,panic);
   typed network skip; rustfmt clean.

## Must prove (falsifiers)

1. `CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-python/.local/target cargo test
   -p compiler-driver --test python_packages -- --nocapture` → 10/10, 20
   corpus lines, zero typed-terminal lines, ZERO warnings from the file.
2. Mutation analysis IN THE RETURN (mis-pin locally, record exact failure
   text, restore): one DocstringPrefix and one non-empty DecoratorSequence.
3. `rg -n "expect:|ExpectedTerminal|lane_full_terminal|child_lane_full_terminal"
   compiler/driver/tests/python_packages.rs` → zero matches.

## Commit and return

One coherent checkpoint: `test(python): corpus pins carry substantive decoded evidence`.
Stage ONLY the owned file. Return: commit hash; final 20-row pin table; both
mutation failure messages verbatim; gate tails; smallest remaining red.

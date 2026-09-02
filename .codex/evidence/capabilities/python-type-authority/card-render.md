# Card R — python-render-golden

Registered role: nudox_luna_implementer.
Baseline: commit 2c0b26f86 on branch canonical; shared worktree; you never stage files outside
your owned paths.

## Owned paths (exhaustive)
- compiler/driver/tests/python_render.rs (new). Nothing else.

## Forbidden adjacent surface
No production code, no manifests, no other test files, no compiler/ir/** (render.rs is
read-only for you; it already exposes `SignatureDisplay`, `TypeDisplay`, `DocsDisplay`,
`EmbeddingDisplay` over `Ir`).

## One public terminal
`cargo test -p compiler-driver --test python_render` is green, proving struct rendering is
driven by the Python semantic lanes through the shipping compiler-ir renderers, with exact
golden text.

## Required proof
1. One in-file fixture source (or a small family) that forces these lanes through
   `compile_ir(Stage::LowerIr, Python(PythonVersion::Python314))`:
   module docstring; a plain class with fields and methods; a `TypedDict` class; a `Protocol`
   class; a function with positional-only, keyword-only, varargs, and kwargs parameters with
   defaults; written compound annotations covering `list[int]`, `dict[str, int]`,
   `tuple[int, str]`, `Callable[[int], str]`, `Optional[int]` and `int | None`, a `Literal`,
   a quoted annotation, and a PEP 695 `type` alias; local call occurrences and an import
   occurrence; one decorator; one `@typing.overload` pair plus implementation.
2. Golden assertions: for at least six entities, assert the EXACT `SignatureDisplay` text, and
   for at least four compound types the EXACT `TypeDisplay` text, plus one `DocsDisplay` and
   one `EmbeddingDisplay` (profile `DOCUMENTED`) exact text. Goldens are inline literals in
   the test file; a mismatch is a typed diff, not a boolean.
3. Lane-drivenness falsifier: render must read IR, not source. Prove it once by building a
   second `Ir` through the shipping API where one function's written annotation lane differs
   from another's source spelling (two entities, same source text region count, different type
   records), and assert their `TypeDisplay` texts differ exactly as the lanes dictate.
4. Determinism without pyrefly: the golden test must pass on a machine with no pyrefly
   provisioned (the lane honestly degrades to syntax tiers). Separately, ONE live test gated
   on `Pyrefly::from_env().is_available()` asserts that a checker-derived inference appears in
   the rendered type of an unannotated constant; typed skip when unavailable.

## Proof-matrix rows this card owns
R1 (render from published lanes) — weakened implementations: rendering re-parsed source,
boolean assertions instead of goldens, goldens requiring pyrefly. Falsifiers: rows 2-4 above.
Additionally assert render-identity of the same source compiled twice (two independent
`compile_ir` calls produce identical render text) to pin lane determinism.

## Bounds
No new dependencies. No unsafe. No unwrap/expect/panic; typed test errors like
compiler/driver/tests/rust_semantic_lane.rs. rustfmt-clean; clippy-clean for dev target.

## Exact commands
- cargo test -p compiler-driver --test python_render
- cargo fmt -p compiler-driver -- --check
- cargo clippy -p compiler-driver --tests 2>&1 | tail -5

## Commit protocol
One commit: `test(python): golden struct rendering driven from semantic lanes`.
Return: commit hash, exact test counts, the golden strings you froze, and the smallest
remaining red row.

## Plan closure
If the renderer cannot express a lane fact (e.g., a Python-specific kind renders misleadingly),
do NOT patch render.rs; freeze the exact golden as-is, record the observation in the return,
and mark that row as a Terra escalation candidate.

# Card d3-python-fidelity — reserved anchoring, method labels, repaired goldens

registered role: nudox_luna_implementer
baseline: HEAD aff5024a2 (card d2 landed). Terra captured post-repair live-render actuals
into this card; they are the frozen goldens. Do not invent other strings.

## Owned paths (no overlapping writer)

- `compiler/driver/lower/python.rs` — emission only: `emit_classes` /
  `structural_class_members` anchoring, `protocol_method_row` child name cells, and the
  one python.rs unit test named below.
- `compiler/driver/tests/python_render.rs` — the three stale test updates named below and
  deletion of the two `zz_probe_*` tests (their evidence is recorded in the journal; do
  not commit the probes).

Everything else is forbidden: `compiler/driver/lower.rs`, `compiler/ir/**`, other
frontends, manifests, the purl/packages test trees.

## Laws

1. **Reserved anchoring.** A structural class (`TypedDict` / `Protocol`) whose members are
   all hostable and within `MAX_TYPE_CHILDREN` must host its member rows even when it is
   the FIRST pushed fact: anchor its member rows to the class fact's reserved ordinal
   (`facts.len()` at emission time) using the trunk's `intern_reserved_anchor_type_row`,
   then push the class fact immediately next. The self-nominal degradation survives ONLY
   for a member that cannot be hosted or member-lane overflow — never for "no anchor"
   (that case no longer exists). Later structural classes keep the existing
   already-pushed-anchor path.
2. **Method labels.** `protocol_method_row` appends each parameter child with the exact
   parameter name bytes in the child NAME cell (the result child stays unnamed). Method
   signature labels must survive the pooled-row round trip.
3. **Repaired goldens.** The live-Ir display truths recorded by Terra's probes (journal,
   2026-09-02) are frozen as exact expectations; stale assertions that encoded the
   pre-repair defect (empty tails, absent compounds, absent docs, `callback` as Apply)
   are rewritten to the captured actuals below.

## Exact golden updates (python_render.rs)

Test `python_lane_renders_exact_declarations_and_docs`:
- `overloaded`: `/* visibility unknown */ fn overloaded(value: ?unsupported) -> str`
- `calls`: `/* visibility unknown */ fn calls(value: ?unsupported, enabled: bool) -> str`
- `answer`: `/* visibility unknown */ static answer: ?unsupported | ?unsupported`
- `items`: `/* visibility unknown */ static items: ?unsupported<?unsupported>`
- `lookup`: `/* visibility unknown */ static lookup: ?unsupported<str, ?unsupported>`
- `callback`: `/* visibility unknown */ static callback: fn(param: ?unsupported) -> str`
- `maybe`: `/* visibility unknown */ static maybe: ?unsupported | ?unsupported`
- `Plain` docs: exact text `Plain documentation.` (was asserted empty — flip to exact)
- `Plain` DOCUMENTED embedding: `/* visibility unknown */ struct Plain\n\nPlain documentation.` (exact, including the blank line)

Test `python_fragment_planes_carry_what_the_ir_tree_omits`: the compound loop must accept
`items`/`lookup` as `SemanticTypeTag::Apply` and `callback` as
`SemanticTypeTag::FunctionPointer` — Callable's honest lane fact is a FunctionPointer row,
not an Apply.

Test `python_lane_renders_compound_types_and_is_deterministic`: replace the
"compound type leaked into live IR" negative for `items`, `lookup`, `callback`, `answer`,
`maybe` with positive exact displays (the strings above, via `display_type`). Keep the
determinism double-compile law. In the alternate-source block, `left: int` must display
`?unsupported` and must NEVER display `i32` (the arch-signed law stays; the honest
unsupported-unknown lift is accepted), `right: str` stays `str`.

python.rs unit test `first_declaration_typed_dict_degrades_to_the_self_nominal`: rewrite
as the hosting law — `class Movie(TypedDict): title: str` lowers to an AnonymousRecord
with exactly one child (`title`, Primitive str row) owned by the Movie fact; rename it
`first_declaration_typed_dict_hosts_its_members`. Keep or extend the existing degradation
coverage with a member that cannot be hosted (the degradation law must still have a red
falsifier).

## Must prove (falsifiers, exact commands)

1. `cargo check -p compiler-driver --lib` — clean.
2. `cargo test -p compiler-driver --test python_render` — ALL tests green, none ignored,
   no probes. (7 tests: 5 named + the two python.rs-side checks run in the lib? No —
   python_render has exactly its own set; report the exact count you see.)
3. `cargo test -p compiler-driver --lib lower::python` — the python.rs unit tests,
   including the flipped hosting test and the retained degradation falsifier, all green.
4. `cargo test -p compiler-driver --test python_render --
   python_fragment_forward_reference_structural_rows_validate` explicitly green
   (Mapping hosts 2 members, Reader hosts 1, zero ForwardReference).

## Bounds

- No new allocations classes: member rows were already pooled; anchoring reuses the
  existing reserved-anchor path. No wire-format change. No display changes.
- Do not alter emission for non-structural classes, imports, aliases, occurrences, or docs.
- Do not strengthen pyrefly-dependent paths (checker-off behavior must stay identical).

## Stop decisions

- If the reserved anchor cannot host the first class without touching lower.rs or the
  wire — STOP and report (authority fork).
- If a golden from this card does not match the actual render EXACTLY, capture the actual,
  do not silently edit the golden — report the diff and stop on that test.

## Commit and return

- One coherent checkpoint commit: `feat(python): host first structural classes and freeze repaired renders`
- `cargo fmt` both owned files.
- Return: commit hash; per-file LOC delta; exact tails of the four commands; the smallest
  remaining red.

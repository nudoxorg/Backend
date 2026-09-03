# Card L3 — ForwardReference surgical repair (manager investigation attached)

Registered role: nudox_luna_implementer. Repository /Users/mileswirht/Downloads/backend,
shared worktree. No git commands. Own ONLY compiler/driver/lower/python.rs and
compiler/driver/tests/python_render.rs. compiler/driver/lower.rs and compiler/ir/** are
READ-ONLY (adjacent lane actively edits the trunk).

## Established facts (manager-verified, do not re-derive)
- Fault: `Prepare -> TypeFacts::ForwardReference { ordinal, position, target }` — a type-fact
  row's child target must be STRICTLY backward in the final lane. Fault instances observed:
  z_reader (Z + Reader-Protocol with `read(self, size: int) -> bytes`) → {3, 0, 4};
  z_proto_read_noargs (`read(self) -> bytes`) → {2, 0, 3}; pair/full fixtures analogous
  (target = ordinal+1 each time).
- Final lane layout (trunk): anonymous rows first (provisional ordinals 128+i, remapped to
  i), then fact rows (i + anonymous_rows). Remap closure in lower.rs (read-only).
- Manager instrumentation is ALREADY in python.rs: `eprintln!` in `parent_row` and
  `intern_row`. A temporary `temp_diag_prepare_payload` test in python_render.rs compiles
  FORWARD_REFERENCE_SOURCE and prints the full failure payload, then decodes passing
  variants. Verified emission for z_reader: self→128, int→129, bytes→130, callable=131
  (children [128,129,130]) — all correct backward anon coordinates — yet prepare still
  faults at ordinal 3 (the callable) position 0 target 4. NOTE target 4 = remap(FACT 0):
  some appended child target is a raw FACT coordinate, or a nominal cell trips the forward
  check — find WHICH construct and WHY.
- Known-good layout reference (decoded): z_td fixture lanes decode as [anon str, anon int,
  anon wrapper(Nominal), Mapping fact(AnonymousRecord, 2 pooled children)].
- The Python lane crate tests (35) stay green at HEAD; only multi-structural-class fragment
  prepares fail.

## Required work
1. Use/extend the existing instrumentation to identify the EXACT construct that emits the
   offending child target or nominal cell (trace target value 4 = remap of fact 0). Keep
   prints only while debugging; remove them before commit.
2. Fix the emission order/coordinate so every type-lane child target and nominal cell is
   strictly backward. Honest laws: NEVER erase a derivable link (no Unknown-for-class
   substitutions); NEVER fabricate; a reference that cannot be expressed backward must
   degrade the WHOLE member row to the lane's existing honest fallback (`Ok(None)` paths),
   never silently point forward.
3. Regression tests in python_render.rs:
   - `python_fragment_forward_reference_structural_rows_validate` (exists) must pass:
     the pair source compiles, validates, and the decoded type-fact lane carries Mapping's
     member rows and Reader's callable row.
   - `python_fragment_planes_carry_what_the_ir_tree_omits` (exists) must pass (full fixture).
4. Keep `python_lane_renders_exact_declarations_and_docs`,
   `python_lane_renders_compound_types_and_is_deterministic`, and
   `checker_inference_is_rendered_when_pyrefly_is_available` green.

## Stop trigger
If the offending target provably originates in the trunk's remap/lane construction (not
python's emission), STOP and return the minimal counterexample with your trace.

## Gates (all must actually run; if the shared lib is broken by other lanes, say so and
deliver anyway — do not report unrun gates as green)
- cargo test -p compiler-driver --test python_render   (6 tests green)
- cargo test -p compiler-languages-python              (35 green)
- rustfmt --check --edition 2024 compiler/driver/lower/python.rs compiler/driver/tests/python_render.rs
- cargo clippy -p compiler-driver --tests 2>&1 | tail -5

## Return
The offending construct (file/line + emitted coordinate), the fix mechanism in one sentence,
diff summary, all four gate outputs, smallest remaining red row.

# Card d1-anonymous-child-protocol — pooled-lane child bookkeeping repair

registered role: nudox_luna_implementer
baseline: HEAD d9411952f8235d09aafd7e0589173de1c03ee8c0; per-file frozen baselines
recorded in index.toml (lower.rs 1786 LOC a205626c4c224fd4, python_render.rs 655 LOC
96b70278652efde2).

## Owned paths (no overlapping writer)

- `compiler/driver/lower.rs` — ONLY the FactSet anonymous-row child bookkeeping
  (`intern_anonymous_type_row`, `intern_reserved_anchor_type_row`, and any private field
  those two functions alone need). No signature changes; no other function in the file.
- `compiler/driver/tests/python_render.rs` — ONLY the two `#[ignore = "blocked by trunk
  ForwardReference ..."]` attribute lines (delete them, including the now-false message
  text). Zero other edits in this file.

Everything else is forbidden: `compiler/driver/lower/python.rs`, `compiler/driver/lower/*.rs`
frontends, `compiler/ir/**`, `compiler/ir-vocabulary/**`, every other frontend's tests, and
the `--lib` test modules of typescript/clang (which are mid-edit by another lane and
currently break `cargo test -p compiler-driver --lib` — that failure is NOT yours and must
not be repaired or referenced as evidence).

## Law

`FactSet::intern_anonymous_type_row` documents: "Children of the new row are appended with
`FactSet::anonymous_type_child` **before** the next row is interned, keeping the pooled lane
topologically backward." The invariant owners are:

1. each interned row's serialized children are exactly and only the children appended for
   that row between the previous intern and this intern;
2. every child target points at an already-interned anonymous row or already-pushed fact
   (strictly backward after the fragment's remap: anonymous rows occupy type-lane ordinals
   `0..anonymous_rows`, fact rows follow at `anonymous_rows..`);
3. rows with no children serialize zero children.

## Current red state (verified by Terra, 2026-09-02)

`cargo test -p compiler-driver --test python_render -- --ignored` fails both regression
tests with `Compile("prepare")`. The exact fault for `FORWARD_REFERENCE_SOURCE`
(`import typing` + one `typing.TypedDict` + one `typing.Protocol`) is:

```
Prepare { .., cause: TypeFacts { fault: ForwardReference { ordinal: 3, position: 0, target: 4 } } }
```

Mechanism found by instrumented emission (do NOT trust this; prove it): python appends the
protocol row's three member children to pooled slots 0–2, then interns the FnPtr row;
the intern records `child_starts = children_total (= 3)` and `counts = pending (= 3)`,
so serialization reads stale slots 3–5. Determine the correct bookkeeping yourself and make
the documented protocol hold for every append/intern interleaving.

## Must prove (falsifiers, exact commands)

1. `cargo check -p compiler-driver --lib` — clean (the lib test target is broken by another
   lane's in-flight modules; `--lib` without tests is your compile gate).
2. `cargo test -p compiler-driver --test python_render` — **all five tests run, none
   ignored, all pass**:
   - `python_fragment_forward_reference_structural_rows_validate` (consecutive structural
     classes; Mapping degrades to its documented self-nominal, Reader hosts the anonymous
     record and FnPtr with exactly its three member rows),
   - `python_fragment_planes_carry_what_the_ir_tree_omits` (two structural classes with
     different child counts plus compounds, docs, overload, quoted annotation),
   - the three pre-existing render tests must remain green unchanged
     (leaf-only and live-Ir paths must not regress).
3. `cargo test -p compiler-driver --test python_purl_lifecycle --no-run` and
   `cargo test -p compiler-driver --test python_packages --no-run` — your change must not
   affect their compile state (currently broken by unrelated lane files; failing to compile
   there is expected and not yours — verify only that your diff does not add new errors by
   confirming the same five pre-existing errors remain via
   `cargo test -p compiler-driver --test python_purl_lifecycle --no-run 2>&1 | grep -c "^error"` == 5
   before and after your change; record both counts).

## Bounds and resources

- The fix is expected to be constant-time bookkeeping arithmetic; no new allocations, no
  new public items, no wire-format change (fragment geometry, `compiler/ir` validation, and
  the remap in `admit` are fixed context, not edit surface).
- Do not "fix" callers to work around the bookkeeping; the emission protocol is already
  compliant and stays untouched.
- No new tests in `lower/tests.rs` (that target cannot compile while another lane's modules
  are mid-edit); the integration falsifiers above are the proof.

## Stop decisions (report back instead of acting)

- If making the documented protocol hold requires changing the protocol itself, the
  fragment wire format, `compiler/ir` validation, or any frontend — STOP; that is an
  authority fork.
- If the two un-ignored tests expose a second, distinct defect beyond the bookkeeping that
  cannot be fixed inside owned paths — STOP and report the exact failure output.

## Commit and return

- One coherent checkpoint commit on the current branch:
  `fix(compiler-driver): serialize each anonymous row's own children`
- `cargo fmt` the two owned files only.
- Return: commit hash; per-file LOC delta; exact tail of each gate command; one-paragraph
  mechanism statement naming the precise line-level defect and repair; smallest remaining
  red row you observed.

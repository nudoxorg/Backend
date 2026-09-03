# Card L — ForwardReference lane repair (digest-frozen)

Registered role: nudox_luna_implementer. Repository /Users/mileswirht/Downloads/backend,
shared worktree. Do NOT run git commands. Do NOT touch compiler/driver/lower.rs (shared
trunk, another lane is editing it) or compiler/ir/**.

## Owned paths
- compiler/driver/lower/python.rs (production lane + its own #[cfg(test)] module)
- compiler/driver/tests/python_render.rs (regression test only)

## Defect (reproduced)
`compile` of this source fails at `Prepare: TypeFacts::ForwardReference`:

```text
import typing
class Mapping(typing.TypedDict):
    name: str
    count: typing.NotRequired[int]
class Reader(typing.Protocol):
    def read(self, size: int) -> bytes: ...
```

Facts established by the manager's bisect: every single construct compiles alone
(TypedDict alone, Protocol alone, Plain+field, quoted refs, overloads, union static,
callable alias all pass); TypedDict-followed-by-Protocol fails; Plain+TypedDict+Protocol
fails; the live `compile_ir` path builds the Ir fine — only the durable fragment type lane
rejects. The fragment builder lays anonymous rows first, then fact rows, remapping lane
coordinates (see the `remap` closure in compiler/driver/lower.rs — READ-ONLY). A row's
child target evidently keeps a coordinate that lands forward in the final lane when more
than one structural class interns anonymous rows.

## Required work
1. In compiler/driver/lower/python.rs's own test module, build the FactSet for the failing
   source via the lane entry points and dump the exact anonymous-row graph (records, child
   targets, owners) to identify the forward edge precisely. Remove the dump before commit.
2. Fix the emission so python type-lane coordinates never point forward after the trunk's
   anonymous-then-facts layout + remap: the honest fix is in python.rs's interning/emission
   order (reference interned before referencing row, correct coordinate space per target).
   Do not special-case the failing fixture; the law is total ordering.
3. Add the failing source as a RED regression test in compiler/driver/tests/python_render.rs
   (a new test next to python_fragment_planes_carry_what_the_ir_tree_omits): it must
   `compile` the pair source, `FragmentView::validate` the bytes, and assert the decoded
   type-fact lane carries Mapping's member rows and Reader's method callable row.
4. The existing `python_fragment_planes_carry_what_the_ir_tree_omits` test must then pass
   (it compiles the full fixture through `compile`).

## Stop trigger
If the forward edge provably originates in the trunk remap or compiler-ir prepare rules
(not python's emission), STOP: keep the minimal counterexample in your return; do not edit
any file outside your owned paths to work around it.

## Gates
- cargo test -p compiler-driver --test python_render
- cargo test -p compiler-languages-python
- cargo fmt -p compiler-driver -- --check && rustfmt --check --edition 2024 compiler/driver/lower/python.rs
- cargo clippy -p compiler-driver --tests 2>&1 | tail -5
(shared tree: if unrelated lib errors block, report and still deliver your files)

## Return
Exact forward-edge diagnosis (which row referenced which and why), the fix mechanism in one
sentence, test names + counts, gate outputs, smallest remaining red row.

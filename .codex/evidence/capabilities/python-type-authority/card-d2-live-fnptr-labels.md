# Card d2-live-fnptr-labels — pooled FunctionPointer label resolution

registered role: nudox_luna_implementer
baseline: HEAD 7bf08a76088863483400917a4cf038ddd32540d5; file baseline recorded below.

## Owned paths (no overlapping writer)

- `compiler/driver/lower.rs` — ONLY the `FunctionPointer` arm of `fn live_type` (and, if
  strictly needed, a private helper it alone calls). No signature changes, no other arms,
  no other functions.

Everything else is forbidden: all `compiler/driver/lower/*.rs` frontends, `compiler/ir/**`,
all test trees, all manifests.

## Law

A `FunctionPointer` type row's children are ordered parameter rows followed by the optional
result row (result flag in `payload1`). Each parameter's LABEL has exactly one honest
source, in priority order:

1. the child's own name cell (present for pooled rows whose child is a type row at
   `ANONYMOUS_ROW_BASE` or above);
2. the target fact's name (`facts.names[target]`) when the child target is a fact ordinal
   below `facts.len` (fact-level rows whose children are parameter fact ordinals);
3. otherwise no label.

The current arm reads `facts.names.get(target)` for EVERY child, which raises
`BuildError::Dangling { space: Entity, raw: 128+ }` whenever a pooled row's children are
anonymous type rows — the normal case for every Python protocol method row. It survived
before only because a bookkeeping defect (fixed in 7bf08a760) pointed the serialized
children at stale zeros. The child closure already returns `(target, name, flags)` and the
arm discards the name.

## Must prove (falsifiers, exact commands)

1. `cargo check -p compiler-driver --lib` — clean.
2. `cargo test -p compiler-driver --test python_render` — the two live-Ir render tests
   `python_lane_renders_exact_declarations_and_docs` and
   `python_lane_renders_compound_types_and_is_deterministic` must get PAST the build stage
   (they may still fail on stale golden expectations in the test file — that is the NEXT
   card's surface, not yours; report their new failure mode if any).
3. `cargo test -p compiler-driver --test python_render zz_probe_source_type_lane
   zz_probe_live_renders -- --nocapture` — both Terra probe tests must pass, and the
   `zz_probe_live_renders` output must show method signatures rendering with parameter
   labels sourced per the law (e.g. `fn read(self: ?unsupported, size: ?unsupported)
   -> ?unsupported` once labels exist, or no-label rendering where the cell is absent);
   record the exact final lines in your return.

## Bounds

- The label lookup is constant-time; no new allocations beyond the existing atom interning;
  no wire-format change; no display-code changes (`compiler/ir` render stays untouched).
- Do not "fix" the Python frontend to avoid pooled FnPtr rows; the pooled row is correct.
- Do not touch the `zz_probe_*` test bodies except to READ them.

## Stop decisions

- If honest label resolution requires changing the wire format, the `TupleElement` type,
  `compiler/ir` display, or any frontend emission — STOP and report.

## Commit and return

- One coherent checkpoint commit on the current branch:
  `fix(compiler-driver): resolve function-pointer parameter labels from their rows`
- `cargo fmt` the owned file only.
- Return: commit hash; LOC delta; exact tails of the three commands; one-paragraph
  mechanism statement; smallest remaining red.

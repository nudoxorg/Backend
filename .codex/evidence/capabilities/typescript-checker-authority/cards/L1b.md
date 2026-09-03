# Card L1b — migrate the lane's inline tests to the top-level tree; freeze the golden decoded-facts table

Registered role: nudox_luna_implementer (opencode `luna` subagent).
Baseline: HEAD (e973b05a or newer) with the current working set; shared
worktree; stage ONLY owned paths.

## Owned paths

- `compiler/driver/lower/typescript.rs` — DELETE the entire inline
  `mod tests` region (everything from `mod tests {` near line ~3120 to the
  end of the file). Nothing else in the file changes.
- `compiler/driver/tests/typescript_lower.rs` — NEW top-level target: the
  migrated falsifiers, rewritten against the PUBLIC pipeline
  (`compile`/`compile_ir` with `SemanticAuthorityInput::TypeScript
  { report }` injection; models: `typescript_authority.rs`,
  `python_render.rs`). Golden fixture/transcript paths from the new
  location: `../languages/typescript/tests/...`.
- Do NOT touch: `typescript_authority.rs` (concurrent card), every other
  file, compiler/ir, sibling lanes.

## Migration laws

1. EVERY existing assertion's law survives. The source-level unit tests
   become public-journey tests; where a helper relied on `pub(crate)`
   internals (`crate::lower::admit`, `FactSet`, tail-bytes), reimplement
   the observation through the public fragment/Ir surface. The one lawful
   loss: the admission-internal output-tail law (crate-wide, not
   TypeScript-specific) — note it in a test comment, do not fake it.
2. Pin the migrated test count: at least 12 `#[test]` functions migrated
   (count the current inline `#[test]` fns first and preserve EVERY one,
   merging only exact duplicates). Record the count in the return.
3. R5 golden table: add `golden_lowered_facts_match_the_frozen_table` —
   the golden fixture + golden transcript through public compile_ir must
   decode to a declarative table committed IN the test file: entity name ->
   (kind, declared-tag, computed-tag, computed shape) for at least the
   fixture's exported declarations (n, inferred, union, applied, table,
   total, fn, callOne, callTwo, Box, made, list, widened, Slot, slot,
   viaSlot, term, Holder, g x2). Hand-write expected values from the
   golden transcript's own facts (source truth), not from observed output.
4. R1 gate: `cargo test -p compiler-driver --test typescript_lower` green;
   the inline region is gone, so `grep -c "mod tests" compiler/driver/lower/typescript.rs`
   = 0.

## Exact gates

1. `cargo test -p compiler-driver --test typescript_lower` green.
2. `cargo test -p compiler-driver --test typescript_authority` green (untouched).
3. `cargo test -p compiler-languages-typescript` green (1+17+5).
4. `cargo check -p compiler-driver --lib` clean; `grep -c "mod tests" compiler/driver/lower/typescript.rs` = 0.
5. Sibling bundle (`python_render rust_render_golden go_image java_image csharp_image authority_terminal`) green.
6. `cargo fmt` on the two owned files only.

## Commit

One checkpoint: `test(typescript): host the lowering falsifiers in the public test tree`
staging ONLY the two owned files.

## Return

commit hash; migrated test count; the frozen table's row count; gate tails;
deviations; smallest remaining red row.

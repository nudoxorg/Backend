# Card L1a-r3 — live extension plane completes; golden rejection root-caused

Registered role: nudox_luna_implementer (opencode `luna` subagent).
Baseline: working tree at HEAD 66f2c4619 (or newer) INCLUDING the uncommitted
L1a-r2 working set in `compiler/driver/lower.rs` and
`compiler/driver/tests/typescript_authority.rs` (keep it; build on it).

## Terra-reproduced state (both failures reproduced by Terra)

1. `injected_report_reaches_public_ir` (and every extension-carrying compile):
   `Build ... Dangling { space: TypeParameters, raw: 0 }`. CAUSE: L1a-r2
   populated the live Ir extension plane, but the live Ir's type-parameters
   plane is empty — every `TypeScriptFacts.type_parameters` cell carries a
   FactSet pool-local start ordinal (`TypeParameterListId::new(start)`,
   lower.rs push at ~752-766), which dangles in the live Ir.
2. Golden fixture + golden transcript (mutually consistent, digest-verified
   by Terra against a fresh driver run) fail the PUBLIC pipeline with
   `LoweringUnsupported { cause: NoSupportedDeclaration }`. CAUSE UNKNOWN —
   the lane folds every bounded-lane rejection to the coarse terminal
   (`lane_rejection()` discards the `FactFault`). The inline suite that would
   localize it has never run against this fixture+transcript pair (the
   shared `--lib` binary is blocked by sibling lanes).

## Owned paths

- `compiler/driver/lower.rs` (build_ir region +, for diagnosis only, the
  TypeScript lane's rejection fold — see workstream B rules)
- `compiler/driver/tests/typescript_authority.rs`
- `compiler/driver/lower/typescript.rs` — ONLY for temporary diagnostic
  instrumentation and ONLY if the root cause lands in this lane's passes
  (then the FIX is committed here; the instrumentation is reverted)

## Forbidden

compiler/ir/** (the live builder APIs `intern_type_parameters`,
`intern_computed`, and the atoms path used by `commit` are sufficient),
compiler/driver/types/**, sibling lanes, new dependencies, unsafe.

## Workstream A — complete the live extension plane (fixes Dangling)

In `build_ir`:
1. Intern each fact's pooled type-parameter slice into the live builder:
   `builder.intern_type_parameters(&slice)` where slice = the fact's
   `ExtensionTypeParameter` rows mapped to `compiler_ir::TypeParameter`
   (`name: AtomId` — intern the borrowed spelling through the same atom
   route `tree.commit` uses for item names; `constraint`/`default`:
   `Option<TypeId>` = `live_type(tree, facts, row, ...)` on the stored raw
   ordinal (FactSet raw ordinals are exactly live_type's row space; do NOT
   apply admit's remap); `variance: Variance::Invariant` default and
   `is_const: false` unless a source-backed fact exists — do not invent
   variance).
2. Build a rewritten `[compiler_ir::TypeScriptFacts; MAX_EMISSION_FACTS]`
   local array whose `type_parameters` cells name the live list ids, and
   populate the extension plane from THAT array (preserving the L1a-r2
   TypeScript-only rule: other lanes' variants stay `None` with the gap
   comment).
3. A fact with zero pooled parameters gets its extension row's list id from
   interning an empty slice (a valid empty list), never a raw 0 that
   assumes a populated pool.

## Workstream B — root-cause the golden rejection, then fix

1. TEMPORARY diagnosis (revert before commit): enrich the rejection path so
   the exact `FactFault`/`TypeScriptCollectError` Debug reaches the test
   output (e.g. temporarily return the fault's Debug through
   `lane_rejection`'s callers or log it), run
   `cargo test -p compiler-driver --test typescript_authority -- --nocapture`,
   and capture the exact failing pass + fact name/ordinal + cause.
2. If the cause is a bounded lane-internal bug in the committed narrowing /
   checker-reference / extension passes (the fixture exercises `widened`
   narrowings, `slot.get()` oracle calls, `term` console foreign base):
   fix it with the minimal typed change in this lane, keeping every
   existing assertion's law.
3. If the cause requires a compiler-ir, vocabulary, or protocol change:
   REVERT the instrumentation, leave falsifier B red, and report the exact
   cause + the escalation you recommend. Do not force it.

## Falsifiers to land (typescript_authority.rs, already drafted by L1a-r2)

A. `build_ir_populates_the_typescript_extension_plane`: golden pair through
   public compile_ir -> non-empty `language_extensions.typescript` plane
   with at least one non-None computed cell; resolves without Dangling.
B. `mutated_computed_report_changes_the_ir_extension_plane`: mutate one
   computed declaration (golden `n` computed number -> string) -> the two
   planes differ at that row.
C. `injected_report_reaches_public_ir` returns to green (no Dangling).
D. The three R2 terminals stay green (env-var discipline unchanged).

## Gates (repo root; `export PATH="/opt/homebrew/bin:$PATH"`)

1. `cargo test -p compiler-driver --test typescript_authority` green (9-10 tests).
2. `cargo test -p compiler-languages-typescript` green (1+17+5).
3. `cargo test -p compiler-driver --test python_render --test rust_render_golden --test go_image --test java_image --test csharp_image --test authority_terminal` green.
4. `cargo check -p compiler-driver --lib` clean; no new warnings from owned files.
5. `cargo check -p compiler-driver --lib --tests 2>&1 | grep "^error" | grep -c "lower/typescript.rs"` = 0.

## Commit

One checkpoint: `fix(typescript): resolve live extension type parameters and the golden public journey`
staging ONLY owned paths. Temporary diagnostic scaffolding must NOT be in
the commit.

## Return

commit hash; the root cause found in B (exact pass, fact, cause) and what
you fixed; gate tails; deviations; smallest remaining red row.

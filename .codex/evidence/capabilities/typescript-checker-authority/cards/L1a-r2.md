# Card L1a-r2 — extension plane reaches the live Ir; deterministic discriminator

Registered role: nudox_luna_implementer (opencode `luna` subagent).
This is a DECOMPOSITION repair: the R3-discriminator law failed twice
(L1a deferred it; L1a-r1 hit a real product defect + an env race). The law
is now split into two narrow falsifiers with an exact mechanism named below.

BASELINE: commit 1e7ad881 in /Users/mileswirht/Downloads/backend (shared
worktree; preserve sibling uncommitted work).

## Terra-established facts (verify, then use — do not re-derive)

1. `compiler/driver/lower.rs` `build_ir` (~line 831-920) hardcodes
   `extension: None` in BOTH TreeItemInput constructions (empty_item ~901
   and items ~915). The live Ir therefore drops every language-extension
   fact. The fragment path (`admit` ~1972+) commits them correctly.
2. `compiler/ir` already has the full ingestion route:
   `TreeItemInput.extension: Option<LanguageExtensionInput>` (semantic.rs
   2306) and the live `language_extensions` TypeScript plane
   (semantic.rs ~1888 push, ~2078 column view, ~3918 storage_columns).
   NO compiler/ir change is needed or permitted.
3. `EmissionExtension::TypeScript(TypeScriptFacts)` facts are three
   coordinate cells (`type_parameters`, `declared: Option<TypeId>`,
   `computed: Option<ComputedTypeId>`) with NO provisional atoms — honest to
   mirror into the live plane exactly as the fragment's extension section
   stores them.
4. The OTHER lanes' facts (CSharp etc.) carry provisional atom coordinates
   that admit rewrites (lower.rs ~1990-2005); mirroring those into the live
   plane would publish pre-rewrite coordinates. Do NOT populate them — map
   them to `None` with a one-line comment naming that gap (their lanes own
   it).
5. Computed trees are retained in `facts.anonymous_records` and are already
   materialized into the live type lane whenever declared types reference
   them. Full live-Ir computed-type materialization via the minted proof
   coordinates is an ESCALATION CANDIDATE (compiler-ir boundary) — do not
   attempt it.

## Owned paths (exactly)

- `compiler/driver/lower.rs` — `build_ir` ONLY: populate
  `TreeItemInput.extension` per fact ordinal from
  `self.extensions[ordinal]`: `TypeScript(facts)` ->
  `Some(LanguageExtensionInput::TypeScript(facts))` (borrow lives in
  `&self`; the items array unifies lifetimes within the function); every
  other lane's variant -> `None` + the gap comment.
- `compiler/driver/tests/typescript_authority.rs` — rewrite the
  discriminator test (below).

## Forbidden

compiler/ir/**, compiler/driver/lower/typescript.rs entirely, sibling lane
files, new dependencies, unsafe.

## Falsifiers

A. `build_ir_populates_the_typescript_extension_plane` (new test in
   typescript_authority.rs): golden fixture source
   (`compiler/languages/typescript/tests/fixtures/source.ts`) + golden
   transcript (`.../transcripts/golden.json`) through the PUBLIC `compile_ir`
   with `SemanticAuthorityInput::TypeScript { report }` -> the returned
   `ir.storage_columns().language_extensions.typescript` plane contains
   rows: at least one row with a non-None `computed` cell, and the row
   count/facts are non-empty. (This also disproves or root-causes the
   earlier claim that the golden fixture "produced no supported
   declarations" through the public pipeline — if the public pipeline
   genuinely rejects this fixture+report, STOP and report the exact typed
   failure as a blocker; that is a legitimate outcome of this card.)
B. `mutated_computed_report_changes_the_ir_extension_plane` (rewrite the
   existing red test): golden report, mutate ONE computed declaration's
   type (flip the computed `n` Primitive number -> string; the golden
   digest already binds the fixture, so NO digest recomputation and NO
   env/no real checker run), inject both -> the two
   `language_extensions.typescript` planes MUST differ (assert the exact
   differing row: the one whose computed cell changed).
   The test must NOT touch `NUDOX_TYPESCRIPT_CHECKER_BIN` and must not
   spawn node — golden decode + injection only, fully deterministic.
C. Keep the three R2 terminal tests green; they are the only tests allowed
   to touch the env var (under the existing ENVIRONMENT lock).

## Exact gates (repo root; `export PATH="/opt/homebrew/bin:$PATH"`)

1. `cargo test -p compiler-driver --test typescript_authority` green
   (expect 9 tests: 4 prior + 3 R2 terminals are inside the prior count —
   report the actual count).
2. `cargo test -p compiler-languages-typescript` green (1+17+5).
3. Sibling render/image gates UNCHANGED (extension-plane population must
   not disturb them; render.rs never reads extensions):
   `cargo test -p compiler-driver --test python_render --test rust_render_golden --test go_image --test java_image --test csharp_image --test authority_terminal`
   all green.
4. `cargo check -p compiler-driver --lib` clean.
5. `cargo check -p compiler-driver --lib --tests 2>&1 | grep "^error" | grep -c "lower/typescript.rs"` = 0.

## Commit

One checkpoint: `fix(typescript): populate the live Ir extension plane and prove report-to-IR flow`
staging ONLY the two owned files.

## Return

commit hash; gate tails; the exact differing extension row from falsifier B;
whether falsifier A confirmed the golden fixture lowers through the public
pipeline (or the exact typed failure if not); deviations; smallest red row.

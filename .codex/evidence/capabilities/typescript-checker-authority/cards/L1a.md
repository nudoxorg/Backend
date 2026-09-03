# Card L1a — checker authority first-class at the public boundary

Registered role: nudox_luna_implementer (opencode `luna` subagent).
Baseline: 38be78d9 (packet-repair commit) + sibling working set; shared
worktree, path discipline instead of isolated checkout.

## Owned paths (exactly)

- `compiler/driver/types/request.rs` — add the borrowed variant
  `SemanticAuthorityInput::TypeScript { report: &'source compiler_languages_typescript::Report }`.
  Keep `Clone, Copy` (borrowed field). Enum stays exhaustive; fix the two
  rustc-forced matches in `compiler/driver/types/compile.rs`
  (authority-profile mismatch ~233-245, direct-authority ~292-306) with
  explicit TypeScript arms; no sibling arm may change behavior.
- `compiler/driver/types/compile.rs` — route `LanguageProfile::TypeScript`:
  injected report -> `lower::typescript::collect_with_checker(.., Some(report), ..)`;
  `SemanticAuthorityInput::None` -> REQUIRED checker run
  (`compiler_languages_typescript::Checker::default().run(profile, source)`),
  every error (unavailability AND malformed/stale/bound-fault) maps to the
  existing `CompileFailure::Authority` plumbing retaining the exact
  `CheckerError` cause in its diagnostic. `compile` and `compile_ir` share
  `emit_facts`; both terminals must behave identically.
- `compiler/driver/lower/typescript.rs` PRODUCTION REGION ONLY (everything
  before `mod tests`): DELETE the swallow arm in `collect()`
  (`ToolingUnavailable | ModuleUnavailable => None` — the function becomes
  the required-checker path or is dissolved into `collect_with_checker`).
  Update its doc comment (no "proceeds at OXC fidelity" degradation claim).
- `compiler/languages/typescript/checker.rs` — DEBT m4: replace the
  saturating `u64::try_from(..).unwrap_or(u64::MAX)` in the `Timeout`
  payload (line ~800) with a representation that never reports a sentinel
  as the requested value (e.g. carry the configured duration faithfully;
  changing the public field type inside this owned crate is permitted).
  Also reword the `run` doc's "degrade honestly" (line ~611) — the errors
  are typed causes, not degradation.
- `compiler/driver/tests/typescript_authority.rs` — NEW top-level tree
  (pubic API only; models: `compiler/driver/tests/authority_terminal.rs`,
  `python_render.rs` for `CompileRequest`/`ResolvedToolchain`/scratch
  construction of a Direct-route language).

## Forbidden

- `compiler/driver/lower/typescript.rs` `mod tests` region (a later card
  migrates it). `compiler/driver/lower.rs`. Every sibling lane file.
  `compiler/ir/**`. No new dependency, no unsafe, no formatting churn
  outside touched hunks.

## Laws (already frozen in brief.md — do not restate or weaken)

- Checker-unavailable TypeScript compile is a typed public failure, never a
  silently degraded success; zero facts emitted.
- The three DETERMINISTIC unavailability causes and their typed
  `CheckerError` variants: `NUDOX_TYPESCRIPT_CHECKER_BIN` -> nonexistent
  path => `CheckerError::Spawn`; env var -> a script exiting 3 =>
  `CheckerError::ModuleUnavailable`; env var containing an OS error (e.g.
  invalid unicode or a `ToolingUnavailable`-mapping input) =>
  `CheckerError::ToolingUnavailable`. ("node absent" folds into `Spawn` —
  same variant, same plumbing.)

## Falsifiers to implement (typescript_authority.rs)

1. R2a/b/c — three tests: each forced cause produces
   `CompileFailure::Authority` whose diagnostic bytes retain the exact
   `CheckerError` Display text; and the compile emitted NO fragment/IR.
2. R3-injection — golden transcript (decode
   `compiler/languages/typescript/tests/transcripts/golden.json` via
   `Checker::default().decode`) injected over
   `compiler/languages/typescript/tests/fixtures/source.ts` through the
   PUBLIC `compile_ir` -> computed cells present in the returned `Ir`.
3. R3-binding — the same report over mutated source bytes -> failure rooted
   in `CheckerError::SourceBinding` (assert the diagnostic text).
4. R3-discriminator — decode the golden report, mutate ONE fact's type with
   the source digest RECOMPUTED in-test, re-inject: the flipped fact must
   change the corresponding IR cell (proves report facts flow to IR).
5. R3-copy — `const fn _assert_copy<T: Copy>() {}` instantiated for
   `CompileRequest<'static, 'static, 'static>` and
   `SemanticAuthorityInput<'static>`.
6. Profile guard — `SemanticAuthorityInput::TypeScript` with a non-TS
   profile => the existing `AuthorityInputProfileMismatch` typed failure.

## Exact gates (from repo root; `export PATH="/opt/homebrew/bin:$PATH"` first)

1. `cargo test -p compiler-driver --test typescript_authority` — green.
2. `cargo test -p compiler-languages-typescript` — green (1+17+5).
3. `cargo check -p compiler-driver --lib` — clean.
4. `cargo check -p compiler-driver --lib --tests 2>&1 | grep -c "lower/typescript.rs"` — the
   count of errors from the lane's OWN file must not grow beyond the
   pre-existing sibling census (expected: 0 from typescript.rs; clang/rust/
   python errors are out of scope — do not touch).
5. `git status --short` — only owned paths changed beyond the pre-existing
   sibling working set.

## Commit protocol

One coherent checkpoint commit:
`feat(typescript): make the checker authority first-class at the public boundary`
staging ONLY owned paths. If a falsifier cannot pass without touching a
forbidden path, STOP and report the exact blocker instead of expanding
scope.

## Return schema

- commit hash; gate outputs (tails); the R3-discriminator cell that flipped;
  any deviation with one-line reason; smallest remaining red row.

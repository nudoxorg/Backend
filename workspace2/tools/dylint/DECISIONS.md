# Semantic lint boundary

Status: red-fixture checkpoint; implementation follows.

## Toolchain authority

The lint library follows Dylint 6.0.4's generated library template exactly:

- Dylint crates are pinned to `6.0.4`;
- rustc internals are pinned to `nightly-2026-05-28` with `rustc-dev` and
  `llvm-tools-preview`;
- `clippy_utils` is pinned to the template's matching Rust-Clippy revision,
  `9fca3bc9fc2bc83c60bde26d18ed68f11564b228`.

This is intentionally separate from workspace2's stable shipping toolchain. The dynamic lint ABI is
not stable across rustc versions, so updating any one of these pins requires updating all of them and
re-recording every UI diagnostic.

Sources:

- <https://github.com/trailofbits/dylint/tree/v6.0.4/internal/template>
- <https://github.com/trailofbits/dylint#writing-lints>
- <https://rustc-dev-guide.rust-lang.org/diagnostics.html>

## First semantic bundle

The first bundle chooses three narrow rules whose meaning can be proven from HIR and type checking.

1. `NUDOX_ERASED_MAP_ERR` finds `Result::map_err` closures whose argument is `_` and whose body
   directly constructs or names the replacement error. It does not guess about an arbitrary method
   named `map_err`, does not inspect macro-generated code, and permits a named source passed to an
   exact cold diagnostic helper.
2. `NUDOX_STRINGLY_STATE_FIELD` finds fields named `step`, `expected`, or `observed` whose resolved
   type is an immutable string slice. It does not reject diagnostic message text or unrelated string
   data.
3. `NUDOX_REDUNDANT_PUBLIC_ACCESSOR` finds a public inherent zero-argument receiver method whose body
   only returns an already-public field of the same receiver. Private fields and transformed or
   validated projections are excluded.

Each rule ignores external macro expansions. Unit fixtures include a failing pattern and the nearest
allowed pattern so broadening a rule becomes an explicit review decision.

## Deliberate exclusions

- `panic!`, `expect`, `unwrap`, and `unreachable!` already have precise Clippy lints. Workspace policy
  should enable those rather than fork their semantic implementation.
- Public unit structs can be namespace objects, proof markers, domain markers, or inhabited values.
  A local lint cannot infer the intended role reliably without a declared annotation or whole-program
  consumer model.
- Manual `Display`/`Debug` can be boilerplate or deliberate formatting. A name suffix such as `Error`
  is not semantic proof.
- Tuple projections and numeric literals are meaningful in some algorithms and accidental in others.
  Linting them without a domain declaration would reproduce the regex false positives.
- Source length, word count, and parameter count are not design properties and receive no replacement
  gate. Control/data complexity remains under Clippy's cognitive-complexity lint and human review.

Repository scans remain appropriate for dependency graph bans, serde/dynamic-dispatch product policy,
and the explicitly reviewed unsafe-file allowlist: those are cross-file repository facts, not local
Rust semantic patterns.

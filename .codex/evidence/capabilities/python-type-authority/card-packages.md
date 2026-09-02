# Card F — python-packages-fidelity (wave 2; dispatch only after Card P lands)

Registered role: nudox_luna_implementer.
Baseline: the commit produced by Card P; shared worktree; never stage outside owned paths.

## Owned paths (exhaustive)
- compiler/driver/tests/python_packages.rs (new)
- files ONLY under compiler/driver/tests/python_packages/

## Reuse rule
Fetch/unpack/workspace helpers come from `compiler/driver/tests/python_support/mod.rs` via
`#[path]` module include. You may NOT edit python_support (its owner is Card P); if a helper is
missing, add it under python_packages/ instead and report the gap.

## One public terminal
`cargo test -p compiler-driver --test python_packages` is green, proving lane fidelity on real
packages: pinned `requests` 2.32.3 (sdist), `attrs` 25.3.0 (sdist), `flask` 3.1.1 (sdist),
`six` 1.17.0, and `wcwidth` 0.2.13 — versions may be adjusted to the latest resolvable pin if
the named pin is gone, but pins must be exact and recorded.

## Required proof per package (decoded lanes, never "no error")
- requests: `Request.__init__` parameter list with kinds and defaults; `Session` class present;
  `hooks`/`adapters` dict-ish annotations lowered; at least one `urllib3`/`charset_normalizer`
  import occurrence carrying a pypi package foreign key; module docstring present.
- attrs: `@define`/`attr.s` decorator facts retained as extension atoms; `@overload` rows (if
  present in api) as distinct declarations; `Optional[...]` and `list[...]` annotations
  lowered; `__init__` synthesized-by-decorator handling must NOT fabricate parameters the
  source does not write.
- flask: `Flask` class with `route` decorator facts; `app` Blueprint methods; docstrings.
- six: `PY2`/`PY3` constants; `with_metaclass` callable; python2-compat spellings parse clean.
- wcwidth: `zero_width`/`combining` table constants; `wcwidth` function signature.
- Cross-package: every package's fragment decodes with at least N>50 entities; occurrences
  tiers use Index/Import/Oracle honestly (Oracle only where the checker proved local, Import
  only where the checker resolved the module).
- Each discovered lane defect (crash, wrong tier, missing fact, dishonest Unknown) is recorded
  in the return as a named edge case with exact source coordinates; do not fix production code.

## Bounds
Same environment/bounds as Card P (8 MiB cap per artifact, 60s timeout, one retry, typed
terminals, temp-dir hygiene, cleanup). rustfmt-clean, clippy-clean. No manifest edits (Cards P
already added the dev-deps; attrs/flask/requests are pure sdists — no build runs).

## Exact commands
- cargo test -p compiler-driver --test python_packages
- cargo fmt -p compiler-driver -- --check
- cargo clippy -p compiler-driver --tests 2>&1 | tail -5

## Commit protocol
One commit: `test(python): real-package fidelity matrix over decoded semantic lanes`.
Return: commit hash, per-package assertion counts, and the ranked edge-case list (the feed for
repair cards).

## Plan closure
A package that cannot be fetched or parsed produces a typed failing test naming the terminal —
never a vendored copy, never a silent skip.

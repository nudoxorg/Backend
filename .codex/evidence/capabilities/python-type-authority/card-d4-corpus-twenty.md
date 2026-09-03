# Card d4-corpus-twenty — 20-package real-source fidelity matrix

registered role: nudox_luna_implementer
baseline: HEAD 4187ec9b5 (python lane green: render 5/5, purl 5/5+1 ignored, packages 9/9,
checker lane 35/35).

## Owned paths (no overlapping writer)

- `compiler/driver/tests/python_packages.rs` — extend the existing per-package matrix.
- `compiler/driver/tests/python_support/mod.rs` — ONLY if a helper generalization is
  strictly required by the matrix (prefer zero changes; the existing helpers suffice).

Everything else is forbidden.

## Law

Every package entry must prove, from the REAL PyPI sdist (pinned exact version, fetched at
test time with the established typed-skip on network/tool absence):

1. the pinned sdist downloads and its PyPI-declared sha256 verifies;
2. the module chosen for deep asserts is the package's PRIMARY module, unless it exceeds
   the lane's frozen 128-fact capacity (MAX_EMISSION_FACTS) — then the largest module
   under the import root that compiles is asserted INSTEAD, and the primary's capacity
   terminal is asserted explicitly (exact LoweringUnsupported cause) so the gap stays
   countable;
3. deep asserts vs source truth: exact named declarations of at least four real symbols
   (classes/functions with exact EntityKind), exact parameter-kind evidence (one function
   with kinds), decorator facts where the source has them, the module docstring lane, and
   one Foreign pypi package key or Local occurrence proven from an expression-position use
   in the source;
4. layout diversity: at least four packages must exercise a NON-flat layout (src/
   directory, package-dir, or single-module distributions) and the module locator must
   handle each without per-package hacks.

## Required matrix (15 additions to the existing requests/attrs/flask/six/wcwidth)

Pick current stable versions at implementation time and pin them exactly. Suggested set
(substitute a same-shaped package if one is unavailable): idna, certifi, packaging,
pyparsing, iniconfig, pluggy, click, itsdangerous, jinja2, markupsafe, werkzeug, colorama,
PyYAML, tomli, webencodings. jinja2/markupsafe/packaging give src- or package-dir layouts;
iniconfig/tomli/webencodings are single-module distributions.

## Must prove (falsifiers, exact commands)

1. `cargo test -p compiler-driver --test python_packages` — all package tests green (the
   pre-existing 9 plus every new one); none ignored.
2. `cargo test -p compiler-driver --test python_render` — stays 5/5 (no shared-helper
   regression).
3. Every capacity-blocked primary module appears in the test output as an explicit
   capacity observation (count them and report the number; these feed the D2 escalation).

## Bounds

- No production code changes (compiler/** stays untouched except the two owned test
  files); no new dependencies; no manifest edits.
- Assertions name EXACT source facts (symbol names, kinds, module docstring text
  prefixes), never "did not error".
- Keep the file's forbid(unsafe)/deny(unwrap,expect,panic) law.

## Stop decisions

- If more than 5 of the 15 additions prove impossible for the same root cause (e.g.
  capacity), STOP and report the pattern instead of forcing the remainder.

## Commit and return

- One coherent checkpoint commit: `test(python): twenty-package real-source fidelity matrix`
- `cargo fmt` the owned files.
- Return: commit hash; the package→module table (package, version, asserted module,
  layout, deep-assert count, capacity-blocked?); exact gate tails; smallest remaining red.

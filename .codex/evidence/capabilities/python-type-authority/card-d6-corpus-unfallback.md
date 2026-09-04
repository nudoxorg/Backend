# Card d6-corpus-unfallback — no terminal-tolerant arms, deep decoded asserts

registered role: nudox_luna_implementer
baseline: commit 345718c8d (branch codex/fidelity-python)
- compiler/driver/tests/python_packages.rs | 1204 LOC | d2c612eda88ce5f7 (pre-edit; verify with shasum)

## Owned path (no other writer)

- compiler/driver/tests/python_packages.rs

Everything else forbidden. Production code forbidden.

## Context (verified live by Terra at this baseline)

All twenty primaries now LOWER COMPLETELY at the raised geometry (2048 facts /
32 children): the corpus run prints 20 "python package selected" lines and ZERO
"python package typed terminal" lines, 10/10 pass. Measured entities:
pyparsing 1498, click 744, attrs _make 517, jinja2 environment 495. The
expect/terminal machinery is now DEAD CODE that still encodes the old
capacity fallbacks.

## Law

The corpus matrix must contain no fallback, no terminal-tolerant arm, and no
silent acceptance — every package deep-asserts decoded lanes against source
truth or the test fails.

1. DELETE the ExpectedTerminal struct, lane_full_terminal, child_lane_full_terminal,
   the LANE_FULL_FACT const, the `expect` field, and every match arm that
   accepts a terminal (including attrs' silent arm in attrs_real_sdist_...).
2. Tag each package with its layout class as a typed enum
   (FlatSingleModule / SingleModuleDistribution / SrcLayout / PackageDir —
   pick names that fit the real taxonomy: six=flat single module,
   certifi/webencodings=flat package dir, requests/attrs/flask/idna/packaging/
   pyparsing/iniconfig/pluggy/click/itsdangerous/jinja2/markupsafe/werkzeug/
   tomli=src layout, PyYAML=lib/-rooted package-dir, colorama=flat package
   dir) and PRINT one summary line per package:
   `python corpus: <pkg>@<ver> class=<class> entities=<n> assertions=<k>`
3. DEEPEN the decoded-IR asserts: for each of the 20 packages add at least one
   exact spot check derived from that sdist's real source truth, e.g. a named
   function whose decoded parameter-kind evidence matches the written
   signature, a named class whose decorator atom list matches the source
   decorators in order, or a named module docstring whose decoded text prefix
   matches the source. Derive these from the actual pinned sdists (fetch the
   source you need; the host has network). Name the spot-check symbol in the
   PackageFacts row (a new `spot: SpotCheck` field or equivalent — your
   representation, but it must assert exact decoded bytes/kinds, not counts).
4. Every changed assertion must keep the file's forbid(unsafe)/
   deny(unwrap,expect,panic) law and typed-skip behavior for network absence.

## Must prove (falsifiers)

1. `CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-python/.local/target cargo test
   -p compiler-driver --test python_packages -- --nocapture` → 10/10 pass,
   20 corpus summary lines, ZERO "typed terminal" lines.
2. Hostile mutant check (in your return, not committed): state which pinned
   spot check fails if (a) one EntityKind is swapped in the entities lane,
   (b) one decorator atom is dropped — name the exact assertion that dies.
3. `rg -n "expect:|ExpectedTerminal|lane_full_terminal|child_lane_full_terminal"
   compiler/driver/tests/python_packages.rs` → zero matches.

## Bounds

- No production code, no other test files, no new dependencies.
- rustfmt clean; one coherent checkpoint commit:
  `test(python): corpus matrix without fallback arms and with per-package spot checks`

## Commit and return

Return: commit hash; the package→(class, entities, spot-check) table; the
mutant analysis; exact gate tails; smallest remaining red.

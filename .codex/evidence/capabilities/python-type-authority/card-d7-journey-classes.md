# Card d7-journey-classes — codify the package-class lifecycle matrix

registered role: nudox_luna_implementer
baseline: commit 202b42aa2 (branch codex/fidelity-python)
- compiler/driver/tests/python_purl_lifecycle.rs | 658 LOC | 49d8984f97b7bf2e
- compiler/driver/tests/python_support/mod.rs | 495 LOC | a2e608296a1963d1

## Owned paths (no other writer)

- compiler/driver/tests/python_purl_lifecycle.rs
- compiler/driver/tests/python_support/mod.rs

Everything else is forbidden (python_packages.rs is another worker's file;
production code is forbidden).

## Law

The PURL lifecycle integration tier must codify one full
fetch→digest-verify→unpack→locate→compile→authorities→fragment→publish→reopen→
index→gen-2→old-fragment-revalidate journey PER package layout class, and the
class taxonomy must be explicit, not incidental. Today three journeys share
`package_class_lifecycle`: six (flat single-module), idna (src/ layout),
PyYAML (lib/-rooted package-dir — the non-standard layout). The taxonomy is
implicit in test names only.

1. Make each journey's layout class an explicit, typed fact of the test tree
   (e.g. a `LayoutClass` enum on `Journey` — flat-single-module, src-layout,
   package-dir — and a per-journey assertion that the located primary actually
   sits in that class's shape). A mislocated primary (right file, wrong class
   shape) must fail the journey with a typed error.
2. Prove the locator is class-generic: a crafted-archive unit test per class
   (reuse the existing typed-tar fixtures in python_support) whose primary
   sits ONLY in that class's shape, and one negative per class (primary
   absent → exact typed error, never a fallback to another file).
3. Add the fourth real journey for the `webencodings 0.5.1` single-module
   distribution through the full shared skeleton (its primary lowers today;
   pin exact named declarations from its source truth, e.g. `Encoding`,
   `decode`, `lookup`, `ascii_lower`).
4. Shortcut hunt, owned files only: `cargo test -p compiler-driver --test
   python_packages --no-run` currently warns six dead helpers in
   python_support (parse/locate/decode_hex64/find_primary unused in that
   target) because both test targets `#[path]`-include the whole module.
   Eliminate the warnings by structure (split journey-only helpers into their
   own module included only by the lifecycle target, or equivalent), NOT by
   `#[allow(dead_code)]`. Zero warnings must remain in every python test
   target. Also remove any silent error-swallowing (`let _ = ...` on fallible
   cleanup, catch-all matches) you find in the two owned files.

## Must prove (falsifiers)

1. `cargo test -p compiler-driver --test python_purl_lifecycle` — all
   journeys green (was 8/8; the fourth journey adds ~1 test), zero ignored.
2. `cargo test -p compiler-driver --test python_purl_lifecycle -- --nocapture`
   — each journey prints its class + the located primary path; no journey
   silently skips its index leg unless the typed EntityLimit terminal fires
   with exact numbers (six's {maximum: 256, observed: 331} is the one known).
3. `cargo test -p compiler-driver --test python_packages --no-run` AND
   `cargo test -p compiler-driver --test python_purl_lifecycle --no-run` AND
   `cargo test -p compiler-driver --test python_render --no-run` — zero
   warnings from python_support in every target.
4. The crafted-archive unit tests fail a plausible weakened locator (one that
   returns the first .py file) — state in the return which negative test
   kills which shortcut.

## Bounds

- No production code, no new dependencies, no manifest edits.
- Keep the file's forbid(unsafe)/deny(unwrap,expect,panic) law and the
  typed-skip pattern for network/tool absence.
- Assertion style unchanged: exact named declarations, exact digests, typed
  errors everywhere.

## Commit and return

- One coherent checkpoint: `test(python): typed package-class journey matrix`
- Return: commit hash; journey→class table (journey, class, primary, index-leg
  outcome); the warning-elimination mechanism; exact gate tails; smallest
  remaining red.

# Card P2 — old-generation validation via public APIs (digest-frozen)

Registered role: nudox_luna_implementer. Repository /Users/mileswirht/Downloads/backend,
shared worktree. Do NOT run git commands. Own ONLY compiler/driver/tests/python_purl_lifecycle.rs.

## Defect (manager review)
The card requires: after the SECOND publication, re-read generation 1's stored fragment bytes
from the reopened immutable store and prove they validate and match the bytes originally
written. Today the test compares against an arbitrary `read_dir` hit BEFORE gen2 lands.
`GenerationBindingStore`/`ImmutableManifestStore` are `pub(crate)` — not usable from tests.

## Required work
1. Determine the public route to generation-1 bytes: at FIRST open, `open_published` returns
   `OpenedCompilation` and your scratch held the parsed manifest (`FragmentRangeManifest`
   facts). Inspect what those public types expose (fragment file identity, lengths,
   artifact-relative names). Use exactly that public evidence to re-read generation 1's
   fragment bytes from `artifacts/` AFTER the second publication, assert sha256 equality
   with the bytes captured at first publish, and `FragmentView::validate` them.
2. If NO public route exists (the store's addressing is unobservable from a test), STOP and
   return: (a) the exact public surface you inspected, (b) the minimal API capability that
   is missing, (c) the strongest evidence you CAN prove publicly — implemented. Do not
   fake addressing by scanning directories with assumptions.
3. Keep everything else (lineage chain, breach terminals, index round-trip, probe
   assertion, retained error causes) unchanged.

## Gates
- cargo test -p compiler-driver --test python_purl_lifecycle
- cargo fmt -p compiler-driver -- --check
- cargo clippy -p compiler-driver --tests 2>&1 | tail -5

## Return
The public route used (or the escalation evidence), test names + counts, observed
generation numbers and the gen-1 sha256 equality result, gate outputs.

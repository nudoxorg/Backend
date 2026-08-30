# P1 C1a12B behavioral and coherence proof card

Base: isolated repair branch `codex/prototype-canonical-root-hydration-c1a-repair` at
`83631f94`. C1a12A independently passed production quality; its only remaining all-target
clippy findings occur in the deliberately incomplete public C1a test target and are owned here.
This card owns only ordinary public behavioral tests and the two permitted hydration UI fixtures.
It does not add a layout-lab binary/TSV, change production source, C1b, or P2.

Allowed paths: `crates/nudox-root/tests/canonical_root_view.rs` and exactly
`crates/nudox-hydration/tests/ui/forged_validated_root.rs`,
`forged_validated_root.stderr`, `escaped_borrowed_root.rs`, and
`escaped_borrowed_root.stderr`. Test code must make
`cargo clippy -p nudox-root --all-targets -- -D warnings` clean without blanket allow.

The root test uses public APIs and must prove every C1a10 case: canonical permutation and
identity equivalence; all header/count/geometry/short/trailing/schema/order/parent/missing/cycle
mutations, including global priority collisions; pointer containment; 0/1/100k shallow/deep;
one-root/self-cycle, branch into a destroyed tail, and a cycle through an incoming branch; exact
lookup/full scan, locality-only identity stability and mismatch rejection; fact-read source
compatibility; scratch drop/allocation-free post-validation source-observable behavior; and
sentinel/overflow theorem. A large test must remain deterministic and safely constructed from
public C0 input.

The UI fixtures must demonstrate direct `ValidatedRoot` and `BorrowedGenerationView` literal
forging fails because private fields are inaccessible, direct fact reassignment fails despite
field reads, and both root and view lifetime escapes fail. Checked stderr is required. Do not
attempt runtime tests or fabricated measurements in these files.

The worker commits and reports actual test/all-target clippy output. A fresh read-only Terra
review must pass this boundary before the C1a12C lab-only card starts.

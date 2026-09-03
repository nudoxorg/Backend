# Capability brief: csharp-roslyn-fidelity (finish-perfect phase)

## Public terminal
`nuget:pkg@ver` lowers end-to-end: locate (NuGet v3 flat container + SHA-512
sidecar) -> bounded fetch -> unpacked source tree -> Roslyn authority image
(v3 NCAI emitted by the vendored producer) -> canonical fragments -> durable
publication -> reopen -> index build/seal -> second generation on the same
journal. The decoded IR is asserted against source truth per artifact
(nullability, ref kinds, effects, partial roles, signatures, doc links,
occurrences).

## Chief-owned red journey (consumed, not redefined)
Trunk landed: geometry 1024, journal-chained generations, exact terminals,
render truth. This lane consumes those canonically first; the lane's earlier
v3 image reader, explicit-interface-implementation wire, and
operator/conversion work stand.

## Non-negotiable laws
1. Producer parity: `helper/` emits exactly the wire `image.rs` validates —
   version 3, 256-byte header, 11 canonical sections, domain-separated
   SHA-256, ABSENT=0xFFFFFFFF, closed vocabularies, no silent fallback.
2. Old fragments keep validating: every fixture/golden that passes at
   baseline still passes at closure (regression law, explicit list below).
3. Corpus artifacts are REAL nuget packages fetched from nuget.org and
   digest-verified; nothing synthetic masquerades as a corpus row.
4. No invented cells: image-retained facts with no lane cell are escalated
   to trunk with exact evidence, never stuffed into spare bits.
5. No degradation: no silent fallback, no ignored test, no evidence-erasing
   fault folding survives on this lane's surface.
6. Luna workers write all production code; Terra reviews from diffs and
   reproduces falsifiers.

## Explicit negative space
- No new wire cells on the NCAI v3 image (format frozen by the reader).
- No new external dependencies for the corpus transport beyond the crate
  set already in workspace (ureq, flate2 dev-deps of compiler-driver).
- No analyzer/generator execution beyond the producer's configured rules.
- No edits to other lanes' semantics (rust/go/java/ts lowering behavior);
  their broken test modules at baseline are repaired mechanically to the
  current signatures only to unblock shared gates, never re-designed.

## Baseline / concurrent path ownership
Baseline commit 309acc8f1 (see index.toml digests). Terra owns evidence/,
driver test-support repairs, integration commits. Luna cards own disjoint
paths named per card. No overlapping writer.

## Affected consumers / dependency direction
compiler-languages-csharp <- compiler-driver <- compiler-application.
Fragments consumed by compiler-publication, server-journal,
server-index-build, server-index-publish (corpus journey). The helper is
built by dotnet and never compiled by the Rust crate.

## TESTING.md digest
TESTING.md, ORCHESTRATION.md, and COMPILER_IR_GREENFIELD_PLAN.md are absent
from this worktree (documented for the old workspace2 layout; four-boundary
crates here are compiler/, heart/, interface/, server/). Evidenced
exclusion, recorded verbatim: the digest cannot be taken from a file that
does not exist at any path under the worktree root (`find -maxdepth 2
-name TESTING.md` -> empty). The applicable craft/evidence clauses are
taken from .opencode/skills/deliver-reviewed-rust-slice/SKILL.md and
.opencode/skills/build-greenfield-compiler-ir/SKILL.md, present and
committed here; their clause -> row mapping lives in the proof matrix
(boundary cases -> R-IMG-*, R-COR-*; exact diagnostics -> R-LOW-*,
R-SHORT-*; real-package fixtures -> R-COR-1; allocation/timing evidence is
test-support transport, excluded as non-shipping).

## Product-authority questions only (escalations)
1. Generic variance + params modifier: canonical `TypeParameter.variance`
   exists (compiler/ir/semantic.rs:1215) but the emission lane
   `ExtensionTypeParameter` (compiler/ir/extension_pools.rs:17) has no cell
   and `build_ir` hard-codes `Variance::Invariant` (driver/lower.rs:985,
   1005); `CSharpFacts` has no params cell. Closing requires a shared-crate
   shared-surface edit owned by trunk. Escalation packet filed; do not
   invent cells locally.

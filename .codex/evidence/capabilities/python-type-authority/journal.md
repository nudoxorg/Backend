# Research journal — python-type-authority

2026-09-02 | repo scout | Prior state in mandate was stale: pyrefly checker (2098 LOC), compound
lowering, occurrence tiers with pypi package keys, extension facts, docstring fragments already
exist in working tree (uncommitted). Mandate deltas reduce to: golden render tests, PURL
lifecycle integration tests, real-package fidelity tests, overload fidelity proof, edge-case
repairs. | decision: do not re-implement; prove, then fill.

2026-09-02 | environment probe | pyrefly 1.2.0 via uvx live; uv/uvx present; network to
pypi.org/simple live (HTTP 200); python3 present (RustPython shim). | decision: live
integration tests may fetch pinned artifacts; every external tool gate uses the typed-skip
pattern already established in the checker live tests.

2026-09-02 | shared-worktree hazard | clang lane's in-flight lower/clang.rs broke
compiler-driver builds during scouting; fixed upstream mid-session (build now clean).
| decision: commit only owned paths; never stage adjacent lanes' files.

2026-09-02 | fetch/unpack mechanism | wheels are zip; workspace deps include flate2 (gzip) and
ureq (HTTP) but no zip crate. | decision: sdist (.tar.gz) unpack via flate2+manual tar reading
or system tar; wheel unpack via system `unzip` with typed skip when absent — no new product
dependency; test-tree-only concern. Revisit only if unzip is unavailable on the gate host.

2026-09-02 | calibration (card-purl) | v1: reader exact; misreader found manifest overreach,
vendoring loophole, and a nonexistent pin (six@2.16.0 -> 1.17.0). Rewrote: five-line manifest
bound, sha256 lineage chain, real pin. v2: reader exact; misreader attacks reduced to
enforcement strategies (fake lineage, unconnected breach paths, in-memory compare) and worker
taste. | decision: capability scope held; enforcement is Terra's ingestion duty (mechanical
diff checks: lineage assertions present, breach paths call the happy-path downloader, gen-1
bytes re-read from store). Round-3 trials not spent on taste; residual discretion is
deliberate. Card digest de298972741c795d+4 edits, current digest recorded in commit.

2026-09-02 | golden iteration (render card ingestion) | True lane+renderer output vs worker
aspirations: (1) every entity renders a `/* visibility unknown */` prefix — python pushes no
visibility lane; (2) ALL classes incl. Protocol push EntityKind::Record (Protocol semantics
live in the AnonymousRecord type row, not the entity kind); (3) function signatures render no
parameter/return tail and module statics render no type — build_ir (driver trunk, shared)
lifts only Primitive records matching bool/i32/str into the renderable type DAG; compound
python lattice rows stay in the pooled planes. | decision: goldens freeze TRUE output; the
compound-surfacing question (extend shared trunk lifting vs pooled-lane-only rendering) is a
product fork -> escalate with evidence, do not collide with the lane actively editing
lower.rs.

2026-09-02 | shared-trunk churn | compiler-driver lib repeatedly broken mid-session by
adjacent lanes (clang.rs earlier; csharp.rs ReferenceTag::InterfaceImplementation +
typescript.rs narrowings now; active lower.rs edits). | decision: lane-crate tests stay the
always-runnable gate; driver-level gates run in green windows; never touch adjacent files.

2026-09-02 | six.py capacity probe | six 1.17.0 six.py: ~49 top-level + 115 member
declarations (~164 facts) > MAX_EMISSION_FACTS=128; compile fails with the honest
LoweringUnsupported::NoSupportedDeclaration after fetch/unpack/workspace all pass. |
decision: lane capacity is a shared-trunk product bound -> escalate; journey test kept as
evidence-linked ignore, pre-compile lifecycle stages proven passing.

2026-09-02 | cross-lane blocker | Fragment prepare rejects multi-structural-class python
modules (ForwardReference {ordinal 2..4, position 0, target ordinal+1}) — e.g. Z(plain) +
Reader(Protocol, one method). Three Luna repair rounds; the third hit its stop trigger: the
offending coordinate arises in the shared trunk's lane construction (lower.rs, actively
edited by the adjacent lane), not in python emission — python's appended children are
backward anon coordinates [128,129,130]. | decision: escalate as cross-lane trunk defect
with counterexample; two fragment regression tests committed as evidence-linked ignores;
python.rs kept pristine at HEAD.

2026-09-02 | pax tar support | six sdist carries pax extended headers (tar kind 120 'x');
unpacker now honours `path=` overrides and skips other pax records; corruption/cap/timeout
terminals unchanged and exercised.

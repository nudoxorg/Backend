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

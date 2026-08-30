# Wave C.7 application service — Phase 0 brief

Capability: `wave-c7-application-service`

Public terminal: one concrete, typed, client-independent command/query service over the accepted
compiler language/stage vocabulary and typed snapshot identity. It returns deterministic ordered
replies for package generation, snapshot status, lexical search, graph, vector, locality, health,
and bounded progress. Each reply carries correlation and one explicit terminal:
`Complete`, `Partial`, `Degraded`, `Cancelled`, or `Failed`.

The service owns semantic validation, exact structured errors (including rejected operands and
causal source), stable ordering, bounded replay cursors, and progress state. It consumes existing
typed facts; it does not own filesystem/network/runtime SDKs, JSON/MCP/CLI/GPUI protocol details,
serde, caching, `dyn`, boxed streams/futures, or a graph/vector backend.

Must preserve: accepted compiler language/stage names, typed snapshot identity, exact error/source
fidelity, deterministic ordering, client-independent state, useful local facts while remote work is
unavailable, and disabled typed-probe laziness.

Negative space: constant-body/input-ignoring implementations, empty success after remote outage,
lost terminal, unstable order, cursor-specific shadow state, and limit+1 acceptance are failures.

Path custody: Terra owns this evidence directory and, after Phase 0 review, only
`workspace2/crates/nudox-application/src/**` and focused tests below `src/`. Sol owns the manifest,
workspace lockfiles, top-level integration tests, quality inventory, and adapters. Existing crates
are read-only.

Resource controls: fixed command/reply/progress bounds; no per-poll allocation after setup; no
unbounded queue or replay retention; local facts are borrowed or concrete owned values only when
the result lifetime requires it. Any allocation must record exact bound, owner lifetime, rejection,
and a measured scalar control.

Baseline: `f2565a9fb33af06053bd19721d4dc2753ec09ed5`, tree
`8477cb2ab93763c468d5431740cfb1d5e4c4cf82`.

TESTING.md digest: `c29ae328a26117dd347c9b4952b24774c0cd7a5c6b8cc0b28ae2d7f22fda8e7b`.

Required public falsifier: a deterministic golden semantic corpus exercises every command through
the service, two independent replay cursors, malformed semantic input, limit+1, cancellation,
remote degradation, and repeated terminal polling. It compares exact typed results/order,
correlation, diagnostics, and terminal facts.

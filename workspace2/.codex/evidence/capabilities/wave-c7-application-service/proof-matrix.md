# Wave C.7 proof matrix (Phase 0)

| ID | Law and falsifier | Owner | State |
|---|---|---|---|
| C7-01 | Typed command/query vocabulary rejects malformed language/stage/snapshot operands, preserving exact operand and state; malformed corpus row | application service | CARD FROZEN |
| C7-02 | Generate and status consume real snapshot/compiler facts; input-ignoring constant-body mutant fails exact result comparison | application service | CARD FROZEN |
| C7-03 | Search, graph, vector, and locality return deterministic typed order and bounded limits; reordered corpus and limit+1 rows | application service | CARD FROZEN |
| C7-04 | Health distinguishes local-ready, unavailable, and degraded facts; outage-to-empty-success mutant fails | application service | CARD FROZEN |
| C7-05 | Progress has explicit terminal exactly once and stable keyed updates; collector-loses-terminal and duplicate-terminal mutants fail | application service | CARD FROZEN |
| C7-06 | Two independent cursors replay the same stable progress without per-client shadow state; reconnect/replay corpus | application service | CARD FROZEN |
| C7-07 | Cancellation/partial/degraded/failed preserve chronology, correlation, and causal source | application service | CARD FROZEN |
| C7-08 | Disabled typed probe does not build/format/allocate; existing lazy Probe seam and allocation counter | nudox-observe seam + service | CARD FROZEN |
| C7-09 | Bounds cover command, replies, progress, replay cursor, and allocations; zero/one/full/+1 schedule | application service | CARD FROZEN |
| C7-10 | No adapter/dependency leakage, dyn service, boxed stream/future, serde, cache, or backend claim; manifest and API inventory | Sol-owned integration | OUT OF CONTRACT (Sol gate) |

Phase 0 has no production-writing workers yet. A source-isolated pre-edit Terra review is required
before implementation authority is released.

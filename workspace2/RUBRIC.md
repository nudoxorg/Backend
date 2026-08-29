# GEM rubric

This rubric scores implementation evidence, not confidence. Eight means the agreed plan is fully
implemented and production-worthy. Nine and ten are the same design taken materially further, not
unrequested feature sprawl.

## Score meanings

| Score | Meaning |
|---:|---|
| 0 | Missing or not reviewable. |
| 1 | Names and scaffolding only. |
| 2 | Compiles on a narrow happy path; core semantics absent. |
| 3 | Main path exists but invariants live in comments/caller discipline. |
| 4 | Useful implementation with typed errors and basic tests; important edge cases remain. |
| 5 | Correct normal design with bounded resources; adversarial and integration evidence incomplete. |
| 6 | Strong implementation: invariants are structural, failures tested, API reasonably compressed. |
| 7 | Release-candidate quality with end-to-end evidence and measured resource behavior; some plan item or review concern remains. |
| 8 | Full plan complete: all acceptance tests, architectural laws, API review, memory/compute budgets, documentation, and integration gates pass. No known correctness debt. |
| 9 | Exceptional: simpler than the plan or materially stronger under failure/load, with measurements proving the improvement and no readability cost. |
| 10 | GEM: robust stretch version of the same scope—demonstrably tiny, elegant, adversarially hardened, unusually clear, and measurably excellent. An expert reviewer finds no needless state, allocation, copy, abstraction, or exposed invalid state. |

## Weighted dimensions

| Dimension | Weight | Eight requires |
|---|---:|---|
| Correctness and invariants | 22 | Invalid states are unrepresentable where practical; malformed input and state transitions fail precisely. |
| API and type design | 17 | Each abstraction owns one semantic axis and survives new cases without catch-all widening; public surface is minimal, ergonomic, statically composed, and has no backend leakage. |
| Memory behavior | 14 | Borrowed/inline/caller-scratch fast paths are proven; all growth is bounded and no retained heap owner or redundant materialization lacks a lifetime/capacity reason. |
| Compute and latency | 10 | Work proportional to requested/change set; no hidden scans; representative benchmark or instruction/work counters. |
| Concurrency and cancellation | 9 | Explicit ownership, bounded admission, terminal/cancel paths tested; lock-free claims model-tested or removed. |
| Wire/storage compatibility | 8 | Canonical bytes, golden vectors, size/offset limits, version behavior and adversarial mutations tested. |
| Integration completeness | 8 | Real end-to-end path crosses the swath boundary; partial/truncated/overload behavior included. |
| Test quality | 6 | Tests prove semantics rather than implementation trivia; property/model tests cover combinatorial boundaries. |
| Simplicity and readability | 4 | Every abstraction pays rent; small invariant-named functions/modules; crate roots are maps; no speculative framework or duplicated vocabulary. |
| Documentation and evidence | 2 | Public contracts explain invariants, complexity, ownership, and measured budgets without narrating obvious code. |

Overall score is the weighted mean, capped by the gates below.

## Score caps

- Does not compile or test: maximum 2.
- Public API permits malformed state that the type system can cheaply prevent: maximum 4.
- Unbounded queue/allocation or whole-payload buffering on a streaming path: maximum 4.
- Hot-path global mutex, erased causal errors, or cloneable progress cursor: maximum 4.
- Handwritten unsafe without a reviewed safety argument and Miri/model evidence: maximum 4.
- Missing adversarial tests for untrusted bytes or state recovery: maximum 5.
- Missing cross-crate integration test: maximum 6.
- No measured memory/compute evidence for a performance claim: maximum 6.
- Any known plan item incomplete: maximum 7.
- Monolithic implementation roots, one-letter public generic soup, or trivial tests: maximum 7.
- Namespace structs, unexplained layout arithmetic, bespoke wire accessors, or handwritten ordinary
  error boilerplate: maximum 7.
- Heap ownership on a central path without a measured lifetime/capacity reason and a borrowed or
  caller-scratch alternative: maximum 7.
- Immutable roots, locality maps, manifests, or indexes whose primary representation deserializes one
  canonical artifact into multiple independent heap collections: maximum 5. A borrowed packed view or
  measured generic owner/output boundary is required for 8.
- A monotone locality/index set stores payload coordinates derivable from rank, or adopts one sparse
  representation without measured density/access crossovers: maximum 5.
- A store forces one heap payload-owner class or performs dynamic inline/heap backend dispatch inside
  its hot lookup loop when static client/server storage policies are available: maximum 5.
- Public impossible-state/Internal errors, silent omission from validated data, or new cases handled
  by weakening a closed invariant into Option/bool/catch-all caller discipline: maximum 4.
- Any `_`-discarded conversion/allocation/ownership error or application-owned raw hash
  personalization literal: maximum 4.
- Stringly state-machine diagnostics, positional tuple counters for distinct semantic facts, magic
  workload cardinalities, or manual `Debug`/`Display` trees for ordinary errors: maximum 5.
- Public or cross-crate primitive soup for counts, offsets, byte quantities, capacities, epochs,
  protocol codes, or packed states: maximum 5. A replacement only lifts the cap when it prevents
  interchange, derives the right `From`/`TryFrom`/borrow ergonomics, and has exact zero-cost layout
  evidence; decorative wrappers whose main API is `.get()` do not count.
- A monolithic resource transition implemented as acquisition plus compensating rollback ladders,
  when linear ownership can represent the states directly: maximum 5.
- Coupling locality-only changes to semantic-root rebuilding or re-identification: maximum 4 for the
  object/hydration swath.
- Manual `expect`/`unwrap`/`panic!` used for fallible production, scenario, fixture, thread-join, or
  poll handling: maximum 4. Failure reporting must retain typed source and observed state.
- Eight is forbidden while TODOs, ignored tests, undocumented panics, or review findings remain.

## Ten-point stretch evidence

A ten must add at least three relevant stretch proofs without broadening the product scope:

- zero heap allocations after warm-up for the central borrowed/read path;
- single-copy or no-copy end-to-end bytes through store, validation, and consumer;
- deterministic concurrency exploration with Loom plus sustained overload testing;
- mutation/fuzz corpus covering every header/offset/discriminant boundary;
- format evolution proof across two schema versions;
- memory high-water less than the declared bound under cancellation storms;
- benchmark showing work scales with changed/requested data rather than corpus size;
- API reduction that deletes a meaningful abstraction while retaining all capability.
- zero-allocation borrowed/inline fast paths with bounded fallible spill proven under allocator
  instrumentation;
- one authoritative typed layout whose size/offset assertions and golden bytes prevent magic-number
  drift across crates.

## Final Wave 1 scorecard

The evidence-weighted mean is **7.9/10**, capped to **7.0/10** because known plan items remain. This
is a release-candidate foundation, not the completed distributed product.

| Dimension | Score | Final evidence |
|---|---:|---|
| Correctness and invariants | 8.3 | Exact malformed-frame/locality errors, readiness typestate, first-write-wins ownership, typed terminal facts, durable crash-prefix replay. |
| API and type design | 7.8 | Domain IDs, direct validated fields, static storage/accounting/probe policies, lending local cursors, concrete associated futures. |
| Memory behavior | 7.4 | Borrowed frame/locality views, sparse planning scratch, inline/heap/caller-arena runtime policies, generic store payload ownership. |
| Compute and latency | 7.7 | Changed-only merge, demand-bounded closure, forward sparse cursors, cached locality geometry, measured SIMD crossover. |
| Concurrency and cancellation | 8.1 | Linear physical credits, scoped admissions, in-place terminals, nine Loom models, cancellation/drop conservation, focused Miri. |
| Wire/storage compatibility | 8.4 | Typed zerocopy records, exact offsets/golden bytes, exhaustive mutation classes, canonical 68-B workflow records. |
| Integration completeness | 7.1 | One public driver proves exact local/partial/runtime/workflow semantics and the same 15 events through OTEL; distributed adapters remain later work. |
| Test quality | 8.2 | Boundary matrices, Bolero, compile-fail witnesses, allocation cliffs, million-row laws, differential SIMD, Loom, Miri, exact OTEL topology/overload. |
| Simplicity and readability | 7.5 | Removed compatibility owners, terminal queue, no-op structs, duplicated state, dynamic dispatch, and generic invariant errors; several implementation files remain large. |
| Documentation and evidence | 8.2 | Architecture, legacy audit, async/observability contracts, layout lab, raw SIMD evidence, review ledger, reproducible quality tiers. |

The cap is caused by concrete omissions, not polish: `GenerationRoot` still retains one boxed native
row arena instead of a generic canonical-byte owner/view; store metadata has static heap/inline but no
caller-arena policy; hardware contention/cache evidence is incomplete; and the horizontally scaled
index, vertically elastic compiler fleet, range transport/object packs, and lean GUI are preserved as
later swaths rather than falsely scaffolded here.

# Testing infrastructure plan

Test strength is assessed by the defects a suite would catch, not test count.

## Pyramid

| Layer | Tool/shape | Required target |
|---|---|---|
| Exhaustive finite | table/macro generated | every enum value, transition pair, N-1/N/N+1 bound |
| Golden protocol | fixed byte corpus | encode/decode identity and every format version |
| Property/model | Bolero or proptest-style generator | canonical roots, diff, plans, stores, workflows |
| Concurrency | Loom production shim | slots, credits, cancellation, drop, ABA |
| Fuzz | replayable corpus + cargo-fuzz/Bolero | frame bytes, root builders, event logs |
| Type proof | compile-fail crate/trybuild | cross-domain IDs, forged witnesses, duplicate lease use |
| Memory/work | allocation, retained-byte, probe/visit counters | zero-copy and proportional-work claims |
| Integration | public APIs only | local, degraded, hydrated, overlay, overload, crash recovery |
| Endurance/sim | fixed-seed virtual network/time | reorder, loss, cancellation storms, remote outage |

## Harness laws

1. Valid generators call public checked constructors. Negative generators start from a valid value and
   break exactly one invariant, so the expected error is unambiguous.
2. A pure reference model is smaller and slower than production. It does not share transition code,
   indexes, or encoding helpers with the implementation under comparison.
3. Stateful tests compare output and complete observable state after every command, including rejected
   commands and drops.
4. Complexity is observable: comparisons, visited rows, probes, bytes written/read, allocations, and
   retained capacity are counters in test builds. Hot iterators additionally expose search count and
   common/exceptional route decisions so an allocation reduction cannot hide a branch-prediction or
   repeated-search regression.
5. Random failures print a deterministic seed and shrink/replay case; accepted regressions are checked
   into a corpus.
6. Snapshot tests cover large stable diagnostics only. Exact typed variants and fields are asserted
   before rendering; snapshot updates require human diff review.
7. Tests live in focused files/crates. Production modules do not become 400-line test containers.
8. No acceptance assertion is merely `is_err`, wildcard `matches!`, nonzero length, or “didn't panic.”
9. Scenario drivers return a compact typed evidence record or a concrete `ScenarioError`. Fallible
   setup, polling, joins, and teardown use `?`; an unexpected variant becomes structured
   expected/observed evidence. Manual `expect`, `unwrap`, and `panic!` are forbidden because they
   discard the causal chain and state needed by the failure reporter.
10. Semantic tests are concise `rstest` cases over named fixtures/schedules. They state the law,
    invoke one driver, and compare exact evidence. Setup, poll loops, joins, conservation snapshots,
    flight-tail capture, and source enrichment live behind focused helpers. A reporter may render,
    snapshot, or export a correlated OTEL failure without making a test body verbose.
11. Deterministic simulation drives the same public scenario commands and production state machines.
    Clock, network, filesystem, and process-lifetime effects are injected only at narrow adapter
    capabilities in a nested harness workspace; simulator types never enter core signatures.
12. Every scheduled failure reports and persists `{ seed, action_index, action, causal_error,
    conservation, flight_tail }`. Delay, reorder, duplicate, disconnect, restart, cancellation, short
    I/O, and torn append are explicit schedule values, never opaque probability hidden in a mock.
13. An unexpected stream event is data, not a panic: the driver converts it to a typed
    `ExpectedEvent { step, expected, observed }` source and returns it through `ScenarioError`.
14. A broad public-API journey is assembled from independently testable scenario components. No
    single test owns frame corruption, store admission, hydration, operation polling, runtime
    overload, and workflow replay as one procedural block merely to claim end-to-end coverage.

## Per-swath minimum before 8/10

Foundation: field mutation classification, every truncation exact class/offset, closed registries,
goldens, canonical re-encode, compile-time no-allocation proof, borrowed pointer identity.

Object/hydration: permutation canonicality, semantic/locality independence, streaming diff reference
model, range/ancestor work bounds, open-address collision model, exact capacity rollback, coverage and
publication transition cross-product.

Runtime/workflow: exhaustive phase×event table, state-machine command generator, production Loom
slots+credits, deterministic waker stream tests, concurrent sustained load, every drop/cancel prefix,
monotone delayed duplicates, recovery command exactness.

E2E: corrupt every exchanged artifact class, exhaust every physical credit, local/remote outage and
recovery, out-of-order fetches, stale generation substitution, zero half-publication, replay without
repeated committed work.

The same E2E driver survives deterministic restart after every durable transition and cancellation at
every registered async wake boundary. After every action it checks credit/lease conservation and
workflow/model equality, so a later failure cannot hide the first divergence.

## Gate tiers

- `quick`: format, check, focused deterministic unit/table tests.
- `pr`: workspace Clippy/docs/tests, nextest partitioning, Loom, compile-fail, fixed property cases,
  replay corpus, memory/work assertions.
- `nightly`: high-case property/fuzz, Miri, LLVM coverage, mutation testing, large simulation and
  endurance, benchmarks and binary-size budgets.

`tools/test-tier.sh quick|pr|nightly` is the deterministic entry point. It uses nextest when installed,
never retries a flaky failure, and includes every nested adapter/harness workspace in PR/nightly.
Nightly repeats the property/replay corpus under release optimization. Long-running cargo-bolero fuzz
campaigns are separate scheduled jobs named by each registered target; they must have an explicit time
budget and persist their corpus, rather than hiding an unbounded fuzz process inside this script.
Nextest writes CI JUnit output and has a single-threaded Loom profile in `.config/nextest.toml`.

`tools/semantic-audit.sh` is a review report, not an automatic rejection gate. Every reported scalar
must be classified as a raw wire/storage cell, a representation-local loop value, a semantic unit, or
a proof-bearing coordinate. The first two remain primitive only behind their owning boundary; the
latter two use transparent typed representations with conversion/borrow/layout evidence. This keeps
the audit exhaustive without rewarding decorative `.get()` wrappers.

The blocking quality gate rejects `.ok()?`/`filter_map` omission specifically inside validated
frame/root/locality representations. Once validation has established canonical record bounds and
closed discriminants, iteration is infallible; converting a later internal failure into `None` creates
a shorter valid-looking artifact and hides the first invariant breach.

A passing line-coverage number is not a release argument. Mutation survival in critical transition
code, uncovered error variants, or a missing complexity counter blocks 8/10 even at 100% line coverage.

Bolero is the preferred raw/structured property front end because one harness runs under ordinary
`cargo test`, exhaustive small-space enumeration, and cargo-bolero fuzz engines, following s2n-quic's
successful pattern. Keep generators semantic and bounded:
https://github.com/camshaft/bolero

`trybuild` is available only for high-value API impossibility proofs such as cross-domain identity
substitution, forged readiness, or duplicated linear leases. Do not snapshot generic compiler errors
for every wrong argument type:
https://docs.rs/trybuild/latest/trybuild/

Deterministic executor integration belongs in a nested harness after Wave 1's minimal manual-waker
driver is sound. Evaluate Turmoil before a bespoke network/filesystem simulator; keep Madsim out of
the core dependency graph because broad dependency replacement would constrain the portable product
graph. Whichever adapter wins must accept and emit our explicit replay schedule:
https://github.com/tokio-rs/turmoil

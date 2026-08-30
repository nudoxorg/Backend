# Wave C.7 unified application chief brief

## Identity and custody

- Capability: `wave-c7-unified-application`
- Baseline: `f2565a9fb33af06053bd19721d4dc2753ec09ed5`
- Baseline branch: `orchestra-shared`
- Integration branch: `codex/wave-c7-unified-app`
- Chief role config: `.codex/agents/nudox-sol-steward.toml`
- Chief runtime: top-level Codex task delegated from
  `01a04a90-d810-7d71-8bea-44cda7e5b969`
- Checkout: `/Users/mileswirht/.config/codex/worktrees/762f/backend`
- Forbidden checkout: `/Users/mileswirht/Downloads/backend`

This brief owns the public terminal and cross-adapter architecture. Capability Terras own the
representations and focused proof inside their assigned service, process/protocol, or GPUI slice.

## Current accepted inputs

The baseline exposes only these application-relevant facts:

- `nudox-compile-vocab`: closed `Language::{RustSubset, TypeScriptSubset}` and
  `Stage::{Parse, LowerIr}` with typed `FrontendError`;
- `nudox-compile-registry`: concrete static dispatch over the two current language rows;
- `nudox-index-vocab`: distinct checked `IndexSnapshotId`, `ExactSegmentId`, and
  `LexicalSegmentId` identities;
- `nudox-operation`: concrete non-erased cursor vocabulary with explicit complete, partial, and
  cancelled terminals;
- `nudox-observe`: typed lazy `Probe` and bounded allocation-free flight recorder;
- root/locality, hydration, runtime, and workflow facts remain in their current owners.

There is no accepted graph engine, vector engine, application service, CLI, MCP adapter, or GPUI
package at this baseline. The Wave C.7 service may report unavailable or degraded capabilities
honestly and may operate on bounded caller-supplied local facts. It must not manufacture backend
completion, shadow compiler/index truth, or establish compatibility with legacy/prototype APIs.

## Public terminal

One versioned golden semantic corpus executes the same ordered application requests through four
paths:

1. direct in-process typed dispatch;
2. a real CLI child process;
3. a real MCP JSON-RPC protocol adapter;
4. visible state projected by real GPUI entities/views under the GPUI test context.

The corpus contains package generation, snapshot status, lexical search, graph, vector, locality,
health, cancellation, and bounded progress/replay cases. Every path must yield the same typed
semantic result, order, terminal disposition, diagnostic code and operands, and correlation ID.
Protocol-only envelopes and UI presentation facts are compared separately rather than treated as
business results.

The terminal remains red unless all of the following are true:

- local proven facts remain available when a remote capability is unavailable;
- `Complete`, `Partial`, `Degraded`, `Cancelled`, and `Failed` remain distinct and occur exactly
  once before fused exhaustion;
- two clients replay one stable keyed progress history independently without a per-adapter queue or
  shadow application state;
- exact-capacity and capacity-plus-one cases preserve item and byte accounting;
- malformed semantic requests preserve exact rejected operands and leave state unchanged;
- malformed CLI/MCP inputs become adapter errors without entering business dispatch;
- cancellation has one named winner and cannot later become completion;
- input removal, constant result bodies, empty-success outage translation, reordered results,
  erased error sources, and dropped correlation all fail the corpus;
- typed tracing builders are not evaluated, formatted, or allocated when disabled;
- GPUI updates are keyed by semantic identity, bounded to one coalesced visible update per applied
  service batch, and require no polling or accessibility automation;
- the base service and CLI stay within recorded dependency and release-binary budgets, while GPUI
  remains an optional nested adapter graph.

## Single ownership boundary

The typed application service is the sole owner of:

- semantic request validation;
- business errors and exact causal operands;
- result ordering and cardinality;
- partial/degraded/cancelled/failed/complete terminals;
- progress chronology, cursor/replay, and cancellation state;
- correlation propagation across business events.

CLI, MCP, and GPUI may decode or present. They may not duplicate those rules, collect an unbounded
stream, create their own work queues, translate an outage into empty success, or retain a shadow
catalog/job state. No public dynamic service, boxed future, or boxed stream is permitted.

## Bounded capability slices

| Slice | Public terminal | Production ownership |
| --- | --- | --- |
| `wave-c7-application-service` | direct typed golden corpus including stable replay and all terminal families | `crates/nudox-application/src/**` |
| `wave-c7-process-protocol` | CLI process and MCP JSON-RPC produce the same typed corpus and exact adapter errors | nested application adapter crates, manifests, locks |
| `wave-c7-gpui` | GPUI package renders and updates generation/status/search/graph/vector/locality/health/progress from service replies | nested GPUI adapter crate |
| chief integration | one ordinary top-level cross-crate test compares all four paths | owning application crate `tests/**` |

Concurrent writers receive disjoint paths. The chief owns shared manifests, quality inventory,
golden corpus, public integration tests, dependency budgets, compaction, and the final closure
receipt.

## Negative space

- No test-only crate or shipping scenario/support API.
- No accessibility automation or unbounded/global filesystem search.
- No compatibility layer for `workspace/`, deleted docs, prototype branches, or GUI review APIs.
- No fabricated graph/vector/index/compiler backend result.
- No public `dyn`, boxed future/stream, `async-trait`, ambient runtime, detached task, or unbounded
  channel/collection.
- No `serde`, GPUI, runtime, network, filesystem, or MCP dependency in the portable typed service
  unless a later authority decision records why the edge adapter cannot own it.
- No cache or memoization presented as state ownership.
- No panic/no-panic-only oracle, string state, catch-all `Internal`, erased `map_err`, or formatted
  error comparison.

## Chief red oracle

The chief-owned oracle will live in an ordinary top-level `tests/` tree of the shipping application
workspace. It will use real process I/O for CLI and MCP and real GPUI entity/view tests, while
comparing their decoded output to direct typed dispatch. Focused unit, property, allocation, and
protocol-fault tests remain beside their owning crates.

The strongest initial mutant is a dispatcher that ignores every request and returns one successful
empty result. It must fail on result variant, ordered payload, terminal, diagnostics, correlation,
progress replay, local-fact preservation, and GPUI keyed state.

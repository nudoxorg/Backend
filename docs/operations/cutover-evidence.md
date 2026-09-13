# Executable cutover evidence

This page is the evidence index for the Astra review. It distinguishes a test
that exercises a contract from production wiring that has actually been
promoted. A test result is accepted only when the named Rust process or public
API runs to completion inside its declared deadline; a shell wrapper may select
the command and collect its bounded output, but it cannot establish a product
claim by inspecting a fabricated counter.

The independent regression suite is declared in
`.config/fixtures/cutover/regression-cases.json` and is run by the process
journey wrapper. It covers:

| Review concern | Executable evidence | What the oracle proves |
|---|---|---|
| Shadow full result vs delta | `full_recompute_shadow_and_delta_transition_have_the_same_state` | A fresh before/after join and the simultaneous three-term delta consolidate to the same weighted state. |
| Randomized multirow ordering | `randomized_multi_leaf_orderings_match_full_rebuild_oracle` | Eight deterministic randomized permutations, including 37-row batches, agree with a fresh `BTreeMap` and canonical root. |
| Large payload byte accounting | `large_payload_is_rejected_before_a_budgeted_page_clones_it` | A 28,672-byte owned payload is rejected under a 256-byte page envelope before row/byte credit is consumed, then succeeds under its measured envelope. |
| One-row delta independence | `one_row_delta_delivery_does_not_hydrate_the_growing_view` | A 100,001-row view produces one root-fenced event containing one changed row after a one-row append. |
| Broad disjoint join | `disjoint_hundred_thousand_row_join_is_bounded_and_matches_full_oracle` | Two disjoint 100,000-row inputs produce an empty full result with bounded indexed probes and no fanout. |
| Threshold and stalled observer pressure | `stalled_observer_gets_bounded_compaction_debt_then_releases_it` | A pinned observer blocks since advancement, each merge is bounded, level zero remains bounded, and release permits compaction. |
| Journal threshold and torn-tail recovery | `compacts_before_an_event_can_cross_the_scan_file_bound`, `torn_tail_repair_is_durable_across_a_second_open` | The local durable view journal compacts before its scan bound and repairs a crash-truncated suffix before the next open. |
| Over-budget publication | `large_payload_map_budget_fails_without_claiming_a_completed_update` | A failed update leaves the original immutable map unchanged. |

Seven named executable journeys remain the required end-to-end boundary. Five
are black-box Unix process tests and two are library integration journeys that
exercise daemon-owned scheduling and durable effects without substituting a
shell oracle:

1. real worker negotiation, same work identity, and attested output;
2. mid-execution cancellation followed by accepted control traffic;
3. local service restart with checked workspace recovery and unadmitted-result rejection;
4. cold Product dispatch, exact memo reuse, warm adjacent-root frontier reuse,
   forged-result local takeover, cancellation, late-result fencing, and byte reuse;
5. daemon-owned remote ticket correlation, backpressure, fallback, and reuse;
6. durable effect checkpoint/compaction with audit retention and forged-selection rejection;
7. CLI, MCP, and desktop clients sharing one process root and proof-bearing
   reply, including an async Trustfall query over the live polyglot view.
The five black-box journeys are implemented in
`tests/journeys/tests/cutover_e2e.rs`; daemon ticket custody and effect
checkpointing are implemented in `tests/journeys/src/lib.rs`. The contract
names all seven tests and the runner invokes the journey library, compiled
endpoints, process target, and independent regression target.
The runner gives reproducible cold preparation its own 900-second ceiling and
builds the process binaries plus every exact test harness with `--no-run`.
It then enforces a separate 45-second deadline on execution alone. A missing
binary, preparation timeout, runtime timeout, failed process, or missing test
name fails the suite. This keeps compilation outside the process SLO while
leaving both phases finite.

## Current K0–K12 disposition

The following status is deliberately conservative. “Evidence present” means
the repository contains a real bounded check. “Production complete” requires
the migration acceptance gate and the deletion conditions below; it is not
inferred from the presence of a test.

| Milestone | Evidence present now | Production disposition |
|---|---|---|
| K0 contracts/baseline | Control-plane JSON schemas, mutation fixtures, scope fixture, and process contract are checked by `.config/nu/cutover/tests.nu`. | Evidence present; baseline receipts and measured workload thresholds remain required. |
| K1 version/store | Independent canonical encoder, map history, payload budget, pack identity, crash matrix, and selected-head recovery tests. | Kernel evidence present; promotion still depends on the controlled full workspace run. |
| K2 flow | Independent weighted algebra, full/delta shadow, randomized arrangement roots, 100k join, bounded pages, and compaction churn tests. | Flow evidence present; no product authority is declared cut over by these tests. |
| K3 local vertical | A shared local-service composition, thin headless locald launcher, native GPUI host, typed `ProductSourceRelation`, checked workspace/view reconstruction, and CLI/MCP/desktop journeys. The desktop ownership test proves one GUI embeds the owner while another surface attaches and indexes through its endpoint. | The tested Product source slice is wired end to end; broader product domains remain outside this promotion scope. |
| K4 authority bridge | Rust, Python, TypeScript/JavaScript, Go, Java, C#, and Clang frontends feed one typed source contract; all seven native leaves expose the same bounded authority adapter and persistent protocol. | The complete seven-language local slice is executable. Promotion as the sole production authority for every existing provider remains a rollout gate. |
| K5 shared semantic arrangements | Arrangement, subscription, recursion, and provider adapter tests exist. | Open: complete semantic recipe coverage and result comparison are not yet promoted. |
| K6 execution planner | Remote/local admission, cancellation, fences, reuse, backpressure, and reservation tests exist. | Evidence present for the tested dispatcher paths; production planner cutover remains open. |
| K7 replication/memo | Framed transfer, sparse resume, corruption, stale basis, restart, and partial-header/payload timeout tests exist. Locald, the worker cancellation loop, and background worker transport share one incremental decoder with reusable receive storage. | Evidence present for tested protocol paths; full workspace replication promotion remains open. |
| K8 remote warm compute | One real process journey now composes three Product generations. The first worker run walks a multilevel authenticated relation and matches an independent local fixed-state projection; an exact retry causes no worker request. The adjacent root reuses retained sibling digests, sends fewer node proofs than cold plus one fixed input object, and is then reused without dispatch. A third generation receives a forged attempt, activates the reserved local fallback, sends exact cancellation, rejects the held late result, and reuses the local publication. The route learner preserves local and remote evidence in independent fixed slots, and the production scheduler lease covers transfer plus declared worker wall time. | The tested Product source slice uses remote work as an admitted acceleration tier while the local workspace remains authoritative. Broader recipes and production SLO/interference receipts remain rollout gates. |
| K9 vector/recursion | Recursive closure, SCC merge/split/delete, and exact fallback laws exist. | Open: complete vector and heavy-recursion product wiring is not claimed. |
| K10 native sessions | Typestate prevents concurrent request aliasing within one native session; a bounded `SessionKey` cache reuses one supervised process across serial and concurrent callers, validates every response, records cumulative output/RSS envelopes, and falls back cold after cancellation, protocol failure, or toolchain drift. All seven language leaves use this contract. | The shared persistent-session substrate and seven-language adapter matrix are present. Real-toolchain differential corpora, per-language crash injection, and production promotion remain rollout work. |
| K11 layout/specialization | Structural evidence records logarithmic path-copy work, sparse transfer resume, coalescing, hedge reservations, and loser cancellation. A 65,536-row one-key edit visits 6 nodes, copies 3, and reuses 134; sparse resume retains 256 bytes, resumes 768, and retransmits 0. | Evidence is green for the current representation. Hot/cold column layout and SIMD promotion still require K0 baselines, allocator evidence, and result-root equality. |
| K12 cutover/contraction | The seven required executable journeys, fault matrix, rollback-oriented recovery checks, and this evidence index exist. | Not complete: external old-writer fencing, production compatibility export/replay, rollback drill evidence, and final deletion gates remain open. |

From the workspace root, enter the pure verification shell through the
workspace-root flake (the `.config` flake remains its source of truth):

```console
nix develop ".#verification" --command nu --no-config-file .config/nu/cutover/tests.nu
nix develop ".#verification" --command nu --no-config-file .config/nu/cutover/e2e.nu
```

The original configuration-root entrypoint remains valid for the same two
commands when a caller is already anchored at the workspace root:

```console
nix develop "path:$PWD/.config#verification" --command nu --no-config-file .config/nu/cutover/tests.nu
nix develop "path:$PWD/.config#verification" --command nu --no-config-file .config/nu/cutover/e2e.nu
```

The workspace-root entrypoint is the one that admits the workspace-built
`backend-control` package during pure evaluation. It uses the Git source view,
so ignored Cargo targets do not enter the flake input. The control binary's
derivation is filtered further to the root Cargo files and its `control`,
`store`, and `version` dependency crates; changing a journey or document does
not trigger an unrelated release rebuild. The configuration-root entrypoint
supplies the pinned tools and control-plane artifact without reaching outside
its flake source, so pure evaluation remains valid even when the workspace is
a separate runtime checkout.

The controlled commands are:

```console
nu --no-config-file implementation/.config/nu/cutover/tests.nu
nu --no-config-file implementation/.config/nu/cutover/e2e.nu
cargo test --manifest-path implementation/Cargo.toml -p backend-laws --test cutover_regressions -- --nocapture
cargo test --manifest-path implementation/Cargo.toml -p backend-crash-tests -- --nocapture
cargo test --manifest-path implementation/Cargo.toml -p backend-concurrency-tests -- --nocapture
```

The first two commands validate control-plane custody and launch the real
process suite. The Rust targets provide the semantic, crash, and concurrency
evidence. They do not by themselves authorize promotion: K12 remains blocked
until the old writer is fenced, the compatibility export/replay drill passes,
and the deletion/rollback conditions in `migration.md` are recorded for the
scope being cut over.

## Verified implementation snapshot

On 2026-09-09, the current implementation snapshot passed both commands above
through `path:$PWD/.config#verification`, the complete
`cargo test --workspace --all-targets --offline` graph, strict workspace Clippy
with warnings denied, formatting, and the structural executable. The latter
also verified one coalesced follower, two hedge attempts, distinct local,
remote, and transfer reservations, and cancellation of the losing attempt.
Inside the pinned cutover gate, 23 integration journeys completed in 1.77s,
five real-process journeys in 22.57s, and seven independent law regressions in
7.96s, all within the execution-only 45-second envelope.
The checkout's `.git` file currently points at a missing external Downloads
worktree, so the run records its revision as `unknown`; the explicit Nix
workspace snapshot still makes the source and tool inputs concrete. These are
implementation checks, not substitutes for the external K12 promotion and
rollback receipts.

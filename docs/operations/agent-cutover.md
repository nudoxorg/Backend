# Versioned agent execution and regression-gated cutover

The backend migration should use the same idea as the backend itself: **work is a versioned object, evidence is a versioned relation, and integration advances one atomic head**. Agents should not exchange growing transcripts or repeatedly audit the repository. They should consume immutable contract and source roots and produce a candidate root; controlled evaluators produce the evidence that lets an independent verifier decide whether that candidate may advance the integration head.

This is the execution companion to [Architecture](../architecture/versioned-engine.md), [Data structures and layout](../architecture/layout.md), [Local and remote execution](../architecture/local-remote.md), [Semantic ledger and laws](../schemas/semantic-ledger.md), and [Structure and migration](migration.md). The source-backed setup reports are [local-agent-setup.md](../research/local-agent-setup.md), [danluu-testing-agents.md](../research/danluu-testing-agents.md), and [agent-cutover.md](../research/agent-cutover.md).

“Without regressions” means that every declared semantic, durability, compatibility, resource, security, and product-journey gate passes before authority moves. A finite campaign cannot prove the absence of every defect. The design therefore keeps rollback cheap, makes mismatches durable evidence, and continues comparison after promotion.

## 1. Use the control plane already in the repository

The checkout already has a more disciplined agent system than a generic fleet design. `.config/nix/control.nix` defines four roles and digest-addressed capabilities; `.config/nix/role-tools.nix` derives candidate identity from Git content; `.config/nu/agents` verifies and grades evidence; `.config/nu/quality` owns formatting, linting, tests, and measurements. The migration should extend this system rather than create another orchestration framework.

The live work-dispatch boundary is `backend-control` behind
`main cutover control`; it owns typed work rows, fenced execution leases,
expiring candidate leases, schema-separated evaluator/reviewer/Sol receipts,
and reusable completion state. Its CLI resolves the exact selected predecessor
at each transition and accepts an explicit receipt ID for stale-work detection;
accepted evaluation becomes `evaluated`, accepted review
becomes `reviewed`, and only an accepted Sol decision becomes `completed`.
The append-only `main cutover candidate|evaluation|receipt|decide` path owns the
separate Git promotion effect. It checks the candidate tree and ancestry,
persists a prepared decision, and calls Git's expected-old-value `update-ref`
before recording the committed effect. It cannot mint a live dispatch receipt.

| Repository role | Migration responsibility | Authority boundary |
|---|---|---|
| **Terra academic** | Turn K0–K12 into a dependency graph of invariant cells, prepare context packets, assign exclusive paths, repair integration-local issues, and decide the next useful experiment | May implement and orchestrate; may not self-certify semantic or performance claims or merge the train |
| **Luna pair** | Produce one narrow candidate against an opaque evaluator | One exclusive path set and the `test` capability; no contract changes, broad suite, lint policy, measurement, or merge |
| **Terra reviewer** | Blind first-pass review, execution of registrar-issued holdouts, semantic lint, affected validation, fault campaigns, and measurements | May not edit the candidate, author the candidate's hidden oracle, move the baseline, or merge it |
| **Sol integrator** | Own the integration head, compose independently accepted candidates, run public journeys and workspace closure, make promotion/rollback decisions | Short-lived exclusive merge custody; no speculative implementation or benchmark tuning |

The table describes actor roles. Store, flow, semantics, native languages, replication, product surfaces, and operations remain **ownership lanes**, not new agent roles. A Terra coordinator can issue a store cell or a Python-authority cell to Luna; a separate Terra reviewer verifies it; Sol integrates it. This preserves one enforceable vocabulary across the entire migration.

A non-builder holdout registrar creates the salted evaluator commitment and retains the hidden corpus. It can be a human/card authority or a small policy service; it is not a fifth coding-agent role. Terra reviewer runs the committed evaluator, and Sol may audit a controlled reveal. The registrar records version, expiry, access, backup, rotation, and compromise revocation so neither the builder nor reviewer can silently redefine the hidden test after seeing a candidate.

The role labels do not prove which model ran. The active `/Users/mileswirht/.codex/config.toml` currently selects `gpt-6-astra` for the main task and `gpt-5.6-luna` at medium effort for agents, while `/Users/mileswirht/.config/codex/config.toml` has a different main-model default. Every receipt must record the resolved config root, exact model ID, effort, role-contract digest, and tool bundle. It must never copy credentials or private shell/session contents.

## 2. Make an agent assignment a versioned execution cell

The unit of delegation is an invariant that can be falsified, not a crate or a broad theme. “Implement `flow`” is too large. “For any two consolidated batches at the same logical time, signed join output is independent of batch grouping and retracts unsupported rows” is a cell. Other cells include a canonical tree cut law, crash recovery at one publication boundary, one language-manifest invalidation rule, or one old/new product journey.

Every ready cell has a canonical descriptor:

```rust
struct CellDescriptor {
    milestone: MilestoneId,          // K0 .. K12
    parent_head: IntegrationHead,
    plan_root: PlanRoot,
    contract_roots: Box<[ContractRoot]>,
    source_roots: Box<[ScopedSourceRoot]>,
    prerequisite_evidence: Box<[EvidenceRoot]>,
    acceptance_root: AcceptanceRoot,
    allowed_paths: Box<[CanonicalPath]>,
    forbidden_capabilities: CapabilitySet,
}

struct CandidateReceipt {
    cell: CellId,
    candidate_tree: CandidateTreeRoot,
    parent_head: IntegrationHead,
    contract_roots: Box<[ContractRoot]>,
    toolchain_root: ToolchainRoot,
    role_contract: RoleContractDigest,
    model: ModelReceipt,
    controller_checks: Box<[ControllerRunReceipt]>,
}

struct EvaluationKey {
    cell: CellId,
    candidate_tree: CandidateTreeRoot,
    evaluator_root: EvaluatorRoot,
    holdout: HoldoutCommitment,
    environment: EnvironmentRoot,
}
```

`CellId` is the hash of `CellDescriptor`; it is not a field inside the bytes it identifies. The referenced `AcceptanceContract` owns the required public `TestContract` roots and aggregate resource budget, avoiding a second copy of their oracle/coverage/fault fields in the cell. `AgentWorkKey` additionally commits to the model/effort class and toolchain root. `IntegrationHead` and `CandidateTreeRoot` identify Git integration state and stay distinct from the product engine's `CommitId`, `StateRoot`, and `WorkspaceRoot`. Two coordinators asking for the same work coalesce onto one running cell. A completed candidate may be reused only when every production input matches. A source, public contract, acceptance contract, native toolchain, or parent-head change creates a new cell; it never relies on a prompt author remembering to say that context changed.

Evaluation identity is separate. Rotating a hidden seed, holdout corpus, evaluator implementation, or measurement environment creates a new `EvaluationKey` and re-evaluates the frozen candidate; it does not spend another implementation run. `CandidateReceipt` may reference only controller-minted public/opaque check receipts. Reviewer runs and measurements live in a reviewer-owned `EvaluationReceipt`, and the final `DecisionReceipt` cites both. The candidate may propose claims and artifact paths but cannot write admitted evidence or the decision ledger.

The schema binding should make invalid state transitions hard to express:

```rust
Cell<Draft> -> Cell<Ready> -> Cell<Running> -> Candidate<Frozen>
                                                   |
                                                   v
                                      Evaluation<Pending>
                                         |              |
                                         v              v
                               Evaluation<Failed> Evaluation<Passed>
                                                               |
                                                               v
                                                    Decision<Verified>
                                                               |
                                                               v
                                                        Merge<Merged>
```

Generated Rust bindings may use sealed state markers so a draft cannot run and an unverified candidate cannot enter Sol's merge queue. Generative lifetimes can bind `WriteLease<'lane>` and `Candidate<'run>` to one worktree/run; a receipt from a different lane cannot be attached accidentally. Capability witnesses such as `BlindHoldout<'review>`, `MeasureLease<'target>`, and `MergeLease<'train>` are validated against the immutable policy root.

These types are domain schemas over the existing Nix/Nu policy compiler and, after K1, the same v2 object/relation engine. They are not a second Rust orchestration service. `PlanRoot`, `ContractRoot`, `EvidenceRoot`, `EvaluatorRoot`, `PolicyRoot`, and run/decision roots are domain-separated typed uses of the common canonical object/state machinery, not separate stores or hash algorithms. The current role PATH and contract-digest checks provide policy and command custody; they do not prevent a process from invoking an absolute executable or editing a path outside its assignment. Actual isolation therefore needs an external boundary: make non-owned paths read-only in the worker sandbox or mount namespace where available, record pre/post hashes for every path outside the lease, reject a candidate with an out-of-scope change, and let only Sol update the integration ref with compare-and-swap (`expected old head -> accepted new head`). Protect the integration branch from direct agent pushes.

The holdout uses commit-and-controlled-reveal. K0 records a cryptographic commitment to the hidden corpus, generators, seeds, thresholds, and a secret high-entropy nonce; the nonce makes a small or guessable corpus commitment hiding. Luna sees the public acceptance root and acceptance dimensions but not hidden examples or cutoffs. After the candidate is frozen, the reviewer evaluates the committed holdout. The reviewer can reveal the committed material to Sol or an audit custodian to prove it did not move; it is never added to a later Luna context, and any broadly revealed corpus is rotated before it can serve as a holdout again. A changed holdout creates a new evaluation and invalidates decisions that depended on the old one, while leaving the frozen candidate available for reevaluation. This prevents both candidate overfitting and a reviewer moving the goalposts after seeing a result.

## 3. Reuse memory as facts and evidence, not conversation

Create an append-only evidence graph with these node families:

```text
Contract ──governs──> Cell ──produces──> Candidate
    │                   │                    │
    ├──covered-by──> Oracle                 ├──measured-by──> Run
    └──supersedes──> Contract               └──judged-by────> Decision

SourceRoot ──input-to──> Cell
ToolchainRoot ─────────> Run
FailureFixture ──derived-from──> Mismatch
Mismatch ──invalidates──> Evidence / Decision / Contract assumption
```

Each fact stores a bounded claim, provenance, the roots under which it is valid, and supersession links. Candidate-authored claims enter a proposed namespace. Only the controller, evaluator, reviewer, or Sol runner may admit the receipt for the capability it owns, and each writes outside candidate-writable storage. Its receipt binds authority and fence identity, command, complete bounded stdout/stderr artifact digests, configuration, inputs, environment, outcome, and elapsed resources. A `DecisionReceipt` references those immutable authority receipts; it does not trust a JSON file merely because the candidate hashed it. The context builder queries the transitive dependency slice for a cell and sends only those facts plus the delta since the receiving agent's last acknowledged `ContextRoot`. Large logs, code, and corpora are addressed by path and digest. They are not pasted into every prompt.

This creates four forms of work avoidance:

1. **Research reuse.** The original inventory, 53-to-28 package map, schema ledger, source verification, and prior subsystem reports are immutable inputs. A new audit is dispatched only when a changed source root intersects the prior audit's coverage relation.
2. **Build/test reuse.** Candidate identity includes tracked, staged, unstaged, and untracked content. Green receipts are reused only for the same candidate, immutable config, target triple, feature set, native tools, and evaluator root.
3. **Failure reuse.** Every mismatch becomes a minimized fixture linked to the law and source roots it falsified. The relevant generator and public regression corpus gain the case. Later cells inherit the fixture automatically.
4. **Agent-context reuse.** The next agent receives the governing facts and unresolved decisions, not previous agents' prose reasoning. Reviewer independence is preserved because the blind reviewer receives the contract, diff, code, and commands before any builder narrative.

The live evidence graph uses the v2 object store. Role runners may export canonical JSON plus BLAKE3 digests for audit and recovery, but those files cannot authorize a lifecycle transition. The selected relation retains controller, evaluator, reviewer, and Sol receipts as a domain-separated chain, and Sol's Git-effect journal commits to the accepted artifacts and integration compare-and-swap. The JSON form is an export oracle rather than a second mutable source of dispatch truth.

## 4. Local setup and worktree bootstrap

The product environment is Nix/direnv-controlled. Rust 1.97.1 is pinned for stable work, nightly is separate for Dylint/compiler tools, nextest is pinned, and native compiler versions come from the flake. The current non-direnv agent shell did not expose `backend` or `cargo-nextest`, so a receipt from ambient `cargo` is not equivalent to a repository-controlled run.

The tracked product root has no governing `AGENTS.md`. A Turso-specific `AGENTS.md` exists only in the external historical checkout and does not govern this workspace. Historical orchestration files under old Codex review snapshots are evidence to revalidate, not live policy. The generated `.config` role contract and the current task packet are the operative migration controls.

Every agent worktree begins with:

```text
direnv exec . backend doctor
direnv exec . backend catalog --json
direnv exec . backend agents verify
direnv exec . backend scope changed --json --base <train-head>
```

If direnv is unavailable inside an executor, use `nix shell 'git+file:///absolute/path/to/integration#luna-tools' --command cargo ...` and record that invocation. The Git flake reference evaluates committed tool configuration without copying unrelated untracked artifacts into the Nix store; Cargo still runs in and compiles the caller's current worktree. Use `path:.` only when the shell definition itself has an uncommitted change that must be tested. The shell shadows Cargo with the repository's pinned parallel wrapper. Final artifacts remain isolated in each worktree's `.local/target`, while Cargo 1.97 intermediate artifacts use one of four leased warm build directories. A fifth simultaneous build spills into an invocation-local directory instead of waiting on a Cargo build lock, then removes that overflow graph after Cargo exits so worktree count cannot make cache storage unbounded. One bounded `sccache` reuses dependencies compiled from stable source paths and repeated work within a lane; it does not currently normalize workspace Rust compilations across different roots. The wrapper divides Cargo's job budget across warm slots. `NUDOX_CARGO_BUILD_SLOTS`, `NUDOX_BUILD_CACHE_ROOT`, `CARGO_BUILD_JOBS`, and an explicit `CARGO_BUILD_BUILD_DIR` allow controlled overrides. Keep `.local/nextest`, native-output, and evidence roots isolated as before.

Before K0 implementation, perform a worktree census. The current repository reports 106 registered worktrees, of which 81 are prunable; 84 are branch-backed and 22 detached. The Downloads checkout is dirty and cannot serve as a clean baseline. Record every live worktree path, head, branch, dirty state, owner, target directory, and lease expiry. Run `git worktree prune --dry-run --verbose` first. Remove only proven stale registrations in an implementation operation with explicit custody; preserve all user changes and unknown branches. Then choose a clean base commit and record the dirty source checkout separately.

Use one worktree and exclusive path lease per candidate. The coordinator records a complete tree digest before and after the run and proves that every changed path belongs to the lease; OS-level read-only mounts or an equivalent sandbox enforce the boundary when available. Prototypes may share a contract tag, but candidates merge through a linear train. Agents never repeatedly rebase onto a moving contract. When a prerequisite changes, the scheduler marks affected cells stale through the evidence graph and regenerates their context deltas. Sol advances the integration ref only with a compare-and-swap against the receipt's exact `IntegrationHead`, so a candidate cannot overwrite intervening accepted work.

An `AgentWorkKey` lease has one writer owner, an owner epoch/fence, bounded read-only waiters, a heartbeat/expiry rule, and terminal states for frozen, cancelled, expired, and quarantined output. A replacement owner increments the epoch. Late output from an expired owner can be inspected but cannot satisfy the current cell. Coalescing shares immutable results and status; it never grants a second coordinator write custody.

## 5. Redesign `.config` as a versioned policy compiler

The initial role/capability policy left 17 of the 53 historical product crates unmatched while `allow_empty_scope = false`. The replacement policy now derives ownership from every Cargo package under `crates/`, `frontends/`, `extensions/`, `apps/`, `tests/`, and `tools/`, plus explicit shared roots for configuration, agent metadata, and architecture evidence. The exactly-one fixture checks that relation against the live Cargo graph and Git inventory.

Before broad migration, split policy by authority while keeping one generated, content-addressed control artifact:

```text
.config/
  contracts/agent/
    cell.schema.json
    evidence.schema.json
    decision.schema.json
    workload.schema.json
  nix/
    policy/
      roles.nix
      commands.nix
      tests.nix
      measurements.nix
      telemetry.nix
    artifacts.nix
    shells.nix
    toolchains.nix
  nu/
    agents/       # issue, verify, grade, freeze, supersede
    cutover/      # shadow, compare, canary, promote, rollback
    quality/      # format, lint, test
    scope/        # Git path -> package/contract/journey cells
  fixtures/
    control-plane/
    cutover/
```

This is a responsibility split, not extra runtime configuration. Nix compiles it into one immutable `PolicyRoot`; role bundles refer to the digest. The scope table covers the six live package roots and the `.config` contract/fixture paths before work is dispatched. A mechanical fixture requires every Cargo package and governed Git path to match exactly one intended scope, with an explicit shared-scope declaration where cross-cutting config is deliberate. The 53 historical product crates and the new versioned control plane map to 28 product packages. Changed-package ownership alone is insufficient: a contract change expands to its laws, journeys, and every receipt it can invalidate.

Define canonical encodings once for cell, evaluation, run, evidence, decision, supersession, lease, and cursor records, then generate Nu, Rust, and JSON-schema bindings from that source. K0 must prove history-independent IDs, domain separation, unknown-field/version rejection, authority/fence checks, and lossless import/export between custody-separated bootstrap JSON and the v2 store. Retain the bootstrap export until its compatibility deadline has passed.

Add declared measurements for the v2 claims before accepting an optimization:

- canonical map update/diff: edit size, equal subtrees skipped, hashed/copied bytes, cut ripple;
- trace update/compaction: changed rows, support rows, retained batches/bytes, frontier delay;
- end-to-end edit: native authority time, downstream propagation, time to coherent `ViewRoot`;
- warm restart: objects read, packs mapped, work replayed, resident and peak bytes;
- remote hit/miss/hedge: input and output bytes, queue delay, duplicate compute, cancellation delay;
- crash/replay: recovery time, repeated effect attempts, old-or-new root result;
- seven native sessions: cold/session parity, RSS, restart count, conservative fallback rate;
- product journeys: desktop, CLI, MCP, publication, offline edit/reconnect, old snapshot open.

The quick `backend test changed` selects library tests. Keep it as Luna's fast opaque loop, but do not treat it as candidate closure. Terra runs affected package targets, including integration and binaries; the reviewer runs semantic/fault/measurement gates; Sol runs one workspace closure at checkpoint boundaries. Add a journey-to-source relation so a change to shared canonical types selects product and compatibility journeys even if Cargo dependency selection misses a runtime/config edge.

## 6. Testing contracts derived from Dan Luu's findings

Dan Luu's September 2026 [agent testing experiments](https://danluu.com/agentic-testing/) found that asking agents to use TDD, fuzzing, property testing, formal methods, or another named method often produced superficial technique use. His July 2026 [agentic coding notes](https://danluu.com/ai-coding/) argue that testing needs an externally designed process and that review cannot absorb unconstrained volumes of generated code. [The benchmarkpocalypse](https://danluu.com/benchpocalypse/) shows visible benchmark optimization producing reward-hacked wins that disappear under broader checks. His language/token study warns that repeated access to visible tests can produce brittle solutions ([Programming language, token efficiency, and correctness](https://danluu.com/pl-tokens/)); [Bug blindness](https://danluu.com/bug-blind/) explains why expert dogfooding can normalize defects and workarounds. These are Dan's reported experiments and arguments. The following rules are our application to this backend.

Never issue “add tests,” “use fuzzing,” or “do TDD” as a cell. A `TestContract` must state:

```rust
struct TestContract {
    law: LawId,
    input_grammar: GeneratorSpec,
    bug_ingredients: Box<[FailureDimension]>,
    oracle: OracleSpec,
    independence: IndependenceClass,
    reducer: ShrinkerSpec,
    coverage: CoverageContract,
    fault_sites: Box<[FaultSite]>,
    budget: ResourceBudget,
    retained_regression: RegressionPolicy,
}
```

The generator must create valid, semantically interesting edit histories: deletes and re-adds, key-preserving value changes, key changes, simultaneous retractions/additions, SCC merge/split, negative dependency changes, authority partial/unavailable states, cursor gaps, cancellation, crash boundaries, and remote stale attempts. Random invalid bytes that only prove “does not panic” do not satisfy a semantic law.

Use at least three oracle families and do not let one implementation silently define correctness:

1. **Executable mathematical model.** A deliberately small full-recompute model defines map, batch, join, frontier, support, and transition laws. It shares no optimized helper code with the candidate.
2. **Legacy/native behavior.** The old backend and native compilers supply compatibility evidence. Each observed behavior is classified as required, intentionally changed, or incidental; old defects do not become the new specification by accident.
3. **Metamorphic and independent standards.** Equivalent edit groupings, restart/replay, map build order, full-versus-delta execution, cold-versus-session authority, and old/new readers must converge. Standards or native toolchains remain authoritative where appropriate.

An implementation agent cannot write the only oracle for its candidate. Differential implementations must be genuinely independent: duplicating the same algorithm, parser, canonical encoder, or helper crate can reproduce the same bug. The reviewer executes registrar-owned hidden holdouts and mutation/falsification checks. For every core law, deliberately inject representative defects—drop a retraction, reuse a stale facet, advance a frontier early, accept the wrong authority, duplicate an effect, skip a selected target, forge a receipt, or edit outside the lease—and prove the evaluator turns red.

Benchmarks use development, public-regression, and committed hidden-holdout corpora. Report distributions and workload classes, not one aggregate score. A gain on a one-key value edit cannot pay for worse broad-edit p99, compaction debt, retained memory, remote egress, or lower semantic coverage. Inspect extreme wins as potential omitted work. Every performance run compares result roots and coverage before comparing time.

Fresh-user product journeys are separate from expert dogfooding. Give a verifier only the public behavior and a clean workspace; record confusing recovery, stale UI, and required workarounds even if an experienced developer can proceed. Escaped production bugs become fixtures, generator dimensions, and evidence-graph invalidations rather than isolated patches.

## 7. Regression matrix and promotion evidence

Each cell declares which matrix rows it can affect. A candidate cannot be green while an affected row is `unknown`.

| Dimension | Required evidence before promotion |
|---|---|
| Canonical identity | Golden and generated encoding/key-order/hash-domain tests; history-independent roots; cross-version mapping |
| Delta semantics | Full-recompute equality; grouping/order/retraction/support laws; no-op output suppression |
| Coverage and authority | Complete/partial/unavailable distinctions; every field facet assigned; native manifest/toolchain identity |
| Durability | Fault injection around encode/flush/journal/pack/head; restart yields the old or new coherent root |
| Compatibility | Old reader/new writer and new reader/old writer matrices where promised; old snapshot open/export |
| Concurrency | Loom/model schedules, stale attempt fencing, cancellation, lease/pin safety, frontier progress |
| Memory and storage | Peak and retained RSS, pin census, amplification, compaction debt, GC reachability |
| Latency and work | p50/p95/p99 plus changed rows/nodes, copied/hashed bytes, native work, queue delay |
| Local/remote trust | Offline local journey, manifest/recipe/root validation, corrupt/stale worker rejection, bounded hedge waste |
| Product behavior | Desktop/CLI/MCP/publication journeys, stable row identity, freshness/partial-state UI, side-effect idempotency |
| Operations | Observability cardinality/privacy, rollout cohort stability, alert/revert drill, backup/recovery |

No retry converts a failure into green. The default nextest policy already uses zero retries and treats flaky results as failures. A flake is its own defect with seed, schedule, resource group, run root, and owner. Typed skips require a registered missing prerequisite and leave the affected gate unresolved.

## 8. Delegation and merge plan for K0–K12

Use at most 15 concurrent agents because that is the configured ceiling, not a target. Begin with four implementation cells and two reviewers. Increase concurrency only when path leases, build targets, native resource groups, and reviewer capacity remain disjoint. The existing resource groups already serialize allocator, display, service, process, and telemetry global tests and cap native compiler/concurrency work. The scheduler chooses cells by critical-path reduction and expected information gain per cost; a speculative lane that would wait on an unstable contract should instead falsify that contract or improve its oracle.

| Slice | Terra prepares | Luna candidate cells | Terra reviewer evidence | Sol checkpoint |
|---|---|---|---|---|
| **Bootstrap** | Worktree census, clean base, policy/config root, lane leases | Small control-plane schema/tool fixtures | Reject mixed config roots, shared targets, stale scopes | Select and freeze base; no product merge |
| **K0 contracts** | Field coverage ledger, five boundary schemas, reference models, workloads; request registrar commitment | Mechanical schema expansion and isolated fixtures | Evaluate committed holdout; evaluator mutation campaign | Merge contracts/oracles before engines |
| **K1 version/store** | Split canonical map, pack, journal, pin/GC cells | One law or crash boundary per cell | Random history, cut-ripple, replay, corruption, compatibility | Integrate behind old-store adapter |
| **K2 flow** | Batch/trace/frontier/operator dependency DAG | Consolidation, join, arrangement, frontier cells | Independent full model; SCC/support/retraction cases | Integrate without product authority |
| **K3 vertical local** | One docs/name/outline journey through local service | Narrow engine/CLI/MCP/desktop adapters | Fresh-user and restart journey; stable view rows | First end-to-end shadow slice |
| **K4 authority bridge** | Shared manifest/result contracts, then seven independent language matrices | One Luna per disjoint language leaf or manifest invariant | Cold/subprocess parity, partial/error states, native corpus | Merge each leaf separately; shared contract once |
| **K5 arrangements** | Exact/name/lexical, graph, docs/source, statistics, vector consumers | One projection law per cell | Full provider differential, top-k ties, deletion and global-stat cases | Promote only complete subscribed facets |
| **K6 execution** | Work interning, admission, reservations, cancellation/fence cells | Planner operators and accounting | Stale attempt, no-op, overload, resource conservation | Enable placement locally first |
| **K7 replication** | Negotiation, object transfer, receipts, branch/reconcile cells | Pure transport/state-machine pieces | Loss/duplication/reorder/corruption/revocation campaign | Immutable object/memo transfer only |
| **K8 remote** | Warm trace, prefetch and pure-hedge policy cells | Remote adapters and bounded queues | Local latency, stale output, waste and outage holdouts | Canary remote acceleration; locally promised operations remain sufficient |
| **K9 vector/recursion** | Exact fallback, ANN overlay and SCC plan contracts | Disjoint index/recursive operators | Recall-bound + exact oracle; SCC merge/split/delete | Keep approximation explicit in recipe |
| **K10 sessions** | Per-language persistent process protocol | One language session at a time | Cold/session equality, leak/crash/cancel/toolchain drift | Retain subprocess fallback per language |
| **K11 layout** | Freeze semantics, select measured bottlenecks | Column COW, dictionary, SIMD, compaction cells | Hidden workload distributions and result-root equality | Merge physical wins independently |
| **K12 cutover** | Compatibility inventory, canary plan, rollback runbook, deletion map | Only bounded bridge/removal candidates | Full matrix, fresh journeys, soak, rollback | Move atomic head, then delete after retention gates |

Luna agents are most valuable in the numerous narrow leaf cells: schema enumeration, operator implementations, per-language adapters, fault fixtures, bridges, and measured physical kernels. Terra keeps the architecture coherent and avoids asking each Luna to rediscover it. Terra reviewer supplies an independent context and attacks each claim. Sol remains scarce: it should see compact accepted receipts at train checkpoints instead of reviewing every exploratory branch.

## 9. Cut over authority without a dual-writer trap

Cutover advances by coherent scope: workspace, language authority, query recipe, or worker pool. The phases are:

1. **Old writer, shadow new execution.** One old canonical head remains authoritative. Feed the same immutable inputs and deltas into the new path. Write new results under `(candidate_schema, input_root, recipe, run_id)` where product readers cannot discover them. Compare only at closed frontiers.
2. **Old authority, dual read.** A bridge reads both representations. Product behavior uses the old result. Mismatches return the old result, persist both roots and the minimized input, and freeze promotion for that scope.
3. **Deterministic derived-read canary.** Select cohorts from stable workspace/operation identity so retries do not change paths. Start with queries and derived views whose new result can be discarded. Continuously compare a sampled shadow against the old path.
4. **Atomic writer transition.** Pause admission for one workspace, drain or fence old attempts, validate the new root and compatibility export, atomically move the selected-head pointer, then resume. All user mutations first enter a stable logical-intent journal versioned independently of either storage layout. Before promotion, its executable compatibility matrix must classify every intent as losslessly understood by old and new writers, explicitly one-way with data-head rollback disabled for that scope, or rejected before acknowledgment. Never let old and new writers race on the same canonical head.
5. **New authority with live fallback.** Keep old readers, export bridge, artifacts, and rollback pointer for the declared retention window. Continue sampled differential execution and fresh-user journeys.
6. **Legacy deletion.** Remove old writers, wrappers, providers, and lifecycle systems only after every deletion gate is recorded and rollback from the last pre-deletion release succeeds.

For local-first behavior, remote compute never owns the user's sole durable state. A remote result is admitted only when its input root, authority manifest, recipe version, coverage, output digest, and attempt fence match the local request. During outage or uncertainty, reserved local execution continues for operations whose contract promises local capability; explicitly remote-only capabilities return a typed availability state. Remote shadow and hedges have explicit CPU, memory, egress, and cancellation budgets; they shed before interactive local work.

Rollback has two distinct forms. A **code/read rollback** returns execution and reads to the old implementation while retaining or translating the current authoritative data head. A **data-head rollback** selects a prior root and is safe only when no acknowledged mutation would be lost; otherwise the intent journal must export and forward-replay every acknowledged mutation into an old-compatible root before authority moves. K0 sets numeric recovery-point and recovery-time objectives per product journey. Promotion requires demonstrated zero loss of acknowledged locally durable intents for the promised operations and a measured recovery time within that declared objective.

Rollback stops new admissions, fences/cancels candidate attempts, preserves candidate objects for diagnosis, and uses pins and reader leases to prevent premature GC. External effects use durable intent and receipt objects with stable idempotency keys, but a local receipt alone cannot guarantee exactly-once delivery: the destination must enforce the idempotency key, or reconciliation must detect and repair the ambiguous “effect happened, receipt was not durable” state. A sink without either property remains outside automatic replay and requires a compensating-effect protocol. An irreversible effect has its own narrow canary and operator-owned compensation rule; moving a workspace head cannot roll it back.

## 10. Prove the execution system with one bounded pilot

The bounded pilot used the repeated semantic-image planning work from the external historical checkout. That path measured a full image during compilation, measured it again during publication, and reconstructed the full canonical plan during encoding. The pilot established the ownership and lifetime contract for retaining one prepared view of immutable input; the production engine generalizes that lesson through versioned roots, interned work keys, and exact deltas.

Terra should specify ownership and lifetime laws, the exact current-output oracle, and memory/latency measurements. Luna implements only the prepared-plan candidate and its public parity cases. Terra reviewer checks that the length and bytes match the old independent path, exercises invalid lifetime/identity cases, measures retained memory as well as CPU/allocation reduction, and attempts a mutation where an old plan is paired with a different image. Sol integrates only if the candidate is a local simplification with a clear owner and no new cross-crate lifecycle.

This pilot does not substitute for the delta architecture. It validates the cell schemas, opaque evaluator, evidence graph, worktree isolation, measurement custody, and four-role handoff on a real repeated-work problem. If that workflow cannot produce a compact, independently verified candidate here, it should be repaired before multiplying K1–K12 lanes.

## 11. Final deletion gates and ongoing feedback

A legacy mechanism can be deleted only when all applicable facts are `pass` for the declared scope:

1. Every row in the 53-to-24 ownership map has a final owner, converted callers, and an identified deletion commit.
2. Canonical schemas, hash domains, tree cuts, recipes, coverage, wire envelopes, and compatibility mappings are versioned and mechanically complete.
3. Shadow and dual-read campaigns cover no-op, edit, deletion, broad fan-out, partial/unavailable authority, restart, offline/reconnect, and concurrent-client cases with zero unexplained mismatches.
4. The new path is the single selected writer. Durable intents use the layout-independent logical journal and have a tested export/replay and reader bridge until the compatibility deadline; rollback meets the declared RPO/RTO without losing acknowledged writes.
5. Fault injection at every prepare, encode, flush, journal, pack, head, notification, transfer, native-process, and external-effect boundary recovers to an old or new coherent root, and ambiguous external effects are reconciled against a sink-side idempotency record or compensating protocol.
6. All seven language matrices pass cold/subprocess behavior; promoted sessions also pass cold/session equality, cancellation, crash, leak, and toolchain-drift gates.
7. Local product journeys satisfy their contract without the network. Remote results fail closed under stale, corrupt, revoked, incomplete, and late attempts.
8. Performance distributions meet K0 workload-specific budgets without worse coverage, retained memory, write amplification, compaction debt, remote cost, or p99 responsiveness.
9. GC proves that no live root, branch, reader, lease, published view, evidence receipt, or compatibility consumer needs the old representation.
10. A rollback drill from the last pre-deletion release succeeds, and the deletion stays reversible through its declared retention deadline.

After promotion, use fractional rollout and external feedback as continuing evaluators. Compare sampled roots and work counters, watch latency/memory/queue/freshness distributions, and route support reports and observed workarounds into the evidence graph. Each escaped defect produces a public regression, a generator dimension when generalizable, an invalidation of any evidence that assumed the broken law, and a new cell on the smallest responsible boundary. Silence from expert users is not proof of quality.

The end state is one execution discipline from research through production: immutable inputs, exact deltas, scoped work, independent evaluation, atomic head movement, and retained evidence. That lets many Luna agents work in parallel without multiplying interpretation, permits Terra to reuse deep memory instead of repeating audits, and gives Sol a small, decisive integration surface.

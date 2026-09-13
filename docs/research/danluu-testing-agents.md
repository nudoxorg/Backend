# Dan Luu on testing, agents, reliability, review, and benchmarks

**Research date:** 2026-09-08. **Scope:** Dan Luu's own public pages and the repository evidence in `/Users/mileswirht/Downloads/backend`. This is architecture research; no production source was edited and no build was run.

## Source ledger

Dates below come from Dan Luu's Hugo RSS feed (`https://danluu.com/atom.xml`), which is the site's primary publication index. The URL and title are retained verbatim. “Author claim” is what Dan reports or argues; “application” is our design inference and should not be attributed to him.

| Date | Exact title | Primary URL | Use |
|---|---|---|---|
| 2026-09-01 (feed build date; page discusses “September 2026”) | *How well do agents use test/verification techniques?* | [danluu.com/agentic-testing](https://danluu.com/agentic-testing/) | Agent testing experiments |
| 2026-07-03 | *Agentic test processes, LLM benchmarks, and other notes on agentic coding from Galapagos Island* | [danluu.com/ai-coding](https://danluu.com/ai-coding/) | Agent behavior, review, testing, benchmark variance |
| 2026-07-23 | *Bad benchmarks and evals: Senior SWE-Bench, napkin math, and winter tires* | [danluu.com/exercise-7](https://danluu.com/exercise-7/) | Benchmark validity and task selection |
| 2026-08-09 | *How does programming language affect token efficiency and correctness?* | [danluu.com/pl-tokens](https://danluu.com/pl-tokens/) | Visible-test brittleness and task/language variance |
| 2026-08-17 | *The benchmarkpocalypse* | [danluu.com/benchpocalypse](https://danluu.com/benchpocalypse/) | Holdouts and reward hacking |
| 2026-08-21 | *There's no reason for software to be slow anymore* | [danluu.com/perf-opt](https://danluu.com/perf-opt/) | Agent performance work and experimental design |
| 2026-08-30 | *Bug blindness* | [danluu.com/bug-blind](https://danluu.com/bug-blind/) | Workarounds, expert dogfooding, and outside feedback |
| 2021-08-27 | *Measurement, benchmarking, and data analysis are underrated* | [danluu.com/why-benchmark](https://danluu.com/why-benchmark/) | Measurement as engineering work |
| 2016-03-01 (feed date) | *How often is the build broken?* | [danluu.com/broken-builds](https://danluu.com/broken-builds/) | Clean-build reliability and sampling caveats |
| 2015-05-17 (feed date) | *Given that we spend little effort on testing, how should we test software?* | [danluu.com/testing](https://danluu.com/testing/) | Randomized testing, fuzzing, coverage |

The current page HTML does not print a publication date for all older pages; the feed dates are therefore the auditable source of dates. The first item is a special case: its feed's current build timestamp is 2026-09-01, while the article itself says “until now (September 2026).” I do not infer a more precise day.

## What the author actually says

### Testing must be designed, not requested by name

In *How well do agents use test/verification techniques?*, Dan compares many explicit conditions (TDD, fuzzing, property-based testing, formal methods, and other named techniques) over repeated Zstd implementation runs. His reported observation is that simply naming a technique often produces weak tests; agents did better when the testing process had concrete structure and an effective oracle. He also cautions that the failures are idiosyncratic and that one eval is not enough to make broad language or agent claims.

In *Agentic test processes…*, he reports that default LLM-generated tests are commonly judged poor by people who care about quality. He describes randomized/fuzz testing as a high-leverage way to spend limited testing effort, and describes using agents to create test infrastructure while supplying the testing judgment and evaluation setup. In the older *Given that we spend little effort on testing…*, he argues that random generation, coverage-guided search, and regression retention can find more bugs per unit effort than hand-written tests in suitable domains.

**Application to v2:** every agent task needs an explicit oracle, generated-input strategy, negative cases, and a retained regression artifact. “Add tests” is not an acceptable task contract. A test-writing agent must be paired with an independent adversarial verifier because the implementation and a generated example can encode the same wrong behavior.

### Review cannot absorb unlimited generated output

In *Agentic test processes…*, Dan describes a high-quality environment where code review was not the primary correctness mechanism and says agents can produce more code than people can reasonably review. He also describes a traditional workflow in which human review remains useful for disagreements about intended behavior and for code quality. These are observations from his settings, not a universal prescription to remove review.

**Application to v2:** use risk-triggered review. A routine operator implementation can pass typed invariants, differential tests, and an independent verifier without line-by-line human review. A human or designated semantic owner must review changes to stable key definitions, authority boundaries, retractions, frontiers, persistence formats, publication side effects, or rollback behavior. Review volume is controlled by narrow contracts and evidence, not by merging a fleet of autonomous branches.

### Benchmarks are easy to overfit and hard to generalize

In *Bad benchmarks and evals…*, Dan questions benchmark task representativeness, small sample sizes, and ranking stability. In *The benchmarkpocalypse*, he gives reward-hacking examples where optimization against a visible workload harms unseen workloads, and explains why a holdout changes the incentive. In *Agentic test processes…*, he emphasizes variance across tasks, conditions, and setup. In *There's no reason for software to be slow anymore*, he reports that agents can do specialized optimization once a human establishes a sound measurement framework, but are weak at experimental design without that framework.

**Application to v2:** benchmark agents receive a hidden, independently maintained holdout. The implementation agent never sees the holdout or its acceptance thresholds. Report correctness and resource metrics separately from speed; keep raw traces, workload versions, and failed runs. A single aggregate “incremental speedup” score cannot certify delta semantics.

### Reliability is a process property

In *How often is the build broken?*, Dan uses CI data while warning that projects are not necessarily comparable and that a main branch's purpose changes the meaning of a broken build. Across the testing and measurement articles he treats regression retention, clean builds, measurement, and post-failure analysis as core engineering work.

**Application to v2:** the old full rebuild is the oracle during migration. Every mismatch is retained with the exact revision, edit batch, work key, frontier, and serialized outputs. A red branch pauses downstream cutover; it does not get hidden behind a retry or a cache hit.

### Visible tests and expert dogfooding are incomplete feedback

In *How does programming language affect token efficiency and correctness?*, Dan reports high hidden-test failure rates and idiosyncratic task/language effects after agents repeatedly worked against visible tests. In *Bug blindness*, he argues that experienced users often internalize workarounds, so ordinary dogfooding can miss severe usability defects.

**Application to v2:** freeze candidates before hidden evaluation, keep model/language results disaggregated, and include clean-workspace journeys executed by reviewers who have not learned the backend's workarounds. Post-cutover support reports and observed workarounds feed the retained regression and generator corpus.

## Concrete agent delegation and cutover strategy

The ambition is a delta-native backend, but delegation is bounded by authority and evidence. Agents work in isolated worktrees and communicate through versioned artifacts, not informal claims. The coordinator owns the dependency DAG, frontier advancement, and integration queue.

### Agent roles and contracts

1. **Planner/spec agent:** writes the relation schema, stable key rules, dependency facets, side-effect fences, and old-vs-new semantic equivalence properties. It cannot modify implementation.
2. **Key/algebra agent:** implements or prototypes `ObjectKey`, weighted batches, consolidation, antichain frontiers, retractions, and canonical remapping. Deliverables include algebra properties and a small reference model.
3. **Capture agent:** adapts `compiler/ir/vcs.rs` and `compiler/ir/reader.rs` into object-map changes with explicit before/after revisions, additions, deletions, and unknown/native-authority markers.
4. **Compiler authority agent:** wraps `compile_semantic` and language lowerers. It may scope invalidation and reuse immutable artifacts; it may not invent semantic deltas where the native compiler is authoritative and non-incremental.
5. **SCC/dependency agent:** maintains reverse dependencies, negative dependencies, recursive SCC invalidation, and demand scheduling. It must prove no stale result survives a frontier.
6. **Consumer agents:** independently adapt publication, docs, search, graph, vector, embedding, and UI projections to weighted relation changes. Each owns only its projection and schema version.
7. **Adversarial test agent:** gets the public contract and reference model, not implementation internals. It generates random edit batches, reorderings, cycles, failures, stale reads, and malformed/partial authority results.
8. **Benchmark agent:** owns hidden corpora and workload generation. It reports cold/warm, source-authority, downstream propagation, persistence, memory, and queue metrics. It cannot certify its own optimization.
9. **Contrarian reviewer:** checks evidence manifests, key stability, invalidation completeness, side effects, and rollback. It must attempt to falsify the claim before integration.
10. **Integration/cutover agent:** runs shadow comparison and canaries, records generation/frontier state, and performs rollback. It cannot delete the old path until the soak criteria pass.

Every task returns: changed artifact or patch; schema/key version; dependency facets; commands and raw outputs; differential cases; known gaps; and a machine-readable evidence manifest. A verifier is selected from a different context and does not inherit the implementer's conclusions.

### Dependency DAG and cutover gates

The work order is:

`stable keys + reference algebra → VCS/object change capture → immutable store and frontier manager → compiler authority adapter → SCC/reverse-dependency maintenance → projections (publication/docs/search/graph/vector/UI) → remote memoization → shadow mode → language/package canaries → default path → old-path retirement`.

At each edge, the coordinator requires:

- **Contract gate:** typed API, schema version, ownership of invariants, and negative-dependency behavior are documented.
- **Model gate:** reference full recomputation agrees with the delta model on generated edits and arbitrary batch order.
- **Shadow gate:** old full rebuild and delta execution run for the same revision; serialized semantic outputs and all projections compare by stable key, with an explicit explanation for intentionally nondeterministic fields.
- **Failure gate:** kill/restart at capture, authority, consolidation, SCC, persistence, and publication boundaries; recover from the last durable frontier without duplicated side effects.
- **Resource gate:** bounded RSS, retained object count, queue depth, p95/p99 latency, and compaction work are within declared budgets.

Canary one language and one package at a time. Keep old outputs authoritative while delta output is compared. Then use a percentage of read traffic, followed by write/ publication canaries. Rollback is a pointer to the previous object-store generation/frontier, not a destructive cache flush. Retain both representations through a soak period and only then remove duplicate computation.

### Tests that fit delta execution

The adversarial suite should include:

- Conservation: applying a batch to revision `r` and consolidating yields exactly the same keyed object map as full recomputation at `r+1`.
- Group laws: empty batch is identity; applying batches in equivalent grouped order is equal; duplicate updates consolidate; replay of a durable batch is idempotent.
- Retractions: delete/re-add, changed source spans, changed visibility/docs/occurrence facets, and removed dependencies produce no stale positive or negative rows.
- SCCs: cycles, SCC merges/splits, recursive type changes, and dependency deletion invalidate precisely the affected component.
- Canonicalization: local IDs may change, but stable semantic keys and canonical ordering remap outputs deterministically; consumers never key by transient arena index.
- Authority boundaries: a native language compiler rerun is treated as one opaque replacement relation; downstream deltas may reuse unaffected projections but cannot assume an internal delta.
- Faults: crash after object write before frontier advance, after frontier advance before projection, and after an external publication side effect. Side effects require an idempotency key and a durable fence.

Fuzzers should generate source edits and relation batches independently. Differential tests must compare the delta engine with a deliberately simple full reference implementation. A test generated by the implementer is evidence of a case, not evidence that the oracle is correct.

### Benchmark design

Maintain three disjoint datasets: development, public regression, and hidden holdout. Holdout edits and acceptance thresholds are unavailable to implementation agents. Include small and large crates, all supported languages, generated code, documentation-only changes, visibility-only changes, occurrence/embedding changes, deletions, broad dependency fanout, and SCC-heavy graphs.

Measure separately:

- source capture and hash/diff cost;
- native authority reruns versus downstream delta propagation;
- cold start, warm restart, and remote memo hit/miss;
- p50/p95/p99 wall time and queue wait;
- allocations, peak and retained RSS, object count, batch size, compaction/reclamation time;
- mismatch rate, stale-output rate, replay/duplicate-side-effect rate, and failure recovery time.

The benchmark report must include workload identity, revision pair, edit size and fanout, frontier, cache/memo facets, raw trace, and confidence interval. Never accept a speedup measured only on the visible workload. Keep a baseline full rebuild in every benchmark run so “faster” cannot silently mean “did less semantic work.”

## Backend-specific authority and migration map

The current code gives a precise boundary for this design:

| Area | Current evidence | v2 disposition |
|---|---|---|
| Semantic request/lowering | `compiler/driver/types/compile.rs:49-94`; `compiler/driver/lower.rs:1824-1914,4054-4190` | Keep native lowering authoritative; expose immutable output facets and explicit invalidation keys. Delete repetitive orchestration only after shadow equivalence. |
| VCS/change capture | `compiler/ir/vcs.rs:82-149,262-370` | Convert stable diff information into object-map batches; distinguish changed source, removed object, and unknown/native authority. |
| Reader contracts | `compiler/ir/reader.rs:102-223` | Use typed reader identity and revision as capture inputs; do not use local IDs as cross-revision keys. |
| Publication/reopen | `compiler/publication/publication/publish.rs:100-295`; `compiler/publication/publication/open.rs:137-283` | Projection and side-effect consumers of deltas; durable fence before external publication and replay-safe idempotency keys. |
| Semantic image canonicalization | `compiler/ir/semantic_image/canonical.rs:78-162`; `compiler/ir/semantic_image/full/model.rs:198-234` | Canonical stable ordering/remap is a materialization boundary. Persist the plan or keyed relation; do not compare arena positions. |
| Payload hash | `compiler/driver/README.md:25-37`; `compiler/ir/semantic.rs:2326-2373` | Prohibited as a docs/visibility/occurrence/embedding reuse key until dependency facets are explicit. Use a complete `WorkKey` with schema, revision, authority, language, options, and facet identity. |
| Runtime ownership | `compiler/application/runtime.rs:374-503,609-892` | One owner-local bounded queue and explicit generation/frontier state. Avoid eager all-futures scheduling and an unbounded global cache. |

The already observed full-image path is a high-value first target: `compiler/application/compiler.rs:304` calls `full_semantic_image_len`; `compiler/publication/publish.rs:126` measures again; `:175` calls `encode_full_semantic_image`, which rebuilds `FullSemanticImagePlan`. The plan construction is repeated in `compiler/ir/semantic_image/full_wire/encode.rs:18-31` and `compiler/ir/semantic_image/full_wire/plan.rs:74-76`. A `PreparedSemanticImagePlan<'ir>` retaining exact length, canonical remap, and encoder view should be created once per immutable semantic image and passed through measurement and encoding. This is a conventional retained plan optimization; it is separate from the larger delta architecture. Benchmark plan storage against current transient rebuilds before committing to retention duration.

## Cutover policy and success criteria

An agent can merge an operator after independent verification shows semantic equivalence on the reference model, no stale rows under generated deletions/retractions/SCC edits, clean builds, and bounded memory. A human semantic owner signs changes to key algebra, authority, persistence, frontier advancement, or external effects. A package/language canary advances only after zero unexplained shadow mismatches over a representative soak and a recovered-failure run. Any mismatch freezes advancement, preserves the evidence bundle, and reverts the read pointer to the last known-good generation.

This strategy applies Dan's evidence conservatively: his pages motivate explicit oracles, independent testing, holdouts, measurement, and review proportional to risk. They do not establish that all agents, languages, or review processes behave identically in this repository. The backend's own shadow results remain the authority.

## Gaps and limits

I did not run builds, benchmarks, fuzzers, or source modifications. The reports cited above are Dan Luu's primary pages; linked social posts were not used as evidence. The RSS feed provides publication dates but may represent a feed/build date for the newest article, so that item is labeled accordingly. The repository inspection does not yet quantify every language lowerer's internal allocation or every publication consumer's dependency facets; those require instrumentation and a source-wide call graph during implementation planning.

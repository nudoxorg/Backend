# Resolution research: treesitter / consumer codebases → IR

**Parent user request (Claude session `17cc2883-6b32-44ea-abc5-c1c9f07d0bf2`):** research how to convert the treesitter representation of an arbitrary codebase into a resolved state against the documentation IR (example usage, instances, cross-package refs), across all languages, and produce an exhaustive plan.

Agents hit Anthropic session/rate limits mid-flight. This directory holds **recovered, verified, and completed** deliverables.

| Doc | Status | Description |
|---|---|---|
| [01-internal-architecture-audit.md](./01-internal-architecture-audit.md) | ✅ Complete + verified | Ground truth of treesitter, occurrences, SymbolTable/resolve, graph projection, IR, storage; gap analysis with file evidence |
| [02-prior-art-resolution.md](./02-prior-art-resolution.md) | ✅ Completed in recovery | Stack graphs, SCIP/LSIF, Kythe/Glean, tree-sitter natives, oracle reuse, hybrid tiers, usage-example mining |
| [03-exhaustive-plan.md](./03-exhaustive-plan.md) | ✅ Reconciled plan (superseded in part by 04) | Hybrid architecture, phases PR-A…H, policies, metrics |
| [04-ir-unification.md](./04-ir-unification.md) | ✅ Normative, implementation-grade (2026-07-19, Rev 4) | Two planes of one IR: declaration (`.nir`) + **implementation** (`.nb` body file). Rev 4 fully qualifies the impl plane (beginner-implementable, no deferrals): frozen 18-kind region taxonomy (arm=sub-record, suspension=op, cleanup=discriminant); cross-language places with StableRef field identity; universal **capability tuple** (Pony-lattice + escape axes) replacing Rust-only modes; the **dual-tier merge = deterministic anchor-join** (not "same line"); Datalog **summary ruleset** + access-path widening; CWE-mapped **taint ontology** + confidence tiers + honest soundness; `cfg`-per-node; models-as-packages ops. Query plane = one Trustfall adapter unifying semver+security+nav; graph = backend routes (Ladybug/KuzuDB cut). Backed by 6 bleeding-edge research reports |
| [_recovered/](./_recovered/) | Provenance | Raw agent dump + prompts |

Each numbered topic also has `NN-name/PLAN.md` (copy) for consistency with `docs/research/librarification/`.

## Recommended next step

Implement **UR-1..UR-4** from [04](./04-ir-unification.md): declaration wire (treesitter-as-producer) + host-side resolve-in-session (σ) + reflections over dep IR (`DepIrProvider`), then the **implementation-plane skeleton** — the `.nb` companion body file, the frozen region taxonomy (§7), `NdBody` frame grammar (§8), cross-language places (§9), `cfg`-per-node (§11), edit-stable naming (§13) — riding SEMANTIC-IR-VCS P1/P2/P4 with amendments SIV-A1..A4. Then the **dual-tier anchor-join merge** (UR-5, §12), oracle enrichment + capability tuples (UR-6, §10), summaries/taint-ontology/model-packs (UR-7, §14–16), and the Trustfall query plane (UR-8/9). Each construct is fully qualified against 6 bleeding-edge research reports (capabilities, regions, access-paths, merge, taint, CPG/Trustfall).

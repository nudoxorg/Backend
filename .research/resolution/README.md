# Resolution research: treesitter / consumer codebases → IR

**Parent user request (Claude session `17cc2883-6b32-44ea-abc5-c1c9f07d0bf2`):** research how to convert the treesitter representation of an arbitrary codebase into a resolved state against the documentation IR (example usage, instances, cross-package refs), across all languages, and produce an exhaustive plan.

Agents hit Anthropic session/rate limits mid-flight. This directory holds **recovered, verified, and completed** deliverables.

| Doc | Status | Description |
|---|---|---|
| [01-internal-architecture-audit.md](./01-internal-architecture-audit.md) | ✅ Complete + verified | Ground truth of treesitter, occurrences, SymbolTable/resolve, graph projection, IR, storage; gap analysis with file evidence |
| [02-prior-art-resolution.md](./02-prior-art-resolution.md) | ✅ Completed in recovery | Stack graphs, SCIP/LSIF, Kythe/Glean, tree-sitter natives, oracle reuse, hybrid tiers, usage-example mining |
| [03-exhaustive-plan.md](./03-exhaustive-plan.md) | ✅ Reconciled plan | Hybrid architecture, phases PR-A…H, policies, metrics |
| [_recovered/](./_recovered/) | Provenance | Raw agent dump + prompts |

Each numbered topic also has `NN-name/PLAN.md` (copy) for consistency with `.research/librarification/`.

## Recommended next step

Implement **Phase 1** from [03](./03-exhaustive-plan.md): `MergedSymbolTable` + External import bind against dependency Indexes.

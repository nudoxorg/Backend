# Remote swarm recovery inventory — 2026-10-05

## Comparison base and preservation

The comparison checkout is /Users/mileswirht/Downloads/backend at a4f06552a7e4dd40528ed859c0948b08be26e13e. Its only existing working-tree edit observed was .config/nix/tools.nix; it was left unchanged. No builds or tests were run in this audit.

Both SSH repositories remain on older fleet branches. ILO /root/backend is clean at fleet/orchestrator 2e4992147d594763842e1b87ce0d83f7d378c848; h16001mac ~/backend is clean at fleet/orchestrator 8ee56c722d8b2407af2f37e25474e79af3c1efbd. Their canonical refs are 959f8911f626aed867d174cb502908d7358f65d2 (local) and f9c158af0dfc229c4567bf5cd725c5c0828043fa (github), and neither repository contains a4f06552. This means remote branch reachability cannot be used as proof that a candidate is already in the current checkout.

Worktree lists contain many old cross-host paths marked prunable, along with active audit worktrees. No worktree was reset, pruned, or edited. The current agent-owned paths named in the task were not inspected. The non-Git service path /opt/nudox was only checked for Git metadata and is not a Git checkout.

Two remote stashes were exported before opening their patch contents. The read-only patch archives and their read-only manifests are in /tmp/remote-swarm-recovery-20261005.YcdiER:

| Source | Stash commit | Export | SHA-256 |
| --- | --- | --- | --- |
| ILO Python authority WIP | 91efa90111ceb8a7aaf6b6721306d357ad3dc2cc | ilo-python-authority-perf.patch | 644bc3341d3de52aaaf4e284f0c0625fa32208d2726ed18dd4b8abf5e90cb534 |
| h16001mac Rust authority WIP | e242680ea69b3d3a289f3f1c7adea1605c71e7fc | h16001mac-rust-authority.patch | a3495ab1df7a5b0216c457e11e69a90bdfc9adea5ab9d7a9c8ea2b21683dc261 |

The exports are mode 0444 in a mode 0700 directory. Both remote stashes remain intact. The current local stash@{0} is the preserved Nix edit and was not read or changed. A bounded artifact-name sweep found /tmp/backend-warning-quality-96e96.bundle on ILO; it was left unopened because it appears to belong to an active lane.

## TypeScript / TSZ / IR

The active cutover ref codex/tsz-authority-cutover-20261005 points at the comparison base a4f06552. The July TSZ foundation e6379c39485c235466c9a509d242270fde1c6d2e and July 26 IR/TSZ restoration 4a19390b78a1d95762ff7a2611c6ed01a0ccc0e9 are already ancestors of a4.

Two July oracle commits are present in the object store but are not ancestors of a4:

| Ref / commit | Source evidence | Disposition |
| --- | --- | --- |
| 521e63e667ee4ef5605044d6667403b3939011b0 | workspace/compiler/compile/typescript/oracle/tsz.rs added a pure-Rust in-process TSZ checker oracle, removed the external tsgo path, emitted declarations, then re-parsed them through OXC. | Useful history only; this workspace/compiler tree is absent from a4. The source tests assert inferred numeric and async Promise returns; no test was run in this audit. |
| 940cbf4438189ef5875e9012ec9bde428f7b7f3c | Adds tsz_types.rs. Direct cases: intrinsics, literals, arrays, readonly/NoInfer, unions/intersections, tuples, keyof, this, unresolved names. Rich cases fall back through checker.format_type → OXC parse/lower. Correlation uses binder SymbolId plus (file_stem,name), then a unique bare-name fallback. | Better design reference than the prior emit/reparse pass, but still the absent workspace/compiler architecture. Tests cover inferred primitive and Promise return only, not rich-shape or duplicate-name joins. |

The matching newer but unmerged branch codex/ts-tsz-terra ends at b8af922f7547268399b28d2a104d24baf1643750. It tests Conditional, Mapped, TemplateLiteral and FunctionPointer feature shape counts. Its code path is the absent compiler/driver tree; the patch also contains a debug eprintln for Text children and the test checks child counts without checking template text. Do not cherry-pick it directly.

Related compiler-core refs 6832f9e32da9641006c76c60f4af5b385223410f, 1f0a7e75a463ab8c9de4b420b4d18d048d15ad05, and 730fa6c9b09cb4d53aea8737436a39f7a000986f add typed canonical data, semantic child roles, and a fragment-byte falsifier under workspace2/crates/nudox-ir-*. They include a large canonical-data suite and a 149-line fragment test, but use an architecture absent from a4. These commits were forwarded to luna_tsz_authority_cutover with their test limits and compatibility caveats.

## Persistent history, replay, lineage, and residency

The core foundations are already present in a4: crates/replication/src/ir_generation_store/history, crates/replication/src/ir_hydration_store/history_v2/residency.rs, crates/replication/src/ir_residency.rs, and crates/semantic/src/ir/{lineage_match.rs,row_index.rs}. The current tree includes history bridge tests, row-diff/replay oracles, and bounded resident-byte accounting.

Several unmerged refs are older versions of these ideas, not missing foundations: codex/semantic-lineage-matcher cecb84e6f6 adds a 1,016-line matcher with ambiguity tests, while current a4 has successor d3724ebd47 and later fail-closed refinements. codex/persistent-stable-row-index 758d12424b adds two-root diff/replay tests, while current row_index has later borrowed-subtree certificates and bounded streaming diffs. codex/ir-vcs-adaptive-cache 7f01ce36f9 and codex/adaptive-ir-residency 5997b46060 are large prototypes; current history/residency sources have been materially reshaped. Use their tests and invariants as review references, not as direct commits.

## Python / Go admission work worth porting carefully

Remote reflogs preserve two measured commits that are not ancestors of a4. ILO fleet/orchestrator records 66bba166e352b3b1dff6220a92ad83c58bfd130a for Go and 5e4c85749087da482be446eeff7e9afa504b8237 for Python.

The Go commit adds a length-framed batch helper in frontends/go/src/legacy/oracle/batch.go, Rust child framing in oracle.rs, engine prefetch, and cold-fallback/equality tests. Its commit records a 22-file production package result of 15.1 seconds to 915 ms, plus a 4-file wire result of 54.4 ms to 13.2 ms, byte-identical batch/cold images, a successful workspace build, and a green 17-module corpus lane. Current a4 retains GoPackageAuthorityWitness but has no authority_image_batch or batch_wire; adapt the old protocol to the current witness and package-source ownership.

The Python commit adds a batched Pyrefly wire and package prefetch. Its commit records a three-file package reduction from 989 ms to 495 ms and five child processes to two; a six-module wire reduction from 3.334 seconds to 448 ms; cold/batch report equality; a green package/corpus lane; and a successful workspace build. Current a4 exposes analyze_in_package with an exact package_root but has no analyze_batch or PythonAnalysisBatchRequest; PackageSourceSet currently invokes authority per source. Preserve package-root isolation and per-source typed fallbacks when adapting this design.

The exported ILO Python stash is an earlier divergent batch variant, not the strongest source. Its request shape omits current analyze_in_package package-root semantics. The Mac Rust stash uses a thread-local one-file byte check; current a4 instead uses RustWorkspaceSessionLane leases, metadata-policy identity, and the full source frontier. The stash’s untracked traitlets probe hard-codes one Nix store path and allocates very large buffers; keep it as diagnostic history only.

## Index, cache, ingestion, Iroh, Tantivy, Turso, and GUI references

| Candidate ref | What is preserved | Current comparison / rank |
| --- | --- | --- |
| github/jimmy/search-snapshot-reuse-7a89 d5af1611f6 | Tantivy posting reuse across view changes; 220 test lines and a projection-revision benchmark. | Not an ancestor; current engine is substantially rewritten (over 5,000 changed lines against this tree). Review invariants and benchmark design before considering a port. No run receipt was reproduced here. |
| github/jimmy/turso-row-delta-7a89 930f78e596; graph refs 4f6178177f, 670976efaf, e0a9360167 | Keep unchanged projection rows and graph facts across view-root changes, with tests. | Current extensions/turso contains successor write-only-changed-row and digest-skip changes (aa17a7c84b, 8485c3edbe, 7fd5a2fad8). The old files have diverged; do not reapply blindly. |
| codex/catalog-parallel 635ee08fb2 | Cache catalog roots and merge sparse deltas. | Current acquisition module has since grown substantially, including receipt recovery and coalescing. Reuse only the sparse-delta tests/invariants after a focused comparison. |
| codex/ingestion-service-20260906 aadddcb62b and codex/ingestion-worker-composition-20260906 dee4fba1fb | A verified product-ingest bridge and fenced claimed-worker prototype under server/index/product-ingest. | That source root is absent in a4. Treat as design notes; current product/index ownership lives in different crates. |
| codex/iroh-connect-diagnostics ab2138f10f | Exact compiler-probe diagnostic surface. | Not an ancestor; current cluster_dispatch.rs has later diagnostic refinements and substantial divergence. |
| codex/live-index-gui-v8 7852365da9; codex/index-finish-20260905 7ee5cfa062 | Live GUI/index integration and an earlier interface/gui launch surface. | Current apps/desktop/runtime/client.rs was rewritten around saved claims and exact owner receipts; interface/gui is absent. Keep capture/test ideas only. |

## Evidence limits

All exact refs and status statements above come from Git metadata or source inspection. Existing commit messages on the remote Go/Python performance commits contain historical build/test/benchmark receipts, but this audit did not rerun them. No runtime claim is made for the unmerged TSZ, Tantivy, Turso, GUI, cache, or stash-only changes. No fetch, reset, prune, GC, stash mutation, service change, or code integration was performed.

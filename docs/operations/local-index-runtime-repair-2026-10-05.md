# Local indexing repair checkpoint — 2026-10-05

The Linux Build Failures 4 report establishes that canonical `fb526fe542`
builds and opens a window on Parrot/Rust 1.99. It also establishes that fresh
Angular and React/Vite indexing fails, retry does not reliably rewrite state,
and a large project cannot fit its complete file frontier into one canonical
row. Earlier compiler checks and GUI fixture captures did not establish this
real startup-to-publication workflow. These are release blockers.

## Changes reviewed and integrated

The original pushed source checkpoint is `9b6af13481d8b855f4eccd09a6e11fba52056640`,
tree `42b28e63d0a9a6a3300cbe95d62c526ca76423b9`. The subsequent integration
contains the changes below; execution evidence is pinned separately rather
than transferred automatically to newer source revisions.

* First dispatch retains the exact persisted operation claim through ordinary
  publications from the same owner. A mutation lease distinguishes an owner
  replacement from a newer read observation. Saved requests wait for the
  watcher to admit the current root; they do not poll every frame.
* The actor and authenticated connection both check the lease before entering
  Start. A proven unsent failure clears the claim; an ambiguous result after
  entering Start retains the same key for reconciliation. Explicit Retry after
  a terminal result creates a new key. Cancel before dispatch persists a
  cancelled state. Refusals identify the failed admission condition.
* Large project membership uses immutable content-addressed pages rather than
  a larger canonical row or a truncated file list. Project, page, file and
  semantic changes publish in one intent. Validation checks the complete
  frontier on preparation and persisted replay, including page ownership,
  ordering, content identity, counts, missing rows and foreign deletions.
* Small projects retain inline membership. Pages use the existing content cut
  planner with 256/768/1024-key geometry, preserving unchanged page identities.
  Forced maximum cuts can still reflow a suffix. Full frontier validation and
  compilation planning remain linear; this is not a constant-work claim.
* Deferred compiler completion validates its own project's captured frontier
  against the current relation. It does not reject an unrelated package's
  publication merely because the whole workspace root advanced. A bounded
  digest includes the complete encoded prior file rows, including changes in
  an unavailable-source reason that preserve the Project source version.
* Ingest moves the valid UTF-8 source buffer into its String, preserving exact
  bytes, identity and byte accounting without a second full source allocation;
  the existing owned compiler-source constructor now consumes that String.
* Retired-layout recovery recognizes the exact authenticated old Project/file
  frontier through a read-only private probe. Current, mixed, foreign and
  damaged layouts remain refused. Only a positively identified older layout
  receives the typed diagnosis needed by the existing quarantine workflow.
  This does not widen the current validator or publish an old-layout repair.
* Semantic replies admit a selected generation per exact package coordinate
  and language profile, matching the compiler's existing selection model.
  Duplicate selections for the same target remain refused. Local selected
  records carry the complete checked Project membership count and source
  identities from the same immutable owner snapshot that answered the query.
  The borrowed tree visitor validates file ownership without cloning source
  payloads into a second result vector. This validation still performs work
  proportional to the membership; no query-latency benchmark is claimed.
* CLI help exposes the existing `index /absolute/folder` alias. Durable
  acceptance must use keyed IndexOperationStart/Status; legacy Add success
  alone does not prove the desktop operation lifecycle.
* The selected-frontier field changes a strict semantic reply shape, so the
  transport DTO version advances to 16. Version mismatch diagnostics name the
  observed and supported versions and direct the operator to matching builds.
  Deploy GUI, locald, CLI and MCP together; do not reuse a v15 client and
  interpret a nested decoding failure as an indexing failure.

## Evidence and its limits

Root ran this check on a clean, unchanged source with the already realized Nix
environment, Rust/Cargo 1.97.1, one compiler job and offline locked dependencies:

```sh
cargo check --locked --offline -j1 --workspace --all-targets --keep-going \
  --features backend-desktop/visual-harness,backend-local-service/search-bench \
  --message-format=short
```

It exited **0**, 2026-10-05 01:52:45–01:55:39 UTC. Cargo.lock SHA-256:
`8467227b9623f00e3e4d1950333a97bcc8c869c179d0847befd26affedf2da0c`.
Log SHA-256:
`36209e3ddc8ac9d2bcf5f14520112a1b5a7c49e5bd0b20daa0d70e9af970bf20`.
Local receipt:
`/private/tmp/nudox-index-integrated-check-9b6af1-20261005/result.json`.
The configured warning backlog remains. This check type-checks tests; it does
not execute them or launch an updated GUI.

An earlier targeted test build at `8af41c67dc` was stopped when other compiler
jobs pushed the fleet above the four-build limit. Its parent observed exit
143; **no regression tests executed**. The warmed cache is retained. Actual
execution at `9b6af13481` subsequently ran 53 tests: **44 passed, 9 failed**.
The failures exposed a real retired-layout startup gap as well as incorrect
test scheduling, asynchronous-save expectations and outdated fixture facts.
Those failures are retained in
`/private/tmp/nudox-index-targeted-tests-retry-9b6af1-20261005/index-result.json`.
An intermediate `e20a3e4b78` test build failed to compile a new fixture's
unchecked String where ProductText was required; **no tests ran** there.

The corrected integration at `db8bc805ef23f5d929643ad7558ca00a85f1c238`, tree
`6ee5dc96a01ac66338eca3715ba8d1cce7bb3da1`, passed the full workspace/all-targets
check above on 2026-10-05 03:19:06–03:21:23 UTC. The log SHA-256 is
`a202d5ad9e4cdce805a293c33d269c26176666ac475b4d4ec3fe48ca0937bbfc`.
Actual test results from that exact source, with one compiler job, are:

| Suite | Passed | Failed | Interpretation |
| --- | ---: | ---: | --- |
| Desktop `index_` | 52 | 1 | The remaining failure is an authority-bearing Cargo fixture built from a trimmed capture without resolved feature observations. |
| Desktop `durable_writer_tests` | 12 | 0 | Real writer acknowledgement, cancellation, freshness recovery and exact-claim schedules. |
| Desktop `runtime::owner::tests` | 7 | 0 | Owner observation and lifecycle admission. |

The receipts and full logs are retained at
`/private/tmp/nudox-index-repaired-runtime-tests-db8bc8-20261005/`.
The retired-layout test build was subsequently stopped with exit 143 after
two fleet censuses found five compiler workloads. Only Root's process group
was stopped; no retired-layout tests ran, and the parent did not execute its
remaining membership/frontier/DTO selectors. The warm compilation cache is
retained. Newer selected-frontier, multi-profile reply and transport-version
changes are not covered by the `db8bc805ef` execution evidence.

The startup oracle uses real BootClient, persistence and an embedded service
after a forced launch-snapshot timeout. Its closed compiler set deliberately
produces a real failed operation to exercise exact receipts and retry. It is
not successful language compilation evidence. Its cold restoration reloads
desktop state while the service remains alive; service-journal restart is a
separate gate. Controlled writer/actor tests cover the save-time publication
race independently. None replace native user-flow testing.

The real C# helper was restored and published offline from immutable helper
source and SHA-512-verified NuGet archives, without changing the user's Nix
configuration. The Unicode image exactly matches its golden. Two independent
fidelity invocations produce the same 6,856-byte image; the retained 6,684-byte
golden is stale against intentional property-write and method-call changes.
The structural review additionally found a false MethodGroup reference for
an attribute name. Until that source defect is fixed and the corrected image
is reviewed, the fidelity gate remains failed; a successful helper build is
not whole-project C# acceptance. Evidence is retained at
`/private/tmp/nudox-roslyn-realization-20261005-run1/receipt.json`.

## Remaining acceptance gates

1. Execute the focused startup, delayed-writer, owner-replacement, queued
   cancellation and retry tests; execute durable paged-membership commit/edit/
   reopen and corruption tests. Treat a zero-test selection as failure.
2. Build matching GUI, locald, CLI and MCP binaries with source and artifact
   receipts. Index fresh real repositories through every supported language
   authority; retain exact typed publication receipts and semantic profile
   evidence. Missing tools are blocked cases, not successful skips.
3. Use the native GUI to add, cancel, retry and reopen those projects, verify
   shelf updates without navigating away, and test Graph/Page/Code, keyboard,
   settings and disconnected operation. Cold restart must include the owner.
4. Verify an accepted large-project frontier beyond the old single-row limit.
   A Git-aware candidate count is not an accepted/indexed file count.
5. Finish bounded source replay into the existing compiler authorities and
   embedding pipeline. Ingest still has separate **64 MiB source** and
   **64 MiB encoded relation-row** budgets. Paging does not remove these
   aggregate bounds or establish that Nudox's own repository can be indexed.
6. Verify native Linux runtime behavior and the relocatable macOS investor
   bundle, including real compiler helpers, signing and cold launch. No new
   release executable or investor bundle is validated by this checkpoint.

The mutation lease also cannot prove a process identity change that is never
observed and reuses an identical endpoint and authority. Do not describe the
current attachment check as a process-nonce protocol.

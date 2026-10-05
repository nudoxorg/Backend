# Local indexing repair checkpoint — 2026-10-05

The Linux Build Failures 4 report establishes that canonical `fb526fe542`
builds and opens a window on Parrot/Rust 1.99. It also establishes that fresh
Angular and React/Vite indexing fails, retry does not reliably rewrite state,
and a large project cannot fit its complete file frontier into one canonical
row. Earlier compiler checks and GUI fixture captures did not establish this
real startup-to-publication workflow. These are release blockers.

## Changes reviewed and integrated

The source checkpoint is `9b6af13481d8b855f4eccd09a6e11fba52056640`, tree
`42b28e63d0a9a6a3300cbe95d62c526ca76423b9`, before this evidence document.

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
  publication merely because the whole workspace root advanced.
* Ingest moves the valid UTF-8 source buffer into its String, preserving exact
  bytes, identity and byte accounting without a second full source allocation.
* CLI help exposes the existing `index /absolute/folder` alias. Durable
  acceptance must use keyed IndexOperationStart/Status; legacy Add success
  alone does not prove the desktop operation lifecycle.

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
143; **no regression tests executed**. The warmed cache is retained. A new
one-job test build at `9b6af13481` is pending at this checkpoint; neither that
build nor its tests are credited as a pass here.

The startup oracle uses real BootClient, persistence and an embedded service
after a forced launch-snapshot timeout. Its closed compiler set deliberately
produces a real failed operation to exercise exact receipts and retry. It is
not successful language compilation evidence. Its cold restoration reloads
desktop state while the service remains alive; service-journal restart is a
separate gate. Controlled writer/actor tests cover the save-time publication
race independently. None replace native user-flow testing.

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

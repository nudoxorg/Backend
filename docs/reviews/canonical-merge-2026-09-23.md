# Canonical merge: GUI rollout × versioned-engine cutover (2026-09-23)

This merge joins two lines that diverged at `88198ba03` (2026-09-14):

- **codex/nudox-gui-rollout** (`45926061a`, 167 commits): the gpui-ce desktop,
  registry/acquisition/advisory, rich-IR graph, onboarding and project shelf,
  MCP reconnect and budgets, and the Windows platform seam.
- **origin/canonical** (`56e35d818`): Mikita Slabysh's Windows fixes plus the
  owner's cutover commits `930aa72f9` ("complete versioned engine v2
  cutover", −292k lines) and `56e35d818` ("restore complete v2 source graph",
  −134k lines).

A textual merge produced 276 conflicts, 237 of them modify/delete. Worse, it
would have silently taken every file only the cutover changed. Those files were
adapted to APIs the cutover removed and that the rollout still calls.

## Method

The rollout tree is the structural base: the merge commit uses the `ours`
strategy with both parents recorded. Each cutover contribution was then
examined and either ported explicitly or declined with evidence. Five read-only
analyses (desktop; library/CLI/MCP; engine/local-service; frontends/semantic/
extensions; infrastructure/core) checked every cutover deletion against the
rollout tree with `git grep`.

## Why the cutover's deletions were not applied

Every module the cutover removed is still referenced by rollout code:

| Area | Cutover change | Rollout dependency |
|---|---|---|
| `crates/engine` | removed `application`, `capability`, `driver`, `index_build`, `index_publish`, `publication`, `registry`, `retrieval`, `telemetry` (210 files) | registry grew under 9 feature commits (Maven/NuGet, CAS, multi-source routing, native facts); all are used by local-service and journeys |
| `crates/library` | `wire/` → `protocol/`, 60–95% of the surface removed (`SurfaceCommand`, paging commands, `Lane`, `SourceExcerpt`) | `SurfaceCommand` is the desktop's command vocabulary (27 files); `Lane`/`SourceExcerpt` have ~200 call sites |
| `crates/present` | deleted | the shared grammar/render layer for `apps/cli` (8 files) and `apps/mcp` (6) |
| `frontends/*` | collapsed to one `lib.rs` each | the collapse restores MB's `tags-v1` queries; the rollout's `tags-v2` recover fields, variants and constants |
| `crates/semantic` | 145 files deleted | 144 present verbatim and all `pub mod`-live; one renamed into `ir/semantic_render` |
| `crates/runtime` | collapsed to `lib.rs` | drops `pub mod server` (used by telemetry) and the macOS `/tmp` identity fix |
| `apps/worker` | rewritten | drops the `TcpExposure` loopback gate and the `BoundedFileImage` symlink-safe reads |
| `crates/store` | recovery simplified | drops crash-recovery temp-file scavenging |
| `extensions/{qdrant,tantivy,trustfall}` | halved; trustfall `query.rs` −1283 | `SemanticQueryRequest`/`SemanticQueryCorpus` are called directly by `commands.rs` |
| `apps/desktop` | replaced by a 17-file `native/` GUI on git gpui `d435891` | the rollout GUI (~144 files) is on crates.io `gpui-ce 0.2.2`; the two type universes cannot coexist, and `native/` is superseded except for the metadata below |
| `tools/control` | moved to `crates/control` as a product crate | reverses the deliberate MB decision that the agent control plane stays outside the product DAG |

## What was ported from the cutover side

| Port | Source | Destination |
|---|---|---|
| Portable directory fsync barrier (`open_directory`, Windows backup semantics) | Mikita `4c8459009` | `crates/platform/durability.rs`; 25 call sites via three-way apply, plus the two later rollout sites in `local-service/builtin/registry.rs` |
| Desktop on Windows: host/ui/views built for `any(unix, windows)`; liveness probe over `LocalStream` | Mikita `12e8534d4` | `apps/desktop/src/lib.rs`, `host/lease.rs` |
| Quoted paths from "Copy as path"; platform-spelled example path | Mikita `06e6ff08e` | `views/project_admission.rs`, `views/mod.rs` |
| Turso full-text search: FTS index, batched rebuild with in-transaction fence recheck, `OPTIMIZE INDEX`, replay-safe `apply`, single-snapshot `MATCH` search, metadata validation | cutover `extensions/turso` | the rollout's modular `extensions/turso/src/*` |
| Typed evidence receipts (candidate / evaluation / decision) | cutover `crates/control/src/evidence.rs` | `tools/control/src/evidence.rs`, compiled and tested for the first time; the CLI module is compiled but not yet exposed |
| Offline local package metadata (`cargo metadata --offline` + manifest fallback, README, deps, features, repository) | cutover `apps/desktop/src/native/package_metadata.rs` | `apps/desktop/src/model/local_package/`, dispatched through the engine actor's local-read lane |
| Review records | cutover `docs/reviews/*2026-09-19*`, `luna-forge-index.md` | unchanged |

### Corrections made while porting

- **Turso schema upgrade.** Full-text search bumps the projection schema to 2. On
  its own that makes every existing workspace refuse to start `locald`. The
  projection is derived from the journal, so `TursoProjection::open_or_rebuild`
  discards an older-schema file and repopulates it; a newer schema is still
  refused.
- **Turso concurrent writers.** A writer that loses a race to publish the same
  root now gets `Reused` rather than `StaleTransition`.
- **Evidence receipt schemas.** Evidence receipts get their own schemas rather
  than reusing the ledger's custody-receipt schemas, which hash a different
  layout.
- **Evidence size limits.** The cutover derived receipt ids through
  `Identity::from_bytes`, which caps input at 4096 bytes. Its documented limits
  of 1024 references and 64 KiB could therefore never be reached; they are now
  the limits actually enforced.
- **Local package metadata outside a Cargo project.** A folder without a
  `Cargo.toml` skips Cargo entirely. Otherwise Cargo walks up the tree and
  describes an unrelated enclosing workspace.

## Declined, with reason

- **`library/cursor.rs` reset-root rule** ("all coverage complete"): correct only
  in the cutover's model, where partial coverage no longer exists. The rollout
  keeps partial lanes (e.g. semantic), so porting the rule would reject valid
  resets.
- **`version/persistent/lazy.rs`**: the cutover inlines `lazy/types.rs` back
  into one file. The content is identical, so there is nothing to port.
- **`crates/platform` lint table** (Mikita `9541b5ec5`): only needed under a
  workspace-wide `forbid(unsafe_code)`. The rollout keeps `deny` with audited
  per-module exceptions.
- **`relation.rs` package and dependency facts, and `package_graph.rs`**: a
  Cargo-only subset of the rollout's `crates/library/package_graph.rs`
  (cross-ecosystem, evidence-typed). The new fact variants have no producer
  in the rollout.

## Pre-existing defects fixed so the gate is green

- `runtime::tests::result_coalescing_retires_the_replaced_request` failed
  deterministically on the rollout branch. It stopped polling at the replaced
  request's stale result, before the actor answered the replacement. It now
  polls until the replacement retires, with a deadline.
- The `gui-contract` flake check never ran. Its `jq` step used bash line
  continuations inside a Nushell build script, and the journeys manifest was
  configured as `.config/gui/journeys.json` relative to the `.config` root,
  which doubled the prefix.

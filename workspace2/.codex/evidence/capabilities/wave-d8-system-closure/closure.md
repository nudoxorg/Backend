# Wave D.8 system closure receipt

## Verdict: EVIDENCE_BLOCKED

The requested public red integration journey was not implementable from commit
`f2565a9fb33af06053bd19721d4dc2753ec09ed5` without inventing product APIs or duplicating product
logic. No product source, test, fixture, manifest, lockfile, Nix expression, or quality tool differs
from that baseline on the controlling branch.

The repository currently exposes only these relevant dependency slices:

- `domains/ir`: `nudox-ir-vocab`, containing typed dense IDs but no documented IR
  format/view/build/diff seam.
- `planes/compiler`: `nudox-compile-vocab` and `nudox-compile-registry`. The registry's documented
  C0 dispatcher accepts `RustSubset`/`TypeScriptSubset`, supports parse/lower stage selection, and
  returns borrowed source bytes rather than a real frontend and compact IR pipeline.
- `planes/index`: `nudox-index-vocab`, containing index ID aliases but no documented immutable
  exact/Tantivy format/view/build/query/publish seam.

No accepted public seams exist at this baseline for real frontends, compile recipe/driver/schedule,
compact-IR publication, immutable exact or Tantivy indexing, Trustfall graph execution,
Qdrant/local-vector search, adaptive local-first placement, or equivalent in-process/CLI/MCP/GPUI
observation. The domain plans also leave the concrete package/API names for the last five concerns
undocumented. Tests against guessed names would violate the public-seam constraint and could not
prove the product terminal.

## Delegation and custody

The controlling task invoked `/root/wave_d8_seam_manager` as the registered
`nudox_terra_orchestrator`, requesting `gpt-5.6-terra` at `xhigh`. It used the isolated worktree
`/private/tmp/nudox-wave-d8-seam-readiness-f2565a9f` on branch
`codex/wave-d8-seam-readiness-terra`, based on the exact required baseline and tree
`8477cb2ab93763c468d5431740cfb1d5e4c4cf82`.

The manager made two bounded attempts to obtain the mandatory source-isolated pre-edit
`nudox_terra_reviewer`:

1. Sidecar task `01a0513d-e809-7bd1-a597-9282bbb0c7ad` failed because a full-history fork inherited
   the parent agent type. No reviewer child was created.
2. Sidecar task `01a05140-a448-7052-a784-a9a14f4348b4` used a source-free fork, but its router
   rejected the configured role path with `Unknown model gpt-5.6-luna`; it exposed only
   `gpt-5.6-sol, gpt-5.6-terra`. No reviewer child was created.

Both source snapshots remained read-only and retained the same aggregate SHA-256 before and after:
`261ecea710ae6bd73c69642d601d69750640b47b7549a308330a7bdb97bb476b`.

Because the required hostile pre-edit custody did not close, the governing skill forbade dispatching
a Luna writer. Therefore no `gpt-5.6-luna`/`max` implementer ran. Manager/reviewer runtime task IDs,
effective models, effort, and sandbox receipts not emitted by the collaboration runtime remain
`UNVERIFIED`; configured role values are not reported as actual values. Full attempt receipts live in
the sibling `wave-d8-seam-readiness` evidence directory.

## Raw gates and environment

From a clean controlling worktree, the existing repository quality entry point completed with exit
status 0 under the pinned Nix environment:

```sh
cd workspace2
nix develop --offline --command ./tools/quality.sh
```

Observed green gates were Dylint UI, all inventoried workspace tests, Clippy with all targets and
features, rustdoc, compile-fail tests, the Loom model, dependency policy, and unsafe policy. This
proves only the baseline capabilities; it does not prove Wave D.8.

Environment receipt:

- Lix `2.95.2`
- Rust `1.97.1 (8bab26f4f 2026-07-14)`, LLVM `22.1.6`, target `aarch64-apple-darwin`
- Cargo `1.97.1`
- Darwin `24.6.0`, ARM64 `T6030`, 11 logical CPUs, 38,654,705,664 bytes RAM

The terminal executable, deterministic 200-package corpus, optional local-service fixtures, mutation
suite, every-prefix durability schedules, cancellation/overload/authentication/outage/recovery
schedules, typed OTEL correlation laws, and disabled/exporter-failure laws do not exist at this
baseline. Consequently peak bytes, allocations by lifetime, passes/branches/atomics, cold/warm
latency, throughput, and binary size are all `UNVERIFIED`, not zero.

## Strongest counterexample

A constant implementation can currently return input bytes from the compiler registry and still
pass every existing quality gate. It cannot distinguish two source programs in compact IR, publish a
durable generation, alter lexical/graph/vector results, or produce equivalent CLI/MCP/GPUI
observations. Thus a green baseline quality run is not evidence for any Wave D.8 terminal law.

## Salvage and uncertainty

No substantial work was deleted, so no symbol/mechanism salvage ledger entry was required. Retained:
the frozen public-seam inventory, explicit undocumented-seam rows, red falsifiers, resource controls,
and both raw custody failures. Rejected: guessed APIs, constant package lists, fake local services,
duplicate product logic, a manager self-review, direct non-isolated review, and test-only crates.

External actions required are (1) repair registered sidecar role routing so the separate Terra/xhigh
reviewer can run with source isolation, and (2) land the accepted dependency seams in their owning
waves. Retry Wave D.8 from a newly frozen accepted baseline. It is uncertain whether those eventual
public seams will retain the draft package boundaries in the current plans; no claim is made that
they will.

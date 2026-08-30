# Wave D.8 documented-public-seam readiness

## Public terminal

`nix run .#wave-d8-seam-readiness -- --require-ready` examines the actual
locked, offline Cargo metadata for the root workspace and each documented
nested workspace, and emits one deterministic JSON record. `READY` means only
that the documented package/workspace inventory and named public-seam
documentation are sufficient to begin authoring a Wave D.8 public journey. It
does **not** claim that a package implements a product behavior, that a public
API is usable, or that Wave D.8 is product-closed. `BLOCKED` names every exact
missing package, workspace, metadata source error, or undocumented public seam.

## Frozen baseline and custody

| fact | value |
| --- | --- |
| baseline commit | `f2565a9fb33af06053bd19721d4dc2753ec09ed5` |
| baseline tree | `8477cb2ab93763c468d5431740cfb1d5e4c4cf82` |
| manager branch | `codex/wave-d8-seam-readiness-terra` |
| manager checkout | `/private/tmp/nudox-wave-d8-seam-readiness-f2565a9f` |
| source checkout status at intake | clean |
| Sol branch excluded from writes | `codex/wave-d8-system-closure` |
| forbidden checkout | `/Users/mileswirht/Downloads/backend` |

## Laws and negative space

- The command obtains source facts from actual Cargo metadata; a copied or
  constant package list is insufficient.
- It uses the pinned Nix quality closure and `cargo metadata --locked --offline
  --no-deps`; it has no network, build, service, or product dependency.
- It records `PRESENT`, `ABSENT`, and `ERROR` source facts separately from the
  derived gate state. A metadata failure is `BLOCKED`, never an empty inventory.
- No new product crate, product API, scenario module, adapter, mutable domain
  state, serde, `dyn`, `Box`, `Vec`, `Arc`, macro, unsafe, SIMD, cache, or
  shipping dependency is authorized.
- Tooling and its mutation gate are restricted to `flake.nix` and `tools/`.
  `ROADMAP.md` is read-only.
- Presence of C0/I0 vocabulary cannot satisfy later compiler/index, graph,
  vector, placement, or unified-interface stages.

## Documented inventory contract

The gate checks these manifest paths, exact package names, and no invented
crate/API names. The first column is the source of the expectation; the live
metadata result is a source fact emitted by the command.

| workspace manifest | documented packages | source |
| --- | --- | --- |
| `Cargo.toml` | `nudox-frame`, `nudox-hydration`, `nudox-id`, `nudox-object`, `nudox-object-pack`, `nudox-observe`, `nudox-operation`, `nudox-root`, `nudox-runtime`, `nudox-schema`, `nudox-store-memory`, `nudox-view`, `nudox-workflow` | current root metadata |
| `domains/ir/Cargo.toml` | `nudox-ir-vocab`, `nudox-ir-format`, `nudox-ir-view`, `nudox-ir-build`, `nudox-ir-diff` | `COMPILER_IR_GREENFIELD_PLAN.md` source topology |
| `planes/compiler/Cargo.toml` | `nudox-compile-vocab`, `nudox-compile-registry`, `nudox-compile-driver`, `nudox-compile-schedule`, `nudox-compile-publish` | `COMPILER_IR_GREENFIELD_PLAN.md` source topology |
| `planes/index/Cargo.toml` | `nudox-index-vocab`, `nudox-index-format`, `nudox-index-view`, `nudox-index-build`, `nudox-index-query`, `nudox-index-publish` | `INDEX_GREENFIELD_PLAN.md` source topology |

The exact source documents identify the following stages but do not document a
public package or API name. They are emitted as `UNDOCUMENTED_PUBLIC_SEAM`, not
silently mapped to a guessed crate, type, method, or adapter:

| seam id | documented stage | missing documentation fact |
| --- | --- | --- |
| `tantivy-lexical` | Wave B.4 lexical adapter | an `adapters/lexical-tantivy/` path is named, but no Cargo package or public API is named |
| `trustfall-graph` | Wave B.5 typed async graph seam | no public Cargo package/API is named |
| `qdrant-local-vector` | Wave B.5 replaceable vector projection/local fallback | no public Cargo package/API is named |
| `adaptive-placement` | Wave C.6 local-first placement controller | no public Cargo package/API is named |
| `unified-inprocess-cli-mcp-gpui` | Wave C.7 unified interface | no public Cargo package/API is named |

## Coupling skeleton

| module/path | invariant owner | terminal | dependencies | state/control boundary |
| --- | --- | --- | --- | --- |
| `tools/wave-d8-seam-readiness.sh` | metadata interpreter | stable JSON record | pinned cargo, jq | one metadata result per fixed manifest |
| `tools/wave-d8-seam-readiness-self-test.sh` | mutation oracle | false inventory rejected | readiness command only | added/removed/duplicate/malformed/failing metadata |
| `flake.nix` | pinned command entry point | `nix run` invokes the gate | Nix shell packages | no product dependency edge |
| evidence directory | claim/evidence binding | receipt and packet digests | Git SHA-256 | Phase 0 -> build -> review -> closed |

## Current consumer and authority boundary

The direct consumer is the future ordinary top-level Wave D.8 integration
author. This capability provides only a documented-seam readiness record to
that author; the Sol steward remains the owner of the actual public red
journey. Adding a missing public package/API name is a documentation/product
authority decision outside this tooling capability.

## TESTING.md mapping

`TESTING.md` SHA-256 is bound in `index.toml`. Applicable clauses map to rows:

| TESTING.md clause | matrix row | applicability or exclusion |
| --- | --- | --- |
| Allocation and layout | R7 | excluded: the command does not allocate or retain payload/domain owners; shell process allocation is not a product claim |
| Universal rules: negative space and exact error | R1, R2, R5 | applicable: every failure is a typed record fact, never empty success |
| Universal rules: deterministic sequences | R6 | applicable through deterministic fixture mutation sequence |
| Universal rules: complexity and representation | R7 | applicable: fixed four-manifest traversal, sorted output, bounded metadata-only input |
| Universal rules: no shadow mocks | R1, R6 | applicable: normal command consumes real metadata; fake metadata is isolated to mutation falsifiers |
| Foundation fabric | R3 | excluded: no binary product format or frame validation is introduced |
| Object/root/store/hydration | R3 | excluded: no product object/root/store path is touched |
| Operation/runtime/workflow | R3 | excluded: no product runtime, async, durability, or mutable state is touched |
| End to end | R4, R8 | applicable boundary: status never claims product correctness; it records that required seams are absent or undocumented |

## Resource control plan

The gate executes exactly four metadata commands, each with `--no-deps`,
`--locked`, and `--offline`; it does not invoke `cargo build`, `cargo test`, or
service/network tooling. It retains only sorted package names and fact rows for
the current process. The deterministic JSON serializer uses `LC_ALL=C`, fixed
manifest order, fixed documented package order, and no timestamp, machine path,
PID, hash map, or cache output. Any metadata command failure is included as a
fact and makes the gate `BLOCKED`.

# Orientation

For coding agents. Dense and factual; no narrative. Read
[../README.md](../README.md) for the human-facing version.

Written against `main` = `b388f36d`, 2026-08-13. Re-grep any `file:line` that
does not match.

## What this repo is

A Rust monorepo that compiles packages from eight language ecosystems into a
shared IR (`nudox-ir`), indexes it, and serves it two ways: an axum HTTP API
(`driver`) and an MCP server (`nudox-mcp`). Ingest runs each language producer
inside an ephemeral microVM. State lives in DoltLite (versioned SQLite), blobs
in an `object_store`, vectors in Qdrant, package text in Tantivy. Cargo is the
authoritative build; Buck2 is stale and partly broken.

## The two planes

**This is the fact that most often makes an agent do the wrong thing.** There
are two largely disjoint dependency trees. They share exactly one crate,
`nudox-ir`. Neither supersedes the other.

```
SERVER PLANE — workspace/*
        heart  ←  transport  ←  index
          ↑           ↑           ↑
          └──  registry           ir-vcs
                  ↑                ↑
                  └───  driver  ───┘        bin: driver
                          ↑
                       sandbox

LOCAL-FIRST PLANE — crates/nudox-*  (+ workspace/gui)
 nudox-ir ← nudox-store ← nudox-graph ← nudox-engine ← { nudox-mcp, lindsey }
                ↑
      nudox-producer, nudox-producer-{rust,typescript,python,clang,go,java,csharp}
```

`nudox-ir` is a path dep of `driver`, `index`, `registry`, and `ir-vcs`, and in
all four it is **aliased as `ir`** (`workspace/driver/Cargo.toml:55`,
`workspace/index/Cargo.toml:93`, `workspace/registry/Cargo.toml:34`,
`workspace/ir-vcs/Cargo.toml:25`). Inside those crates `ir::` means
`nudox_ir::`.

**No `workspace/*` crate depends on any `nudox-producer-*` crate.** The server
plane produces IR by exec'ing a guest binary inside a microVM
(`workspace/driver/coordination/indexing.rs:719`), not by calling
`nudox_producer::produce`.

## Which plane does my task belong to?

| Signal in the task | Plane | Where |
| --- | --- | --- |
| HTTP route, wire contract, status code | server | `workspace/driver/http/` |
| Package ingest, job queue, outbox, blob store | server | `workspace/driver/coordination/`, `workspace/index/` |
| Catalog schema, migrations, ecosystem grammars | server | `workspace/index/` |
| Qdrant, embeddings, reranking, dep-shards | server | `workspace/registry/vector/`, `workspace/driver/{search,bakery,rerank}.rs` |
| Federation, overlays, `Sourced<T>` | server | `workspace/driver/lib.rs`, `workspace/heart/access/` |
| Pijul patches, NdIrF1 stream, semver diff | server | `workspace/ir-vcs/` |
| microVM, capability budget, sealed command | server | `workspace/compiler/sandbox/` |
| MCP tools, agent-facing schema | local-first | `crates/nudox-mcp/` |
| Trustfall queries, graph adapter | local-first | `crates/nudox-graph/` |
| Corpus, package views, name/kind indexes | local-first | `crates/nudox-store/` |
| Streaming to the GUI, chunking, doc rendering | local-first | `crates/nudox-engine/` |
| The `Producer` trait, a language frontend | local-first | `crates/nudox-producer*/` |
| GPUI views | local-first | `workspace/gui/` (non-member; own lockfile) |
| Entry/kind/wire/change model, `Lowering` | **both** | `crates/nudox-ir/` — changes here reach everything |

## Naming you must use

Readers grep. Do not invent friendlier names.

- **`pub type Server<M> = Driver<M>`** (`workspace/driver/lib.rs:135`). Same
  type, both spellings appear freely.
- **`driver` has a crate-local `pub mod registry` that shadows the `registry`
  crate** (`workspace/driver/lib.rs:35`). Inside `driver`, bare `registry::…`
  and `crate::registry::…` both mean the *facade*. The real crate is imported
  as `xregistry` (`workspace/driver/Cargo.toml:36`) and reachable as
  `::registry`. There is a second facade `pub mod vector` at
  `workspace/driver/lib.rs:78`. Detail:
  [architecture.md](../architecture.md#the-registry-facade-inside-driver).
- Vocabulary: `Driver`, `SourceStores`, `Federation`, `Lowering`, `SmolvmCage`,
  `SealedCommand`, `CapabilityBudget`, `SinkKind`, `heart::query::Query`,
  `PristineIntroTable`, `ReadCap`/`WriteCap`/`AdminCap`, `EngineHandle`,
  `CorpusAdapter`, `SymbolKey`.

## Formatting conventions per tree

- `workspace/driver/` uses **tabs**. `crates/nudox-*` uses **spaces**. Both are
  rustfmt-clean; match the file you are editing.
- `rustfmt.toml` sets `imports_granularity = "Crate"`, `normalize_comments`,
  `wrap_comments`, `hex_literal_case = "Lower"`, edition 2024. Several are
  nightly-only options — the devShell sets `RUSTC_BOOTSTRAP=1`
  (`flake.nix:409`).
- Commits must be Conventional Commits: the `convco` pre-push hook runs
  `convco check` over the pushed range (`flake.nix:258-266`).

## Before you touch anything

1. The repo **does not build from a clean checkout** — seven `include_str!`'d
   assets are missing. [gotchas.md](gotchas.md#the-build-is-broken-right-now).
2. The root `README.md` is **substantially fiction**. Do not read it for facts.
   [gotchas.md](gotchas.md#readmemd-is-fiction).
3. The ~18 root `*-PLAN.md` files are **intent, not description**. Their
   section markers (INDEX-PLAN §9, SMOLVM-PLAN §5.1, LR-6, ID-8, §L0) are cited
   throughout the source and are useful for *why*; several describe systems
   that were built and then removed.
4. Do not run `cargo build`/`check`/`nextest` casually in this tree if you care
   about not writing `target/` — set `CARGO_TARGET_DIR` outside the repo.

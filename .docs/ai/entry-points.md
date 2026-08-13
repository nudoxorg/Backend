# Entry points

Task → the exact files you touch. Every path verified against `main` =
`b388f36d`, 2026-08-13.

Where a task spans both dependency planes, that is called out — see
[orientation.md](orientation.md#the-two-planes).

---

## Add an HTTP route

Server plane. Four files, in this order:

| Step | File | What |
| --- | --- | --- |
| 1 | `workspace/driver/http/handlers/<group>.rs` | write the handler. Take `State<Arc<Server<M>>>`, a `Principal` (or `AdminPrincipal`), and `Json<T>`; return `ServerResult<…>`. |
| 2 | `workspace/driver/http/handlers/mod.rs` | `pub mod <group>;` if it is a new group |
| 3 | `workspace/driver/http/router.rs:179` (read), `:195` (write), `:217` (admin), `:240` (compiled), `:257` (vector) | add the `.route(...)` to the correct plane — the plane decides body limit and timeout |
| 4 | `workspace/driver/http/router.rs:29` | add the handler module to the `use crate::http::handlers::{…}` list |

Then: if it needs a new request/response shape, `workspace/driver/http/dto.rs`.
If it needs new business logic, `workspace/driver/coordination/`. If it needs a
new error, `workspace/driver/error.rs` — add a typed variant and give it a
status in `ServerError::status` (`error.rs:317`).

**Update [../api-contract.md](../api-contract.md).** It is the canonical
contract and the Nudox/Web docs link to it; a route added without a contract
entry is invisible to the other repo.

Body ceilings are consts at `router.rs:33` (read, 2 MiB), `:38` (write/admin,
64 KiB), `:44` (compiled lookup, 256 KiB), `:248` (rerank, 256 KiB). Note the
write plane takes `min(config.max_request_bytes, 64 KiB)` (`router.rs:196`), so
config can only tighten it.

## Add a language ecosystem

Spans both planes and roughly a dozen sites. Adding a variant to
`heart::Language` breaks every exhaustive match below at compile time, which is
the intended forcing function — follow the errors.

**Server plane:**

| File | What |
| --- | --- |
| `workspace/heart/ecosystem.rs:35` | the `Language` variant. `#[serde(rename_all = "lowercase")]` + `#[strum(serialize_all = "lowercase")]` at `:33-34` give the wire token. |
| `workspace/heart/ecosystem.rs:105` | a `Toolchain` variant |
| `workspace/index/ecosystem/<lang>.rs` | implement `EcosystemSpec` (`workspace/index/ecosystem/mod.rs:59`): name grammar, version grammar, manifest shape, archive format, search normalization |
| `workspace/index/ecosystem/mod.rs:307` | add the arm to `spec(language) -> &'static dyn DynSpec` |
| `workspace/driver/http/dto.rs:55` | `required_or_default_origin` — the default `RegistryOrigin` for the ecosystem |
| `workspace/driver/coordination/initialization.rs:73` | `provisional_toolchain` — the stand-in recorded before first compile |
| `workspace/driver/coordination/indexing.rs:719` | `producer_command` — the guest binary path and argv |
| `workspace/driver/coordination/indexing.rs:817` | `producer_profile` — the sandbox resource ceilings / threat tier |
| `workspace/driver/lib.rs:558` | a `CatalogFollower`, only if the ecosystem is mirrorable. Today only `CSharp` and `Rust` have one; other languages log and skip (`lib.rs:565`). |

**Local-first plane:**

| File | What |
| --- | --- |
| `crates/nudox-ir/src/body.rs:38` | the `nudox_ir::body::Language` variant. **This is a different enum from `heart::Language`** — 10 variants vs 8, and it splits `C` from `Cpp`. Its `tag()` (`body.rs:57`) is wire-visible and frozen. |
| `crates/nudox-producer-<lang>/` | a new crate implementing `Producer` |
| root `Cargo.toml` members list | add the crate |

**And:** the guest producer binary the server plane actually execs is *not*
built from these crates — see
[../open-questions.md](../open-questions.md#oq-14--what-builds-the-guest-producer-binaries).
A new ecosystem's `producer_command` arm will point at a binary nothing in this
repo produces.

## Add or change a producer

Local-first plane. See [../flows/02](../flows/02-producer-to-ir.md) for the
full contract.

| File | What |
| --- | --- |
| `crates/nudox-producer/src/lib.rs:267` | the `Producer` trait — read it first; it is 50 lines |
| `crates/nudox-producer/src/lib.rs:331` | `produce()` — the driver: `invoke → lower → finish → seal` |
| `crates/nudox-producer/src/oracle.rs:57` | `run_json` — use this for a subprocess oracle |
| `crates/nudox-producer-python/src/producer.rs:36` | the smallest complete implementor; copy its shape |
| `crates/nudox-producer-rust/src/lib.rs:124` | the same contract at scale, with a self-referential `Oracle` |

**Bump `ProducerId`'s version suffix whenever output shape changes**
(`crates/nudox-producer/src/lib.rs:107`). It becomes part of the job key and
gates cached-IR invalidation; getting it wrong silently serves stale IR
forever, with no runtime check that would catch it.

Note only four of seven producer crates implement the trait — `go`, `java`, and
`csharp` do not and do not depend on `nudox-producer`
([OQ-13](../open-questions.md#oq-13--are-the-go-java-and-c-producers-meant-to-implement-producer)).

## Change the IR schema

`crates/nudox-ir/` — the shared bottom of **both** planes. Highest blast radius
in the repo.

| File | What |
| --- | --- |
| `crates/nudox-ir/src/kind.rs:5` | `register_kinds!` — the `Kind` enum and its **wire discriminants**. Adding a kind means adding a `crates/nudox-ir/src/kinds/<kind>.rs` body type. |
| `crates/nudox-ir/src/kinds/` | one module per kind body (`function.rs`, `record.rs`, `trait_.rs`, `ty.rs`, …) |
| `crates/nudox-ir/src/content.rs` | **the content-hash encoding.** Every new enum variant anywhere covered here needs a new opcode. See [invariants.md](invariants.md#the-content-hash-encoding-is-wire-stable). |
| `crates/nudox-ir/src/lower.rs:126` | `Lowering` — the producer-facing sink (`declare` `:183`, `refer` `:263`, `finish` `:336`) |
| `crates/nudox-ir/src/entry.rs`, `view.rs`, `index.rs` | entry model, read views, indexes |
| `crates/nudox-ir/src/change/` | the change / lineage model |

Then check every downstream consumer: `nudox-store` (corpus), `nudox-graph`
(the Trustfall vertex mapping in `src/vertex.rs` and the SDL schema),
`nudox-engine` (chunking and rendering), `ir-vcs` (the NdIrF1 wire protocol),
and `workspace/driver/coordination/indexing.rs:1047` (`ingest_ir_bytes`, which
decodes that protocol).

## Add or change an MCP tool

Local-first plane. See [../flows/04](../flows/04-local-first-mcp.md).

| File | What |
| --- | --- |
| `crates/nudox-mcp/src/tools.rs` | the arg struct, the result struct, and the `do_*` body |
| `crates/nudox-mcp/src/server.rs:143` | the `#[tool]` wrapper under `#[tool_router]`. **The `description` string is the only documentation an agent ever sees** (`server.rs:9-11`). |
| `crates/nudox-mcp/src/key.rs` | `SymbolKeyDto` — LR-1 says the tool argument *is* `SymbolKey`, spelled `ecosystem:name#introhex` |

Constraints that are review-blocking, not style: no `serde_json::Value` in any
tool signature (LR-2); every tool must bottom out in an `EngineHandle` call and
hold no corpus of its own (LR-8). Full list:
[invariants.md](invariants.md#the-mcp-standing-decisions).

## Add a Trustfall query

`crates/nudox-graph/src/queries/` — write `<name>.trustfall`, then add an
`include_str!` const and a result-row struct in
`crates/nudox-graph/src/queries/mod.rs` (existing consts at `:23`, `:27`,
`:31`, `:35`, `:39`).

**All five existing `.trustfall` files are missing from disk and untracked**;
`.gitignore` will silently ignore your new one too. See
[gotchas.md](gotchas.md#the-build-is-broken-right-now) — you must un-ignore the
extension or the file will not be committed.

## Change the ingest pipeline

`workspace/driver/coordination/indexing.rs`, 1 637 lines, the whole pipeline in
order. See [../flows/01](../flows/01-package-ingest.md).

| Line | Function |
| --- | --- |
| `:90` | `run_indexing_job_on` — the four phases |
| `:118` / `:133` / `:179` / `:252` | `execute_{acquire,extract,compile,emit}_phase` |
| `:482` | `run_worker_until` — the dequeue loop |
| `:525` | `drive_job` — deadline, heartbeat, settle |
| `:719` | `producer_command` — the per-language guest invocation contract |
| `:892` | `run_producer_in_cage` — golden prepare/fork, budget, exec, teardown |
| `:1047` | `ingest_ir_bytes` — the NdIrF1 decode |

Fan-out is `workspace/driver/poll.rs` (`outbox_consumer` `:73`, `materialize`
`:167`). The commit point is `workspace/index/blob/emit.rs:44`.

## Change configuration

`workspace/driver/config.rs`. See [../flows/05](../flows/05-boot-and-config.md).

| Line | What |
| --- | --- |
| `:27` | `ServerConfiguration` — add the field with a `#[serde(default = "…")]` |
| `:369` | `impl Default` — add it there too |
| `:573` | `mod defaults` — the default fn |
| `:424` | `resolve()` — the figment layering; usually needs no change |
| `:455` | `validate()` — add validation here if the field can be wrong |

Env var mapping is automatic: `NUDOX_<PATH>` with `__` for nesting
(`config.rs:524`). No registration step.

## Change the search path

| Task | File |
| --- | --- |
| the wire query algebra | `workspace/heart/query.rs:264` — **the wire is the domain**; changing it is an API break |
| wire → execution lowering | `workspace/driver/http/handlers/search.rs:180` |
| precise vs semantic decision, the budget | `workspace/driver/search/planner.rs:41` |
| the two arms | `workspace/driver/coordination/search.rs:89` (precise), `:106` (semantic) |
| the store traits | `workspace/driver/search/mod.rs:32` (`SearchTarget`), `:53` (`SymbolStore`) |
| federation merge | `workspace/driver/search/mod.rs:66` |
| embeddings + Qdrant | `workspace/driver/search/semantic/mod.rs:43`, `semantic/embedder.rs` |
| package search | `workspace/driver/coordination/packages.rs:38`, `workspace/driver/search/registry.rs` |

**If you are making symbol search return results,
`workspace/driver/search/mod.rs:96` is the function to wire.** It returns
`Ok(None)` unconditionally, and that single stub is why precise search,
semantic search, `GET /symbols/:id`, and `POST /expand` all return nothing.
See [../flows/03](../flows/03-http-query.md#what-is-a-stub).

## Change the sandbox / microVM boundary

`workspace/compiler/sandbox/` — a Cargo member despite living under the dead
`workspace/compiler/` tree. `lib.rs` is `#![deny(missing_docs)]` and its module
doc states the invariants encoded in the type system. Modules: `backend`,
`budget`, `cage`, `cancel`, `cgroup`, `golden_sync`, `job`, `limits`, `node`,
`observer`, `overrides`, `probe`, `profiles`, `seal`.

Its only in-repo caller is
`workspace/driver/coordination/indexing.rs:892`.

## Add a derived store (outbox sink)

| File | What |
| --- | --- |
| `workspace/heart/sink.rs:37` | `DerivedStore` — the serving-side variant |
| `workspace/index/enums.rs:180` | `SinkKind` — the catalog `TEXT` column codec. **A different enum.** |
| `workspace/index/coordination/outbox.rs:84` / `:92` | `catalog_sink` / `serving_sink` — the bridge between them |
| `workspace/index/coordination/outbox.rs:298` | `sink_slot` — the in-process lock slot |
| `workspace/driver/poll.rs:167` | `materialize` — the upsert and delete arms |

`serve()` spawns one consumer per `heart::DerivedStore` variant automatically
(`workspace/driver/lib.rs:537`); no registration needed there.

## Run and test

```sh
cargo metadata --no-deps --locked --offline   # inspect without compiling
cargo nextest run                             # the authoritative test command
cargo check --workspace
cargo run -p driver --bin driver              # needs a reachable Qdrant
```

`cargo nextest run` is authoritative because it is exactly what the
`testrust` pre-merge-commit hook runs (`flake.nix:285-292`). There is no CI.

**Do not use the devShell `build` / `check` / `run` commands** — they pass
`--bin nudox` and no such binary exists
([gotchas.md](gotchas.md#the-devshell-build-commands-are-broken)).

## Where the two binaries are

`driver` → `workspace/driver/main.rs:20`.
`ingest` → `workspace/index/ingest/main.rs:17` — **a stub that prints a message
and exits.**
`lindsey` → `workspace/gui/src/main.rs`, a third binary in a non-member crate
with its own lockfile.

# Flow: local-first corpus → MCP

**Entry point:** `crates/nudox-mcp/src/endpoint.rs:87` — `McpEndpoint::start`
**Terminates at:** `nudox-ir` entries held in an in-memory `Corpus`
**Crosses:** a loopback HTTP socket (MCP transport) and a `LocalSet` task
boundary inside the engine. **No network, no database, no server-plane crate.**

> **This flow does not compile from a clean checkout, and nothing in this
> repository starts it.** `nudox-graph` `include_str!`s seven files that are
> absent from disk and untracked in git, which takes down `nudox-graph`,
> `nudox-engine`, and `nudox-mcp` with it — see
> [building-and-testing.md](../building-and-testing.md#the-repo-does-not-build-from-a-clean-checkout).
> Separately, `McpEndpoint::start` is called only from
> `crates/nudox-mcp/tests/endpoint.rs`; `lindsey` (`workspace/gui`) does not
> depend on `nudox-mcp` at all.

## Why this flow matters

This is the other half of the product — the local-first plane, the one under
active development, and the one that lets a coding agent read a workspace's
documentation corpus without a server, a registry, or a network. It shares
exactly one crate with the server plane (`nudox-ir`), so nothing you learn from
[flow 01](01-package-ingest.md) or [flow 03](03-http-query.md) transfers here.

It is also the plane where the dependency law is load-bearing: the whole
argument for `nudox-engine` existing is that the GUI and an agent must never be
able to disagree about what a workspace contains.

## The §L0 dependency law

Stated in `crates/nudox-engine/src/lib.rs:3-13`:

```
lindsey  →  nudox-engine  →  {nudox-graph, nudox-store}  →  nudox-ir
```

Two rules:

- **`lindsey` must not import `nudox-ir`, `nudox-store`, or `nudox-graph`
  directly.** Reaching past the engine would let a view depend on the shape of
  the IR rather than on the protocol (`workspace/gui/Cargo.toml:34-37`).
- **`nudox-engine` must not import `gpui`.**

`crates/nudox-engine/src/lib.rs:13` says both are "enforced by
`scripts/lint-gui-no-block.sh`". **That script does not exist.** There is no
`scripts/` directory in the repository (`ls scripts/` → no such file), and
`find . -name 'lint-gui-no-block.sh'` returns nothing. The law is currently
enforced by the `Cargo.toml` graph and by review, not by a lint.

The structural half does hold today: `workspace/gui/Cargo.toml` names
`nudox-engine` as its only backend dependency, and it is deliberately **not** a
member of the root Cargo workspace — it keeps its own lockfile so `gpui` stays
out of the backend dependency graph (`workspace/gui/Cargo.toml:6-10`).

`nudox-mcp` adds one arrow the diagram above omits: it depends on
`nudox-graph` as well as `nudox-engine`, for `SCHEMA_SDL` and the canned
queries (`crates/nudox-mcp/src/lib.rs:44-47`). Root `Cargo.toml:66-70` states
the full law as `lindsey → engine → {graph, store} → ir; mcp → {engine, graph}`.

## The path

1. **`crates/nudox-mcp/src/endpoint.rs:87` — `McpEndpoint::start(server)`**
   Delegates to `start_with_token(server, SessionToken::generate())` (`:94`).

2. **`crates/nudox-mcp/src/endpoint.rs:98`** — binds `LOOPBACK_BIND`
   (`endpoint.rs:56`), which is the **constant** `127.0.0.1:0`. Not a setting:
   there is no code path in the crate that binds anything else
   (`endpoint.rs:20-21`), and a `debug_assert!` at `:105` re-checks that the
   bound address is loopback. Port `0` asks the kernel for a free port, so the
   endpoint's address is only knowable after `start` returns.

3. **`crates/nudox-mcp/src/endpoint.rs:117`** —
   `Router::new().nest_service(MCP_PATH, mcp)`, where `MCP_PATH` is `/mcp`
   (`endpoint.rs:60`). `endpoint.url()` (`:143`) formats
   `http://{addr}/mcp`; `client_config_snippet()` (`:158`) renders a
   paste-ready client configuration.

   Note `nudox-mcp` pins `axum = "0.8"` while `driver` pins `"0.7"`. Two
   different axum majors in one repository, on two different planes.

4. **`crates/nudox-mcp/src/server.rs:143` — `#[tool_router]` on `NudoxMcpServer`**
   Six `#[tool]` methods, each a thin wrapper over a `NudoxTools` body. The
   `description` strings are not decoration: they are "the *only* documentation
   an LLM client ever sees" (`server.rs:9-11`), so each opens with a
   `PREFER THIS…` usage directive.

5. **`crates/nudox-mcp/src/tools.rs`** — the tool bodies. Every one bottoms out
   in an `EngineHandle` call:

   | Tool | Body | Engine call |
   | --- | --- | --- |
   | `search_symbols` (`server.rs:159`) | `do_search` (`tools.rs:291`) | `EngineHandle::search(SearchQuery, gen)`, drained to `SearchEvent::Done` |
   | `get_symbol` (`server.rs:178`) | `do_get_symbol` (`tools.rs:389`) | `EngineHandle::open_symbol(key, gen)`, drained to `DocEvent::Done` |
   | `find_usages` (`server.rs:195`) | `do_find_usages` (`tools.rs:425`) | `EngineHandle::query` with `nudox_graph::queries::FIND_USAGES` and a `key` binding |
   | `list_packages` (`server.rs:212`) | `do_list_packages` (`tools.rs:461`) | `EngineHandle::query` with `PACKAGES_QUERY` |
   | `graph_query` (`server.rs:233`) | `do_graph_query` (`tools.rs:483`) | `EngineHandle::query` with the caller's own Trustfall query |
   | `graph_schema` (`server.rs:251`) | — | none: serves `crate::SCHEMA_SDL` verbatim |

   `find_usages`, `list_packages`, and `graph_query` share `run_query`
   (`tools.rs:506`) so all three agree on column handling and truncation.

   Every streaming tool binds the returned `StreamHandle` as `_stream` and
   holds it for the drain — **dropping it cancels the stream mid-drain**
   (`tools.rs:317`, `:392`, `:508`). A stream that ends without its terminal
   event is `McpError::TruncatedStream`, never a silently short result
   (`tools.rs:335`, `:415`, `:540`). `get_symbol` additionally enforces the §9.3.1 protocol
   invariant that `Head` precedes everything: a `Done` with no `Head` is a
   broken engine stream, not an empty symbol (`tools.rs:418-420`).

6. **`crates/nudox-engine/src/runtime.rs` — `EngineHandle`**
   A Tokio multi-thread runtime plus a `LocalSet` for query execution (§L9).
   The `LocalSet` exists because the Trustfall fork's `AsyncAdapter` result
   stream is `!Send`. Every call returns `(StreamHandle, Receiver<Event>)`
   except `versions`, which is the one synchronous call — it reads data that is
   already fully resident and has nothing for a `Gen` to guard
   (`crates/nudox-engine/src/lib.rs:51-54`).

7. **`crates/nudox-graph/src/adapter.rs` — `CorpusAdapter`**
   The Trustfall `AsyncAdapter` over the corpus. Two properties worth knowing
   before you write a query:
   - **No `unwrap`/`expect` in resolution paths** (LR-6). Every fallible step
     returns `Result<_, GraphError>`; errors propagate as stream items and the
     Trustfall engine terminates the query on the first one (`adapter.rs:3-9`).
   - **Filter pushdown.** `resolve_starting_vertices` for the `Symbols`
     entrypoint inspects the `ResolveInfo` and uses
     `VertexInfo::statically_required_property` to detect equality filters on
     `key`, `name`, and `kind` *before* any iteration, routing straight to the
     corpus `entry()` lookup, the `NameIndex`, or the `by_kind` map. With no
     usable filter it falls back to full enumeration and emits a
     `tracing::debug!` so slow queries are diagnosable (`adapter.rs:17-27`).
     This is why the crate implements `AsyncAdapter` directly rather than
     `AsyncBasicAdapter`, whose `resolve_starting_vertices` never receives the
     hints (`adapter.rs:29-36`).

8. **`crates/nudox-store/src/corpus.rs:61` — `Corpus`**
   An in-memory map of `PackageLineageId → Arc<PackageView>` with async
   accessors: `insert` (`:78`), `package` (`:88`), `entry` (`:97`),
   `packages` (`:110`), `len`/`is_empty`. The accessors are `async`, which is
   what forces the adapter's `stream::once(async {…}).flat_map(…)` shape.

9. **`nudox-ir`** — the shared bottom. The only crate both planes link.

## The Trustfall fork

Root `Cargo.toml:116`:

```toml
trustfall = { git = "https://github.com/philocalyst/trustfall",
              rev = "7e71e35715dfac561e3453c834b469a200330997",
              features = ["async"] }
```

Pinned by **rev**, not branch, under the same LD-10 pin discipline as `gpui`.
Two things upstream 0.8.1 lacks justify it: a real `Adapter::Error` channel
(LR-6) and the `AsyncAdapter` stream engine, whose result stream is `!Send` and
therefore drives the engine's `LocalSet`.

`workspace/registry` separately pins `trustfall = "=0.8.1"` from crates.io
(`workspace/registry/Cargo.toml:36`).
**Two different Trustfalls coexist in one Cargo workspace** — one per plane.

## Standing decisions this crate is judged against

From `crates/nudox-mcp/src/lib.rs:34-53`. These are not style notes; a change
that violates one is a review-blocking finding.

| # | Decision |
| --- | --- |
| **LR-1** | The MCP tool argument *is* `SymbolKey`, spelled `ecosystem:name#introhex`. No parallel id type. |
| **LR-2** | No `serde_json::Value` in any tool signature. Every schema is `schemars`-derived from a named type. |
| **LR-7** | One schema. `SCHEMA_SDL` is re-exported from `nudox_graph::SCHEMA_SDL` so there is exactly one `include_str!` in the workspace; `tests/schema_source.rs` asserts byte-identity. |
| **LR-8** | `nudox-mcp` reads no `nudox-ir` type directly and holds no corpus, no index, no IR. It opens no files and walks no `Entry`. "If a tool here ever answers a question the engine could not, that is a bug." |
| **LR-11** | `session::Unauthenticated` and `session::Session` are different types, not a `bool`. |
| **LR-12 / §L7.5** | `McpError` is a `#[non_exhaustive]` `thiserror` enum. No `anyhow` outside tests. |

## What is a stub

`EngineHandle`'s own public-surface table marks five calls as stubs
(`crates/nudox-engine/src/lib.rs:33-47`):

| Call | Status |
| --- | --- |
| `search(q, gen)` | live |
| `open_symbol(key, gen)` | live |
| `open_package(id, gen)` | **stub** |
| `packages()` | live |
| `versions(pkg)` | live (the one synchronous call) |
| `select_version(pkg, v, gen)` | live |
| `resolve_project(root)` | **stub** |
| `sync()` | **stub** |
| `jobs()` | **stub** |
| `command(cmd)` | **stub** |
| `query(q, gen)` | live |
| `schema()` | live |

None of the six MCP tools touch a stubbed call, so the MCP surface itself is
not gated on them. `resolve_project` and `sync` being stubs does mean there is
no wired path that *populates* a corpus from a project on disk through the
engine — see [OQ-15](../open-questions.md#oq-15--what-populates-the-local-first-corpus).

`get_symbol` deliberately drops three `DocEvent` variants: `Highlight` spans
are a GUI-only progressive upgrade carrying nothing an agent can use, and
`Refs`/`Impls` pages are served by `find_usages` and `graph_query` instead
(`tools.rs:400-404`). That is a design choice, not a gap.

## The compile blocker

`nudox-graph` cannot build. Seven files are `include_str!`'d by committed
source, and all seven are absent from disk **and** untracked in git:

| Missing file | `include_str!` site |
| --- | --- |
| `crates/nudox-graph/schema.graphql` | `crates/nudox-graph/src/lib.rs:31` and `:51` |
| `crates/nudox-graph/src/queries/find_symbol_by_key.trustfall` | `crates/nudox-graph/src/queries/mod.rs:23` |
| `crates/nudox-graph/src/queries/list_package_functions.trustfall` | `…/mod.rs:27` |
| `crates/nudox-graph/src/queries/find_usages.trustfall` | `…/mod.rs:31` |
| `crates/nudox-graph/src/queries/find_implementors.trustfall` | `…/mod.rs:35` |
| `crates/nudox-graph/src/queries/symbols_mentioning_type.trustfall` | `…/mod.rs:39` |
| `workspace/index/ecosystem/assets/cpp_alias_seed.ron` | `workspace/index/ecosystem/cpp/alias.rs:65` |

`include_str!` resolves at compile time, so this is a hard build error, not a
runtime one. `find_usages` — one of the six tools — references
`nudox_graph::queries::FIND_USAGES`, which is one of the missing files.
Full detail and root cause in
[building-and-testing.md](../building-and-testing.md#the-repo-does-not-build-from-a-clean-checkout).

## Failure modes

| What fails | What an agent sees | Notes |
| --- | --- | --- |
| `kinds` names an unknown symbol kind | `McpError::InvalidArgument { argument: "kinds" }` listing the valid names | `tools.rs:302-310` |
| A malformed `SymbolKey` | `InvalidArgument` (`invalid_params`), **not** an empty result set — `find_usages` validates the key before running the query specifically to avoid that confusion | `tools.rs:429-431` |
| Empty `graph_query` | `InvalidArgument`, "call graph_schema for the queryable types" | `tools.rs:489-493` |
| The engine stream ends without its terminal event | `McpError::TruncatedStream` | `tools.rs:335`, `:415`, `:540` |
| `Done` arrives with no preceding `Head` | `TruncatedStream` | `tools.rs:420` |
| The engine emits `Failed` | `McpError::Engine(error)` carrying the engine's typed error | `tools.rs:329`, `:409` |
| The result exceeds the limit | truncated, with `truncated: true` on the result — never silently cut | `SearchResult`/`UsagesResult`/`QueryResult` all carry the flag |
| Trustfall resolution error | the Trustfall engine terminates the query on the first error item | `adapter.rs:3-9` |

## Where to start reading

1. `crates/nudox-mcp/src/lib.rs` — 80 lines of module doc that state the whole
   design and every standing decision.
2. `crates/nudox-mcp/src/tools.rs` — the six tool bodies. `do_search`
   (`:291`) shows the drain pattern all of them use.
3. `crates/nudox-engine/src/lib.rs:33-47` — the `EngineHandle` surface table,
   including which calls are stubs.
4. `crates/nudox-graph/src/adapter.rs` — read the module doc before writing a
   Trustfall query; the pushdown rules determine whether your query is O(1) or
   O(corpus).

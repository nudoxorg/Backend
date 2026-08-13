# Architecture

## The shape in one paragraph

Packages from eight language ecosystems go in as source archives; a shared
intermediate representation comes out, indexed and queryable. A package is
named by coordinates (`ecosystem`, `name`, `version`), acquired from its
upstream registry, extracted under a sanitizing allowlist, compiled to IR by a
language producer running inside an ephemeral microVM, written to a
content-addressed blob store, and fanned out through a transactional outbox to
derived stores — a vector index in Qdrant and a full-text package index in
Tantivy. State lives in a versioned SQLite catalog (DoltLite): the package
lifecycle, the job queue's durable half, the outbox, and the sink watermarks.
Queries arrive over HTTP as a single query algebra and leave as NDJSON or paged
JSON. A second, largely independent plane serves the same IR to a desktop GUI
and to coding agents over MCP, entirely locally, with no registry and no
network.

## Two planes

The single most important structural fact about this repository: **there are
two largely disjoint dependency trees, and they share exactly one crate.**

They are not two layers of one system, and neither supersedes the other. They
were built for different deployment shapes — one hosted and federated, one
on-your-laptop — and they arrived at similar problems by different routes. A
reader who assumes a single pipeline will spend a day trying to find how
`nudox-store` feeds `driver`. It does not.

**Server plane — `workspace/*`.** The HTTP service, the catalog, ingest, and
the IR-native VCS.

```
        heart  ←  transport  ←  index
          ↑           ↑           ↑
          └──  registry           ir-vcs
                  ↑                ↑
                  └───  driver  ───┘     ← the axum binary
                          ↑
                       sandbox
```

**Local-first plane — `crates/nudox-*` (+ `workspace/gui`).** The GPUI app and
the MCP server.

```
 nudox-ir ← nudox-store ← nudox-graph ← nudox-engine ← { nudox-mcp, lindsey }
                ↑
      nudox-producer, nudox-producer-{rust,typescript,python,clang,go,java,csharp}
```

**The shared crate is `nudox-ir`.** It is a path dependency of `driver`,
`index`, `registry`, and `ir-vcs`, and in all four it is **aliased as `ir`**:

```toml
ir = { package = "nudox-ir", path = "../../crates/nudox-ir" }
```

(`workspace/driver/Cargo.toml:55`, `workspace/index/Cargo.toml:93`,
`workspace/registry/Cargo.toml:34`, `workspace/ir-vcs/Cargo.toml:25`.)

The producer crates are consumed **only** by the local-first plane —
`nudox-store` links `nudox-producer-rust`, and nothing under `workspace/`
depends on any `nudox-producer-*` crate. The server plane produces IR by
exec'ing a guest binary inside a microVM instead; see
[flows/02](flows/02-producer-to-ir.md#two-producer-planes).

### Server plane — `workspace/`

Verified by `cargo metadata --no-deps --locked --offline` and by
`find … -name '*.rs' | xargs wc -l`, both executed during writing.

| Crate | Path | Lib | Bins | Int. tests | LOC | Role |
| --- | --- | --- | --- | --- | --- | --- |
| `heart` | `workspace/heart` | `heart` | — | 7 | 7 287 | shared vocabulary: `Query`, `Page`, `Scored`, `Sourced`, `Symbol`, `Language`, ids, `ResolutionState`, telemetry, the sync seam |
| `index` | `workspace/index` | `index` | `ingest` | 8 | 45 073 | the catalog and data plane: DoltLite schema v4, `MetaStore`, outbox, queue, blob/CAS, ecosystem grammars, search, metadata, bakery, upstream followers |
| `registry` | `workspace/registry` | `registry` | — | 0 | 14 817 | the graph (Trustfall) and vector planes |
| `ir-vcs` | `workspace/ir-vcs` | `ir_vcs` | — | 0 | 25 551 | IR-native VCS over a libpijul fork: patches, diff, semver, serve, the NdIrF1 stream protocol |
| `transport` | `workspace/transport` | `transport` | — | 0 | 803 | the one abstracted iroh/bao provide-fetch |
| `driver` | `workspace/driver` | `driver` | **`driver`** | 18 | 15 321 | the composition crate and the axum binary — the only place `index` + `registry` + `heart::client` are composed |
| `sandbox` | `workspace/compiler/sandbox` | `sandbox` | — | 3 | 8 572 | `SmolvmCage`, `CapabilityBudget`, `SealedCommand`, the typestate layer |
| `rusqdoltlite` | `workspace/vendor/rusqdoltlite` | lib | — | 1 | — | vendored DoltLite bindings; an implicit member via path dep |

**`heart`** is the bottom: it has **zero internal path dependencies**. That is
what lets both `index` and `registry` link it without creating a cycle, and
what keeps the wire vocabulary (`heart::query::Query`, `heart::Page`) free of
any storage concern. It is also where the `Query` type lives that
[the API contract](api-contract.md#the-query-algebra--heartqueryquery) is
defined by — the wire *is* the domain, and this crate owns both.

**`index`** is by a wide margin the largest crate, and it holds most of what a
newcomer will need. `index::catalog` is the versioned relational spine;
`index::coordination` the outbox and its watermarks; `index::queue` the durable
job queue; `index::cas` the content-addressed store; `index::ecosystem` the
per-language name/version/archive grammars; `index::search` the package tantivy
index and the usage-query backend; `index::pack` the ObjectPack container
family. It builds the workspace's other binary, `ingest` — which is a stub that
prints a message and exits (`workspace/index/ingest/main.rs:17`).

**`driver`** is the serving crate: HTTP router and handlers, the four-phase
indexing pipeline, the background poll loops, config resolution, session
graphs, the archive sanitizer, the search planner and semantic surface, the
bakery worker, and the federation sync driver. It is the dissolved `server`
crate rewired onto the re-layered planes, and it carries the compatibility
facade that makes that rewiring invisible to the moved code — see
[The `registry` facade](#the-registry-facade-inside-driver).

**Exactly two binaries exist in the whole Cargo workspace: `driver` and
`ingest`.** (`lindsey` is a third, in a non-member crate — see below.)

### Local-first plane — `crates/nudox-*`

| Crate | Lib | Int. tests | LOC | Role |
| --- | --- | --- | --- | --- |
| `nudox-ir` | `nudox_ir` | 2 | 16 595 | **the shared IR data model.** Entry/symbol/kind/wire/change/view/vocab, `Lowering`, `PristineIntroTable`. Carries its own minimal `body::Language`, deliberately not `heart::Language`, so it need not depend on `heart`. |
| `nudox-store` | `nudox_store` | 2 | 4 600 | the in-memory `Corpus`, package views, name/kind indexes, and the `ProducerSource` that drives `nudox_producer::produce` |
| `nudox-graph` | `nudox_graph` | 6 | 2 353 | the Trustfall `AsyncAdapter` over the corpus, the SDL schema, and five canned queries. **Does not compile — see below.** |
| `nudox-engine` | `nudox_engine` | 9 | 15 858 | the streaming facade between IR and the GUI: chunking, doc rendering, search fan-out, query execution on a `LocalSet`, versions, timelines |
| `nudox-mcp` | `nudox_mcp` | 5 | 4 689 | the MCP server: six tools, loopback-only, `/mcp` |
| `nudox-producer` | `nudox_producer` | 0 | — | the `Producer` trait and `produce()` |
| `nudox-producer-{rust,typescript,python,clang}` | lib | 0–1 | — | implement `Producer` |
| `nudox-producer-{go,java,csharp}` | lib | 0–1 | — | **do not** implement `Producer`, and do not depend on `nudox-producer` |
| `lindsey` | `workspace/gui` | — | 28 024 | the GPUI desktop app. **Not a Cargo workspace member** — it keeps its own lockfile so `gpui` stays out of the backend dependency graph, and reaches `nudox-engine` by path (`workspace/gui/Cargo.toml:6-10`). |

Producer crates total 31 546 lines of Rust across all seven.

#### The §L0 dependency law

`crates/nudox-engine/src/lib.rs:3-13` states it:

```
lindsey  →  nudox-engine  →  {nudox-graph, nudox-store}  →  nudox-ir
```

with `nudox-mcp → {nudox-engine, nudox-graph}` (root `Cargo.toml:66-70`). Two
prohibitions: `lindsey` must not import `nudox-ir`, `nudox-store`, or
`nudox-graph` directly, and `nudox-engine` must not import `gpui`.

The doc comment says both are "enforced by `scripts/lint-gui-no-block.sh`".
**That script does not exist** — there is no `scripts/` directory in the repo.
The law is enforced today by the `Cargo.toml` graph (which does hold) and by
review.

#### `nudox-graph` cannot compile

Seven files are `include_str!`'d by committed source and are absent from disk
*and* untracked in git. `include_str!` resolves at compile time, so
`nudox-graph` fails to build, and `nudox-engine`, `nudox-mcp`, `lindsey`, and
`index` fall with it. Full detail, evidence, and root cause in
[building-and-testing.md](building-and-testing.md#the-repo-does-not-build-from-a-clean-checkout).

### What the two planes share, and why the split exists

`nudox-ir` and nothing else. Concretely: both planes agree on what a symbol,
an entry, a kind, and a stable reference *are*, and disagree about everything
else — how packages are acquired, where state lives, how queries are expressed,
and what a "producer" is.

The split is a deployment split, not a layering one. The server plane assumes a
catalog, an object store, a vector database, an embeddings service, and a
federation of registries. The local-first plane assumes a directory and a
process. Neither can be expressed as a special case of the other without
dragging the whole store stack into a desktop app or a `LocalSet` into a
horizontally-scaled service.

The two even pin **different versions of the same dependency**: the local-first
plane uses a `trustfall` git fork pinned by rev (root `Cargo.toml:116`), because
upstream 0.8.1 lacks a real `Adapter::Error` channel and the `AsyncAdapter`
stream engine; `workspace/registry` pins `trustfall = "=0.8.1"` from crates.io
(`workspace/registry/Cargo.toml:36`). Two Trustfalls, one workspace.

## Data stores

**There is no TerminusDB and no Postgres.** Both were removed. `grep -rni
'terminusdb|postgres' --include=*.rs` returns 184 hits and every one is a
comment or an identifier name; no workspace `Cargo.toml` declares either
dependency. The README still describes them (see
[The README is fiction](#the-readme-is-fiction)).

| Store | Crate / dep | Role | Configured by |
| --- | --- | --- | --- |
| **DoltLite** (versioned SQLite) | `rusqdoltlite`, vendored, path dep of `index` under feature `dolt-engine` | The catalog: schema v4, `MetaStore`, migrations, the outbox, `sink_watermarks`, the bakery ledger. Opened **from a local directory** — no URL, no credentials. | `endpoints.catalog_directory` (default `./data/catalog`); the file is `catalog.dolt` inside it |
| **rusqlite** (bundled) | `index::scratch` | The ephemeral scratch store: the job queue, claims, `wanted`, and session graphs. `scratch.sqlite`, delete-anytime, never replicated. | co-located under `data_directory` |
| **Tantivy** | `index` | The replica-local **package** search index. The *symbol* text index was removed. | `data_directory/packages` |
| **Qdrant** (`qdrant-client`) | `index`, `registry`, `driver` | Vector store for symbol embeddings. The collection schema is bound to the compiled-in model brand. | `endpoints.qdrant` (default `http://127.0.0.1:6334`) |
| **`object_store`** | `index::cas` | Content-addressed blob store: `cas/`, `ptr/`, `compiled/`. The vendored build enables **no cloud features**, so `file://` and `memory://` are the schemes this deployment speaks. | `endpoints.object_store` |
| **iroh / iroh-blobs** | `transport`, used by `index::pack` and `ir-vcs::sync` | Content transfer between nodes | `SourceConfig.sync_endpoint` (hex `EndpointId`) |
| **libpijul** (vendored fork) | `ir-vcs` | IR-native VCS: patches, diff, semver, serve | — |
| **Embeddings endpoint** (HTTP) | `driver` via `embedrs` | OpenAI-compatible `/v1/embeddings` | `endpoints.embeddings` |
| **Rerank endpoint** (HTTP) | `driver` | External cross-encoder, TEI/Cohere wire shape | `[rerank] endpoint` — unset by default |

Who writes and who reads:

- The **catalog** is written by the ingest pipeline (`advance`, `record_stored`)
  and by `ensure_initialized`; read by every handler that resolves a package,
  by the queue worker, and by the outbox consumers.
- The **scratch store** is written and read only in-process. Two connections to
  the same file (queue and sessions) is fine — SQLite serializes.
- The **object store** is written once per successful job, in
  `emit::emit`, and read by `POST /v1/compiled/lookup`, the admin verify path,
  and freshness recomputation.
- **Qdrant** is written by the `SinkKind::Vector` outbox consumer and read by
  the semantic search arm.
- The **package tantivy index** is written by `package_index_poller` (which
  reads the Text-sink outbox rows under its own watermark) and read by
  `POST /packages/search`.
- The **graph sink** is written by nobody. `SinkKind::Graph` fan-out is a no-op
  (`workspace/driver/poll.rs:199`).

The two `[patch.crates-io]` entries (root `Cargo.toml`) both point into
`workspace/vendor/`: `libpijul` to a fork with a regenerable patch at
`workspace/vendor/libpijul-fork.patch`, and `qdrant-edge` to 0.7.2 verbatim
plus a raised `recursion_limit`.

## Identity and hashing

Identity in this system is **deterministic and derived**, not allocated. That
is what lets two independent replicas index the same package and agree on every
id without coordinating.

| Id | Derived from | Notes |
| --- | --- | --- |
| `SourceId` | `heart::Id::from_name(NAMESPACE, source.name)` (`workspace/driver/config.rs:399`) | which is why source names must be unique — `validate()` enforces it |
| `PackageId` | the package coordinates, via `coordinates.id()` | UUID. `POST /packages` returns it. |
| `SymbolId` | the symbol's identity **salted with the instance token** | UUID |
| `ContentHash` | BLAKE3 of the bytes. The blob snapshot is `ContentHash::of_bytes(manifest.identity_bytes())` (`workspace/driver/coordination/indexing.rs:264`) | 32 bytes, rendered lowercase hex |
| `JobKey` | a 32-byte BLAKE3 digest; the wire form is 64 **lowercase** hex chars, validated at deserialize time (`workspace/driver/http/dto.rs:130`) | the no-double-compile handshake key |
| `IntroId` | the IR-plane introduction identity of a symbol | rendered lowercase hex in `StableReference` |
| `StableReference` | `F:<ecosystem>/<package>#<intro_hex>` (`workspace/heart/query.rs:48`) | the frozen cross-package grammar. `<package>` may contain `/`; `#` is the sole terminator; `<hex>` must be lowercase. |
| `SymbolKey` (MCP) | `ecosystem:name#introhex` | the local-first plane's spelling of the same idea. LR-1: the tool argument *is* this type. |

### The instance token, and the footgun in it

`Endpoints::instance_token()` (`workspace/driver/config.rs:514`) is literally
`format!("{}/{}", instance_organization, instance_database)` — `nudox/registry`
by default. It is passed to `InstanceToken::new` at assembly
(`workspace/driver/lib.rs:389`) and salts every deterministic symbol id minted
against that source.

**Changing `instance_organization` or `instance_database` re-salts every
derived symbol id.** Every symbol previously indexed becomes unreachable by its
old id: existing Qdrant points, existing links, existing session graphs all
point at ids the server will never mint again. The config field's own doc
comment says it — "must be stable, since changing it re-salts every derived id"
(`config.rs:201-202`). These two keys are historical: they were the TerminusDB
organization and database. **The graph database is gone; the salt remains.**

## Federation

The server fronts a `heart::Federation<SourceStores<M>>`: exactly one
**definitive base** plus zero or more **overlays** in declared precedence
order, highest first (`workspace/driver/lib.rs:184`, config at
`config.rs:34-37`).

Each source is a complete, independently-verified stack — its own catalog, blob
store, queue, outbox, Qdrant collection, and package index
(`SourceStores`, `workspace/driver/lib.rs:158`). Assembly brings every source
up before the server exists, so **any single source failing to connect aborts
boot**, overlays included.

Reads resolve **overlay-first**: `federation().in_precedence()` yields overlays
before the base, and `merge_overlay_first`
(`workspace/driver/search/mod.rs:66`) lets the first source to claim a key win,
then ranks the survivors by score and truncates to the limit. That is the
override semantics — a self-hosted overlay can shadow a definitive package.

Writes go to the base only: `ensure_initialized` uses `self.base()`
(`workspace/driver/coordination/initialization.rs:99`), and the accessors
`global_store()`, `blobs()`, `queue()`, `outbox()`, `semantics()` all delegate
to the base (`workspace/driver/lib.rs:449-461`). Federated flows walk
`federation()`; single-source flows use the accessors.

`Sourced<T>` (`workspace/heart/access/federation.rs:17`) is the tag a federated
read carries back: `{ value, source: SourceId, role: "Definitive" | "Overlay" }`.
`GET /symbols/:id` returns `Sourced<Symbol>` for exactly this reason — so a
caller can tell which source answered.

Degradation is a **runtime** state, not a boot-time one: `/readyz` reports a
down overlay as `degraded` while still answering `200`, and only returns `503`
once the definitive base is down (`workspace/driver/http/handlers/health.rs:26`).

## The `registry` facade inside `driver`

This will confuse anyone reading `driver` for the first time, so it is worth
one focused paragraph.

`workspace/driver/lib.rs:35` declares a crate-local `pub mod registry` that
**shadows the extern-prelude `registry` crate**. Both bare `registry::…` and
`crate::registry::…` inside `driver` resolve to that facade, not to the real
crate. The real crate is imported under an alias:

```toml
xregistry = { package = "registry", path = "../registry", … }
```

(`workspace/driver/Cargo.toml:36`) and is reachable as `::registry`. Likewise
`::index` and `::heart`.

The facade exists because ~13 k lines of moved composition code were authored
against the *old monolithic* `registry` surface, before the re-layering split
it three ways. Rather than rewrite every `use`, the facade re-projects the old
paths onto the new crates (`workspace/driver/lib.rs:35-84`):

| Old path | Now resolves to |
| --- | --- |
| `registry::{blob, cas, catalog, coordination, queue, search, metadata, package, pack, …}` | `::index::*` |
| `registry::index` | `::index::catalog` |
| `registry::store` | `::index::cas` |
| `registry::runtime` | `::index::runtime` + `crate::session` |
| `registry::ingest` | `crate::ingest` (driver-local — it was not salvaged into `index`) |
| `registry::{graph, vector}` | `::xregistry::{graph, vector}` |

There is a second facade, `pub mod vector` (`lib.rs:78`), flattening
`::xregistry::vector::core::*` back to the top level so `vector::shard`,
`vector::quant`, etc. keep resolving.

Two more `driver` conventions worth knowing:

- **`pub type Server<M> = Driver<M>`** (`workspace/driver/lib.rs:135`). Both
  names appear freely in the source and mean the same type. `Driver` is the
  composition struct; `Server` is what the moved modules call it.
- **Files in `workspace/driver/` are indented with tabs; `crates/nudox-*` uses
  spaces.** `rustfmt.toml` is shared, so this is a per-tree convention, not a
  setting.

## Dead and vestigial code

This section is load-bearing. It is what stops the next reader spending a day
in `workspace/compiler/`.

| Path | Status | Evidence |
| --- | --- | --- |
| `compiler/` (repo root) | **Dead.** `[package] name = "nudox"`, not a workspace member, depends on `../deno_doc` (which does not exist) and on a `workspace = true` `ir` dep that no longer resolves. Contains only `src/core/` + `src/error.rs`. | absent from `cargo metadata`; `ls ../deno_doc` → no such file |
| `ir/` (repo root) | **Dead.** Two loose files, no `Cargo.toml`, no parent module. | `ls ir/` |
| `workspace/ir/` | **Deleted** 2026-07-27 in `6eec3387` — "refactor: delete workspace/ir — the migration is complete (-7,964 LOC)". `git show --stat` reports 33 files and 8 030 deletions. `crates/nudox-ir` replaced it. | `git show 6eec3387` |
| `workspace/compiler/` (except `sandbox/`) | **Buck2-only and unbuildable.** Not a Cargo member; `workspace/compiler/BUCK:18` calls `member("ir")` → `//workspace/ir:ir`, which no longer exists. Holds the seven-language oracle pipeline, tree-sitter extraction, renderers, and the `producer-worker` / `compiler-daemon` binaries. | `workspace/compiler/BUCK`, `build/rust.bzl:12-18`; `ls workspace/ir` → no such file |
| `workspace/server/` | **Gone.** Dissolved into `driver`. Still referenced by `workspace/default.nix:36` and `:54`, which is why three Nix packages fail to evaluate. | `ls workspace/server` → no such file |
| `workspace/vendor/` | vendored third party: `libpijul` (fork), `qdrant-edge`, `rusqdoltlite`. Live, but not repo code. | root `Cargo.toml` `[patch.crates-io]` |
| `.research/` | scratch. Not part of any build. | — |
| ~15 root `*-PLAN.md` / `*-NOTES.md` files | **Intent, not description.** Several describe systems that were built and then removed. | see below |

**`workspace/compiler/` is the trap most likely to send a reader wrong.** It is
large, thoroughly commented, and describes a producer pipeline in detail — but
it is a *previous generation* of the producer plane, reachable only through a
broken Buck2 build. The live producer plane is `crates/nudox-producer*`, and
the *running* producers on the server plane are guest binaries inside OCI
images that this repository does not build. See
[flows/02](flows/02-producer-to-ir.md#two-producer-planes).

### The plan files

`ECOSYSTEM-PLAN.md`, `INDEX-PLAN.md`, `IR-NORTH-STAR.md`, `SMOLVM-PLAN.md`,
`LIBRARIFICATION-PLAN.md`, `GUI-PLAN.md`, `GUI-LOCAL-PLAN.md`,
`REGISTRYLESS-PLAN.md`, `CONTINUITY-PQGRAM-PLAN.md`, `LOCAL-REMOTE-PLAN.md`,
`IR-MIGRATION-PLAN.md`, `GLOBAL-IR-GRAPH.md`, `CONSOLIDATION-NOTES.md`,
`IR-NOTES.md`, `architecture-idea.md`, `more.md`, `yo.md`, and
`workspace/IR-GUI-AND-PATH.md`.

They are cited throughout the source by section (INDEX-PLAN §9, SMOLVM-PLAN
§5.1, LR-6, ID-8, §L0) and those citations are useful — they explain *why* a
decision was made. Do not read them as descriptions of what exists. Several
describe systems that were built and then removed, and the code always wins.

### The README is fiction

`README.md` at the repo root is substantially wrong. Do not copy from it and do
not "modernize" it — write fresh from source. Every row below was checked
against the tree at write time.

| README claim | Line | Reality |
| --- | --- | --- |
| "Built with Buck2. No Cargo workspace." | `README.md:11` | a 21-member Cargo workspace exists and is authoritative; Buck2 is stale and partly broken |
| graph store is TerminusDB | `:6`, `:27` | no graph store at all; TerminusDB removed |
| global index is Postgres | `:27`, `:41` | DoltLite, a versioned SQLite opened from a directory |
| crates `runtime`, `cas`, `util/caching`, `util/sandbox`, `server`, `compiler/intermediate-representation` | `:41-45` | none exist. Folded into `heart` / `registry` / `index`, or deleted. |
| producers "Rust, TypeScript, Go, Java, Python, Nix" | `:5`, `:19` | seven producer crates: rust, typescript, go, java, python, csharp, clang. **There is no Nix producer crate** (though `producer_command` has a Nix arm). |
| Buck2 aliases `//:server //:runtime //:cas //:caching` | `:82-84` | root `BUCK` declares only `heart`, `ir`, `registry`, `compiler`, `sandbox` |
| config keys `terminus`, `terminus_organization`, `terminus_database`, `terminus_user` | `:129-132` | replaced by `instance_organization` / `instance_database` (now only an id salt) and `catalog_directory` |
| "All routes are defined in `workspace/server/http/router.rs`" | `:164` | `workspace/server/` does not exist; routes are in `workspace/driver/http/router.rs` |
| the route table | `:170-172` | missing `/usages`, `/expand`, `/symbols/:id`, `/sessions/:id`, `/admin/*`, `/v1/compiled/lookup`, `/v1/depshards/…`, `/v1/rerank`. Says `/search` returns JSON; it returns NDJSON. |
| "We use Radicle for hosting" | `:247` | `.gitmodules` and the flake inputs point at Forgejo (`dev.nudox.org`). Whether Radicle is still used alongside is [OQ-8](open-questions.md#oq-8--is-radicle-still-used-for-hosting-alongside-forgejo). |

The README is right about two things worth keeping: CAS blob GC is not
implemented (`:204`), and `/search/semantic` requires Qdrant plus embeddings
(`:213`).

## Mid-migration state

Read this as a changelog. It lets you date any comment you find.

| When | What | Evidence |
| --- | --- | --- |
| — | TerminusDB removed. The graph store is gone; the `{org}/{db}` config keys survive as an id salt. | `config.rs:196-204` |
| — | Postgres removed. Comments and `CHECK`-domain references remain throughout `heart` and `index`. | `grep` |
| — | The replica-local **symbol** tantivy index removed. Precise symbol search became `stream::empty()`; symbol identity lookup became `Ok(None)`. The **package** index survives. | `workspace/driver/search/mod.rs:8-11` |
| — | The long-lived compiler daemon removed (SMOLVM-PLAN). `compiler_client` deleted; IR is produced in an ephemeral `SmolvmCage` per job. `config.compiler_endpoint` survives, read by nothing. | `workspace/driver/lib.rs:94-96` |
| — | `workspace/server` dissolved into `driver`, with the `mod registry` facade added so the moved code kept compiling. `workspace/default.nix` was not updated. | `workspace/driver/lib.rs:17-33` |
| — | The old monolithic `registry` split three ways into `index` (data), `registry` (graph + vector), and `heart` (client vocabulary). | root `Cargo.toml:8-10` |
| — | `caching`, `cas`, `telemetry`, `version` folded into `heart`; `runtime`, `protocol` folded into `registry`. | root `Cargo.toml:8-10` |
| 2026-07-15 | Buck2 files last touched (`BUCK`, `.buckconfig` in `25061c36`; `build/rust.bzl` in `0e3a3fde`). | `git log` |
| 2026-07-27 | `workspace/ir` deleted in `6eec3387` (33 files, −8 030 lines), twelve days after Buck2 was last touched — which is exactly why Buck2's `member("ir")` dangles. `Cargo.toml` last touched the same day. | `git show 6eec3387` |
| — | `sandbox` dropped from the Cargo workspace, then rejoined it (SMOLVM-PLAN). Both statements are in `Cargo.toml`, at `:10` and `:36`. | root `Cargo.toml` |
| 2026-08-12 | `main` = `b388f36d`, "Merge branch 'feat/lindsey-local-first' into main" — the local-first plane's most recent landing. | `git log` |

Branches present locally: `main`, `canonical`, `clang`, `ir-kinds-flesh-out`,
`ir-refactor`, `qdrantdb`. The `linkml` submodule is declared in `.gitmodules`
(→ `https://github.com/philocalyst/linkml`) but the `linkml/` directory is
absent from the working tree — [OQ-9](open-questions.md#oq-9--why-is-the-linkml-submodule-declared-but-absent).

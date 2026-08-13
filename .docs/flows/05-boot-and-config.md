# Flow: boot and configuration

**Entry point:** `workspace/driver/main.rs:20` — `main`
**Terminates at:** `axum::serve` bound to `config.serving_address`, with the
role-gated background loops running
**Crosses:** the filesystem (config file, catalog dir, scratch dir, blob dir,
package-index dir), Qdrant over gRPC, and — only if OTLP is configured — a
telemetry collector

## Why this flow matters

Every store handle, every background loop, and every default a query sees is
decided here, once, before the first request. Assembly is deliberately
all-or-nothing: the server is only constructible after every backing store of
every federated source has connected, so "served a request against a store that
wasn't up" is a compile error rather than a runtime one
(`workspace/driver/lib.rs:5-8`). If you are debugging why a deployment answers
nothing, the answer is usually in this flow, not in the query path.

## The path

1. **`workspace/driver/main.rs:20` — `main`**
   `#[tokio::main]`, returning `anyhow::Result<()>`. The global allocator is
   `mimalloc` (`main.rs:16-17`). The compiled-in embedding model is
   `driver::vector::JinaCodeV2` (`main.rs:13`) — the whole server is
   monomorphized over this brand, so a store built for a *different model* (not
   merely a different dimension) cannot be wired in. Switching models is a
   deliberate recompile plus a migration.

2. **`workspace/driver/config.rs:424` — `ServerConfiguration::resolve()`**
   Three layers, merged through `figment`, increasing precedence:

   1. `ServerConfiguration::default()` (`config.rs:369`) — localhost dev
      defaults, serialized as the base fragment.
   2. A TOML file: `$NUDOX_CONFIG` if set, otherwise `nudox.toml` in the
      current directory (`config.rs:429-431`). **Only if `path.is_file()`** —
      a missing file is silently skipped, not an error. Parsed with the `toml`
      crate and merged as a fragment, because the vendored `figment` build
      carries no `toml`/`env` provider features (`config.rs:420-423`).
   3. `NUDOX_*` environment variables, folded by `environment_fragment`
      (`config.rs:524`).

   Then `figment.extract()` and `validate()` (`config.rs:443-445`).

3. **`workspace/driver/config.rs:524` — `environment_fragment`**
   Strip the `NUDOX_` prefix, split the remainder on `__`, lowercase each
   segment, and use that as a nested field path (`insert_nested`,
   `config.rs:540`). Values are parsed as JSON where possible — numbers,
   booleans, arrays, objects — with a bare-string fallback (`config.rs:533`).
   `NUDOX_CONFIG` is excluded because it names the file layer, not a field
   (`config.rs:528-531`).

   ```sh
   NUDOX_SERVING_ADDRESS=0.0.0.0:8080
   NUDOX_ROLE=forge
   NUDOX_LIMITS__MAX_INFLIGHT_JOBS=4
   NUDOX_MIRROR__FOLLOW='["csharp","rust"]'
   NUDOX_DEFINITIVE__ENDPOINTS__QDRANT=http://qdrant.internal:6334
   ```

4. **`workspace/driver/config.rs:455` — `validate()`**
   Called twice: once at the end of `resolve()` (`config.rs:444`) and again at
   the top of `Driver::assemble` (`workspace/driver/lib.rs:261`). It checks
   exactly two things:
   - Every source name (definitive + overlays) is non-empty and unique. Names
     seed the deterministic `SourceId` via `heart::Id::from_name`
     (`config.rs:399-401`), so a duplicate would collapse two sources into one
     identity.
   - The `rerank.model_id` is not on the license deny-list. A CC-BY-NC reranker
     is a hard boot error, checked **even when no rerank endpoint is
     configured**, so a forbidden id never lies dormant waiting to be activated
     (`config.rs:468-478`).

   **The production credential boot guard described in the doc comment is not
   implemented.** `config.rs:451-454` says "when `Deployment::Production` any
   source whose URL or TerminusDB password is still the well-known development
   default is a hard error", and
   `ConfigValidationError::DefaultCredentialInProduction` exists
   (`config.rs:663`) — but `validate()` never reads `self.deployment`, and
   `grep -rn DefaultCredentialInProduction --include=*.rs` finds exactly one
   hit, the variant declaration. **Setting `deployment = "production"` changes
   nothing today.** See
   [OQ-16](../open-questions.md#oq-16--was-the-production-credential-boot-guard-removed-or-never-written).

5. **`workspace/driver/main.rs:39-41`** — telemetry.
   `TelemetryConfig::resolve(service_version, "buck2-or-cargo")` then
   `heart::telemetry::init`. The guard is held for the lifetime of `main` so
   its `Drop` flushes and shuts down every provider on exit. It runs **after**
   config resolves (env is authoritative for both) and **before** anything that
   could emit a `tracing` event, so nothing during assembly is lost.

   `heart::telemetry::init` also installs the `metrics` recorder that
   `GET /metrics` renders and the W3C TraceContext propagator. `service_version`
   is `option_env!("CARGO_PKG_VERSION")` with an `"unknown"` fallback, and the
   build-system field is the literal `"buck2-or-cargo"` — both are documented
   placeholders pending a real build-version injection (`main.rs:32-38`).

6. **`workspace/driver/lib.rs:260` — `Driver::assemble(config)`**
   In order:
   - `config.validate()?`
   - `http::dto::register_custom_registries(&config.custom_registries)` —
     installs the process-wide origin lookup table (`http/dto.rs:78`).
   - `connect_source(&config.definitive)` — the federation's required root.
   - `ObjectCompiledStore::new(base.blobs.backend())` — the compiled-lookup
     store rides the base's own object-store backend, one namespace for
     `cas/`, `ptr/`, and `compiled/`.
   - each overlay in declared order, `federation.with_overlay(...)`.
   - `HttpEmbedder::new(endpoints.embeddings, endpoints.embeddings_api_key)`
     and a 65 536-entry `EmbeddingCache` (`lib.rs:149`). Note both come from
     the **definitive base's** endpoints only (`lib.rs:277`).
   - `create_dir_all(definitive.data_directory())`, then open
     `scratch.sqlite` there and wrap it as `ScratchSessionStore`. Opening the
     store creates the `sessions` table — there is no separate migrate step.
   - load `Heuristics` if `metadata_data_dir` is set. **A configured-but-
     unloadable directory aborts assembly** rather than silently degrading to
     name-only facets (`lib.rs:305-313`).
   - construct `CatalogEdgepackStore` iff `bakery.enabled || depshards.enabled`
     (`lib.rs:318`). `depshards.enabled` defaults `true`, so this is normally
     `Some`.
   - `usage_backend: ReverseIndexUsageBackend::empty()` (`lib.rs:339`) — see
     [What is a stub](#what-is-a-stub).

7. **`workspace/driver/lib.rs:358` — `connect_source(cfg)`** (per source)
   - `create_dir_all(endpoints.catalog_directory)`, then
     `CatalogEngine::open(catalog_directory/catalog.dolt)` and
     `index::migrations::runner::migrate_to_v4(&engine)`. `CatalogEngine` is
     `index::engine::dolt::DoltEngine` (`lib.rs:154`) — the serving binary is
     versioned-catalog-only; the in-memory facade is test-only.
   - one `Arc<CatalogWriter>` shared by every relational store of this source
     (single-writer, INDEX-PLAN ID-1/ID-5).
   - `create_dir_all(cfg.data_directory())`, then open `scratch.sqlite` there
     for the queue and claims. A second connection to the same file the session
     store uses is fine — SQLite serializes.
   - `InstanceToken::new(endpoints.instance_token())` — the `{org}/{db}` salt.
   - `GlobalStore`, `Queue` (with `INDEXING_RETRY_POLICY`: 5 attempts, 1 s→300 s
     exponential backoff, `lib.rs:142-146`), and `Outbox` construct directly —
     no remote handshake to verify.
   - `Store::new(object_store_backend(&endpoints.object_store))` — a `file://`
     root is created first (`lib.rs:617-622`). **The vendored `object_store`
     build enables no cloud features**, so `file://` and `memory://` are the
     schemes this deployment can speak; anything else fails the parse with the
     scheme named (`lib.rs:611-614`).
   - `Qdrant::from_url(endpoints.qdrant)`, then
     `ensure_collection(&qdrant, CollectionConfig::for_model::<M>())`. There is
     no `Connect` typestate here: constructing the client and bootstrapping the
     collection *is* the connection. `ensure_collection` is idempotent and does
     the network round trip that surfaces a bad endpoint (`lib.rs:406-410`).
     **This is where boot fails when Qdrant is down.**
   - `blobs_cold.connect().await?` — the blob store still carries the `Connect`
     typestate.
   - `create_dir_all(cfg.package_index_directory())` then
     `PackageSearchIndex::open` — the replica-local package tantivy index.

8. **`workspace/driver/main.rs:49-50`** — `Arc::new(Driver::<EmbedModel>::assemble(config).await?)`
   then `server.serve().await?`.

9. **`workspace/driver/lib.rs:499` — `serve()`**
   - build the router (`http::router::router`) and bind a `TcpListener` to
     `config.serving_address`. A bind failure is
     `InternalError::BindFailed { address, source }` → 500-class exit.
   - spawn the role-gated background loops (below).
   - `axum::serve(listener, router).with_graceful_shutdown(shutdown_signal())`.

10. **`GET /readyz`** — `workspace/driver/http/handlers/health.rs:26`
    `200 { ready: true, degraded: [] }` when every source is up; `200
    { ready: true, degraded: [BackendKind] }` when an *overlay* is down —
    degraded is reported, not fatal; `503 { ready: false }` once the definitive
    base is down.

## Role gating

`Role` (`workspace/driver/config.rs:127`) selects which background loops run.
**Every role serves the full HTTP surface** — health, metrics, reads, and
writes. It is only the loops that differ.

| Role | `runs_forge()` | `runs_gateway()` |
| --- | --- | --- |
| `gateway` | no | yes |
| `forge` | yes | no |
| `all` (default) | yes | yes |

`Role::runs_forge` is `config.rs:141`; `runs_gateway` is `config.rs:147`.

| Loop | Gate | Source | What it does |
| --- | --- | --- | --- |
| `queue_worker` | `runs_forge` | `lib.rs:527`, `poll.rs:46` | drains the indexing queue; the one loop that compiles |
| `bakery_worker` | `runs_gateway` **and** `bakery.enabled` | `lib.rs:533` | bakes per-package edge dep-shard artifacts. Default off. |
| `outbox_consumer` × 3 | `runs_gateway` | `lib.rs:537`, `poll.rs:73` | one task per `heart::DerivedStore` variant (`Vector`, `Graph`, `Text`) |
| `package_index_poller` | `runs_gateway` | `lib.rs:540`, `poll.rs:395` | keeps each source's replica-local package tantivy index current |
| `package_signals_poller` | `runs_gateway` | `lib.rs:541`, `poll.rs:419` | refreshes listing/download signals |
| `cas_gc` | `runs_gateway` | `lib.rs:546`, `poll.rs:356` | hourly (`GC_INTERVAL`, `poll.rs:36`); reclaims consumed outbox rows only |
| `catalog_follower_worker` × N | `runs_gateway`, one per `mirror.follow` entry | `lib.rs:552-572`, `poll.rs:529` | actively mirrors an upstream catalog |

Only two languages have a catalog follower — `CSharp` (NuGet) and `Rust`
(crates.io). Any other entry in `mirror.follow` logs
`"no catalog follower implemented for language; skipping"` and is dropped
(`lib.rs:565-568`); an unparseable string logs `"unknown language in
mirror.follow; skipping"` (`lib.rs:555`). Neither is a boot error.

## Shutdown

`shutdown_signal()` (`workspace/driver/lib.rs:637`) resolves on SIGINT or
SIGTERM. If no signal handler can be installed it parks forever rather than
spuriously shutting down (`lib.rs:639-643`).

Then two phases (`lib.rs:581-606`):

1. **Graceful drain of the queue worker.** `drain.cancel()` tells it to stop
   dequeuing; in-flight jobs run to completion or hit their own deadline. Wait
   at most `limits.drain_deadline` (30 s) for a clean return, then force it
   down. Because job settling is idempotent, a forced abort re-delivers rather
   than corrupts.
2. **Abort the remaining gateway pollers** at their next await point and reap
   them. Every unit of poller work is idempotent, so an abort mid-tick is safe.

## Configuration reference

### Top level — `ServerConfiguration` (`config.rs:27`)

| Key | Env | Default | Notes |
| --- | --- | --- | --- |
| `serving_address` | `NUDOX_SERVING_ADDRESS` | `127.0.0.1:8080` | `config.rs:374` |
| `definitive` | — | one source named `definitive` on localhost | required root of the federation |
| `overlays` | — | `[]` | precedence order, **highest first** |
| `custom_registries` | — | `[]` | `[{ name, url }]`; the lookup table behind `AddPackageDto.origin` |
| `limits` | `NUDOX_LIMITS__*` | see below | |
| `role` | `NUDOX_ROLE` | `all` | `gateway` \| `forge` \| `all`, lowercase |
| `deployment` | `NUDOX_DEPLOYMENT` | `development` | `development` \| `production`. **Read nowhere outside `config.rs`** — see step 4. |
| `compiler_endpoint` | `NUDOX_COMPILER_ENDPOINT` | `http://127.0.0.1:8080` (`config.rs:588`) | **dead config.** The compiler daemon is gone; the only remaining mention outside `config.rs` is a stale comment at `main.rs:44`. The doc comment on the field (`config.rs:60`) wrongly says the default is `:9090`. |
| `metadata_data_dir` | `NUDOX_METADATA_DATA_DIR` | unset | expects `tag-synonyms.csv`, `specific-keywords.txt`, `bland-keywords.txt`. Set-but-unparseable aborts boot. |
| `mirror` | `NUDOX_MIRROR__*` | see below | |
| `bakery` | `NUDOX_BAKERY__*` | see below | |
| `rerank` | `NUDOX_RERANK__*` | see below | |
| `depshards` | `NUDOX_DEPSHARDS__*` | `{ enabled: true }` | |

### `SourceConfig` (`config.rs:167`) — `definitive` and each `overlays[n]`

| Key | Default | Notes |
| --- | --- | --- |
| `name` | `"definitive"` for the base | seeds the deterministic `SourceId`; must be non-empty and unique |
| `endpoints` | localhost defaults | below |
| `data_directory` | `<OS tmpdir>/nudox/<name>/` (`config.rs:405-409`) | replica-local state: `scratch.sqlite`, the package tantivy index, watermarks. **Defaulting to the temp dir is fine for development and wrong for production.** |
| `sync_endpoint` | unset | hex-encoded iroh `EndpointId`. `None` means this source is served locally on this node; only a genuinely off-node overlay sets it (`config.rs:180-189`). Parsed by `workspace/driver/sync.rs:62`. |

`package_index_directory()` is `data_directory()/packages` (`config.rs:412`).

### `Endpoints` (`config.rs:195`)

| Key | Default | Notes |
| --- | --- | --- |
| `instance_organization` | `nudox` (`config.rs:577`) | historically the Terminus org; **now only an id salt** |
| `instance_database` | `registry` (`config.rs:578`) | the other half. `instance_token()` is `"{org}/{db}"` (`config.rs:514`). **Changing either re-salts every derived symbol id** — every previously indexed symbol becomes unreachable by its old id. |
| `qdrant` | `http://127.0.0.1:6334` (`config.rs:503`) | gRPC. Boot fails here if it is unreachable. |
| `qdrant_collection` | `symbols` (`config.rs:579`) | **dead config.** Never read outside `config.rs`; the live collection name comes from `CollectionConfig::for_model::<M>()` (`lib.rs:416`). |
| `catalog_directory` | `./data/catalog` (`config.rs:505`) | the DoltLite catalog dir. Opened locally: **no URL, no credentials.** `catalog.dolt` is created inside it. |
| `object_store` | `file://<OS tmpdir>/nudox/blobs` (`object_store_default()`, `config.rs:567`) | `file://` and `memory://` only |
| `embeddings` | `http://127.0.0.1:11434/v1/embeddings` (`config.rs:580`) | OpenAI-compatible. Accepts the full path or the `…/v1` base — `embedrs` posts to `{base}/embeddings`. |
| `embeddings_api_key` | unset | `SecretString`; never `Debug`-printed in the clear (`config.rs:679`) |

Both `catalog_directory` and `object_store` default under the OS temp
directory or the current working directory. A production deployment **must**
set `data_directory`, `catalog_directory`, and `object_store` explicitly or it
loses its catalog on the next reboot.

### `Limits` (`config.rs:337`, defaults at `:483`)

| Key | Default | Notes |
| --- | --- | --- |
| `upload_timeout` | 2 s | write/admin plane; exceeded → **504** |
| `max_request_bytes` | 256 MiB | the write plane clamps to `min(this, 64 KiB)` (`router.rs:196`), so **raising it cannot widen the admin body limit** |
| `max_inflight_jobs` | 16 | also the `for_each_concurrent` width |
| `poll_interval` | 2 s | every background loop's tick |
| `job_lease` | 120 s | heartbeat renews it every `job_lease / 3`, floored at 5 s (`indexing.rs:614`, `MIN_HEARTBEAT` at `:39`) |
| `job_deadline` | 600 s | end-to-end hard deadline for one indexing job |
| `drain_deadline` | 30 s | graceful-shutdown bound |

Durations are serde `Duration`s. Over `NUDOX_*` they take figment's duration
JSON shape, e.g. `NUDOX_LIMITS__POLL_INTERVAL='{"secs":5,"nanos":0}'`; in TOML
they are `poll_interval = { secs = 5, nanos = 0 }`.

### `mirror` (`config.rs:242`), `bakery` (`:262`), `rerank` (`:285`), `depshards` (`:322`)

| Key | Default | Notes |
| --- | --- | --- |
| `mirror.follow` | `[]` | demand-pull only. Only `"csharp"` and `"rust"` have followers. |
| `mirror.queue_ceiling` | 1000 (`config.rs:591`) | pause mirror ingestion above this many pending jobs (`poll.rs:540`) |
| `bakery.enabled` | `false` | explicit operator opt-in — it drives the embedder fleet |
| `bakery.poll_interval` | 30 s (`config.rs:593`) | |
| `rerank.endpoint` | `None` | `None` → `POST /v1/rerank` answers `{"rerank_unavailable":true}` |
| `rerank.model_id` | `mixedbread-ai/mxbai-rerank-base-v2` (`config.rs:597`) | validated against the license deny-list at boot, always |
| `rerank.timeout_ms` | 1200 (`config.rs:601`) | timeout → `rerank_unavailable`, never a 5xx |
| `rerank.api_key` | unset | `SecretString`. Read only by `HttpProxyReranker::from_config`. |
| `depshards.enabled` | `true` (`config.rs:603`) | with the bakery disabled the manifests simply answer `"pending"` |

### A minimal production-shaped `nudox.toml`

Written from the field names in `config.rs`; **not executed during writing.**

```toml
serving_address = "0.0.0.0:8080"
role = "all"
deployment = "production"

[definitive]
name = "definitive"
data_directory = "/var/lib/nudox/definitive"

[definitive.endpoints]
qdrant = "http://qdrant.internal:6334"
catalog_directory = "/var/lib/nudox/catalog"
object_store = "file:///var/lib/nudox/blobs"
embeddings = "http://embeddings.internal:8080/v1/embeddings"

[limits]
max_inflight_jobs = 8

[mirror]
follow = ["rust"]
```

Note that `nudox.toml` at the repo root would be **git-ignored**: `.gitignore`
is deny-by-default (`.gitignore:2`) and its allowlist covers no `*.toml` except
specific `Cargo.toml` paths.

## What is a stub

| Thing | Behaviour | Evidence |
| --- | --- | --- |
| The production credential boot guard | documented, typed, and never executed — `validate()` does not read `deployment` | `config.rs:455-480`; `DefaultCredentialInProduction` has zero constructors |
| `Driver::reranker()` | returns `None` unconditionally; the handler builds its own client per request | `workspace/driver/lib.rs:490-494` |
| `usage_backend` | `ReverseIndexUsageBackend::empty()`, so `POST /usages` is a permanent `503` | `workspace/driver/lib.rs:339` |
| `compiler_endpoint` | resolved, defaulted, validated — and read by nothing | `grep -rn compiler_endpoint --include=*.rs` finds only `config.rs` and a stale comment at `main.rs:44` |
| `qdrant_collection` | same: configurable and unread | `grep -rn qdrant_collection --include=*.rs` finds only `config.rs` |
| `ConfigValidationError::{MissingRequiredEndpoint, DimensionOutOfRange, InvalidUrl}` | declared, never constructed | `config.rs:634`, `:638`, `:647` |
| `Indexer::with_toolchain_images` | exists, never called from `driver`; the compile phase always uses placeholder image digests | `workspace/driver/coordination/indexing.rs:75` |

## Stale comments in this flow

The repo is mid-migration and several comments describe a state one or two
refactors ago. When a comment and the code disagree, the code wins.

| Location | Says | Reality |
| --- | --- | --- |
| `config.rs:60` | `compiler_endpoint` "Defaults to `http://127.0.0.1:9090`" | `defaults::compiler_endpoint()` returns `http://127.0.0.1:8080` (`config.rs:588`) |
| `config.rs:451-454` | production rejects default/TerminusDB credentials | not implemented; no TerminusDB either |
| `main.rs:43-45` | "`Server::assemble` only constructs a `CompilerClient` pointing at `config.compiler_endpoint`" | no `CompilerClient` type exists anywhere in the source. IR is produced in an ephemeral `SmolvmCage` per job. |
| `main.rs:30` | "See `workspace/telemetry`" | no such crate. Telemetry is `heart::telemetry`. |
| `router.rs:80` | "the fan-out recorder in `workspace/telemetry`" | same |
| `indexing.rs:57-59` | "The compile phase is stubbed — see `execute_compile_phase`" | it is not stubbed; it runs the cage for real (`indexing.rs:179-250`) |
| `indexing.rs:646-647` | "the stubbed compile phase surfaces as `Internal`" | same |
| `poll.rs:39` | "a poisoned postgres connection, say" | there is no Postgres in this repo |
| `poll.rs:79-83` | the sink is claimed via "a postgres advisory lock so exactly one replica drains it" | it is an in-process `tokio::sync::Mutex` (`workspace/index/coordination/outbox.rs:277`) |
| `Cargo.toml:10` vs `Cargo.toml:36` | "sandbox was dropped" / "sandbox rejoins the cargo workspace" | both were true in sequence — read that file's header as a changelog, not a description. `workspace/compiler/sandbox` is a member today. |

## Failure modes

| What fails | What happens | Where |
| --- | --- | --- |
| `$NUDOX_CONFIG` names a file that does not exist | **silently ignored** — `path.is_file()` is false, the layer is skipped, and boot proceeds on defaults | `config.rs:432` |
| `nudox.toml` is malformed TOML | `ConfigError::Load`, boot aborts | `config.rs:435-436` |
| An env var does not fit its field's type | `ConfigError::Load` from `figment.extract()` | `config.rs:443` |
| Two sources share a name | `DuplicateSourceName`, boot aborts | `config.rs:462` |
| A source name is empty/whitespace | `EmptySourceName`, boot aborts | `config.rs:459` |
| `rerank.model_id` is license-forbidden | `ForbiddenRerankModel`, boot aborts — even with no endpoint set | `config.rs:474` |
| `metadata_data_dir` set but unloadable | boot aborts with a message naming the directory | `lib.rs:306-311` |
| `catalog_directory` not creatable | `ConnectError(BackendKind::Catalog)`, boot aborts | `lib.rs:365-367` |
| Catalog migration to v4 fails | same | `lib.rs:372-374` |
| Qdrant unreachable | `ConnectError(BackendKind::Qdrant)` from `ensure_collection`, boot aborts | `lib.rs:417-419` |
| `object_store` scheme is not `file://`/`memory://` | `ConnectError(BackendKind::ObjectStore)` naming the scheme, boot aborts | `lib.rs:623-625` |
| `serving_address` already bound | `InternalError::BindFailed`, boot aborts after assembly | `lib.rs:506-511` |
| Embeddings endpoint unreachable | **boot succeeds.** The embedder is constructed, never probed. The failure surfaces as a `503` on the first semantic query. | `lib.rs:278-281` |
| An overlay's stores are down | **boot aborts** — `connect_source` failure on any source aborts assembly (`lib.rs:256-258`). Degradation is a *runtime* state reported by `/readyz`, not a boot-time one. | `lib.rs:272-275` |

## Where to start reading

1. `workspace/driver/main.rs` — 52 lines, the whole entry point.
2. `workspace/driver/config.rs` — 698 lines. Read `ServerConfiguration`
   (`:27`), `resolve` (`:424`), `validate` (`:455`), and `mod defaults`
   (`:573`).
3. `workspace/driver/lib.rs:253-433` — `assemble` and `connect_source`, the
   store-by-store bring-up.
4. `workspace/driver/lib.rs:499-608` — `serve`, role gating, and the two-phase
   shutdown.

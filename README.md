Backend
=======

A multi-language documentation and code-intelligence backend. Language producers
(Rust, TypeScript, Go, Java, Python, Nix) compile packages into a shared IR,
which is indexed into a graph store (TerminusDB), a vector store (Qdrant), a
full-text index (Tantivy), and a content-addressed blob store (object store).
An HTTP API surfaces search, package management, and session-scoped graph
traversal.

Built with Buck2. No Cargo workspace.


Architecture
------------

```
  Language producers (compiler/)
    Rust (rust-analyzer) · TypeScript · Go · Java · Python · Nix
            |
            v
       IR (compiler/intermediate-representation/)
            |
      +-----------+-----------+-----------+-----------+
      |           |           |           |           |
  Graph       Vector       Text       Blobs      Global index
 (Terminus)  (Qdrant)   (Tantivy)  (obj store)  (Postgres)
      |           |           |           |           |
      +-------------------------------------------+
                              |
                       server/ (axum)
```

**Workspace crates** (all under `workspace/`):

| Crate | Role |
| ----- | ---- |
| `heart` | Shared vocabulary: identity types (`PackageId`, `SymbolId`, `ContentHash`), lifecycle typestates (`Cold`/`Live`), error taxonomy, ecosystem enums |
| `compiler` | Language producers → IR → graph emission; tree-sitter CST extraction; IR → renderer for all five languages |
| `compiler/intermediate-representation` | The IR schema: `Entry`, `Index`, type/generics/function/record/protocol nodes |
| `registry` | Write-plane spine: object-store blobs, Postgres global index, job queue, transactional outbox, package ingest sanitizer, package-search tantivy index |
| `runtime` | Read-plane stores: tantivy text index, Qdrant vector/semantic store, TerminusDB graph client, Postgres session store |
| `cas` | Content-addressed storage; three-tier (in-process stampede cache → node-local disk → object store); key type is `ContentHash` (BLAKE3) |
| `server` | HTTP surface (axum), federation assembly, search planner, background pollers, ForgeRuntime (compile-plane capabilities) |
| `util/caching` | Stampede-resistant caching primitives: single-flight coalescing, XFetch probabilistic early expiration, TTL jitter |
| `util/sandbox` | Process isolation: `Cage` trait, `LinuxNamespaces` (bwrap + cgroup + seccomp), `DevPassthrough`, `WorkerPool` for library-form producers |

The server supports a **federated** topology: one definitive (centrally-hosted)
source plus zero or more self-hosted overlay sources, each a complete independent
stack. Overlays take precedence in search resolution.

Deployment roles (`NUDOX_ROLE`): `gateway` (search + derived-store consumers),
`forge` (compile worker), `all` (default, single-node).


Building
--------

Requires the Nix dev shell (provides Buck2 via `buckle`, the Rust toolchain,
and system dependencies):

```bash
# Enter the dev shell
nix develop

# Build the server binary
buck2 build //workspace/server:server

# Build everything
buck2 build //...

# Run all tests
buck2 test //...

# Run tests for one crate
buck2 test //workspace/server:test

# Build and run the server
buck2 run //workspace/server:server
```

Buck2 aliases at the repo root map short names to full targets:
`//:server`, `//:heart`, `//:ir`, `//:registry`, `//:runtime`,
`//:compiler`, `//:sandbox`, `//:cas`, `//:caching`.

Dependency management uses the custom tooling under `build/third-party/`:

```bash
buck2 run //:add    -- <crate>    # add a third-party crate
buck2 run //:check               # verify the registry is consistent
buck2 run //:update              # reconcile registry.bzl with BUCK files
```


Running the server
------------------

Configuration is layered: built-in defaults, then `nudox.toml` (or the path in
`NUDOX_CONFIG`), then `NUDOX_*` environment variables. The `__` separator in env
var names maps to struct nesting (`NUDOX_LIMITS__MAX_INFLIGHT_JOBS=4`).

```bash
# Development (all defaults point to localhost services)
buck2 run //:server
```

The server connects all backing stores at startup and refuses to serve until
every store of every configured source passes its `Connect` check.


Configuration reference
-----------------------

All keys correspond to fields in `workspace/server/config.rs:ServerConfiguration`.
Environment variable names are `NUDOX_` + the field path with `__` as separator.

### Top-level

| Key / env var | Dev default | Notes |
| ------------- | ----------- | ----- |
| `serving_address` / `NUDOX_SERVING_ADDRESS` | `127.0.0.1:8080` | TCP bind address |
| `role` / `NUDOX_ROLE` | `all` | `gateway` \| `forge` \| `all` |
| `NUDOX_CONFIG` | `nudox.toml` | Path to the TOML config file |

### `definitive.endpoints` (and `overlays[n].endpoints`)

| Key / env var suffix | Dev default | Notes |
| -------------------- | ----------- | ----- |
| `terminus` | `http://127.0.0.1:6363` | TerminusDB HTTP endpoint |
| `terminus_organization` | `nudox` | TerminusDB organization |
| `terminus_database` | `registry` | TerminusDB database name — also seeds symbol identity |
| `terminus_user` | `admin` | TerminusDB basic-auth user |
| `terminus_password` | `root` | **DEV DEFAULT — insecure; set in production** |
| `qdrant` | `http://127.0.0.1:6334` | Qdrant gRPC endpoint |
| `qdrant_collection` | `symbols` | Qdrant collection name |
| `postgres` | `postgres://nudox:nudox@127.0.0.1:5432/nudox` | **DEV DEFAULT — insecure; set in production** |
| `object_store` | `file://<tmpdir>/nudox/blobs` | Object-store base URL (`file://` or S3/GCS) |
| `embeddings` | `http://127.0.0.1:11434/v1/embeddings` | OpenAI-compatible embeddings endpoint |
| `embeddings_api_key` | _(unset)_ | Bearer token for the embeddings endpoint |

### `limits`

| Key / env var suffix | Default | Notes |
| -------------------- | ------- | ----- |
| `limits.upload_timeout` | `2s` | Admin-plane request timeout (returns 504) |
| `limits.max_request_bytes` | `256 MiB` | Max body on the write plane (capped at 64 KiB for admin mutations) |
| `limits.max_inflight_jobs` | `16` | Concurrent compile jobs per node |
| `limits.poll_interval` | `2s` | Background-poller tick interval |
| `limits.job_lease` | `120s` | Queue lease duration; heartbeat keeps it alive every ~40 s |
| `limits.job_deadline` | `600s` | Hard per-job deadline |
| `limits.drain_deadline` | `30s` | Graceful-shutdown drain window |

### Source replica-local state

Each configured source stores its replica-local Tantivy indexes under
`data_directory` (field `definitive.data_directory` / `overlays[n].data_directory`).
When unset, defaults to `<OS tmpdir>/nudox/<source_name>/`. Subdirectories:
`text/` (symbol index) and `packages/` (package-search index).


HTTP API
--------

All routes are defined in `workspace/server/http/router.rs`.

### Read plane (up to 2 MiB request body)

| Method | Path | Description |
| ------ | ---- | ----------- |
| `POST` | `/search` | Symbol/text search |
| `POST` | `/search/semantic` | Semantic (vector) search — requires Qdrant + embeddings |
| `POST` | `/packages/search` | Package discovery search |
| `POST` | `/expand` | Graph-traversal expansion from a symbol |
| `GET`  | `/symbols/:id` | Fetch a single symbol by ID |
| `GET`  | `/sessions/:id` | Fetch a session exploration graph |

### Write / admin plane (body limit: min of 64 KiB and `limits.max_request_bytes`; `limits.upload_timeout` applies)

| Method | Path | Description |
| ------ | ---- | ----------- |
| `POST` | `/packages` | Add a package (triggers background indexing job) |
| `GET`  | `/packages/:id` | Get package status |
| `POST` | `/packages/:id/sync` | Force re-index of a package |

### Operational

| Method | Path | Description |
| ------ | ---- | ----------- |
| `GET` | `/healthz` | Liveness probe |
| `GET` | `/readyz` | Readiness probe (checks all backing stores) |
| `GET` | `/metrics` | Prometheus metrics |


Status / known limitations
--------------------------

- **No authentication or authorization.** `Server::authorize()` is a no-op
  (audit trace only). Deploy behind a trusted network boundary or a
  terminating proxy that enforces access control.

- **Single-tenant.** The source federation supports multiple named registries
  but there is no per-user isolation or tenancy model.

- **CAS blob GC not implemented.** `cas_gc` (poll.rs) reclaims consumed outbox
  rows but does not delete orphaned CAS blobs. Blobs accumulate until manual
  intervention. A transactional refcount or snapshot-fenced mark-sweep is the
  documented follow-up (DAEMON-PLAN §5-ops).

- **TypeScript producer emits placeholder symbol kinds.** The TS compile path
  is implemented but type-kind lowering is incomplete; some entries carry
  placeholder kinds.

- **`/search/semantic` is gated.** Semantic search requires a running Qdrant
  instance and a configured embeddings endpoint; it is not available in the
  default dev configuration.

- **Dev defaults are insecure.** `terminus_password = "root"` and
  `postgres = "postgres://nudox:nudox@..."` are compile-time dev defaults.
  Override them in `nudox.toml` or via environment variables before any
  non-local deployment.


Development
-----------

### Nix environment

The repo uses Lix (a modern Nix implementation). Do not install compilers or
runtimes globally — add them to `flake.nix`.

```bash
# Install Lix
curl -sSfL https://install.lix.systems/lix | sh -s -- install

# Enter the dev shell
nix develop
```

### Commit style

Conventional Commits: `<type>(<scope>): <subject>`

Types: `feat` `fix` `refactor` `test` `docs` `build` `chore` `ci` `perf` `revert`

### Version control

We use Radicle for hosting. After `rad auth`:

```bash
rad node connect z6MkmTC76GDv4H7YdZB9UvMhjxpxZXoNTeQaMqGsoiRpZsJf@100.114.38.65:8776
rad clone <RID>
rad sync
```

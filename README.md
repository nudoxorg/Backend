nudox Backend
=============

A Rust workspace that indexes library APIs and symbol occurrences, and serves
them over a REST API. The server ingests Rust and TypeScript packages, stores
structured symbol data in TerminusDB and Qdrant, and exposes search endpoints
for code intelligence tooling.

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


What's in this workspace
------------------------

~~~~
compiler/          Server binary + ingestion pipeline (axum, TerminusDB, Qdrant)
crates/
  nudox-core       Shared types, traits, error type — zero logic
  nudox-embed      Embedder implementations (Mock, Placeholder, InProcess, Remote/OpenAI)
  nudox-blobstore  BlobStore backends: local filesystem via object_store, in-memory
  nudox-search     SearchIndex via Tantivy, VectorIndex via Qdrant, in-memory stubs
  nudox-pipeline   Converts PipelineInput → BlobInfo with tree-sitter snippet extraction
  nudox-orchestrator  Ingest routing, deferred queue management, index rebuild
  nudox-store      SQLite-backed GlobalSymbolStore + FutureParseQueue
  nudox-indexer    Demo binary wiring all crates end-to-end
ir/                Tree-sitter IR: syntax walking, reference classification
linkml/            Schema definitions
terminusdb/        TerminusDB client and embedding service
~~~~

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


Building
--------

All build commands run inside the Nix flake — `Backend/.cargo/config.toml`
requires `clang` as the linker, which is only in the flake environment:

~~~~ bash
# Enter the environment once (or use direnv — see below)
nix develop /path/to/nudox/Backend

# Workspace check
cargo check --workspace

# Run tests (no external services needed by default)
cargo test --workspace

# Qdrant integration tests (requires Qdrant on localhost:6334)
cargo test --workspace --features nudox-search/qdrant-integration

# Build the server binary
cargo build --release -p nudox

# Run the demo indexer
NUDOX_QDRANT_URL=http://localhost:6334 cargo run -p nudox-indexer
~~~~

### direnv (recommended)

~~~~ bash
# Install direnv, then in any nudox repo:
direnv allow
# The flake environment loads/unloads automatically on cd.
~~~~

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


Running the server
------------------

~~~~ bash
# Minimal — no TerminusDB, no Qdrant, text search only
NUDOX_DATA_DIR=.nudox-data cargo run -p nudox

# Full stack
NUDOX_DATA_DIR=.nudox-data \
NUDOX_TERMINUS_URL=http://localhost:6363 \
NUDOX_TERMINUS_ORG=my_org \
NUDOX_TERMINUS_DB=my_db \
NUDOX_QDRANT_ENDPOINT=http://localhost:6334 \
OPENAI_API_KEY=sk-... \
cargo run -p nudox
~~~~

Copy `.env.example` to `.env` and fill in values; the server reads it on
startup.

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


Environment variables
---------------------

| Variable                  | Default                  | Notes                                        |
| ------------------------- | ------------------------ | -------------------------------------------- |
| `NUDOX_DATA_DIR`          | `.nudox-data`            | Storage root for all on-disk state           |
| `NUDOX_BIND_ADDR`         | `0.0.0.0:3000`           | Server listen address                        |
| `NUDOX_TERMINUS_URL`      | —                        | TerminusDB HTTP endpoint                     |
| `NUDOX_TERMINUS_ORG`      | —                        | TerminusDB org name                          |
| `NUDOX_TERMINUS_DB`       | —                        | TerminusDB database name                     |
| `NUDOX_QDRANT_ENDPOINT`   | —                        | Qdrant gRPC endpoint                         |
| `NUDOX_QDRANT_COLLECTION` | `nudox-embeddings`       | Qdrant collection prefix                     |
| `NUDOX_EMBEDDING_MODEL`   | `text-embedding-3-small` | Model name passed to OpenAI-compat API       |
| `OPENAI_API_KEY`          | —                        | Required when `NUDOX_QDRANT_ENDPOINT` is set |
| `NUDOX_BLOB_STORE_ROOT`   | `target/nudox-blobs`     | Indexer demo blob directory                  |
| `NUDOX_TANTIVY_DIR`       | `target/nudox-tantivy`   | Indexer demo Tantivy directory               |
| `RUST_LOG`                | `info,nudox=debug`       | Tracing filter                               |

The SQLite occurrence store (`{NUDOX_DATA_DIR}/nudox-links.db`) and the Tantivy
full-text index (`{NUDOX_DATA_DIR}/tantivy/`) are created automatically at
startup; no manual setup is needed.

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


API
---

### Health

~~~~
GET /healthz
→ { "status": "ok", "tracked_packages": 3 }
~~~~

### Full-text symbol search (Tantivy, no external services)

~~~~
GET /text-search?q=<query>[&limit=<n>]
~~~~

Searches symbol names, qualified names, and documentation text across all
ingested packages. Returns up to `limit` (default 6) results ordered by
relevance score.

~~~~ bash
curl 'http://localhost:3000/text-search?q=Serialize&limit=5'
~~~~

~~~~ json
{
  "query": "Serialize",
  "results": [
    {
      "uri": "Entry/rust/serde/Serialize",
      "score": 4.2,
      "fq_name": "serde::Serialize",
      "language": "rust",
      "package": "serde",
      "version": "1.0.219",
      "symbol_kind": "trait"
    }
  ]
}
~~~~

### Semantic search (requires Qdrant + embeddings)

~~~~
GET /search?q=<query>[&limit=<n>]
~~~~

### Symbol lookup

~~~~
GET /terminus_search?q=<symbol-uri>
GET /terminus_search?symbol=<fq_name>&language=<language>[&package=<pkg>]
~~~~

### Code run search

~~~~
GET /run?q=<query>[&session=<id>][&limit=<n>]
~~~~

### Symbol expand

~~~~
GET /expand?uri=<symbol-uri>[&depth=<n>][&breadth=<n>][&session=<id>]
~~~~

### Package management

~~~~
GET  /api/packages             → list all tracked packages
POST /api/packages             → add a package (triggers background ingest)
GET  /api/packages/{id}        → get package status + snapshot
POST /api/packages/{id}/sync   → force re-sync
~~~~

### Session

~~~~
DELETE /session?session=<id>   → clear a search session
~~~~

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


Crates overview
---------------

See [`crates/README.md`](crates/README.md) for the full reference including
type contracts, trait tables, SQLite schema, and integration notes.

### Quick summary

**`nudox-core`** — contract crate. `BlobInfo` is the central artifact;
everything else (search entry, vector point, SQLite row) is a pointer or
derived value. `BLOB_SCHEMA_VERSION = 2` is validated on every store/retrieve.

**`nudox-pipeline`** — takes `PipelineInput` (raw code + symbol span + origin),
runs tree-sitter to extract a snippet-bounded `SourceChunk`, runs every
configured embedder, and returns `BlobInfo`. No network calls.

**`nudox-orchestrator`** — routes `BlobInfo` to the backends. Repo-local
symbols get a fresh UUID v4 `GlobalSymbolId` immediately. External-lib symbols
either resolve against the global store or are deferred to a queue until
`resolve_lib` is called after that library is parsed. `rebuild_indexes()`
reconstructs all indexes from blob storage alone.

**`nudox-store`** — production `GlobalSymbolStore` and `FutureParseQueue`
backed by SQLite. `GlobalSymbolId` values are deterministic UUID v5 derived
from `(terminus_instance, entry_uri)` — computable offline without a DB
round-trip.

**`nudox-search`** — `TantivySearchIndex` (upsert semantics, persists to disk)
and `QdrantVectorIndex` (idempotent point IDs). Both have in-memory stubs for
tests.

**`nudox-blobstore`** — `ObjectStoreBlobStore` writes blobs as JSON files under
`{root}/blobs/{uuid}.json`. `InMemoryBlobStore` for tests.

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


On-disk layout
--------------

After first run with `NUDOX_DATA_DIR=.nudox-data`:

~~~~
.nudox-data/
  packages.json              tracked package registry
  nudox-links.db             SQLite: global symbols + deferred queue
  tantivy/                   Tantivy full-text index (persists across restarts)
  repositories/
    rust/<source-slug>/      cloned/extracted Rust packages
    typescript/<source-slug>/
  sessions/                  search session state
~~~~

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


Tests
-----

~~~~ bash
# All tests (no external services required)
cargo test --workspace

# Verbose output
cargo test --workspace -- --nocapture

# A specific crate
cargo test -p nudox-orchestrator
~~~~

Test counts per crate (all passing, no warnings):

| Crate              | Count                                          |
| ------------------ | ---------------------------------------------- |
| nudox-core         | 11                                             |
| nudox-embed        | 8                                              |
| nudox-blobstore    | 35                                             |
| nudox-search       | 19                                             |
| nudox-pipeline     | 9                                              |
| nudox-orchestrator | 4 (2 disk persistence + 2 in-memory)           |
| nudox-store        | 15                                             |
| compiler           | 5 (require network — expected to fail offline) |

The `disk_backends_survive_reopen` and `disk_ingest_and_resolve_lib` tests in
`nudox-orchestrator` verify that blob storage, SQLite, and the Tantivy index
all survive process-restart simulation.

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


Development
-----------

### Nix environment

We use **Lix** (a modern Nix implementation) for reproducible environments. Do
not install compilers or runtimes globally — add them to `flake.nix`.

~~~~ bash
# Install Lix
curl -sSfL https://install.lix.systems/lix | sh -s -- install
# Enable Flakes + New CLI when prompted.

# Enter the Backend dev shell
nix develop /path/to/nudox/Backend
~~~~

### Commit style

Follow [Conventional Commits]:

~~~~
<type>(<scope>): <subject>

# Types: feat fix refactor test docs build chore ci perf revert style
~~~~

[Conventional Commits]: https://www.conventionalcommits.org/en/v1.0.0/

### Radicle (version control)

We use [Radicle] for hosting. After installing and running `rad auth`:

~~~~ bash
rad node connect z6MkmTC76GDv4H7YdZB9UvMhjxpxZXoNTeQaMqGsoiRpZsJf@100.114.38.65:8776
rad clone <RID>      # clone a repo
rad sync             # announce your changes to the network
~~~~

Access to private repositories requires your DID to be added to the allow list
on the seed node. Contact the project owner with your `rad self --did` output.

[Radicle]: https://radicle.xyz/

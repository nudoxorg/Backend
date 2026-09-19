# Backend v2

This branch replaces the backend with one local-first, versioned object and
relation engine. Stable logical keys, immutable object versions, canonical
relation roots, exact deltas, retained arrangements, replication, and local or
remote execution all share the contracts in `backend-version`.

The runtime product graph contains 28 packages: twelve shared crates, seven
language frontends, four query/index adapters, and five app surfaces. The
agent cutover controller lives under `tools/`, has zero product dependents, and
is outside the runtime dependency graph. Historical source is kept in
external reference checkouts and the migration map; no legacy crate or owner
remains in the product tree.

The native GPUI desktop is the primary local host. It embeds the same local
service used by the headless `backend-locald` launcher, owns the workspace when
no owner is running, and otherwise attaches to the existing owner. CLI and MCP
connect to that authenticated endpoint, so every surface observes one exact
revision and the same admitted delta stream. Platform packages may select
different project, data, credential, and endpoint paths without changing the
service or protocol contracts.

Launch the native package browser directly from a project:

```console
cargo run -p backend-desktop
```

On macOS, build a normal application bundle with:

```console
apps/desktop/package-macos.sh release
open target/Nudox.app
```

The package page opens first. Its Code and Documentation views read the same
admitted local revision, and source links stay inside the application.

Start with [the architecture](docs/architecture/README.md), then use the
[migration sequence](docs/operations/migration.md) and
[agent cutover protocol](docs/operations/agent-cutover.md) for rollout work.

```console
cargo metadata --format-version 1 --no-deps --offline \
  | cargo run -p backend-workspace --offline --quiet
sh tests/journeys/run-cutover-e2e.sh prepare
cargo test --workspace --all-targets --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
python3 -m unittest discover -s docs/prototypes -p 'test_*.py'
```

The `backend-workspace` command performs the full repository closure when
Cargo metadata includes `workspace_root`: it checks the product DAG and layer
edges, inherited workspace lints, production Rust escape hatches, duplicate
path-copy kernels, local Markdown destinations, and the versioned cutover
contract projections and cutover mutation/E2E contracts. It also rejects stale
legacy package or owner references in the generated Koji/Nix control policy
and requires target scope roots to remain declared. Pass `--metadata-only` for
a small graph fixture or a metadata-only preflight.

The root pointer advances only after immutable objects and the intent journal
are durable. Remote workers may accelerate pure recipes, but local durable
state and result admission remain authoritative.

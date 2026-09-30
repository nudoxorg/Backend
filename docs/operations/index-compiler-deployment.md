# Deploy an index owner and compiler workers

This runbook installs one durable index owner and one or more compiler workers
on private Linux or macOS machines. The owner serves local CLI and MCP through
a Unix socket with mode `0600`; local clients must run as the same effective
user as locald. On a headless index host, that is the `nudox-index` service
account. The desktop application runs a local owner under the launching login
account, so it cannot attach to the separate headless service UID. Iroh also
serves explicitly granted, read-only remote queries and semantic hydration
over direct UDP; this is a separate signed-capability path and does not expose
the Unix socket.

Iroh has no relay, public peer discovery, or DNS lookup. Remote index clients
need the owner's peer ID and direct advertised IP address. A desktop app
launched under a normal user account uses that user's local owner; it does not
automatically query the headless service. A workspace snapshot is not an OS
sandbox: a compiler grant authorizes coordinator-selected host execution. Use
a dedicated service account and operating-system isolation appropriate to the
compiler inputs you accept.

The Rust `ir-vcs` API computes borrowed snapshots and deltas over the
canonical `Ir`/`SemanticReader`; it is not an Iroh query protocol.
`LocalSemanticIndexClient` fetches the selected catalog and exact image
manifest from the authenticated local owner, then requests bounded 16 KiB
IR/embedding ranges. Every request is checked against a fresh selected-head
stamp. The client's `FileSemanticRangeStore` fsyncs partial extents and an
exact-generation checkpoint; only complete segment bytes that pass manifest
admission and CAS read-back count as local coverage. `semantic-hydrate`
reopens that checkpoint after interruption and commits a local generation
only when the requested plane is complete. The full NXFI image and plane
segments are retained under the configured client store. S3 remains one possible
location for immutable compiler objects: local clients use the same owner
endpoint whether the owner reads an object from its disk CAS or S3, and the
worker never receives the owner's S3 credentials.

### Set up a remote read-only index client

Create the client identity on the machine that will run the client. The key
file stays private to that machine; the command prints only its public peer ID.
Keep both the key and copied capability in a private owner-only directory. The
CLI opens them through the platform's no-follow private-file reader and checks
the opened handle before reading; it never prints or logs the key bytes.
Share that ID with the index owner. The owner must have locald running to read
the current product root or semantic selection before signing a grant.

This release uses remote-index protocol and capability version 2. Reissue
existing grant files after updating the owner; an older capability cannot be
upgraded or broadened in place.

```sh
backend --workspace "$CLIENT_DATA" cluster client init --key-file "$CLIENT_DATA/remote-index-client.v1"
```

On the owner, issue a grant for one product view root and a closed set of
operations. Copy the signed capability file to the client using your existing
authenticated file-transfer channel. The capability is bound to the client
peer ID and contains no private key.

```sh
backend --workspace "$OWNER_DATA" --project "$PROJECT" \
  cluster owner grant product create \
  --client-peer "$CLIENT_PEER" \
  --capability-file "$OWNER_DATA/product-read.cap" \
  --operations search,names,document,source,outline,graph,related
```

Connect and run the same proof-admitting product query API used by local
clients. `--owner-peer` is the endpoint ID from `cluster owner show`;
`--owner-address` is its direct advertised IP address and UDP port. `connect`
sends a real bounded request and reports the grant scope without printing
keys.

```sh
backend --workspace "$CLIENT_DATA" cluster client connect \
  --key-file "$CLIENT_DATA/remote-index-client.v1" \
  --owner-peer "$OWNER_PEER" --owner-address 10.0.0.10:40123 \
  --capability-file "$CLIENT_DATA/product-read.cap"
backend --workspace "$CLIENT_DATA" cluster client query \
  --key-file "$CLIENT_DATA/remote-index-client.v1" \
  --owner-peer "$OWNER_PEER" --owner-address 10.0.0.10:40123 \
  --capability-file "$CLIENT_DATA/product-read.cap" \
  --operation search --value cluster_deploy_smoke --limit 20
```

To expose the same typed cross-plane package catalog search to a remote client,
include `index-search` in the product grant. The owner binds that operation to
the current composite index-search snapshot, which covers the product root,
selected package catalog, and discovery revisions. Each page and continuation
cursor is checked against that exact snapshot; if it changes, the client must
receive a stale-snapshot result and the owner must issue a new grant. This
scope does not authorize other product commands.

```sh
backend --workspace "$OWNER_DATA" --project "$PROJECT" \
  cluster owner grant product create \
  --client-peer "$CLIENT_PEER" \
  --capability-file "$OWNER_DATA/catalog-read.cap" \
  --operations index-search
backend --workspace "$CLIENT_DATA" cluster client query \
  --key-file "$CLIENT_DATA/remote-index-client.v1" \
  --owner-peer "$OWNER_PEER" --owner-address 10.0.0.10:40123 \
  --capability-file "$CLIENT_DATA/catalog-read.cap" \
  --operation index-search --value identity --limit 10
# Pass the returned data.next_cursor as --cursor to fetch the next page.
```

To exercise the installed CLI, real locald owner, and a separate remote client
on one host, run the loopback journey after building both binaries:

```sh
BACKEND_CLI=/path/to/backend-cli \
BACKEND_LOCALD_BIN=/path/to/backend-locald \
tests/journeys/run-remote-index-client.sh
```

The journey indexes a temporary Rust fixture, searches it through the remote
client, restarts locald, and repeats the query with the same grant. It uses
temporary owner/client keys, samples combined owner/client RSS during each
query, checks durable response-byte usage remains within the signed budget,
and removes its state on exit. Set `REMOTE_INDEX_RSS_LIMIT_KB` to tune the
sampled process-memory ceiling for the host.

The `run-remote-index-catalog-client.sh` journey exercises this cross-plane
operation against a copied, frozen offline Maven discovery journal. It checks
the typed snapshot and cursor chain across an owner cold restart and compares
all returned source coordinates with independent labels. Its product grant is
limited to 100 requests and 4 MiB of responses, and it checks sampled combined
owner/client RSS against `REMOTE_INDEX_RSS_LIMIT_KB` (default 2 GiB). Set
`REMOTE_MAVEN_CATALOG_JOURNAL` and `REMOTE_MAVEN_LABELS` to use equivalent
local fixtures. For the frozen Maven replay, set both paths explicitly:

```sh
REMOTE_MAVEN_CATALOG_JOURNAL="$FROZEN_CATALOG_JOURNAL" \
REMOTE_MAVEN_LABELS="$FROZEN_CATALOG_LABELS" \
BACKEND_CLI=/path/to/backend-cli \
BACKEND_LOCALD_BIN=/path/to/backend-locald \
tests/journeys/run-remote-index-catalog-client.sh
```

The owner checks the exact selected product root before and after each query;
the typed command also carries that root as its basis. For `index-search`, it
also checks the returned page snapshot and the current composite catalog and
discovery snapshot before exposing the response. If either binding changes
during a query, the client receives a typed stale result and must request a
new grant. Grants also have request, response-byte, and expiry bounds. Restrict
inbound UDP to the client's network and keep the owner peer ID and advertised
address together when configuring clients.

The owner keeps an owner-bound, checksummed grant ledger under its private
workspace. Review active and revoked grants with `cluster owner grant list`;
revoke one client without rotating the owner identity with
`cluster owner grant revoke --grant-id GRANT_ID`. Revocation survives owner
restart and blocks newly admitted requests and results. Every query or
hydration result/refusal frame that is sent, including typed stale outcomes,
reserves its exact canonical wire size (the serialized typed frame plus its
four-byte length prefix) before the QUIC write. Revocation linearizes against
that durable response permit: a result whose permit has not been admitted is
suppressed; a response admitted earlier may finish sending after the revoke
command returns.
A payload-free `CapabilityRevoked` notice may still be sent if its own exact
bytes fit the remaining grant budget. A corrupt ledger disables remote reads
while local CLI, MCP, and compiler operation remain available.

Owner forwarding uses a separate eight-slot blocking-work bound in addition to
the eight authenticated Iroh connection slots. Each admitted request keeps its
work slot through grant metering, the local owner call, and response metering;
if a QUIC caller times out, the slot remains held until the blocking operation
returns or panics. The owner socket dial is limited to five seconds and each
local request/response read or write to twenty seconds. A reconnect therefore
cannot create unbounded detached owner work.

### Remote-client regression matrix

Run these checks against binaries built from the same reviewed source revision.
The two-process journeys invoke the installed CLI against a real locald owner
and use a separate Iroh client identity.

| Control | Executable check | Pass condition |
| --- | --- | --- |
| Product query, semantic catalog, hydration, durable byte usage, cold owner restart, and per-grant revoke | `tests/journeys/run-remote-index-client.sh` | The indexed marker and admitted semantic ranges are returned; ledger bytes stay within the signed budget; after restart a revoked grant is rejected. |
| Product-root/index-search snapshot, pagination cursor resume across owner restart, bounded catalog response budget, and persistent per-grant revoke | `tests/journeys/run-remote-index-catalog-client.sh` with the frozen journal and independent labels | All 96 expected coordinates appear exactly once; each page matches the signed snapshot; request/byte and RSS ceilings hold; after restart, the revoked client is rejected without charging another request and the typed notice is byte-metered. |
| Exact canonical frame metering for large stale-root outcomes | `prepared_response_reports_the_exact_canonical_wire_frame_size` and `stale_root_response_reserves_its_full_canonical_wire_size` | The charged size equals the typed postcard frame actually sent, including both 32-byte roots and the frame prefix. |
| Revoke before/after result admission | `response_admission_is_the_revoke_linearization_point` and `concurrent_revoke_and_response_admission_has_one_durable_winner` | Revoke-first refuses result admission; permit-first retains a valid in-flight send permit, while later request admission is denied. |
| Withheld local owner response, caller timeout, and blocking-work permit lifetime | `withheld_local_owner_response_hits_the_configured_socket_deadline`, `detached_blocking_owner_work_keeps_its_slot_until_completion`, and `blocking_work_slot_is_released_after_worker_panic` | The local response read times out; a detached blocked call continues to consume its bounded slot and releases it only on completion or panic. |
| Interrupted/corrupt semantic range reconnect and resume over two processes | Extend the real client journey with a dropped range stream, corrupted range, owner restart, and resumed `semantic-hydrate` | No unverified bytes become complete; the client resumes the exact selected generation and finishes within signed request/byte budgets. |
| Root/snapshot publication race during live query | Mutate the selected root or discovery snapshot while the installed remote query is in flight | The client receives a typed stale result or a closed refusal; no response is attached to a different current root/snapshot. |

The first two rows are executable end-to-end journeys. The remaining rows are
regression gates: source tests cover the frame and permit-order invariants, while
range interruption and publication-race behavior still require live two-process
coverage before claiming those scenarios as verified.

For semantic hydration, the owner creates a grant only after it has admitted
the current catalog for the exact package, coordinate, and language profile.
The grant is bound to the selection revision, source coordinate, selected
root, closure, and catalog root. On the client, `semantic-catalog` opens the
same bounded remote semantic channel used by
`LocalSemanticIndexClient::connect_remote`; the API then uses the existing
manifest, image, range-resume, Bao verification, and durable checkpoint
operations. The local `semantic-hydrate` command above continues to use the
Unix endpoint unless all remote connection flags are supplied; remote mode
uses that same client API and durable range checkpoint.

```sh
backend --workspace "$OWNER_DATA" --project "$PROJECT" \
  cluster owner grant semantic create \
  --client-peer "$CLIENT_PEER" \
  --capability-file "$OWNER_DATA/semantic-read.cap" \
  --package 'pkg:cargo/my-app@1.2.3' \
  --coordinate 'pkg:cargo/my-app@1.2.3' --profile rust-2024
backend --workspace "$CLIENT_DATA" cluster client semantic-catalog \
  --key-file "$CLIENT_DATA/remote-index-client.v1" \
  --owner-peer "$OWNER_PEER" --owner-address 10.0.0.10:40123 \
  --capability-file "$CLIENT_DATA/semantic-read.cap"
backend semantic-hydrate \
  --package 'pkg:cargo/my-app@1.2.3' \
  --coordinate 'pkg:cargo/my-app@1.2.3' --profile rust-2024 \
  --image-ordinal "$IMAGE_ORDINAL" --plane core \
  --store "$CLIENT_DATA/my-app-semantic-cas" \
  --key-file "$CLIENT_DATA/remote-index-client.v1" \
  --owner-peer "$OWNER_PEER" --owner-address 10.0.0.10:40123 \
  --capability-file "$CLIENT_DATA/semantic-read.cap"
```

If the product root or semantic selection changes, the old grant is stale;
issue a new grant for the new selection. Product and semantic capabilities
are read-only and independently scoped. They do not authorize compiler
execution, publication, owner administration, or arbitrary local commands.

Rust declaration documentation follows rust-analyzer's Rustdoc expansion for
active `#[doc = include_str!("relative/path")]` attributes, including repeated
attributes and feature-selected `cfg_attr` attributes. Include files must be
regular UTF-8 files inside the admitted package, and are bounded by the
compiler's per-file source budget plus a 64 MiB aggregate documentation budget.
The lowered documentation plane also has a 16,384-fragment limit. Missing,
oversized, non-UTF-8, package-escaping, or over-fragment inputs fail the compile
closed. Expressions that cannot be resolved before rust-analyzer expansion,
including `concat!`, `env!`, and generated paths, are currently rejected.
The compiler workspace snapshot follows the configured ignore and generated
file policy. Keep active Rustdoc includes inside that admitted inventory so a
remote compiler receives the same bytes. The V2 compiler-input read frontier
remains marked unproven for cross-invocation reuse.

### Pull the currently selected plane to a local client

Run this on the index owner host as the same account that runs locald. The
client uses the owner's private Unix socket; this command does not create a
remote query endpoint. Replace the package reference and coordinate with the
canonical values for the indexed project. The image ordinal must exist in its
currently selected catalog. Use a client-owned store path so its hydrated
objects and checkpoint are separate from the owner's publication state.

```sh
backend --workspace /var/lib/nudox-index semantic-hydrate \
  --package 'pkg:cargo/my-app@1.2.3' \
  --coordinate 'pkg:cargo/my-app@1.2.3' \
  --profile rust-2024 --image-ordinal 0 --plane core \
  --store "$HOME/.local/share/nudox/my-app-semantic-cas" \
  --checkpoint "$HOME/.local/share/nudox/my-app-core.checkpoint"
```

The command prints the selected root and generation, verified segment count,
range bytes, and page count. If it is interrupted, rerun the same command with
the same store and checkpoint paths. A checkpoint from an older selection is
rejected; it cannot mark bytes complete for the new generation. The client
must run on the owner host as the same effective user as locald. Remote
desktop or remote application access remains outside this runbook.

## 1. Build and copy the binaries

Run the build on a machine with Nix flakes enabled, authenticated SSH access
to both target hosts, and permission to copy Nix store paths to their Nix
stores. Build natively for the same operating system and architecture used by
the owner and worker. The `luna-tools` closure supplies the pinned Rust build
tools, `jq`, and `sha256sum`. A Linux owner and macOS worker, or an x86-64
owner and ARM64 worker, do not share a grantable compiler scope in this
version. Ensure the project checkout is readable by `nudox-index` at the same
absolute path used below; workers receive bounded source snapshots and do not
need that checkout.

```sh
set -eu
test -z "$(git status --porcelain)" # build only from the reviewed source revision
RELEASE_ID=$(git rev-parse --short=12 HEAD)
LUNA_TOOLS=$(nix build '.#luna-tools' --no-link --print-out-paths)
nix copy --to ssh://index-host "$LUNA_TOOLS"
nix copy --to ssh://compiler-host "$LUNA_TOOLS"
CARGO_TARGET_DIR="$PWD/.local/target" nix shell '.#luna-tools' --command cargo build --locked --release \
  -p backend-locald -p backend-cli -p backend-worker -p backend-mcp

mkdir -p "dist/$RELEASE_ID/bin"
install -m 0755 .local/target/release/backend-locald "dist/$RELEASE_ID/bin/"
install -m 0755 .local/target/release/backend-cli "dist/$RELEASE_ID/bin/"
install -m 0755 .local/target/release/backend-worker "dist/$RELEASE_ID/bin/"
install -m 0755 .local/target/release/backend-mcp "dist/$RELEASE_ID/bin/"
printf '%s\n' "$LUNA_TOOLS" > "dist/$RELEASE_ID/luna-tools-path"
tar -C "dist/$RELEASE_ID" -czf "dist/nudox-$RELEASE_ID.tar.gz" bin luna-tools-path
(cd dist && "$LUNA_TOOLS/bin/sha256sum" \
  "nudox-$RELEASE_ID.tar.gz" > "nudox-$RELEASE_ID.tar.gz.sha256")
```

Copy the archive to each target machine using your authenticated deployment
channel. For example, from the build machine:

```sh
scp "dist/nudox-$RELEASE_ID.tar.gz" index-host:/tmp/
scp "dist/nudox-$RELEASE_ID.tar.gz" compiler-host:/tmp/
scp "dist/nudox-$RELEASE_ID.tar.gz.sha256" index-host:/tmp/
scp "dist/nudox-$RELEASE_ID.tar.gz.sha256" compiler-host:/tmp/
```

On each target, extract into a versioned directory and move the `current`
symlink only after setting `RELEASE_ID` to the short revision printed on the
build machine and checking the copied archive digest:

```sh
set -eu
RELEASE_ID=replace-with-the-short-revision-printed-by-git
if command -v sha256sum >/dev/null 2>&1; then
  (cd /tmp && sha256sum -c "nudox-$RELEASE_ID.tar.gz.sha256")
else
  (cd /tmp && shasum -a 256 -c "nudox-$RELEASE_ID.tar.gz.sha256")
fi
sudo install -d -m 0755 "/opt/nudox/releases/$RELEASE_ID"
sudo tar -C "/opt/nudox/releases/$RELEASE_ID" \
  -xzf "/tmp/nudox-$RELEASE_ID.tar.gz"
TOOLS_ROOT=$(cat "/opt/nudox/releases/$RELEASE_ID/luna-tools-path")
test -x "$TOOLS_ROOT/bin/rustc" && test -x "$TOOLS_ROOT/bin/cargo"
file "/opt/nudox/releases/$RELEASE_ID/bin/backend-locald" \
  "/opt/nudox/releases/$RELEASE_ID/bin/backend-worker"
sudo ln -sfn "/opt/nudox/releases/$RELEASE_ID" /opt/nudox/current
```

These binaries may depend on libraries in the pinned Nix closure. Either
install the identical pinned closure at the same canonical paths on both
machines, or build and install on each target from the same revision. A
standalone copied executable is not promised to be portable outside that
runtime closure.

## 2. Prepare private state and the compiler authority

Use a dedicated account per process. The owner workspace contains its local
index, 32-byte MCP authority secret, owner Iroh identity, and worker allowlist.
The worker stores its Iroh identity, imported grants, and pending result
closures separately. Back up the owner's entire workspace and the worker's
config plus data directory together with the binary release identifier.

Example Linux paths. On macOS, replace `/var/lib/nudox-index` and
`/var/lib/nudox-worker` with `/var/db/nudox-index` and
`/var/db/nudox-worker` respectively throughout this runbook; keep tool and
configuration paths unchanged.

```sh
sudo install -d -o nudox-index -g nudox-index -m 0700 /var/lib/nudox-index
sudo install -d -o nudox-worker -g nudox-worker -m 0700 /var/lib/nudox-worker
sudo install -d -o root -g nudox-index -m 0750 /etc/nudox
sudo install -d -o nudox-worker -g nudox-worker -m 0700 /etc/nudox-worker
sudo install -d -o nudox-index -g nudox-index -m 0750 /opt/nudox/compiler-roots/cargo-home
```

The examples assume the `nudox-index` and `nudox-worker` accounts already
exist. Run scope inspection, owner provisioning/invites/trust, and CLI work
inside a shell as `nudox-index`; run worker identity/import/pending commands
inside a shell as `nudox-worker`. On Linux or macOS, open those shells with
`sudo -u nudox-index -H /bin/bash` and `sudo -u nudox-worker -H /bin/bash`.
Keep the shells separate and source each machine's own authority file. Use
the administrator shell for directory/env-file installation, binary
installation, cache transfer, and service management.

Generate the owner's private 32-byte authority secret once, as
`nudox-index`, before starting locald. Do not overwrite an existing secret or
copy it to a worker:

```sh
test ! -e /var/lib/nudox-index/authority.secret
(umask 077; openssl rand 32 > /var/lib/nudox-index/authority.secret)
chmod 0600 /var/lib/nudox-index/authority.secret
test "$(wc -c < /var/lib/nudox-index/authority.secret)" -eq 32
```

Back up this file with the owner workspace. Losing it invalidates local
authority credentials.

Install a pinned compiler closure at the *same absolute, canonical path* on
both machines. `NUDOX_RUSTC`, `NUDOX_CARGO`, `NUDOX_RUST_SYSROOT`,
`NUDOX_CARGO_HOME`, and `NUDOX_CARGO_ROOT` must each name absolute paths.
Cargo home must exist as a directory; Cargo root must exist as the Cargo
registry source directory. The locked project's dependencies must be
available offline in the Cargo home/package root on both hosts. Keep their
verified contents identical and do not put registry credentials in a cache
copied to a worker.

Extract the values from the installed closure on each target. Set
`RELEASE_ID` to the short revision from the build/copy step. This uses the
closure path recorded in the release archive, so the target needs no checkout
of the source repository and cannot accidentally select its host Rust tools.
The owner and worker must report the same `TOOLS_ROOT` path and Rust paths.

```sh
RELEASE_ID=replace-with-the-short-revision-printed-by-git
TOOLS_ROOT=$(cat "/opt/nudox/releases/$RELEASE_ID/luna-tools-path")
RUSTC="$TOOLS_ROOT/bin/rustc"
CARGO="$TOOLS_ROOT/bin/cargo"
SYSROOT=$("$RUSTC" --print sysroot)
test -x "$RUSTC" && test -x "$CARGO" && test -d "$SYSROOT"
printf 'Nix tools: %s\nRustc: %s\nCargo: %s\nSysroot: %s\n' \
  "$TOOLS_ROOT" "$RUSTC" "$CARGO" "$SYSROOT"
```

Create the environment files as an administrator on each host, using the same
release and canonical paths. The Cargo registry source root and home paths
below are examples; keep their absolute values and verified dependency
contents identical on owner and worker. For a project with no registry
dependencies, an empty source root and Cargo home are sufficient. For a real
lockfile, provision the locked registry cache on the owner before enrollment;
compiler execution is offline.

```sh
# Administrator shell on the owner:
RELEASE_ID=replace-with-the-short-revision-printed-by-git
TOOLS_ROOT=$(cat "/opt/nudox/releases/$RELEASE_ID/luna-tools-path")
RUSTC="$TOOLS_ROOT/bin/rustc"
CARGO="$TOOLS_ROOT/bin/cargo"
SYSROOT=$("$RUSTC" --print sysroot)
sudo install -d -o nudox-index -g nudox-index -m 0750 \
  /opt/nudox/compiler-roots/cargo-home
sudo install -d -o nudox-index -g nudox-index -m 0750 \
  /opt/nudox/compiler-roots/cargo-home/registry/src
printf 'NUDOX_RUSTC=%s\nNUDOX_CARGO=%s\nNUDOX_RUST_SYSROOT=%s\nNUDOX_CARGO_HOME=%s\nNUDOX_CARGO_ROOT=%s\n' \
  "$RUSTC" "$CARGO" "$SYSROOT" \
  /opt/nudox/compiler-roots/cargo-home \
  /opt/nudox/compiler-roots/cargo-home/registry/src \
  | sudo tee /etc/nudox/compiler.env >/dev/null
sudo chown root:nudox-index /etc/nudox/compiler.env
sudo chmod 0640 /etc/nudox/compiler.env
```

Switch to the owner service account. Warm only the locked dependency cache;
this may contact the configured registry. The worker uses the copied cache
offline. The `registry` directory excludes Cargo's credentials file:

```sh
set -a
. /etc/nudox/compiler.env
set +a
PROJECT=$(cd -P /srv/repository && pwd)
CARGO_HOME="$NUDOX_CARGO_HOME" "$NUDOX_CARGO" fetch --locked \
  --manifest-path "$PROJECT/Cargo.toml"
rm -f /tmp/nudox-cargo-registry.tgz
umask 077
tar -C /opt/nudox/compiler-roots/cargo-home \
  -czf /tmp/nudox-cargo-registry.tgz registry
chmod 0600 /tmp/nudox-cargo-registry.tgz
scp /tmp/nudox-cargo-registry.tgz \
  worker-admin@10.0.0.20:/tmp/nudox-cargo-registry.tgz
rm -f /tmp/nudox-cargo-registry.tgz
```

On each worker, repeat the value extraction as an administrator and install
the cache and environment file with the worker account/group:

```sh
RELEASE_ID=replace-with-the-short-revision-printed-by-git
TOOLS_ROOT=$(cat "/opt/nudox/releases/$RELEASE_ID/luna-tools-path")
RUSTC="$TOOLS_ROOT/bin/rustc"
CARGO="$TOOLS_ROOT/bin/cargo"
SYSROOT=$("$RUSTC" --print sysroot)
sudo install -d -o nudox-worker -g nudox-worker -m 0750 \
  /opt/nudox/compiler-roots/cargo-home
sudo tar -C /opt/nudox/compiler-roots/cargo-home \
  -xzf /tmp/nudox-cargo-registry.tgz
sudo chown -R nudox-worker:nudox-worker /opt/nudox/compiler-roots/cargo-home
printf 'NUDOX_RUSTC=%s\nNUDOX_CARGO=%s\nNUDOX_RUST_SYSROOT=%s\nNUDOX_CARGO_HOME=%s\nNUDOX_CARGO_ROOT=%s\n' \
  "$RUSTC" "$CARGO" "$SYSROOT" \
  /opt/nudox/compiler-roots/cargo-home \
  /opt/nudox/compiler-roots/cargo-home/registry/src \
  | sudo tee /etc/nudox-worker/compiler.env >/dev/null
sudo chown root:nudox-worker /etc/nudox-worker/compiler.env
sudo chmod 0640 /etc/nudox-worker/compiler.env
sudo rm -f /tmp/nudox-cargo-registry.tgz
```

Keep the files' contents and canonical values byte-for-byte identical. The
`nudox-*` service accounts must be members of their respective groups.
Production discovery is explicit-only: having Rust and Cargo on `PATH` is not
enough. `NUDOX_CARGO` must name the Cargo executable beside the selected Nix
Rust compiler, and `NUDOX_CARGO_HOME` is part of the enrolled capability.

Go compiler authority captures the nearest `go.work` path and bounded contents
as an input, and it ignores an ambient developer-shell `GOWORK` value. In the
current coordinator, a project with a selected `go.work` is local-only and
will not be delegated to an Iroh worker. For other profiles, pin and set their
typed authority paths in the same file on both machines: `NUDOX_CLANG`,
`NUDOX_PYTHON`, `NUDOX_TSC`, `NUDOX_GO`,
`NUDOX_JAVAC`, or `NUDOX_DOTNET`; applicable helpers are
`NUDOX_TYPESCRIPT_NODE`, `NUDOX_TYPESCRIPT_MODULE_ROOT`,
`NUDOX_TYPESCRIPT_REPORT_PROGRAM`, `NUDOX_PYREFLY`, `NUDOX_GO_ORACLE`,
`NUDOX_JDK`, and `NUDOX_ROSLYN_HELPER`. Package roots are
`NUDOX_CARGO_ROOT`, `NUDOX_NPM_ROOT`, `NUDOX_PYPI_ROOT`, `NUDOX_GO_ROOT`,
`NUDOX_MAVEN_ROOT`, `NUDOX_NUGET_ROOT`, and `NUDOX_GENERIC_ROOT`. Set only
paths required for the selected language, but configure those paths
identically on owner and worker. Production discovery is explicit-only; a
compiler that happens to be on `PATH` is not admitted.

The exact grant also binds OS and architecture, compiler version-probe output,
selected absolute compiler/helper/package-root paths, and compiler options.
The toolchain identity hashes version-probe output rather than every byte of
the executable and sysroot; package authority identities bind selected paths,
not every byte in their registry caches. For reliable matching, use the same
pinned Nix closure, identical canonical path mounts, matching OS/architecture,
and identical verified dependency contents. Keep these values unchanged after
enrollment; changing them requires a new scope report and worker grant.

The repository's pinned development shell and `backend` command wrapper set
`NUDOX_RUSTC` and `NUDOX_CARGO` from the same Fenix toolchain. If
`NUDOX_CARGO_HOME` is not supplied explicitly, they use absolute `CARGO_HOME`
when configured, otherwise `$HOME/.cargo` when `HOME` is absolute. The owner
still checks the complete tuple at runtime; missing or relative Cargo-home
configuration produces a typed Rust capability refusal. Desktop startup uses
the selected toolchain provisioner to construct the same explicit tuple.

## 3. Configure addresses and open the direct Iroh path

Choose stable private IP addresses and distinct UDP ports. In the examples,
the owner is `10.0.0.10:40123` and the worker is `10.0.0.20:40124`. The owner
bind can be a wildcard, but its advertised address must be the exact IP the
worker can reach. Addresses are numeric IP literals; DNS names are not
accepted. Permit UDP between each owner/worker pair in both directions on the
chosen ports. For UFW, for example:

```sh
# Owner machine
sudo ufw allow from 10.0.0.20 to 10.0.0.10 port 40123 proto udp
sudo ufw allow out to 10.0.0.20 port 40124 proto udp

# Worker machine
sudo ufw allow from 10.0.0.10 to 10.0.0.20 port 40124 proto udp
sudo ufw allow out to 10.0.0.10 port 40123 proto udp
```

Apply equivalent narrow rules in cloud security groups and any upstream
firewall. Allow the worker's outbound UDP to the owner address/port as well.
The protocol intentionally has no relay or NAT traversal: both processes
need direct UDP reachability. Local CLI/MCP/GUI traffic stays on the owner's
Unix socket and must not be exposed as a network listener.

## 4. Inspect and enroll one exact compiler scope

Use the same project path and profile that indexing will use. Resolve the
owner checkout to its physical absolute path and use that spelling everywhere;
do not use a host-specific symlink alias. For a local Rust project,
`rust-2024` is the profile name and the canonical package label is the
absolute project path. If the indexing command uses a pinned package URL
or supplies `--coordinate`, pass the same package and coordinate here. The
scope command performs bounded probes without starting locald or creating
runtime state. Source it with the same toolchain environment file that the
service manager will load:

```sh
set -a
. /etc/nudox/compiler.env
set +a

TOOLS_ROOT=$(cat /opt/nudox/current/luna-tools-path)
JQ="$TOOLS_ROOT/bin/jq"
BACKEND=/opt/nudox/current/bin/backend-cli
OWNER_DATA=/var/lib/nudox-index
PROJECT=$(cd -P /srv/repository && pwd)
PACKAGE_REF="$PROJECT"
PROFILE_NAME=rust-2024

SCOPE_JSON=$("$BACKEND" --workspace "$OWNER_DATA" --project "$PROJECT" \
  --format json cluster scope show --package "$PACKAGE_REF" \
  --profile "$PROFILE_NAME")
printf '%s\n' "$SCOPE_JSON" | "$JQ" '{target_kind,package,coordinate,namespace,recipe,profile,profile_name,stage,toolchain,environment,target_platform}'
IFS=$'\t' read -r NAMESPACE RECIPE PROFILE TOOLCHAIN ENVIRONMENT TARGET_PLATFORM \
  < <(printf '%s\n' "$SCOPE_JSON" | "$JQ" -er \
    '[.namespace,.recipe,.profile,.toolchain,.environment,.target_platform] | @tsv')
```

The final command reads all six enrollment fields from the JSON report; do
not transcribe hashes by hand. Keep these variables in this shell for owner
provisioning and invite creation. Save the report and copy it to the worker;
it contains public identity hashes, not credentials:

```sh
printf '%s\n' "$SCOPE_JSON" > /tmp/owner-scope.json
scp /tmp/owner-scope.json worker-admin@10.0.0.20:/tmp/owner-scope.json
```

The worker does not need the source checkout for scope inspection or later
assignments; it receives bounded source snapshots. It does need the same
physical package-path spelling as the owner, even if that path is absent on
the worker. Use that exact absolute spelling and avoid a symlink alias. On the
worker, run the corresponding command as `nudox-worker`, source
`/etc/nudox-worker/compiler.env`, and use its installed `backend-cli` as an
operator tool. Use the same package label and profile, then compare the exact
fields:

```sh
set -a
. /etc/nudox-worker/compiler.env
set +a
TOOLS_ROOT=$(cat /opt/nudox/current/luna-tools-path)
JQ="$TOOLS_ROOT/bin/jq"
PROJECT=/srv/repository # replace with the owner's physical absolute path
PACKAGE_REF="$PROJECT"
WORKER_SCOPE_JSON=$( /opt/nudox/current/bin/backend-cli \
  --workspace /var/lib/nudox-worker --project "$PROJECT" --format json \
  cluster scope show --package "$PACKAGE_REF" --profile rust-2024 )
IFS=$'\t' read -r NAMESPACE RECIPE PROFILE TOOLCHAIN ENVIRONMENT TARGET_PLATFORM \
  < <(printf '%s\n' "$WORKER_SCOPE_JSON" | "$JQ" -er \
    '[.namespace,.recipe,.profile,.toolchain,.environment,.target_platform] | @tsv')
diff -u \
  <("$JQ" -S '{namespace,recipe,profile,toolchain,environment,target_platform}' /tmp/owner-scope.json) \
  <(printf '%s\n' "$WORKER_SCOPE_JSON" | "$JQ" -S '{namespace,recipe,profile,toolchain,environment,target_platform}')
```

Any difference is a stop condition: fix the OS/architecture or authority paths
and inspect again before granting access. Do not copy only `recipe` and assume
the remaining fields will match.

Create the owner's persistent Iroh identity once, before starting locald. The
owner config and allowlist are stored under `OWNER_DATA`:

```sh
"$BACKEND" --workspace "$OWNER_DATA" --project "$PROJECT" \
  cluster owner init --bind 0.0.0.0:40123 --advertise 10.0.0.10:40123
"$BACKEND" --workspace "$OWNER_DATA" --project "$PROJECT" cluster owner show
```

On the worker's `nudox-worker` shell, create its persistent identity. Copy the
printed **worker identity fingerprint** to the owner; it is the public peer
ID, not the invite fingerprint:

```sh
/opt/nudox/current/bin/backend-worker cluster init \
  --config /etc/nudox-worker/cluster.bin --bind 10.0.0.20:40124 \
  --namespace "$NAMESPACE" --recipe "$RECIPE"
/opt/nudox/current/bin/backend-worker cluster identity show \
  --config /etc/nudox-worker/cluster.bin
```

At the owner, set `WORKER_PEER` to that printed identity and create a short
one-time invite. The CLI prints the token and the **invite fingerprint**. The
owner command also persists the exact peer/scope grant in
`compiler-worker-trust.v1`. Capture the output in a private variable so the
token does not enter shell history or terminal scrollback:

```sh
WORKER_PEER='paste-worker-identity-fingerprint-here'
INVITE_OUTPUT=$("$BACKEND" --workspace "$OWNER_DATA" --project "$PROJECT" \
  cluster invite create --worker-peer "$WORKER_PEER" \
  --worker-address 10.0.0.20:40124 --namespace "$NAMESPACE" \
  --recipe "$RECIPE" --profile "$PROFILE" --stage lower-ir \
  --toolchain "$TOOLCHAIN" --environment "$ENVIRONMENT" \
  --target-platform "$TARGET_PLATFORM" --ttl-seconds 900)
INVITE_TOKEN=$(printf '%s\n' "$INVITE_OUTPUT" | sed -n \
  's/^Import this one-time token on the worker: //p')
INVITE_FINGERPRINT=$(printf '%s\n' "$INVITE_OUTPUT" | sed -n \
  's/^Fingerprint: //p')
test -n "$INVITE_TOKEN" && test -n "$INVITE_FINGERPRINT"
unset INVITE_OUTPUT
```

Transfer the invite token and fingerprint over SSH or another authenticated,
private channel. This example uses a mode-0600 temporary file and `scp`; use
the actual worker login and host name. The invite expires after 15 minutes:

```sh
umask 077
INVITE_FILE=$(mktemp)
printf '%s\n%s\n' "$INVITE_TOKEN" "$INVITE_FINGERPRINT" > "$INVITE_FILE"
scp "$INVITE_FILE" worker-admin@10.0.0.20:/tmp/nudox-worker-invite
rm -f "$INVITE_FILE"
unset INVITE_TOKEN INVITE_FINGERPRINT
```

On the worker, source the same compiler authority configuration before trust
import. `trust import` verifies the token fingerprint and recomputes the
worker's exact compiler capability; this is why the toolchain environment is
required during enrollment as well as during service startup. The one-time
token briefly appears in the import process arguments, so run it on a host
where untrusted local users cannot inspect the service account's processes.
After `scp`, the worker administrator must transfer the invite file to the
worker service account before switching to its shell:

```sh
sudo chown nudox-worker:nudox-worker /tmp/nudox-worker-invite
sudo chmod 0600 /tmp/nudox-worker-invite
```

```sh
set -a
. /etc/nudox-worker/compiler.env
set +a
INVITE_FILE=/tmp/nudox-worker-invite
IFS= read -r INVITE_TOKEN < "$INVITE_FILE"
INVITE_FINGERPRINT=$(sed -n '2p' "$INVITE_FILE")
/opt/nudox/current/bin/backend-worker cluster trust import \
  --config /etc/nudox-worker/cluster.bin \
  --data-dir /var/lib/nudox-worker \
  --invite "$INVITE_TOKEN" --fingerprint "$INVITE_FINGERPRINT"
unset INVITE_TOKEN INVITE_FINGERPRINT
rm -f "$INVITE_FILE"
```

Do not run `cluster owner init` or `cluster init` again over existing
identities. A worker denies work until trust import succeeds. Revoke a peer
from the owner with:

```sh
/opt/nudox/current/bin/backend-cli --workspace /var/lib/nudox-index \
  cluster trust revoke --peer "$WORKER_PEER"
```

Then rotate the worker identity and issue a fresh grant if it is re-enrolled.

## 5. Supervise the services

Create `/etc/nudox/compiler.env` exactly as above. If S3 is enabled, create a
separate owner-only `/etc/nudox/owner-s3.env` with mode `0640`, owned by
`root:nudox-index`; never install it on a worker. Use an editor so the secret
does not enter shell history:

```sh
sudo install -o root -g nudox-index -m 0640 /dev/null /etc/nudox/owner-s3.env
sudoedit /etc/nudox/owner-s3.env
```

Enter these values in the editor, replacing each placeholder:

```dotenv
BACKEND_S3_ENDPOINT=https://s3.example.internal
BACKEND_S3_BUCKET=nudox-index-packs
BACKEND_S3_REGION=us-east-1
BACKEND_S3_ACCESS_KEY_ID=replace-with-owner-only-key
BACKEND_S3_SECRET_ACCESS_KEY=replace-with-owner-only-secret
BACKEND_S3_PREFIX=production/
# Optional:
# BACKEND_S3_SESSION_TOKEN=...
```

After saving, restore the required owner-only file permissions:

```sh
sudo chown root:nudox-index /etc/nudox/owner-s3.env
sudo chmod 0640 /etc/nudox/owner-s3.env
```

Set all five required S3 values together. The prefix, when set, must end in
`/`; production release mode requires HTTPS. Use a private bucket and a key
restricted to that bucket/prefix. With no `BACKEND_S3_*` variables the owner
uses its local content-addressed store.

For Linux, as administrator, use `sudoedit` to save the following owner unit
as `/etc/systemd/system/nudox-index.service` with mode `0644`:

```ini
[Unit]
Description=Nudox local index owner
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=nudox-index
Group=nudox-index
UMask=0077
EnvironmentFile=/etc/nudox/compiler.env
EnvironmentFile=-/etc/nudox/owner-s3.env
Environment=BACKEND_PROJECT=/srv/repository
WorkingDirectory=/srv/repository
ExecStart=/opt/nudox/current/bin/backend-locald --workspace /var/lib/nudox-index --endpoint /var/lib/nudox-index/locald.sock --profile builtin --authority-secret-file /var/lib/nudox-index/authority.secret --idle-timeout-ms 0
Restart=on-failure
RestartSec=2
TimeoutStopSec=30
ReadWritePaths=/var/lib/nudox-index

[Install]
WantedBy=multi-user.target
```

Use `sudoedit` to save the following worker unit on each worker as
`/etc/systemd/system/nudox-worker.service`, also mode `0644`:

```ini
[Unit]
Description=Nudox trusted compiler worker
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=nudox-worker
Group=nudox-worker
UMask=0077
EnvironmentFile=/etc/nudox-worker/compiler.env
ExecStart=/opt/nudox/current/bin/backend-worker cluster run --config /etc/nudox-worker/cluster.bin --data-dir /var/lib/nudox-worker
Restart=on-failure
RestartSec=2
TimeoutStopSec=30
ReadWritePaths=/var/lib/nudox-worker

[Install]
WantedBy=multi-user.target
```

Check every service account can read its compiler closure and project/package
inputs and can write only its own state directories. Then enable and start:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now nudox-index.service  # owner
sudo systemctl enable --now nudox-worker.service # worker
```

For macOS, create the same owner/worker directories under `/var/db` and use
root-owned launch daemons. Because launchd has no `EnvironmentFile`, create a
root-owned wrapper that loads the compiler paths (and loads S3 credentials on
the owner only). Create `/usr/local/libexec`, then write
`/usr/local/libexec/nudox-index`:

```sh
sudo install -d -o root -g wheel -m 0755 /usr/local/libexec
sudo tee /usr/local/libexec/nudox-index >/dev/null <<'EOF'
#!/bin/sh
set -eu
set -a
. /etc/nudox/compiler.env
if [ -r /etc/nudox/owner-s3.env ]; then . /etc/nudox/owner-s3.env; fi
set +a
exec /opt/nudox/current/bin/backend-locald --workspace /var/db/nudox-index --endpoint /var/db/nudox-index/locald.sock --profile builtin --authority-secret-file /var/db/nudox-index/authority.secret --idle-timeout-ms 0
EOF
sudo chown root:wheel /usr/local/libexec/nudox-index
sudo chmod 0755 /usr/local/libexec/nudox-index
```

Create `/usr/local/libexec/nudox-worker` on the worker without sourcing the
S3 file:

```sh
sudo tee /usr/local/libexec/nudox-worker >/dev/null <<'EOF'
#!/bin/sh
set -eu
set -a
. /etc/nudox-worker/compiler.env
set +a
exec /opt/nudox/current/bin/backend-worker cluster run --config /etc/nudox-worker/cluster.bin --data-dir /var/db/nudox-worker
EOF
sudo chown root:wheel /usr/local/libexec/nudox-worker
sudo chmod 0755 /usr/local/libexec/nudox-worker
```

Create both root-owned launch daemon plists and lock down their modes:

```sh
sudo tee /Library/LaunchDaemons/com.nudox.index.plist >/dev/null <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.nudox.index</string>
  <key>UserName</key><string>nudox-index</string>
  <key>ProgramArguments</key><array><string>/usr/local/libexec/nudox-index</string></array>
  <key>WorkingDirectory</key><string>/var/db/nudox-index</string>
  <key>KeepAlive</key><true/>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>/var/log/nudox-index.log</string>
  <key>StandardErrorPath</key><string>/var/log/nudox-index-error.log</string>
</dict></plist>
EOF
sudo tee /Library/LaunchDaemons/com.nudox.worker.plist >/dev/null <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.nudox.worker</string>
  <key>UserName</key><string>nudox-worker</string>
  <key>ProgramArguments</key><array><string>/usr/local/libexec/nudox-worker</string></array>
  <key>WorkingDirectory</key><string>/var/db/nudox-worker</string>
  <key>KeepAlive</key><true/>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>/var/log/nudox-worker.log</string>
  <key>StandardErrorPath</key><string>/var/log/nudox-worker-error.log</string>
</dict></plist>
EOF
sudo chown root:wheel /Library/LaunchDaemons/com.nudox.index.plist \
  /Library/LaunchDaemons/com.nudox.worker.plist
sudo chmod 0644 /Library/LaunchDaemons/com.nudox.index.plist \
  /Library/LaunchDaemons/com.nudox.worker.plist
```

Keep env files root-owned and readable only by their service group; do not
embed S3 secrets in a broadly readable plist. Load/check/restart with:

```sh
sudo launchctl bootstrap system /Library/LaunchDaemons/com.nudox.index.plist
sudo launchctl bootstrap system /Library/LaunchDaemons/com.nudox.worker.plist
sudo launchctl print system/com.nudox.index
sudo launchctl print system/com.nudox.worker
sudo launchctl kickstart -k system/com.nudox.index
sudo launchctl kickstart -k system/com.nudox.worker
```

Constrain macOS Packet Filter or upstream firewall rules to the two peer IPs
and configured UDP ports. Do not use a different toolchain file for the
interactive owner CLI than the service loaded. In a shell, source it before
`cluster scope show`, owner CLI operations, and CLI indexing.

## 6. Run a real index/worker/client check

The first Add establishes the owner's local placement calibration. Change a
source input and submit a background Add to exercise an actual remote
assignment. Use a small disposable Rust project if this is a new deployment;
the example is intentionally isolated from a production checkout. As admin,
create its root for the index account, then switch to the `nudox-index` shell
and source `/etc/nudox/compiler.env` before running the commands:

```sh
sudo install -d -o nudox-index -g nudox-index -m 0750 /srv/nudox-cluster-smoke
```

```sh
PROJECT=/srv/nudox-cluster-smoke
set -a
. /etc/nudox/compiler.env
set +a
mkdir -p "$PROJECT/src"
cat > "$PROJECT/Cargo.toml" <<'EOF'
[package]
name = "nudox-cluster-smoke"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
EOF
cat > "$PROJECT/src/lib.rs" <<'EOF'
/// CLUSTER_SMOKE_BEFORE
pub fn cluster_deploy_smoke() -> &'static str { "indexed" }
EOF

CARGO_HOME="$NUDOX_CARGO_HOME" "$NUDOX_CARGO" generate-lockfile --offline \
  --manifest-path "$PROJECT/Cargo.toml"

BACKEND=/opt/nudox/current/bin/backend-cli
OWNER_DATA=/var/lib/nudox-index
ENDPOINT="$OWNER_DATA/locald.sock"
"$BACKEND" --workspace "$OWNER_DATA" --project "$PROJECT" \
  --endpoint "$ENDPOINT" add "$PROJECT"

sed -i.bak 's/CLUSTER_SMOKE_BEFORE/CLUSTER_SMOKE_AFTER_/' "$PROJECT/src/lib.rs"
rm -f "$PROJECT/src/lib.rs.bak"

"$BACKEND" --workspace "$OWNER_DATA" --project "$PROJECT" \
  --endpoint "$ENDPOINT" --format json add "$PROJECT" \
  --execution-intent background
```

The owner and worker logs should show the accepted remote result and
`result owner ACK: Stored`. On the worker, verify no retained result remains
after the owner ACK:

```sh
/opt/nudox/current/bin/backend-worker cluster pending \
  --config /etc/nudox-worker/cluster.bin --data-dir /var/lib/nudox-worker
```

On the owner, verify the selected head, indexed symbol, document, and local
MCP path:

```sh
"$BACKEND" --workspace "$OWNER_DATA" --project "$PROJECT" \
  --endpoint "$ENDPOINT" --format json semantic-versions "$PROJECT"
"$BACKEND" --workspace "$OWNER_DATA" --project "$PROJECT" \
  --endpoint "$ENDPOINT" --format json search cluster_deploy_smoke --limit 20
"$BACKEND" --workspace "$OWNER_DATA" --project "$PROJECT" \
  --endpoint "$ENDPOINT" --format json cluster trust list

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"deployment-check","version":"1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"backend.search","arguments":{"query":"cluster_deploy_smoke","limit":20}}}' \
  | /opt/nudox/current/bin/backend-mcp --project "$PROJECT" \
      --workspace "$OWNER_DATA" --endpoint "$ENDPOINT"
```

The search must return `cluster_deploy_smoke`; `semantic-versions` must show
a selected generation; MCP's `backend.search` must return the same symbol.
The desktop application currently works with its login user's local owner.
Its launch environment may select that local session with `BACKEND_PROJECT`,
`BACKEND_LOCALD_WORKSPACE`, `BACKEND_LOCALD_ENDPOINT`, and
`BACKEND_LOCALD_AUTHORITY_SECRET_FILE`; all paths must belong to that same
login UID. It cannot use the `nudox-index` headless service workspace from a
different UID. The GUI does not connect to the compiler worker directly.

Cold-restart both services without deleting state, then repeat the pending,
CLI search, semantic history, and MCP checks. Linux:

```sh
sudo systemctl restart nudox-index.service
sudo systemctl restart nudox-worker.service
sudo systemctl --no-pager --full status nudox-index.service nudox-worker.service
```

macOS: run `launchctl kickstart -k` for both labels shown above. The owner
workspace and worker data directory are the recovery records; never clear a
worker's pending results to force a retry. `cluster pending` and service logs
show whether the owner has acknowledged stored results.

## Rollback and diagnostics

Before upgrading, stop both services and take one consistent, access-controlled
backup of the owner workspace and the worker config/data directories. Record
the release ID, scope JSON, and compiler closure paths. To roll back, stop
both processes, restore the matching backup of all durable state, restore the
matching owner and worker binaries/compiler closure, then start owner before
worker. Do not run an older binary against state already opened by a newer
release; if no matching backup exists, roll forward with a compatible build.
Keep the owner identity, worker identity, authority secret, allowlist, and
pending result data together through the restore.

For Linux, inspect `journalctl -u nudox-index -u nudox-worker -f`; on macOS,
inspect each launch daemon's configured stdout/stderr files and
`launchctl print`. Check the exact conditions in this order:

1. `backend-cli ... cluster owner show` reports the expected advertised IP
   and port; `backend-worker ... cluster identity show` reports the expected
   direct endpoint and no relay.
2. The two hosts pass narrow UDP reachability in both directions. Neither
   firewall nor a NAT is blocking direct Iroh traffic.
3. The scope JSON on owner and worker agrees for `namespace`, `recipe`,
   `profile`, `toolchain`, `environment`, and `target_platform`.
4. The worker import used the matching invite fingerprint, and owner
   `cluster trust list` still contains that peer and exact grant.
5. The configured compiler/helper executables and roots exist at the same
   canonical paths on both hosts. `NUDOX_RUSTC` and its sibling `cargo` must
   be executable; the sysroot and package roots must be readable.
6. The worker's `cluster pending` reports whether results await owner ACK;
   preserve that directory while diagnosing selection or transfer failures.
7. The owner CLI's `semantic-versions`, `search`, and `show`, and local MCP
   query all use the expected workspace and socket.

When `BACKEND_S3_*` is enabled, diagnose owner credentials and bucket policy
on the owner only. Workers do not publish index packs. S3 availability does
not replace the owner's local selected-head journal or authority secret.

This process journey covers a real local owner and Iroh worker, a real Rust
compiler, scoped trust import, selected result visibility through CLI/MCP,
and cold restart/replay. It does not establish a multi-host soak, relay/NAT
support, remote client queries, a remote GUI, complete embedding coverage, or
OS-level sandboxing. Run a release-specific cross-host smoke on the actual
target pair and compiler corpus before production rollout.

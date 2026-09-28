# Compiler nodes

A **compiler node** is a `backend-worker` process running on a separate
machine. It is the remote pure-recipe executor described in
[`docs/architecture/local-remote.md`](../../docs/architecture/local-remote.md):
the local engine keeps the one durable truth, and compiler nodes accelerate the
pure compilation/analysis recipes by executing the same version-bound work over
an authenticated TCP stream.

The four processes in this repo map to product roles like this:

| Role            | Binary            | Crate         |
|-----------------|-------------------|---------------|
| GUI             | `backend-desktop` | `apps/desktop`|
| MCP service     | `backend-mcp`     | `apps/mcp`    |
| Index daemon    | `backend-locald`  | `apps/locald` |
| **Compiler node** | `backend-worker`  | `apps/worker` |

## What the installer does

`install.sh` gets `backend-worker` onto any Linux or macOS machine and runs it
as a compiler node:

1. Finds the source (uses the current checkout, or clones the repo).
2. Ensures the pinned Rust toolchain (`1.97.1`) via `rustup`, installing
   `rustup` if needed.
3. Builds `backend-worker` in release mode (`cargo build --locked --release`).
4. Installs the binary to `<prefix>/bin`.
5. Creates or reuses the shared 32-byte **authority key** (mode `0600`).
6. Installs a service (systemd on Linux, launchd on macOS) that runs the node.

> There is no prebuilt worker binary to download today — the published GitHub
> releases only ship `backend-mcp`/`backend-locald` for macOS arm64. The
> installer builds from source, so the first run takes a few minutes and needs
> network access to fetch crates.

## Usage

From a fresh machine (clones + builds):

```sh
curl -fsSL https://raw.githubusercontent.com/nudoxorg/Backend/canonical/deploy/compiler-node/install.sh | sh
```

From an existing checkout:

```sh
deploy/compiler-node/install.sh
```

Common overrides (flags or env vars):

```sh
# Bind to a private VPN interface on a custom port, reuse a shared key:
deploy/compiler-node/install.sh \
  --bind 10.8.0.4:8760 \
  --authority-secret /secure/backend-authority.secret

# Just build + install the binary and key, no service:
NODE_NO_SERVICE=1 deploy/compiler-node/install.sh
```

See `install.sh --help` for the full option list.

## The authority key is shared by both sides

The worker's TCP listener runs a **mutual authority handshake**: the node
(server) and the coordinator that dispatches to it (client) must possess the
**same 32-byte key**. It is a symmetric secret, not a per-node identity.

- Generate it **once** (the installer does this on the first node), then pass
  the identical file to every other node and to the coordinator via
  `--authority-secret` / `NODE_AUTHORITY_SECRET`.
- The file must be exactly 32 raw bytes, owned by the running user, mode `0600`
  — the loader rejects anything else (wrong length, group/world-readable,
  symlink, wrong owner).
- The installer prints the key in hex at the end. Hand that to whoever
  configures the coordinator.

Generate one by hand if you prefer:

```sh
umask 077 && head -c 32 /dev/urandom > backend-authority.secret
```

## Security model

The post-handshake record stream is **MAC-authenticated but plaintext**. The
worker will bind a loopback address freely, but it **refuses a routable
(non-loopback) bind** unless external exposure is acknowledged — the installer
passes `--external-protected-transport` automatically for a non-loopback
`--bind`, which asserts that an outer confidential transport protects the port.

So for a node reachable from another host, put it behind one of:

- a WireGuard / VPN interface (bind the node to its VPN address),
- an SSH tunnel, or
- a mutually authenticated TLS proxy.

Never expose the worker port directly on the public internet.

## Running by hand

The service just runs this; you can run it in the foreground to debug:

```sh
backend-worker \
  --tcp-listen 0.0.0.0:8760 \
  --authority-secret-file /usr/local/etc/backend-compiler-node/authority.secret \
  --external-protected-transport
```

Other flags: `--profile builtin` (default; the checked production recipe
executor), `--max-frame BYTES`, `--timeout-ms MS`. For a local loopback test
node, drop `--external-protected-transport` and bind `127.0.0.1`.

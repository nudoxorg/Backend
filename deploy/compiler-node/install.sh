#!/bin/sh
# Compiler-node installer for the Nudox backend.
#
# A "compiler node" is a `backend-worker` process: the remote pure-recipe
# executor that the local engine offloads compilation/analysis work to over an
# authenticated TCP stream (see docs/architecture/local-remote.md, "Worker
# protocol and result acceptance"). This script gets that one binary onto any
# Linux or macOS machine and runs it as a service.
#
# There is no prebuilt worker binary published today (the v0.2.x GitHub
# releases only ship backend-mcp/backend-locald for macOS arm64), so this
# installs from source with the pinned Rust toolchain. The build takes a few
# minutes on a cold machine.
#
# Quick start (build from a fresh clone on a remote host):
#   curl -fsSL https://raw.githubusercontent.com/nudoxorg/Backend/canonical/deploy/compiler-node/install.sh | sh
#
# Quick start (from an existing checkout):
#   deploy/compiler-node/install.sh
#
# The authority secret is a single 32-byte key shared by BOTH sides of the
# handshake. Every compiler node AND the coordinator that dispatches to them
# must hold the identical key. Generate it once, then pass the SAME file to
# every node via --authority-secret / NODE_AUTHORITY_SECRET.
#
# Environment variables (all optional; flags override them):
#   NODE_REPO_URL        Git URL to clone when not run inside a checkout.
#                        Default: https://github.com/nudoxorg/Backend.git
#   NODE_REPO_REF        Branch/tag/commit to build. Default: canonical
#   NODE_BIND            host:port the worker listens on. Default: 0.0.0.0:8760
#   NODE_AUTHORITY_SECRET Path to an existing 32-byte authority key to reuse.
#   NODE_PREFIX          Install prefix for the binary. Default: /usr/local
#                        (falls back to $HOME/.local when not writable).
#   NODE_STATE_DIR       Where the authority key is stored when generated.
#                        Default: <prefix>/etc/backend-compiler-node
#   NODE_EXPOSURE        loopback | external. Default: external for a routable
#                        bind, loopback for a 127.0.0.1 bind.
#   NODE_SERVICE         systemd | launchd | none. Default: autodetect.
#   NODE_NO_SERVICE      When set, install the binary + key only, no service.
#
# The TCP record stream is MAC-authenticated but plaintext. A routable bind
# (anything other than a loopback address) therefore requires an outer
# confidential transport -- a WireGuard/VPN interface, an SSH tunnel, or a
# mutually authenticated proxy -- and the worker refuses a non-loopback bind
# unless external exposure is acknowledged. Do not expose the port on the open
# internet.

set -eu

REPO_URL="${NODE_REPO_URL:-https://github.com/nudoxorg/Backend.git}"
REPO_REF="${NODE_REPO_REF:-canonical}"
BIND="${NODE_BIND:-0.0.0.0:8760}"
AUTHORITY_SECRET="${NODE_AUTHORITY_SECRET:-}"
PREFIX="${NODE_PREFIX:-/usr/local}"
STATE_DIR="${NODE_STATE_DIR:-}"
EXPOSURE="${NODE_EXPOSURE:-}"
SERVICE="${NODE_SERVICE:-}"
NO_SERVICE="${NODE_NO_SERVICE:-}"
TOOLCHAIN="1.97.1"

log() { printf 'compiler-node: %s\n' "$*" >&2; }
die() { printf 'compiler-node: error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --repo-url) REPO_URL="$2"; shift 2 ;;
    --ref) REPO_REF="$2"; shift 2 ;;
    --bind) BIND="$2"; shift 2 ;;
    --authority-secret) AUTHORITY_SECRET="$2"; shift 2 ;;
    --prefix) PREFIX="$2"; shift 2 ;;
    --state-dir) STATE_DIR="$2"; shift 2 ;;
    --exposure) EXPOSURE="$2"; shift 2 ;;
    --service) SERVICE="$2"; shift 2 ;;
    --no-service) NO_SERVICE=1; shift ;;
    -h|--help)
      sed -n '2,60p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
done

os="$(uname -s)"
case "$os" in
  Linux|Darwin) ;;
  *) die "unsupported OS: $os (Linux and macOS only)" ;;
esac

# --- Derive defaults that depend on the bind address ------------------------
bind_host="${BIND%:*}"
if [ -z "$EXPOSURE" ]; then
  case "$bind_host" in
    127.0.0.1|::1|localhost) EXPOSURE="loopback" ;;
    *) EXPOSURE="external" ;;
  esac
fi
case "$EXPOSURE" in
  loopback|external) ;;
  *) die "NODE_EXPOSURE must be 'loopback' or 'external'" ;;
esac

# --- Locate or fetch the source tree ----------------------------------------
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_root=""
if [ -f "$script_dir/../../Cargo.toml" ] && [ -d "$script_dir/../../apps/worker" ]; then
  repo_root="$(CDPATH= cd -- "$script_dir/../.." && pwd)"
  log "building from existing checkout at $repo_root"
else
  command -v git >/dev/null 2>&1 || die "git is required to clone the source"
  repo_root="$(mktemp -d "${TMPDIR:-/tmp}/backend-compiler-node.XXXXXX")"
  log "cloning $REPO_URL ($REPO_REF) into $repo_root"
  git clone --depth 1 --branch "$REPO_REF" "$REPO_URL" "$repo_root" \
    || die "clone failed; set --ref to a valid branch/tag or --repo-url"
fi

# --- Ensure the pinned Rust toolchain ---------------------------------------
if ! command -v rustup >/dev/null 2>&1; then
  if command -v cargo >/dev/null 2>&1 && cargo "+$TOOLCHAIN" --version >/dev/null 2>&1; then
    : # a nix/fenix shell already provides the pinned toolchain
  else
    log "installing rustup (needed for the pinned Rust $TOOLCHAIN toolchain)"
    command -v curl >/dev/null 2>&1 || die "curl is required to install rustup"
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain "$TOOLCHAIN" --profile minimal \
      || die "rustup install failed"
    # shellcheck disable=SC1090
    . "$HOME/.cargo/env"
  fi
fi
if command -v rustup >/dev/null 2>&1; then
  rustup toolchain list 2>/dev/null | grep -q "^$TOOLCHAIN" \
    || rustup toolchain install "$TOOLCHAIN" --profile minimal \
    || die "could not install Rust $TOOLCHAIN"
fi

cargo_run="cargo"
if cargo "+$TOOLCHAIN" --version >/dev/null 2>&1; then
  cargo_run="cargo +$TOOLCHAIN"
fi

# --- Build the worker -------------------------------------------------------
log "building backend-worker in release mode (this can take a few minutes)"
( cd "$repo_root" && $cargo_run build --locked --release -p backend-worker ) \
  || die "build failed"
built_bin="$repo_root/target/release/backend-worker"
[ -x "$built_bin" ] || die "expected binary not found at $built_bin"

# --- Install the binary -----------------------------------------------------
install_bin_dir="$PREFIX/bin"
if ! ( mkdir -p "$install_bin_dir" && : > "$install_bin_dir/.write-probe" ) 2>/dev/null; then
  PREFIX="$HOME/.local"
  install_bin_dir="$PREFIX/bin"
  mkdir -p "$install_bin_dir"
  log "no write access to the default prefix; using $PREFIX"
fi
rm -f "$install_bin_dir/.write-probe" 2>/dev/null || true
install -m 0755 "$built_bin" "$install_bin_dir/backend-worker"
installed_bin="$install_bin_dir/backend-worker"
log "installed $installed_bin"

# --- Authority secret (the shared 32-byte key) ------------------------------
[ -n "$STATE_DIR" ] || STATE_DIR="$PREFIX/etc/backend-compiler-node"
mkdir -p "$STATE_DIR"
secret_path="$STATE_DIR/authority.secret"

if [ -n "$AUTHORITY_SECRET" ]; then
  [ -f "$AUTHORITY_SECRET" ] || die "authority secret not found: $AUTHORITY_SECRET"
  bytes="$(wc -c < "$AUTHORITY_SECRET" | tr -d ' ')"
  [ "$bytes" = "32" ] || die "authority secret must be exactly 32 bytes (got $bytes)"
  if [ "$AUTHORITY_SECRET" != "$secret_path" ]; then
    cp "$AUTHORITY_SECRET" "$secret_path"
  fi
elif [ -s "$secret_path" ]; then
  log "reusing existing authority key at $secret_path"
else
  log "generating a new 32-byte authority key at $secret_path"
  umask 077
  head -c 32 /dev/urandom > "$secret_path" \
    || die "failed to read 32 bytes from /dev/urandom"
fi
chmod 0600 "$secret_path"
bytes="$(wc -c < "$secret_path" | tr -d ' ')"
[ "$bytes" = "32" ] || die "authority key at $secret_path is $bytes bytes, not 32"

# Hex form to hand to the coordinator / other nodes.
secret_hex="$(od -An -v -tx1 "$secret_path" | tr -d ' \n')"

# --- Assemble the run command -----------------------------------------------
exposure_flag=""
[ "$EXPOSURE" = "external" ] && exposure_flag="--external-protected-transport"

run_cmd="$installed_bin --tcp-listen $BIND --authority-secret-file $secret_path $exposure_flag"

# --- Optional service install -----------------------------------------------
if [ -n "$NO_SERVICE" ]; then
  SERVICE="none"
elif [ -z "$SERVICE" ]; then
  if [ "$os" = Linux ] && command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
    SERVICE="systemd"
  elif [ "$os" = Darwin ] && command -v launchctl >/dev/null 2>&1; then
    SERVICE="launchd"
  else
    SERVICE="none"
  fi
fi

install_systemd() {
  can_sudo=""
  if [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 && can_sudo="sudo"
  fi
  unit_path="/etc/systemd/system/backend-compiler-node.service"
  # The worker's authority-key loader requires the file's owner to match the
  # process uid, so the service must run as the user that owns the key (the
  # one who ran this installer), not as root.
  run_user="$(id -un)"
  run_group="$(id -gn)"
  tmp_unit="$(mktemp)"
  cat > "$tmp_unit" <<EOF
[Unit]
Description=Nudox backend compiler node (backend-worker)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$run_user
Group=$run_group
ExecStart=$run_cmd
Restart=on-failure
RestartSec=3
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
ReadOnlyPaths=$secret_path
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF
  $can_sudo install -m 0644 "$tmp_unit" "$unit_path" || { rm -f "$tmp_unit"; die "could not write $unit_path (need root)"; }
  rm -f "$tmp_unit"
  $can_sudo systemctl daemon-reload
  $can_sudo systemctl enable --now backend-compiler-node.service
  log "systemd service backend-compiler-node.service is enabled and started"
  log "  logs:   journalctl -u backend-compiler-node -f"
  log "  status: systemctl status backend-compiler-node"
}

install_launchd() {
  label="com.nudox.backend.compiler-node"
  plist="$HOME/Library/LaunchAgents/$label.plist"
  mkdir -p "$HOME/Library/LaunchAgents"
  log_dir="$STATE_DIR/log"
  mkdir -p "$log_dir"
  cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key>
  <array>
    <string>$installed_bin</string>
    <string>--tcp-listen</string><string>$BIND</string>
    <string>--authority-secret-file</string><string>$secret_path</string>
$( [ -n "$exposure_flag" ] && printf '    <string>%s</string>\n' "$exposure_flag" )
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$log_dir/compiler-node.log</string>
  <key>StandardErrorPath</key><string>$log_dir/compiler-node.log</string>
</dict>
</plist>
EOF
  launchctl unload "$plist" 2>/dev/null || true
  launchctl load "$plist"
  log "launchd agent $label loaded"
  log "  logs: tail -f $log_dir/compiler-node.log"
}

case "$SERVICE" in
  systemd) install_systemd ;;
  launchd) install_launchd ;;
  none) log "no service manager selected; run the worker manually (see below)" ;;
  *) die "NODE_SERVICE must be systemd, launchd, or none" ;;
esac

# --- Summary ----------------------------------------------------------------
cat >&2 <<EOF

compiler-node: ready.

  binary          $installed_bin
  listening on    $BIND  (exposure: $EXPOSURE)
  authority key   $secret_path  (0600, 32 bytes)

  run manually:
    $run_cmd

  authority key (hex) -- give the SAME key to the coordinator and every node:
    $secret_hex

Security: the TCP stream is MAC-authenticated but plaintext. A non-loopback
bind must sit behind a WireGuard/VPN interface, SSH tunnel, or authenticated
proxy. Never expose $BIND directly to the internet.
EOF

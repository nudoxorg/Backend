#!/bin/bash
set -euo pipefail
umask 077

workspace="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)"
script="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)/$(basename -- "$0")"

if [[ "${1:-}" != "--inside-nix-shell" ]]; then
  if [[ "${NUDOX_LIVE_STORAGE_READINESS:-0}" != "1" ]]; then
    printf '%s\n' 'Set NUDOX_LIVE_STORAGE_READINESS=1 to opt into the joined public-registry, product MCP, Turso, and GPUI lane.' >&2
    exit 64
  fi
  turso="${NUDOX_TURSO_SQLITE_BIN:-}"
  if [[ "$turso" != /* || ! -x "$turso" ]]; then
    printf '%s\n' 'Set NUDOX_TURSO_SQLITE_BIN to an explicit absolute executable path for tursodb.' >&2
    exit 64
  fi
  turso="$(CDPATH= cd -- "$(dirname -- "$turso")" && pwd -P)/$(basename -- "$turso")"
  run_parent="$workspace/.local/live-readiness-runs"
  mkdir -m 700 -p "$run_parent"
  run_root=''
  run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
  for attempt in {0..99}; do
    candidate="$run_parent/$run_id-$attempt"
    if mkdir -m 700 "$candidate" 2>/dev/null; then
      run_root="$candidate"
      break
    fi
  done
  if [[ -z "$run_root" ]]; then
    printf '%s\n' 'Could not allocate a unique private live-readiness run directory.' >&2
    exit 73
  fi
  printf 'Live readiness artifacts: %s\n' "$run_root"
  mkdir -m 700 "$run_root/logs"
  {
    printf 'environment\t'
    printf '%q ' nix shell "$workspace#luna-tools" nixpkgs#bash nixpkgs#cargo nixpkgs#curl nixpkgs#coreutils nixpkgs#python3 --command bash "$script" --inside-nix-shell "$run_root" "$turso"
    printf '\n'
  } >"$run_root/commands.log"
  if nix shell \
    "$workspace#luna-tools" \
    nixpkgs#bash \
    nixpkgs#cargo \
    nixpkgs#curl \
    nixpkgs#coreutils \
    nixpkgs#python3 \
    --command bash "$script" --inside-nix-shell "$run_root" "$turso" \
    >"$run_root/logs/nix-environment.log" 2>&1; then
    cat "$run_root/logs/nix-environment.log"
    printf 'environment\t0\n' >>"$run_root/phase-exits.tsv"
    exit 0
  else
    status="$?"
    cat "$run_root/logs/nix-environment.log" >&2
    printf 'environment\t%s\n' "$status" >"$run_root/phase-exits.tsv"
    printf 'SKIP\tproduct-build\tnix-shell-exit-%s\n' "$status" >>"$run_root/phase-exits.tsv"
    printf 'SKIP\tjoined-public-ingest-restart-turso\tproduct-build-not-started\nSKIP\tnative-gpui-consumer\tproduct-build-not-started\n' \
      >>"$run_root/phase-exits.tsv"
    printf 'Nix environment setup failed with exit %s; no product build or tests ran.\n' "$status" \
      >"$run_root/outcome.txt"
    exit "$status"
  fi
fi

shift
if [[ $# -ne 2 ]]; then
  printf '%s\n' 'Internal invocation requires a run root and tursodb path.' >&2
  exit 64
fi
run_root="$1"
turso="$2"
if [[ "$run_root" != /* || "$turso" != /* || ! -x "$turso" ]]; then
  printf '%s\n' 'Internal run root and tursodb must be absolute; tursodb must be executable.' >&2
  exit 64
fi
run_root="$(CDPATH= cd -- "$run_root" && pwd -P)"
turso="$(CDPATH= cd -- "$(dirname -- "$turso")" && pwd -P)/$(basename -- "$turso")"
mkdir -m 700 "$run_root/logs" "$run_root/receipts" "$run_root/target"

# App processes receive a deliberately small environment. Preserve public
# network proxy and TLS settings, but never ambient BACKEND_/NUDOX_ source
# selection or authentication values.
while IFS= read -r name; do
  case "$name" in
    BACKEND_*|NUDOX_*) unset "$name" ;;
  esac
done < <(compgen -e)

target="$run_root/target"
receipt="$run_root/receipts/product-build-receipt.json"
owner_pid_file="$run_root/owner.pid"
mcp_pid_file="$run_root/mcp.pid"
locald="$target/debug/backend-locald"
cli="$target/debug/backend-cli"
mcp="$target/debug/backend-mcp"
build_status='not-run'
joined_status='skipped'
gui_status='skipped'
owner_cleanup='not-needed'
mcp_cleanup='not-needed'
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

record_command() {
  local phase="$1"
  shift
  {
    printf '%s\t' "$phase"
    printf '%q ' "$@"
    printf '\n'
  } >>"$run_root/commands.log"
}

run_phase() {
  local phase="$1"
  local limit="$2"
  shift 2
  local log="$run_root/logs/$phase.log"
  record_command "$phase" timeout --signal=TERM --kill-after=30s "$limit" "$@"
  printf 'Starting %s; full output: %s\n' "$phase" "$log"
  set +e
  timeout --signal=TERM --kill-after=30s "$limit" "$@" >"$log" 2>&1
  local status="$?"
  set -e
  printf '%s\t%s\n' "$phase" "$status" >>"$run_root/phase-exits.tsv"
  if [[ "$status" -ne 0 ]]; then
    tail -n 80 "$log" >&2 || true
    printf 'Phase %s exited %s. Full output is preserved at %s\n' "$phase" "$status" "$log" >&2
  else
    printf 'Phase %s passed. Full output is preserved at %s\n' "$phase" "$log"
  fi
  return "$status"
}

process_command() {
  local pid="$1"
  ps -ww -p "$pid" -o command= 2>/dev/null || true
}

stop_recorded_child() {
  local name="$1"
  local record="$2"
  local expected_binary="$3"
  local pid recorded_binary actual attempt
  if [[ ! -f "$record" ]]; then
    printf -v "$name" '%s' 'no-pid-record'
    return
  fi
  {
    IFS= read -r pid || true
    IFS= read -r recorded_binary || true
  } <"$record"
  if [[ ! "$pid" =~ ^[0-9]+$ || "$recorded_binary" != "$expected_binary" ]]; then
    printf -v "$name" '%s' 'pid-record-invalid-preserved'
    return
  fi
  actual="$(process_command "$pid")"
  if [[ "$actual" != "$expected_binary --workspace "* ]]; then
    if [[ -z "$actual" ]]; then
      printf -v "$name" '%s' 'already-exited'
      rm -f "$record"
    else
      printf -v "$name" '%s' 'identity-mismatch-preserved'
    fi
    return
  fi
  if kill -TERM "$pid" 2>/dev/null; then
    for attempt in {0..39}; do
      sleep 0.5
      actual="$(process_command "$pid")"
      [[ "$actual" == "$expected_binary --workspace "* ]] || break
    done
    if [[ "$actual" == "$expected_binary --workspace "* ]]; then
      kill -KILL "$pid" 2>/dev/null || true
      printf -v "$name" '%s' 'killed-after-graceful-timeout'
    else
      printf -v "$name" '%s' 'stopped'
    fi
    rm -f "$record"
  else
    printf -v "$name" '%s' 'stop-failed'
  fi
}

finish() {
  local exit_status="$?"
  trap - EXIT INT TERM
  stop_recorded_child mcp_cleanup "$mcp_pid_file" "$mcp"
  stop_recorded_child owner_cleanup "$owner_pid_file" "$locald"
  python3 - "$run_root" "$started_at" "$exit_status" "$build_status" "$joined_status" "$gui_status" "$mcp_cleanup" "$owner_cleanup" <<'PY'
import json
import pathlib
import sys
from datetime import datetime, timezone

root, started, status, build, joined, gui, mcp_cleanup, owner_cleanup = sys.argv[1:]
result = {
    "schema": "nudox.live.storage-readiness-run.v1",
    "started_at": started,
    "finished_at": datetime.now(timezone.utc).isoformat(),
    "exit_status": int(status),
    "outcome": "passed" if status == "0" else "failed",
    "phases": {"product_build": build, "joined_public_ingest_restart_turso": joined, "native_gpui_consumer": gui},
    "cleanup": {"mcp": mcp_cleanup, "locald": owner_cleanup},
    "run_root": str(pathlib.Path(root).resolve()),
}
(pathlib.Path(root) / "gate-result.json").write_text(json.dumps(result, indent=2) + "\n")
PY
  exit "$exit_status"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [[ "$(git -C "$workspace" status --porcelain --untracked-files=all)" != '' ]]; then
  printf '%s\n' 'The joined live gate requires a clean source tree; no build was started.' >&2
  build_status='rejected-dirty-source'
  exit 65
fi

export CARGO_TARGET_DIR="$target"
export CARGO_BUILD_JOBS=1
export CARGO_INCREMENTAL=0
export CARGO_NET_OFFLINE=true
printf 'Started: %s\nWorkspace: %s\nRun root: %s\n' "$started_at" "$workspace" "$run_root" >"$run_root/provenance.txt"
{
  cargo --version
  rustc --version --verbose
  printf 'tursodb=%s\n' "$turso"
  sha256sum "$turso"
  git -C "$workspace" rev-parse HEAD
  git -C "$workspace" rev-parse 'HEAD^{tree}'
  sha256sum "$workspace/Cargo.lock"
} >>"$run_root/provenance.txt"

if run_phase product-build 150m env \
  CARGO_TARGET_DIR="$target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=true \
  cargo build --locked --offline \
    --package backend-locald --package backend-cli --package backend-mcp; then
  build_status='passed'
else
  status="$?"
  build_status="failed-exit-$status"
  printf 'SKIP\tjoined-public-ingest-restart-turso\tproduct-build-exit-%s\nSKIP\tnative-gpui-consumer\tproduct-build-exit-%s\n' \
    "$status" "$status" >>"$run_root/phase-exits.tsv"
  exit "$status"
fi

python3 - "$workspace" "$target" "$receipt" <<'PY'
import hashlib
import json
import pathlib
import subprocess
import sys
from datetime import datetime, timezone

workspace, target, receipt = map(pathlib.Path, sys.argv[1:])
workspace = workspace.resolve()
target = target.resolve()
def run(*args):
    return subprocess.run(args, cwd=workspace, check=True, stdout=subprocess.PIPE, text=True).stdout.strip()
if run("git", "status", "--porcelain", "--untracked-files=all"):
    raise SystemExit("source tree became dirty during product build")
head = run("git", "rev-parse", "HEAD")
tree = run("git", "rev-parse", "HEAD^{tree}")
lock_sha = hashlib.sha256((workspace / "Cargo.lock").read_bytes()).hexdigest()
binaries = {}
for name in ("backend-locald", "backend-cli", "backend-mcp"):
    path = target / "debug" / name
    if not path.is_file() or not path.stat().st_mode & 0o111:
        raise SystemExit(f"fresh build omitted executable {path}")
    binaries[{"backend-locald": "locald", "backend-cli": "cli", "backend-mcp": "mcp"}[name]] = {
        "path": str(path.resolve()),
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }
record = {
    "schema": "nudox.live.product-build.v1",
    "created_at": datetime.now(timezone.utc).isoformat(),
    "git_head": head,
    "git_tree": tree,
    "cargo_lock_sha256": lock_sha,
    "cargo_version": run("cargo", "--version"),
    "rustc_version": run("rustc", "--version", "--verbose"),
    "target_dir": str(target),
    "binaries": binaries,
}
receipt.write_text(json.dumps(record, indent=2) + "\n")
PY

if run_phase joined-public-ingest-restart-turso 300m env \
  CARGO_TARGET_DIR="$target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=true \
  NUDOX_LIVE_REGISTRY=1 \
  NUDOX_LIVE_ARTIFACT_DIR="$run_root" \
  NUDOX_LIVE_OWNER_PID_FILE="$owner_pid_file" \
  NUDOX_LIVE_MCP_PID_FILE="$mcp_pid_file" \
  NUDOX_LIVE_BUILD_RECEIPT="$receipt" \
  NUDOX_LIVE_LOCALD_BIN="$locald" \
  NUDOX_LIVE_CLI_BIN="$cli" \
  NUDOX_LIVE_MCP_BIN="$mcp" \
  NUDOX_TURSO_SQLITE_BIN="$turso" \
  cargo test --locked --offline --package backend-journeys --test live_registry \
    joined_public_registry_readiness_uses_one_product_owner_and_turso_restore -- --ignored --nocapture; then
  joined_status='passed'
else
  status="$?"
  joined_status="failed-exit-$status"
  printf 'SKIP\tnative-gpui-consumer\tjoined-ingest-exit-%s\n' "$status" >>"$run_root/phase-exits.tsv"
  exit "$status"
fi

if run_phase native-gpui-consumer 90m env \
  CARGO_TARGET_DIR="$target" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=true \
  NUDOX_LIVE_GUI_HANDOFF="$run_root/evidence/gui-handoff.json" \
  cargo test --locked --offline --package backend-desktop --features visual-harness --test shell_capture \
    live_registry_handoff_receipt_is_consumed_by_real_gpui_capture -- --ignored --nocapture; then
  gui_status='passed'
else
  status="$?"
  gui_status="failed-exit-$status"
  exit "$status"
fi

printf '%s\n' 'Joined live storage readiness passed; exact artifacts and phase logs are retained in the run directory.'

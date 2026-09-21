workspace_root="$(@git@ rev-parse --show-toplevel 2>/dev/null || pwd -P)"
cache_home="${XDG_CACHE_HOME:-$HOME/.cache}"
cache_root="${NUDOX_BUILD_CACHE_ROOT:-$cache_home/nudox/cargo-1.97}"
slot_count="${NUDOX_CARGO_BUILD_SLOTS:-4}"

case "$slot_count" in
  ""|*[!0-9]*)
    echo "NUDOX_CARGO_BUILD_SLOTS must be an integer from 1 through 32" >&2
    exit 64
    ;;
esac
if [ "$slot_count" -lt 1 ] || [ "$slot_count" -gt 32 ]; then
  echo "NUDOX_CARGO_BUILD_SLOTS must be an integer from 1 through 32" >&2
  exit 64
fi

wait_ms="${NUDOX_CARGO_WORKTREE_WAIT_MS:-300000}"
slot_wait_ms="${NUDOX_CARGO_SLOT_WAIT_MS:-5000}"
for wait_value in "$wait_ms" "$slot_wait_ms"; do
  case "$wait_value" in
    ""|*[!0-9]*)
      echo "NUDOX_CARGO_WORKTREE_WAIT_MS and NUDOX_CARGO_SLOT_WAIT_MS must be non-negative integers" >&2
      exit 64
      ;;
  esac
done

mkdir -p "$cache_root/build" "$cache_root/locks" "$cache_root/affinity" "$cache_root/sccache"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$workspace_root/.local/target}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
if [ -z "${CARGO_BUILD_JOBS:-}" ]; then
  logical_cpus="$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)"
  case "$logical_cpus" in
    ""|*[!0-9]*) logical_cpus=1 ;;
  esac
  # Divide the host across warm lanes without discarding the remainder. On an
  # 11-core host with four lanes, floor division leaves three cores idle even
  # when all lanes are busy; ceiling division gives each lane a useful bound
  # while Cargo's own scheduler still avoids exceeding it per invocation.
  jobs="$(((logical_cpus + slot_count - 1) / slot_count))"
  if [ "$jobs" -lt 1 ]; then jobs=1; fi
  export CARGO_BUILD_JOBS="$jobs"
fi

# These commands inspect or edit manifests and never ask Cargo to populate its
# intermediate build graph. They can run concurrently in one worktree and do
# not need a compiler-cache daemon or a mutable build-dir lease. Keep unknown
# commands on the conservative compiling path: a new Cargo subcommand should
# not silently bypass isolation until its behavior is understood.
cargo_command=""
for argument in "$@"; do
  case "$argument" in
    +*|--*|-*) continue ;;
    *) cargo_command="$argument"; break ;;
  esac
done
case "$cargo_command" in
  ""|metadata|tree|locate-project|read-manifest|search|help|version)
    exec @cargo@ "$@"
    ;;
esac

export RUSTC_WRAPPER="${RUSTC_WRAPPER:-@sccache@}"
export SCCACHE_DIR="${SCCACHE_DIR:-$cache_root/sccache}"
export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-8G}"
export SCCACHE_CLIENT_SIDE="${SCCACHE_CLIENT_SIDE:-1}"
export SCCACHE_SERVER_UDS="${SCCACHE_SERVER_UDS:-$cache_root/sccache.sock}"
worktree_lines="$(@git@ worktree list --porcelain 2>/dev/null || true)"
worktree_bases="$(printf '%s\n' "$worktree_lines" | sed -n 's/^worktree //p' | paste -sd: -)"
export SCCACHE_BASEDIRS="${SCCACHE_BASEDIRS:-${worktree_bases:-$workspace_root}}"

# `kill -0` alone can mistake a recycled PID for a live owner. `ps lstart`
# exists on both macOS and Linux; comparing the recorded start token closes
# that race while retaining the PID check on minimal systems without `ps`.
process_start_token() {
  LC_ALL=C ps -p "$1" -o lstart= 2>/dev/null | awk '{$1=$1; print}'
}
lock_owner_is_stale() {
  lock="$1"
  [ -d "$lock" ] || return 1
  owner="$(cat "$lock/pid" 2>/dev/null || true)"
  case "$owner" in
    ""|*[!0-9]*) owner="" ;;
  esac
  if [ -n "$owner" ]; then
    if ! kill -0 "$owner" 2>/dev/null; then return 0; fi
    recorded_start="$(cat "$lock/start" 2>/dev/null || true)"
    current_start="$(process_start_token "$owner" || true)"
    if [ -n "$recorded_start" ] && [ -n "$current_start" ] && [ "$recorded_start" != "$current_start" ]; then
      return 0
    fi
    return 1
  fi
  [ -n "$(find "$lock" -type d -mmin +1 -print -quit 2>/dev/null)" ]
}

# sccache's lazy daemon start can race when several fresh lanes arrive at the
# same Unix socket. Elect exactly one starter; peers wait only for the socket,
# never for compilation or a Cargo build lock.
if [ ! -S "$SCCACHE_SERVER_UDS" ]; then
  server_lock="$cache_root/locks/sccache-server.lock"
  attempts=0
  while [ ! -S "$SCCACHE_SERVER_UDS" ] && [ "$attempts" -lt 100 ]; do
    if mkdir "$server_lock" 2>/dev/null; then
      printf '%s\n' "$$" > "$server_lock/pid"
      process_start_token "$$" > "$server_lock/start"
      if ! @sccache@ --start-server >/dev/null; then
        rm -f "$server_lock/pid" "$server_lock/start"
        rmdir "$server_lock" 2>/dev/null || true
        exit 75
      fi
      rm -f "$server_lock/pid" "$server_lock/start"
      rmdir "$server_lock" 2>/dev/null || true
      break
    fi

    if lock_owner_is_stale "$server_lock"; then
      stale="$server_lock.stale.$$"
      if mv "$server_lock" "$stale" 2>/dev/null; then
        rm -f "$stale/pid" "$stale/start"
        rmdir "$stale" 2>/dev/null || true
      fi
    fi
    sleep 0.05
    attempts="$((attempts + 1))"
  done
  if [ ! -S "$SCCACHE_SERVER_UDS" ]; then
    echo "nudox cargo: shared compiler cache did not become ready" >&2
    exit 75
  fi
fi

# Explicit callers retain full control over placement. This check intentionally
# happens after sccache setup, preserving the old wrapper's compiler-cache
# behavior without ever changing the caller's build directory.
if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then
  exec @cargo@ "$@"
fi

# cksum is present in the minimal Nix runtime and the length makes accidental
# collisions much less likely than using the CRC alone. The key names only
# cache-local directories; the canonical path remains in the lease metadata.
worktree_key="$(printf '%s' "$workspace_root" | cksum | awk '{print $1 "-" $2}')"
worktree_lock="$cache_root/locks/worktree-$worktree_key.lock"
affinity_file="$cache_root/affinity/$worktree_key"

selected=""
selected_lock=""
selected_slot=""
selected_overflow_path=""
selected_lock_acquired=false
released=false
worktree_lock_acquired=false
cargo_pid=""

release_all() {
  [ "$released" = true ] && return
  released=true
  if [ "$selected_lock_acquired" = true ] && [ -n "$selected_lock" ]; then
    rm -f "$selected_lock/pid" "$selected_lock/start" "$selected_lock/workspace"
    rmdir "$selected_lock" 2>/dev/null || true
  fi
  if [ -n "$selected_overflow_path" ] && [ -d "$selected_overflow_path" ]; then
    find "$selected_overflow_path" -depth -delete 2>/dev/null || true
  fi
  if [ "$worktree_lock_acquired" = true ] && [ "$(cat "$worktree_lock/pid" 2>/dev/null || true)" = "$$" ]; then
    rm -f "$worktree_lock/pid" "$worktree_lock/start" "$worktree_lock/workspace"
    rmdir "$worktree_lock" 2>/dev/null || true
  fi
}

forward_signal() {
  signal="$1"
  status="$2"
  if [ -n "$cargo_pid" ]; then
    kill -"$signal" "$cargo_pid" 2>/dev/null || true
    # Reap Cargo before EXIT releases its mutable build directory. A child
    # that traps or ignores the first signal therefore cannot race the next
    # owner; the caller can still use the bounded worktree wait on retry.
    wait "$cargo_pid" 2>/dev/null || true
    cargo_pid=""
  fi
  exit "$status"
}

# Install cleanup before acquiring the worktree lease. Signals during slot
# selection must release the worktree lock just as signals during Cargo do.
trap release_all EXIT
trap 'forward_signal HUP 129' HUP
trap 'forward_signal INT 130' INT
trap 'forward_signal TERM 143' TERM

recover_stale_lock() {
  lock="$1"
  if lock_owner_is_stale "$lock"; then
    stale="$lock.stale.$$"
    if mv "$lock" "$stale" 2>/dev/null; then
      find "$stale" -type f -delete 2>/dev/null || true
      rmdir "$stale" 2>/dev/null || true
      return 0
    fi
  fi
  return 1
}

wait_attempts_for() {
  value="$1"
  # Polling at 50ms keeps same-worktree callers cheap while making the bound
  # deterministic enough for shell-level tests and diagnostics.
  if [ "$value" -eq 0 ]; then
    printf '0\n'
  else
    printf '%s\n' "$(((value + 49) / 50))"
  fi
}

worktree_wait_attempts="$(wait_attempts_for "$wait_ms")"
worktree_waited=0
while ! mkdir "$worktree_lock" 2>/dev/null; do
  recover_stale_lock "$worktree_lock" || true
  if [ "$worktree_waited" -ge "$worktree_wait_attempts" ]; then
    echo "nudox cargo: another compile is active for this worktree; waited ${wait_ms}ms" >&2
    exit 75
  fi
  sleep 0.05
  worktree_waited="$((worktree_waited + 1))"
done
worktree_lock_acquired=true
printf '%s\n' "$$" > "$worktree_lock/pid"
process_start_token "$$" > "$worktree_lock/start"
printf '%s\n' "$workspace_root" > "$worktree_lock/workspace"

acquire_slot() {
  candidate="$1"
  lock="$cache_root/locks/slot-$candidate.lock"
  if mkdir "$lock" 2>/dev/null; then
    # Publish ownership to the EXIT trap before any metadata write. A signal
    # in the tiny initialization window must still remove this fresh lock.
    selected_lock="$lock"
    selected="$cache_root/build/slot-$candidate"
    selected_slot="$candidate"
    selected_lock_acquired=true
    printf '%s\n' "$$" > "$lock/pid"
    process_start_token "$$" > "$lock/start"
    printf '%s\n' "$workspace_root" > "$lock/workspace"
    slot_identity="$cache_root/affinity/slot-$candidate.owner"
    previous_workspace="$(cat "$slot_identity" 2>/dev/null || true)"
    # Cargo's intermediate graph is only reusable within one canonical
    # worktree. Cross-worktree reuse is correctness-unsafe: an old rmeta can
    # satisfy a path-compatible crate while omitting the current public API.
    # sccache remains the cross-worktree reuse layer; reset the mutable Cargo
    # graph before handing this slot to a different workspace.
    if [ -e "$selected" ] && { [ -z "$previous_workspace" ] || [ "$previous_workspace" != "$workspace_root" ]; }; then
      if ! find "$selected" -depth -delete 2>/dev/null; then
        echo "nudox cargo: unable to reset build slot $candidate" >&2
        rm -f "$lock/pid" "$lock/start" "$lock/workspace"
        rmdir "$lock" 2>/dev/null || true
        selected=""
        selected_lock=""
        selected_slot=""
        selected_lock_acquired=false
        return 75
      fi
    fi
    affinity_tmp="$slot_identity.tmp.$$"
    printf '%s\n' "$workspace_root" > "$affinity_tmp"
    mv -f "$affinity_tmp" "$slot_identity"
    return 0
  fi
  recover_stale_lock "$lock" || true
  return 1
}

affinity_slot="$(cat "$affinity_file" 2>/dev/null || true)"
case "$affinity_slot" in
  ""|*[!0-9]*) affinity_slot="" ;;
esac
if [ -n "$affinity_slot" ] && [ "$affinity_slot" -ge "$slot_count" ]; then
  affinity_slot=""
fi

slot_wait_attempts="$(wait_attempts_for "$slot_wait_ms")"
slot_waited=0
while [ -z "$selected" ]; do
  start="$(printf '%s' "$workspace_root" | cksum | awk -v count="$slot_count" '{print $1 % count}')"
  if [ -n "$affinity_slot" ]; then
    # Prefer the remembered lane, then use another free lane if a different
    # worktree currently owns it. This preserves affinity in the common case
    # while keeping independent worktrees parallel under contention.
    if acquire_slot "$affinity_slot"; then break; fi
    scan_offset=0
    while [ "$scan_offset" -lt "$slot_count" ]; do
      candidate="$(( (start + scan_offset) % slot_count ))"
      if [ "$candidate" != "$affinity_slot" ] && acquire_slot "$candidate"; then break 2; fi
      scan_offset="$((scan_offset + 1))"
    done
  else
    scan_offset=0
    while [ "$scan_offset" -lt "$slot_count" ]; do
      candidate="$(( (start + scan_offset) % slot_count ))"
      if acquire_slot "$candidate"; then break 2; fi
      scan_offset="$((scan_offset + 1))"
    done
  fi

  if [ "$slot_waited" -ge "$slot_wait_attempts" ]; then break; fi
  sleep 0.05
  slot_waited="$((slot_waited + 1))"
done

if [ -n "$selected_slot" ]; then
  affinity_tmp="$affinity_file.tmp.$$"
  printf '%s\n' "$selected_slot" > "$affinity_tmp"
  mv -f "$affinity_tmp" "$affinity_file"
elif [ -z "$selected" ]; then
  # Every warm lane is leased. A deterministic per-worktree overflow keeps
  # this invocation isolated from other worktrees; same-worktree invocations
  # are already serialized by worktree_lock. The path is removed on exit.
  lane_key="$worktree_key"
  overflow_lock="$cache_root/locks/overflow-$lane_key.lock"
  if mkdir "$overflow_lock" 2>/dev/null; then
    selected="$cache_root/build/overflow-$lane_key"
    selected_lock="$overflow_lock"
    selected_overflow_path="$selected"
    selected_lock_acquired=true
    printf '%s\n' "$$" > "$overflow_lock/pid"
    process_start_token "$$" > "$overflow_lock/start"
    printf '%s\n' "$workspace_root" > "$overflow_lock/workspace"
  else
    recover_stale_lock "$overflow_lock" || true
    if mkdir "$overflow_lock" 2>/dev/null; then
      selected="$cache_root/build/overflow-$lane_key"
      selected_lock="$overflow_lock"
      selected_overflow_path="$selected"
      selected_lock_acquired=true
      printf '%s\n' "$$" > "$overflow_lock/pid"
      process_start_token "$$" > "$overflow_lock/start"
      printf '%s\n' "$workspace_root" > "$overflow_lock/workspace"
    else
      # A live owner can only be an invocation outside this wrapper's
      # worktree lease. Never share its mutable path; use bounded scratch and
      # remove it on exit.
      selected="$cache_root/build/overflow-$lane_key-$$"
      selected_overflow_path="$selected"
    fi
  fi
  if [ "${NUDOX_CARGO_CACHE_VERBOSE:-0}" = 1 ]; then
    echo "nudox cargo: warm slots busy; using isolated overflow $selected" >&2
  fi
elif [ "${NUDOX_CARGO_CACHE_VERBOSE:-0}" = 1 ]; then
  echo "nudox cargo: using warm build slot $selected" >&2
fi
export CARGO_BUILD_BUILD_DIR="$selected"

# Cargo runs under the already-installed signal traps. A normal exit is
# reaped below; a cancellation handler reaps the child before EXIT cleanup.
@cargo@ "$@" &
cargo_pid="$!"
# Poll instead of blocking in `wait`: POSIX shells may defer traps while
# waiting for a foreground child. Polling leaves the shell able to forward a
# cancellation promptly; the final wait still reaps the child and captures
# its exit status.
while kill -0 "$cargo_pid" 2>/dev/null; do
  sleep 0.05
done
if wait "$cargo_pid"; then
  cargo_status=0
else
  cargo_status="$?"
fi
cargo_pid=""
exit "$cargo_status"

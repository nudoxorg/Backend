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

mkdir -p "$cache_root/build" "$cache_root/locks" "$cache_root/sccache"
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
export RUSTC_WRAPPER="${RUSTC_WRAPPER:-@sccache@}"
export SCCACHE_DIR="${SCCACHE_DIR:-$cache_root/sccache}"
export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-8G}"
export SCCACHE_CLIENT_SIDE="${SCCACHE_CLIENT_SIDE:-1}"
export SCCACHE_SERVER_UDS="${SCCACHE_SERVER_UDS:-$cache_root/sccache.sock}"
worktree_lines="$(@git@ worktree list --porcelain 2>/dev/null || true)"
worktree_bases="$(printf '%s\n' "$worktree_lines" | sed -n 's/^worktree //p' | paste -sd: -)"
export SCCACHE_BASEDIRS="${SCCACHE_BASEDIRS:-${worktree_bases:-$workspace_root}}"

# sccache's lazy daemon start can race when several fresh lanes arrive at the
# same Unix socket. Elect exactly one starter; peers wait only for the socket,
# never for compilation or a Cargo build lock.
if [ ! -S "$SCCACHE_SERVER_UDS" ]; then
  server_lock="$cache_root/locks/sccache-server.lock"
  attempts=0
  while [ ! -S "$SCCACHE_SERVER_UDS" ] && [ "$attempts" -lt 100 ]; do
    if mkdir "$server_lock" 2>/dev/null; then
      printf '%s\n' "$$" > "$server_lock/pid"
      if ! @sccache@ --start-server >/dev/null; then
        rm -f "$server_lock/pid"
        rmdir "$server_lock" 2>/dev/null || true
        exit 75
      fi
      rm -f "$server_lock/pid"
      rmdir "$server_lock" 2>/dev/null || true
      break
    fi

    owner="$(cat "$server_lock/pid" 2>/dev/null || true)"
    empty_lock_is_stale=false
    if [ -z "$owner" ] && [ -n "$(find "$server_lock" -type d -mmin +1 -print -quit 2>/dev/null)" ]; then
      empty_lock_is_stale=true
    fi
    if { [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; } || $empty_lock_is_stale; then
      stale="$server_lock.stale.$$"
      if mv "$server_lock" "$stale" 2>/dev/null; then
        rm -f "$stale/pid"
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

# Explicit callers retain full control over placement. This also makes the
# wrapper composable with CI and one-off benchmark isolation.
if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then
  exec @cargo@ "$@"
fi

start="$(( $(printf '%s' "$workspace_root" | cksum | cut -d ' ' -f 1) % slot_count ))"
selected=""
selected_lock=""
offset=0
while [ "$offset" -lt "$slot_count" ]; do
  slot="$(( (start + offset) % slot_count ))"
  lock="$cache_root/locks/slot-$slot.lock"
  if mkdir "$lock" 2>/dev/null; then
    printf '%s\n' "$$" > "$lock/pid"
    selected="$cache_root/build/slot-$slot"
    selected_lock="$lock"
    break
  fi

  # A killed wrapper cannot run its trap. Reclaim only a lock whose recorded
  # owner no longer exists, and rename it atomically before removing it.
  owner="$(cat "$lock/pid" 2>/dev/null || true)"
  empty_lock_is_stale=false
  if [ -z "$owner" ] && [ -n "$(find "$lock" -type d -mmin +1 -print -quit 2>/dev/null)" ]; then
    empty_lock_is_stale=true
  fi
  if { [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; } || $empty_lock_is_stale; then
    stale="$lock.stale.$$"
    if mv "$lock" "$stale" 2>/dev/null; then
      rm -f "$stale/pid"
      rmdir "$stale" 2>/dev/null || true
    fi
  fi
  offset="$((offset + 1))"
done

if [ -z "$selected" ]; then
  lane_key="$(printf '%s' "$workspace_root" | cksum | cut -d ' ' -f 1)"
  overflow_lock="$cache_root/locks/overflow-$lane_key.lock"
  if mkdir "$overflow_lock" 2>/dev/null; then
    printf '%s\n' "$$" > "$overflow_lock/pid"
    selected="$cache_root/build/overflow-$lane_key"
    selected_lock="$overflow_lock"
  else
    # Same-worktree concurrent invocations must never converge on the same
    # deterministic overflow path. This rare path remains invocation-local.
    selected="$cache_root/build/overflow-$lane_key-$$"
  fi
  if [ "${NUDOX_CARGO_CACHE_VERBOSE:-0}" = 1 ]; then
    echo "nudox cargo: warm slots busy; using lock-free overflow $selected" >&2
  fi
elif [ "${NUDOX_CARGO_CACHE_VERBOSE:-0}" = 1 ]; then
  echo "nudox cargo: using warm build slot $selected" >&2
fi
export CARGO_BUILD_BUILD_DIR="$selected"

release_slot() {
  if [ -n "$selected_lock" ] && [ "$(cat "$selected_lock/pid" 2>/dev/null || true)" = "$$" ]; then
    rm -f "$selected_lock/pid"
    rmdir "$selected_lock" 2>/dev/null || true
  fi
}
trap release_slot EXIT HUP INT TERM
@cargo@ "$@"

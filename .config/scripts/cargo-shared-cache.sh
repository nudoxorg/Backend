workspace_root="$(@git@ rev-parse --show-toplevel 2>/dev/null || pwd -P)"
cache_home="${XDG_CACHE_HOME:-$HOME/.cache}"
cache_root="${NUDOX_BUILD_CACHE_ROOT:-$cache_home/nudox/cargo-1.97}"
slot_count="${NUDOX_CARGO_BUILD_SLOTS:-4}"
explicit_target_dir="${CARGO_TARGET_DIR:-}"
default_target_dir="$workspace_root/.local/target"
# The Nix shell exports this exact path before invoking Cargo. Treat that
# canonical in-worktree location as the wrapper-managed default even though it
# arrives through the environment; role-specific or caller-chosen paths remain
# explicit and must already be empty or stamped for this worktree.
if [ "$explicit_target_dir" = "$default_target_dir" ] \
  && [ ! -L "$workspace_root/.local" ] \
  && [ ! -L "$default_target_dir" ]; then
  explicit_target_dir=""
fi

case "$slot_count" in
  ""|*[!0-9]*)
    echo "NUDOX_CARGO_BUILD_SLOTS must be an integer from 1 through 4" >&2
    exit 64
    ;;
esac
if [ "$slot_count" -lt 1 ] || [ "$slot_count" -gt 4 ]; then
  echo "NUDOX_CARGO_BUILD_SLOTS must be an integer from 1 through 4" >&2
  exit 64
fi

wait_ms="${NUDOX_CARGO_WORKTREE_WAIT_MS:-300000}"
slot_wait_ms="${NUDOX_CARGO_SLOT_WAIT_MS:-300000}"
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

export RUSTC_WRAPPER="${RUSTC_WRAPPER:-@rustc_cache_wrapper@}"
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

# An override names a role root, never an exact shared Cargo graph. Explicit
# roots retain up to four worktree-stamped graphs of their own. The host
# capacity permit and each role-graph lease are independent: graph identity
# must not change merely because the host scheduler grants another permit.
explicit_build_dir="${CARGO_BUILD_BUILD_DIR:-}"
explicit_graph_slot_count=4

# cksum is present in the minimal Nix runtime and the length makes accidental
# collisions much less likely than using the CRC alone. The key names only
# cache-local directories; the canonical path remains in the lease metadata.
worktree_key="$(printf '%s' "$workspace_root" | cksum | awk '{print $1 "-" $2}')"
worktree_lock="$cache_root/locks/worktree-$worktree_key.lock"
affinity_file="$cache_root/affinity/$worktree_key"

# Stamp every mutable target with the canonical worktree that produced it.
# Explicit build directories are role roots, not permission to share one Cargo
# graph: up to four role-local graphs are independently leased and reused by
# their owner even when the host capacity permit changes. Explicit target
# directories keep their exact caller-selected path when empty or already
# stamped for this worktree; mismatched or unmarked non-empty directories are
# refused without mutation.
claim_mutable_build_dir() {
  build_dir="$1"
  legacy_owner="${2:-}"
  missing_stamp_policy="${3:-reset}"
  if [ -L "$build_dir" ]; then
    echo "nudox cargo: refusing symlinked mutable build directory" >&2
    return 1
  fi
  if [ -e "$build_dir" ] && [ ! -d "$build_dir" ]; then
    echo "nudox cargo: refusing non-directory mutable build path" >&2
    return 1
  fi
  mkdir -p "$build_dir" || return 1
  stamp="$build_dir/.nudox-worktree-root"
  if [ -L "$stamp" ]; then
    echo "nudox cargo: refusing symlinked mutable build stamp" >&2
    return 1
  fi
  if [ -f "$stamp" ]; then
    previous_owner="$(cat "$stamp" 2>/dev/null || true)"
    if [ -z "$previous_owner" ]; then
      echo "nudox cargo: refusing malformed mutable build stamp" >&2
      return 1
    fi
    if [ "$previous_owner" != "$workspace_root" ]; then
      # This path is a wrapper-managed lane directory and the host slot lease
      # is held. Retire only this lane's Cargo graph; never clean the caller's
      # role root or any other worktree's target directory.
      if ! find "$build_dir" -mindepth 1 -depth -delete 2>/dev/null; then
        echo "nudox cargo: unable to retire the previous lane graph" >&2
        return 1
      fi
      if ! (set -C; printf '%s\n' "$workspace_root" > "$stamp") 2>/dev/null; then
        echo "nudox cargo: unable to stamp the claimed lane graph" >&2
        return 1
      fi
    fi
    return 0
  fi
  existing="$(find "$build_dir" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null || true)"
  if [ -n "$existing" ]; then
    # Older pooled lanes have a separate affinity-owner record. It is safe to
    # adopt their existing contents only when that record names this root;
    # otherwise they are cleared as one managed lane before use.
    if [ "$legacy_owner" = "$workspace_root" ]; then
      if ! (set -C; printf '%s\n' "$workspace_root" > "$stamp") 2>/dev/null; then
        echo "nudox cargo: unable to adopt the existing lane graph" >&2
        return 1
      fi
      return 0
    fi
    if [ "$missing_stamp_policy" = refuse ]; then
      echo "nudox cargo: refusing unmarked mutable build directory" >&2
      return 1
    fi
    if ! find "$build_dir" -mindepth 1 -depth -delete 2>/dev/null; then
      echo "nudox cargo: unable to retire an unowned lane graph" >&2
      return 1
    fi
  fi
  if ! (set -C; printf '%s\n' "$workspace_root" > "$stamp") 2>/dev/null; then
    echo "nudox cargo: unable to stamp the lane graph" >&2
    return 1
  fi
}

claim_explicit_target_dir() {
  target_dir="$1"
  if [ -L "$target_dir" ]; then
    echo "nudox cargo: refusing symlinked explicit target directory" >&2
    return 1
  fi
  if [ -e "$target_dir" ] && [ ! -d "$target_dir" ]; then
    echo "nudox cargo: refusing non-directory explicit target path" >&2
    return 1
  fi
  mkdir -p "$target_dir" || return 1
  stamp="$target_dir/.nudox-worktree-root"
  if [ -L "$stamp" ]; then
    echo "nudox cargo: refusing symlinked explicit target stamp" >&2
    return 1
  fi
  if [ -f "$stamp" ]; then
    if [ "$(cat "$stamp" 2>/dev/null || true)" != "$workspace_root" ]; then
      echo "nudox cargo: explicit target directory belongs to another worktree" >&2
      return 1
    fi
  else
    existing="$(find "$target_dir" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null || true)"
    if [ -n "$existing" ]; then
      echo "nudox cargo: refusing unmarked explicit target directory" >&2
      return 1
    fi
    if ! (set -C; printf '%s\n' "$workspace_root" > "$stamp") 2>/dev/null; then
      if [ "$(cat "$stamp" 2>/dev/null || true)" != "$workspace_root" ]; then
        echo "nudox cargo: unable to stamp explicit target directory" >&2
        return 1
      fi
    fi
  fi
  printf '%s\n' "$target_dir"
}

selected=""
selected_lock=""
selected_slot=""
selected_lock_acquired=false
selected_graph_lock=""
selected_graph_slot=""
selected_graph_lock_acquired=false
released=false
worktree_lock_acquired=false
cargo_pid=""
provenance_start=""

release_explicit_graph_lock() {
  if [ "$selected_graph_lock_acquired" = true ] && [ -n "$selected_graph_lock" ]; then
    if [ ! -e "$selected_graph_lock/pid" ] \
      || [ "$(cat "$selected_graph_lock/pid" 2>/dev/null || true)" = "$$" ]; then
      rm -f "$selected_graph_lock/pid" "$selected_graph_lock/start" "$selected_graph_lock/workspace"
      rmdir "$selected_graph_lock" 2>/dev/null || true
    fi
  fi
  selected_graph_lock=""
  selected_graph_slot=""
  selected_graph_lock_acquired=false
}

# Invoked by the EXIT trap installed below.
# shellcheck disable=SC2329
release_all() {
  [ "$released" = true ] && return
  released=true
  release_explicit_graph_lock
  if [ "$selected_lock_acquired" = true ] && [ -n "$selected_lock" ]; then
    rm -f "$selected_lock/pid" "$selected_lock/start" "$selected_lock/workspace"
    rmdir "$selected_lock" 2>/dev/null || true
  fi
  if [ "$worktree_lock_acquired" = true ] && [ "$(cat "$worktree_lock/pid" 2>/dev/null || true)" = "$$" ]; then
    rm -f "$worktree_lock/pid" "$worktree_lock/start" "$worktree_lock/workspace"
    rmdir "$worktree_lock" 2>/dev/null || true
  fi
}

# Invoked by the signal traps installed below.
# shellcheck disable=SC2329
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
  finish_provenance "$status"
  exit "$status"
}

finish_provenance() {
  status="$1"
  if [ -n "$provenance_start" ]; then
    start_path="$provenance_start"
    provenance_start=""
    if provenance_path="$(NUDOX_PROVENANCE_GIT="@git@" @python3@ @provenance@ finish "$start_path" "$status" 2>/dev/null)"; then
      if [ "${NUDOX_CARGO_CACHE_VERBOSE:-0}" = 1 ]; then
        echo "nudox cargo: provenance $provenance_path" >&2
      fi
    else
      echo "nudox cargo: build provenance could not be finalized" >&2
    fi
  fi
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

acquire_explicit_graph_lock() {
  candidate="$1"
  lock="$explicit_graph_lock_root/slot-$candidate.lock"
  if [ -L "$lock" ]; then
    echo "nudox cargo: refusing symlinked role-graph lease" >&2
    return 2
  fi
  if mkdir "$lock" 2>/dev/null; then
    # Publish ownership before writing metadata so the EXIT trap can release
    # a lease even if a signal lands during initialization.
    selected_graph_lock="$lock"
    selected_graph_slot="$candidate"
    selected_graph_lock_acquired=true
    printf '%s\n' "$$" > "$lock/pid"
    process_start_token "$$" > "$lock/start"
    printf '%s\n' "$workspace_root" > "$lock/workspace"
    return 0
  fi
  recover_stale_lock "$lock" || true
  if [ -L "$lock" ]; then
    echo "nudox cargo: refusing symlinked role-graph lease" >&2
    return 2
  fi
  if mkdir "$lock" 2>/dev/null; then
    selected_graph_lock="$lock"
    selected_graph_slot="$candidate"
    selected_graph_lock_acquired=true
    printf '%s\n' "$$" > "$lock/pid"
    process_start_token "$$" > "$lock/start"
    printf '%s\n' "$workspace_root" > "$lock/workspace"
    return 0
  fi
  return 1
}

inspect_explicit_graph() {
  graph_dir="$explicit_lane_root/slot-$1"
  graph_stamp="$graph_dir/.nudox-worktree-root"
  graph_state=""
  graph_owner=""
  if [ -L "$graph_dir" ]; then
    echo "nudox cargo: refusing symlinked role graph" >&2
    return 2
  fi
  if [ -e "$graph_dir" ] && [ ! -d "$graph_dir" ]; then
    echo "nudox cargo: refusing non-directory role graph" >&2
    return 2
  fi
  if [ ! -e "$graph_dir" ]; then
    graph_state=empty
    return 0
  fi
  if [ -L "$graph_stamp" ]; then
    echo "nudox cargo: refusing symlinked role-graph stamp" >&2
    return 2
  fi
  if [ -f "$graph_stamp" ]; then
    graph_owner="$(cat "$graph_stamp" 2>/dev/null || true)"
    if [ -z "$graph_owner" ]; then
      echo "nudox cargo: refusing malformed role-graph stamp" >&2
      return 2
    fi
    if [ "$graph_owner" = "$workspace_root" ]; then
      graph_state=owned
    else
      graph_state=other
    fi
    return 0
  fi
  existing="$(find "$graph_dir" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null || true)"
  if [ -n "$existing" ]; then
    graph_state=unmarked
  else
    graph_state=empty
  fi
}

select_explicit_graph() {
  if [ -L "$explicit_build_dir" ]; then
    echo "nudox cargo: refusing symlinked explicit build root" >&2
    return 73
  fi
  explicit_lane_root="$explicit_build_dir/.nudox-cargo"
  if [ -L "$explicit_lane_root" ]; then
    echo "nudox cargo: refusing symlinked explicit build namespace" >&2
    return 73
  fi
  explicit_graph_lock_root="$explicit_lane_root/leases"
  if [ -L "$explicit_graph_lock_root" ]; then
    echo "nudox cargo: refusing symlinked role-graph lease root" >&2
    return 73
  fi
  mkdir -p "$explicit_graph_lock_root" || return 73

  graph_wait_attempts="$(wait_attempts_for "$slot_wait_ms")"
  graph_waited=0
  while :; do
    graph_busy_count=0
    for mode in owned empty other; do
      candidate=0
      while [ "$candidate" -lt "$explicit_graph_slot_count" ]; do
        graph_lock_status=0
        acquire_explicit_graph_lock "$candidate" || graph_lock_status="$?"
        if [ "$graph_lock_status" -eq 2 ]; then return 73; fi
        if [ "$graph_lock_status" -ne 0 ]; then
          graph_busy_count="$((graph_busy_count + 1))"
          candidate="$((candidate + 1))"
          continue
        fi

        inspect_status=0
        inspect_explicit_graph "$candidate" || inspect_status="$?"
        if [ "$inspect_status" -ne 0 ]; then
          release_explicit_graph_lock
          return 73
        fi

        # Unknown non-empty graph data is never adopted, erased, or silently
        # bypassed. The caller must inspect/remove it explicitly before this
        # role root can be used again.
        if [ "$graph_state" = unmarked ]; then
          release_explicit_graph_lock
          echo "nudox cargo: refusing unmarked mutable role graph" >&2
          return 73
        fi

        choose_graph=false
        case "$mode:$graph_state" in
          owned:owned|empty:empty|other:other) choose_graph=true ;;
        esac
        if [ "$choose_graph" = true ]; then
          selected="$explicit_lane_root/slot-$candidate"
          if ! claim_mutable_build_dir "$selected" "" refuse; then
            release_explicit_graph_lock
            return 73
          fi
          return 0
        fi
        release_explicit_graph_lock
        candidate="$((candidate + 1))"
      done
    done

    if [ "$graph_busy_count" -eq 0 ]; then
      echo "nudox cargo: no safe role-graph slot is available" >&2
      return 73
    fi
    if [ "$graph_waited" -ge "$graph_wait_attempts" ]; then
      echo "nudox cargo: all role-graph slots are busy; waited ${slot_wait_ms}ms" >&2
      return 75
    fi
    sleep 0.05
    graph_waited="$((graph_waited + 1))"
  done
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
    selected="$candidate"
    selected_slot="$candidate"
    selected_lock_acquired=true
    printf '%s\n' "$$" > "$lock/pid"
    process_start_token "$$" > "$lock/start"
    printf '%s\n' "$workspace_root" > "$lock/workspace"
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

if [ -n "$selected_slot" ] && [ -z "$explicit_build_dir" ]; then
  affinity_tmp="$affinity_file.tmp.$$"
  printf '%s\n' "$selected_slot" > "$affinity_tmp"
  mv -f "$affinity_tmp" "$affinity_file"
fi
if [ -z "$selected_slot" ]; then
  # The warm-lane count is also the host-wide compiler concurrency ceiling.
  # Never manufacture an overflow lane: it defeats the memory bound precisely
  # when contention is highest. A caller may retry after a lease is released.
  echo "nudox cargo: all $slot_count build slots are busy; waited ${slot_wait_ms}ms" >&2
  exit 75
elif [ "${NUDOX_CARGO_CACHE_VERBOSE:-0}" = 1 ]; then
  if [ -n "$explicit_build_dir" ]; then
    echo "nudox cargo: using host capacity permit $selected_slot" >&2
  else
    echo "nudox cargo: using warm build slot $selected_slot" >&2
  fi
fi

if [ -n "$explicit_build_dir" ]; then
  select_explicit_graph || exit "$?"
  if [ "${NUDOX_CARGO_CACHE_VERBOSE:-0}" = 1 ]; then
    echo "nudox cargo: using explicit role graph slot $selected_graph_slot" >&2
  fi
else
  selected="$cache_root/build/slot-$selected_slot"
  slot_identity="$cache_root/affinity/slot-$selected_slot.owner"
  previous_workspace="$(cat "$slot_identity" 2>/dev/null || true)"
  if ! claim_mutable_build_dir "$selected" "$previous_workspace"; then exit 73; fi
  affinity_tmp="$slot_identity.tmp.$$"
  printf '%s\n' "$workspace_root" > "$affinity_tmp"
  mv -f "$affinity_tmp" "$slot_identity"
fi

if [ -n "$explicit_target_dir" ]; then
  CARGO_TARGET_DIR="$(claim_explicit_target_dir "$explicit_target_dir")" || exit 73
else
  if [ -L "$CARGO_TARGET_DIR" ]; then
    echo "nudox cargo: refusing symlinked workspace target directory" >&2
    exit 73
  fi
  mkdir -p "$CARGO_TARGET_DIR" || exit 73
  target_stamp="$CARGO_TARGET_DIR/.nudox-worktree-root"
  if [ -L "$target_stamp" ]; then
    echo "nudox cargo: refusing symlinked workspace target stamp" >&2
    exit 73
  fi
  if [ -f "$target_stamp" ]; then
    if [ "$(cat "$target_stamp" 2>/dev/null || true)" != "$workspace_root" ]; then
      echo "nudox cargo: workspace target directory belongs to another worktree" >&2
      exit 73
    fi
  elif ! (set -C; printf '%s\n' "$workspace_root" > "$target_stamp") 2>/dev/null; then
    if [ "$(cat "$target_stamp" 2>/dev/null || true)" != "$workspace_root" ]; then
      echo "nudox cargo: unable to stamp workspace target directory" >&2
      exit 73
    fi
  fi
fi

export CARGO_BUILD_BUILD_DIR="$selected"
export CARGO_TARGET_DIR

if provenance_start="$(NUDOX_PROVENANCE_GIT="@git@" @python3@ @provenance@ begin \
  "$workspace_root" "$CARGO_BUILD_BUILD_DIR" "$CARGO_TARGET_DIR" "$0" \
  "@wrapper_source@" "@cargo@" "${RUSTC:-@rustc@}" "$RUSTC_WRAPPER" "$@" 2>/dev/null)"; then
  :
else
  echo "nudox cargo: build provenance capture could not start" >&2
fi

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
finish_provenance "$cargo_status"
exit "$cargo_status"

#!/bin/sh
# Fast protocol tests for .config/scripts/cargo-shared-cache.sh. These tests
# replace Cargo, git, and sccache with tiny shims; they never compile the
# workspace or evaluate Nix.
set -eu

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)"
# A Nix check runs this file as a lone store path, where the repository layout
# is gone; it names the script under test explicitly instead.
source_script="${NUDOX_CARGO_CACHE_SCRIPT:-$repo_root/.config/scripts/cargo-shared-cache.sh}"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/nudox-cargo-cache.XXXXXX")"
cleanup() {
  if [ -n "${socket_pid:-}" ]; then
    kill "$socket_pid" 2>/dev/null || true
    wait "$socket_pid" 2>/dev/null || true
  fi
  rm -rf "$test_root"
}
trap cleanup EXIT

fail() {
  echo "cargo-shared-cache: $*" >&2
  exit 1
}
assert_eq() {
  expected="$1"
  actual="$2"
  [ "$expected" = "$actual" ] || fail "expected '$expected', got '$actual'"
}
assert_file_lines() {
  path="$1"
  expected="$2"
  actual="$(wc -l < "$path" | tr -d ' ')"
  assert_eq "$expected" "$actual"
}

mkdir -p "$test_root/bin" "$test_root/roots/a" "$test_root/roots/b"

printf '%s\n' '#!/bin/sh' \
  'if [ "${1:-}" = rev-parse ] && [ "${2:-}" = --show-toplevel ]; then' \
  '  printf "%s\\n" "$NUDOX_TEST_WORKTREE"' \
  '  exit 0' \
  'fi' \
  'if [ "${1:-}" = worktree ] && [ "${2:-}" = list ]; then' \
  '  printf "worktree %s\\n" "$NUDOX_TEST_WORKTREE"' \
  '  exit 0' \
  'fi' \
  'exit 0' > "$test_root/bin/git"
chmod +x "$test_root/bin/git"

printf '%s\n' '#!/bin/sh' \
  'if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then mkdir -p "$CARGO_BUILD_BUILD_DIR"; fi' \
  'if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then' \
  '  marker="$CARGO_BUILD_BUILD_DIR/fake-public-api.rmeta"' \
  '  if [ "${1:-}" = consumer ] && [ -f "$marker" ] && [ "$(cat "$marker")" != "$NUDOX_TEST_WORKTREE" ]; then exit 42; fi' \
  '  printf "%s\\n" "$NUDOX_TEST_WORKTREE" > "$marker"' \
  'fi' \
  'printf "%s|%s|%s\\n" "${NUDOX_TEST_WORKTREE:-}" "${CARGO_BUILD_BUILD_DIR:-}" "${1:-}" >> "$NUDOX_TEST_LOG"' \
  'if [ -n "${NUDOX_TEST_CHILD_PID_FILE:-}" ]; then printf "%s\\n" "$$" > "$NUDOX_TEST_CHILD_PID_FILE"; fi' \
  'trap '\''if [ -n "${NUDOX_TEST_CHILD_DONE_FILE:-}" ]; then : > "$NUDOX_TEST_CHILD_DONE_FILE"; fi; exit 143'\'' HUP INT TERM' \
  'if [ "${NUDOX_TEST_CARGO_SLEEP:-0}" != 0 ]; then' \
  '  end=$(( $(date +%s) + NUDOX_TEST_CARGO_SLEEP ))' \
  '  while [ "$(date +%s)" -lt "$end" ]; do :; done' \
  'fi' \
  'exit "${NUDOX_TEST_CARGO_STATUS:-0}"' > "$test_root/bin/cargo"
chmod +x "$test_root/bin/cargo"

printf '%s\n' '#!/bin/sh' 'exit 0' > "$test_root/bin/sccache"
chmod +x "$test_root/bin/sccache"

{
  printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail'
  sed \
    -e "s|@cargo@|$test_root/bin/cargo|g" \
    -e "s|@git@|$test_root/bin/git|g" \
    -e "s|@sccache@|$test_root/bin/sccache|g" \
    "$source_script"
} > "$test_root/wrapper"
chmod +x "$test_root/wrapper"

# Make a real Unix socket so the fake compiler cache daemon is considered
# ready. The wrapper never connects to it in these tests.
python3 - "$test_root/sccache.sock" <<'PY' &
import socket
import sys
import time

server = socket.socket(socket.AF_UNIX)
server.bind(sys.argv[1])
server.listen(1)
time.sleep(120)
PY
socket_pid="$!"

run_wrapper() {
  NUDOX_TEST_WORKTREE="$1" \
  NUDOX_TEST_LOG="$2" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/cache" \
  NUDOX_CARGO_BUILD_SLOTS="${NUDOX_TEST_SLOTS:-2}" \
  NUDOX_CARGO_WORKTREE_WAIT_MS="${NUDOX_TEST_WORKTREE_WAIT_MS:-300000}" \
  NUDOX_CARGO_SLOT_WAIT_MS="${NUDOX_TEST_SLOT_WAIT_MS:-300000}" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" "$3"
}

# Metadata and formatting remain unrestricted and do not require a socket or
# a mutable build directory.
metadata_log="$test_root/metadata.log"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$metadata_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/no-cache" SCCACHE_SERVER_UDS="$test_root/missing.sock" \
  "$test_root/wrapper" metadata
assert_file_lines "$metadata_log" 1
metadata_dir="$(cut -d '|' -f 2 "$metadata_log")"
[ -z "$metadata_dir" ] || fail "metadata unexpectedly leased $metadata_dir"

# Two compiling commands from one worktree serialize onto one remembered warm
# lane. The second invocation must not take another slot or overflow path.
same_log="$test_root/same.log"
NUDOX_TEST_SLOTS=2 NUDOX_TEST_CARGO_SLEEP=1 run_wrapper "$test_root/roots/a" "$same_log" build &
first_pid="$!"
sleep 0.15
NUDOX_TEST_SLOTS=2 NUDOX_TEST_CARGO_SLEEP=0 run_wrapper "$test_root/roots/a" "$same_log" check &
second_pid="$!"
wait "$first_pid"
wait "$second_pid"
assert_file_lines "$same_log" 2
same_dirs="$(cut -d '|' -f 2 "$same_log" | sort -u | wc -l | tr -d ' ')"
assert_eq 1 "$same_dirs"
same_paths="$(cut -d '|' -f 2 "$same_log")"
first_path="$(printf '%s\n' "$same_paths" | sed -n '1p')"
second_path="$(printf '%s\n' "$same_paths" | sed -n '2p')"
assert_eq "$first_path" "$second_path"

# Sequentially reusing one slot across worktrees resets the old intermediate
# graph before the consumer sees it. The marker stands in for an rmeta whose
# public exports changed between the two worktrees.
api_cache="$test_root/api-cache"
api_log="$test_root/api.log"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$api_log" \
  NUDOX_BUILD_CACHE_ROOT="$api_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build
NUDOX_TEST_WORKTREE="$test_root/roots/b" NUDOX_TEST_LOG="$api_log" \
  NUDOX_BUILD_CACHE_ROOT="$api_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" consumer

# Different worktrees can compile concurrently and receive different mutable
# directories when two warm slots exist.
different_log="$test_root/different.log"
start_time="$(date +%s)"
NUDOX_TEST_SLOTS=2 NUDOX_TEST_CARGO_SLEEP=1 run_wrapper "$test_root/roots/a" "$different_log" build &
different_a="$!"
NUDOX_TEST_SLOTS=2 NUDOX_TEST_CARGO_SLEEP=1 run_wrapper "$test_root/roots/b" "$different_log" build &
different_b="$!"
wait "$different_a"
wait "$different_b"
elapsed="$(( $(date +%s) - start_time ))"
assert_file_lines "$different_log" 2
different_dirs="$(cut -d '|' -f 2 "$different_log" | sort -u | wc -l | tr -d ' ')"
assert_eq 2 "$different_dirs"
[ "$elapsed" -le 2 ] || fail "independent worktrees were serialized (${elapsed}s)"

# An explicit build-dir is never overwritten and bypasses the affinity lease.
explicit_log="$test_root/explicit.log"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$explicit_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/explicit-cache" CARGO_BUILD_BUILD_DIR="$test_root/explicit-build" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build
explicit_dir="$(cut -d '|' -f 2 "$explicit_log")"
assert_eq "$test_root/explicit-build" "$explicit_dir"
if find "$test_root/explicit-cache/locks" -mindepth 1 -print -quit 2>/dev/null | grep . >/dev/null; then
  fail "explicit build-dir acquired a lease"
fi

# A dead owner is recovered without waiting for the configured bound.
stale_cache="$test_root/stale-cache"
stale_key="$(printf '%s' "$test_root/roots/a" | cksum | awk '{print $1 "-" $2}')"
mkdir -p "$stale_cache/locks/worktree-$stale_key.lock"
printf '%s\n' "$$" > "$stale_cache/locks/worktree-$stale_key.lock/pid"
printf 'process-start-token-from-a-different-life\n' > "$stale_cache/locks/worktree-$stale_key.lock/start"
stale_log="$test_root/stale.log"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$stale_log" \
  NUDOX_BUILD_CACHE_ROOT="$stale_cache" SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build
assert_file_lines "$stale_log" 1

# A cancelled compile forwards the signal to fake Cargo and releases both
# the worktree and slot leases before the wrapper exits.
signal_cache="$test_root/signal-cache"
signal_log="$test_root/signal.log"
signal_child_pid="$test_root/signal-child.pid"
signal_child_done="$test_root/signal-child.done"
signal_sleep="${NUDOX_TEST_SIGNAL_SLEEP:-1}"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$signal_log" \
  NUDOX_BUILD_CACHE_ROOT="$signal_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP="$signal_sleep" \
  NUDOX_TEST_CHILD_PID_FILE="$signal_child_pid" NUDOX_TEST_CHILD_DONE_FILE="$signal_child_done" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build &
signal_pid="$!"
signal_waited=0
while [ "$signal_waited" -lt 40 ] && [ ! -f "$signal_child_pid" ]; do
  sleep 0.05
  signal_waited="$((signal_waited + 1))"
done
kill -TERM "$signal_pid"
signal_observed=0
signal_waited=0
while [ "$signal_waited" -lt 40 ] && [ ! -f "$signal_child_done" ]; do
  if ! find "$signal_cache/locks" -type d -name 'worktree-*.lock' -print -quit 2>/dev/null | grep . >/dev/null; then
    fail "cancelled compile released the lease before fake Cargo acknowledged TERM"
  fi
  sleep 0.05
  signal_waited="$((signal_waited + 1))"
done
if wait "$signal_pid"; then
  fail "cancelled compile unexpectedly succeeded"
fi
if find "$signal_cache/locks" -type d \( -name 'worktree-*.lock' -o -name 'slot-*.lock' \) -print -quit 2>/dev/null | grep . >/dev/null; then
  fail "cancelled compile left a lease behind"
fi
[ -f "$signal_child_done" ] || fail "cancelled compile did not reap fake Cargo"
signal_child="$(cat "$signal_child_pid")"
if kill -0 "$signal_child" 2>/dev/null; then
  fail "cancelled compile left the Cargo child alive"
fi

# Interrupting while waiting for a busy warm lane also releases the worktree
# lease. Keep the synthetic slot owner alive for the duration of this check.
slot_wait_cache="$test_root/slot-wait-cache"
mkdir -p "$slot_wait_cache/locks/slot-0.lock"
printf '%s\n' "$$" > "$slot_wait_cache/locks/slot-0.lock/pid"
slot_wait_log="$test_root/slot-wait.log"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$slot_wait_log" \
  NUDOX_BUILD_CACHE_ROOT="$slot_wait_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=5000 NUDOX_TEST_CARGO_SLEEP=0 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build &
slot_wait_pid="$!"
slot_waited=0
while [ "$slot_waited" -lt 40 ] && ! find "$slot_wait_cache/locks" -type d -name 'worktree-*.lock' -print -quit 2>/dev/null | grep . >/dev/null; do
  sleep 0.05
  slot_waited="$((slot_waited + 1))"
done
kill -TERM "$slot_wait_pid"
if wait "$slot_wait_pid"; then
  fail "slot-wait interruption unexpectedly succeeded"
fi
if find "$slot_wait_cache/locks" -type d -name 'worktree-*.lock' -print -quit 2>/dev/null | grep . >/dev/null; then
  fail "slot-wait interruption left a worktree lease behind"
fi
rm -rf "$slot_wait_cache/locks/slot-0.lock"

# With one warm lane occupied, another worktree cannot create an overflow
# compiler. It fails at the configured bound, then succeeds after release.
ceiling_cache="$test_root/ceiling-cache"
ceiling_log="$test_root/ceiling.log"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$ceiling_log" \
  NUDOX_BUILD_CACHE_ROOT="$ceiling_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=1 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build &
ceiling_owner="$!"
ceiling_waited=0
while [ "$ceiling_waited" -lt 40 ] && ! find "$ceiling_cache/locks" -type d -name 'slot-*.lock' -print -quit 2>/dev/null | grep . >/dev/null; do
  sleep 0.05
  ceiling_waited="$((ceiling_waited + 1))"
done
if NUDOX_TEST_WORKTREE="$test_root/roots/b" NUDOX_TEST_LOG="$ceiling_log" \
  NUDOX_BUILD_CACHE_ROOT="$ceiling_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=0 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" check; then
  fail "busy build ceiling unexpectedly admitted another compiler"
fi
assert_file_lines "$ceiling_log" 1
wait "$ceiling_owner"
NUDOX_TEST_WORKTREE="$test_root/roots/b" NUDOX_TEST_LOG="$ceiling_log" \
  NUDOX_BUILD_CACHE_ROOT="$ceiling_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=0 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" check
assert_file_lines "$ceiling_log" 2
if find "$ceiling_cache/build" -maxdepth 1 -type d -name 'overflow-*' -print -quit 2>/dev/null | grep . >/dev/null; then
  fail "hard build ceiling created an overflow directory"
fi

echo "cargo-shared-cache: PASS (affinity, independent lanes, hard ceiling, bypasses, override, stale recovery)"

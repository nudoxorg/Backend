#!/bin/sh
# Fast protocol tests for .config/scripts/cargo-shared-cache.sh. These tests
# replace Cargo, git, and sccache with tiny shims; they never compile the
# workspace or evaluate Nix.
set -eu
unset CARGO_BUILD_BUILD_DIR CARGO_TARGET_DIR

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
  if [ -n "${protocol_socket_a:-}" ]; then rm -f "$protocol_socket_a"; fi
  if [ -n "${protocol_socket_b:-}" ]; then rm -f "$protocol_socket_b"; fi
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
'git_directory=""' \
'if [ "${1:-}" = -C ]; then git_directory="$2"; shift 2; fi' \
'if [ "${1:-}" = rev-parse ] && [ "${2:-}" = --show-toplevel ]; then' \
'  if [ -n "$git_directory" ] && [ -n "${NUDOX_TEST_MANIFEST_WORKTREE:-}" ]; then' \
'    case "$git_directory" in' \
'      "$NUDOX_TEST_MANIFEST_WORKTREE"|"$NUDOX_TEST_MANIFEST_WORKTREE"/*) printf "%s\\n" "$NUDOX_TEST_MANIFEST_WORKTREE" ;;' \
'      *) printf "%s\\n" "${NUDOX_TEST_WORKTREE:-}" ;;' \
'    esac' \
'  else' \
'    printf "%s\\n" "${NUDOX_TEST_WORKTREE:-}"' \
'  fi' \
'  exit 0' \
'fi' \
'if [ "${1:-}" = rev-parse ] && [ "${2:-}" = HEAD ]; then printf "0123456789abcdef0123456789abcdef01234567\\n"; exit 0; fi' \
'if [ "${1:-}" = status ] || [ "${1:-}" = diff ] || [ "${1:-}" = ls-files ]; then exit 0; fi' \
'if [ "${1:-}" = worktree ] && [ "${2:-}" = list ]; then' \
  '  printf "worktree %s\\n" "$NUDOX_TEST_WORKTREE"' \
  '  exit 0' \
  'fi' \
  'exit 0' > "$test_root/bin/git"
chmod +x "$test_root/bin/git"

printf '%s\n' '#!/bin/sh' \
'if [ "${1:-}" = --version ]; then printf "cargo 1.97.1-test\\n"; exit 0; fi' \
'if [ -n "${NUDOX_TEST_TOOLCHAIN_LOG:-}" ]; then printf "%s|%s\\n" "$RUSTC" "$RUSTDOC" >> "$NUDOX_TEST_TOOLCHAIN_LOG"; fi' \
'if [ -n "${NUDOX_TEST_CACHE_ENDPOINT_LOG:-}" ]; then printf "%s|%s\\n" "$SCCACHE_SERVER_UDS" "$SCCACHE_DIR" >> "$NUDOX_TEST_CACHE_ENDPOINT_LOG"; fi' \
'if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then mkdir -p "$CARGO_BUILD_BUILD_DIR"; fi' \
'if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then' \
  '  if [ -n "${NUDOX_TEST_REQUIRE_BUILD_MARKER:-}" ]; then' \
  '    required_marker="$CARGO_BUILD_BUILD_DIR/$NUDOX_TEST_REQUIRE_BUILD_MARKER"' \
  '    if [ ! -f "$required_marker" ] || [ "$(cat "$required_marker")" != "${NUDOX_TEST_EXPECT_BUILD_MARKER_CONTENT:-}" ]; then exit 44; fi' \
  '  fi' \
  '  marker="$CARGO_BUILD_BUILD_DIR/fake-public-api.rmeta"' \
  '  if [ "${1:-}" = consumer ] && [ -f "$marker" ] && [ "$(cat "$marker")" != "$NUDOX_TEST_WORKTREE" ]; then exit 42; fi' \
'  printf "%s\\n" "$NUDOX_TEST_WORKTREE" > "$marker"' \
'fi' \
'if [ "${NUDOX_TEST_CREATE_OUTPUT:-0}" != 0 ]; then mkdir -p "$CARGO_TARGET_DIR/debug"; printf "test executable\\n" > "$CARGO_TARGET_DIR/debug/fake-bin"; chmod +x "$CARGO_TARGET_DIR/debug/fake-bin"; fi' \
'if [ -n "${NUDOX_TEST_MUTATE_RUSTC:-}" ]; then printf "# changed during cargo\\n" >> "$NUDOX_TEST_MUTATE_RUSTC"; fi' \
'if [ "${NUDOX_TEST_BREAK_PROVENANCE:-0}" != 0 ]; then printf "not a directory\\n" > "$CARGO_TARGET_DIR/.nudox-provenance"; fi' \
'printf "%s|%s|%s|%s|%s|%s\\n" "${NUDOX_TEST_WORKTREE:-}" "${CARGO_BUILD_BUILD_DIR:-}" "${CARGO_TARGET_DIR:-}" "$*" "${CARGO_BUILD_JOBS:-}" "${RUSTC_WRAPPER:-}" >> "$NUDOX_TEST_LOG"' \
'if [ -n "${NUDOX_TEST_CHILD_PID_FILE:-}" ]; then printf "%s\\n" "$$" > "$NUDOX_TEST_CHILD_PID_FILE"; fi' \
  'trap '\''if [ -n "${NUDOX_TEST_CHILD_DONE_FILE:-}" ]; then : > "$NUDOX_TEST_CHILD_DONE_FILE"; fi; exit 143'\'' HUP INT TERM' \
  'if [ "${NUDOX_TEST_CARGO_SLEEP:-0}" != 0 ]; then' \
  '  end=$(( $(date +%s) + NUDOX_TEST_CARGO_SLEEP ))' \
  '  while [ "$(date +%s)" -lt "$end" ]; do :; done' \
  'fi' \
  'exit "${NUDOX_TEST_CARGO_STATUS:-0}"' > "$test_root/bin/cargo"
chmod +x "$test_root/bin/cargo"

printf '%s\n' '#!/bin/sh' \
  'if [ -n "${RUSTC_TEST_LOG:-}" ]; then printf "selected:%s\\n" "$*" >> "$RUSTC_TEST_LOG"; fi' \
  'if [ "${1:-}" = --version ]; then printf "rustc 1.97.1-test\\n"; exit 0; fi' \
  'exit "${RUSTC_TEST_STATUS:-0}"' > "$test_root/bin/rustc"
chmod +x "$test_root/bin/rustc"

printf '%s\n' '#!/bin/sh' \
  'if [ "${1:-}" = --version ]; then printf "rustdoc 1.97.1-test\\n"; exit 0; fi' \
  'exit 0' > "$test_root/bin/rustdoc"
chmod +x "$test_root/bin/rustdoc"

printf '%s\n' '#!/bin/sh' \
  'if [ -n "${SCCACHE_TEST_LOG:-}" ]; then printf "%s\\n" "$*" >> "$SCCACHE_TEST_LOG"; fi' \
  'exit "${SCCACHE_TEST_STATUS:-0}"' > "$test_root/bin/sccache"
chmod +x "$test_root/bin/sccache"

sed -e "s|@sccache@|$test_root/bin/sccache|g" \
  "$repo_root/.config/scripts/cargo-rustc-cache.sh" > "$test_root/rustc-cache-wrapper"
chmod +x "$test_root/rustc-cache-wrapper"

{
  printf '%s\n' '#!/bin/sh' 'set -eu'
  sed \
    -e "s|@cargo@|$test_root/bin/cargo|g" \
    -e "s|@git@|$test_root/bin/git|g" \
    -e "s|@sccache@|$test_root/bin/sccache|g" \
    -e "s|@rustc_cache_wrapper@|$test_root/rustc-cache-wrapper|g" \
    -e "s|@python3@|$(command -v python3)|g" \
    -e "s|@rustc@|$test_root/bin/rustc|g" \
    -e "s|@wrapper_source@|$repo_root/.config/scripts/cargo-shared-cache.sh|g" \
    -e "s|@provenance@|$repo_root/.config/scripts/cargo-provenance.py|g" \
    "$source_script"
} > "$test_root/wrapper"
chmod +x "$test_root/wrapper"

# Distinct immutable provider paths model a Nix upgrade from an incompatible
# daemon protocol. Both providers may continue sharing the content cache.
cp "$test_root/bin/sccache" "$test_root/bin/sccache-v2"
sed "s|$test_root/bin/sccache|$test_root/bin/sccache-v2|g" \
  "$test_root/wrapper" > "$test_root/wrapper-v2"
chmod +x "$test_root/wrapper-v2"
protocol_cache="$test_root/protocol-cache"
mkdir -p "$protocol_cache"
protocol_runtime="/tmp/nudox-sccache-$(id -u)"
if [ ! -e "$protocol_runtime" ]; then mkdir -m 700 "$protocol_runtime"; fi
protocol_socket_a="$protocol_runtime/$(python3 -c 'import hashlib, os, sys; print(hashlib.sha256(os.fsencode(sys.argv[1]) + b"\0" + os.fsencode(sys.argv[2])).hexdigest()[:24])' "$test_root/bin/sccache" "$protocol_cache/sccache").sock"
protocol_socket_b="$protocol_runtime/$(python3 -c 'import hashlib, os, sys; print(hashlib.sha256(os.fsencode(sys.argv[1]) + b"\0" + os.fsencode(sys.argv[2])).hexdigest()[:24])' "$test_root/bin/sccache-v2" "$protocol_cache/sccache").sock"

# Make a real Unix socket so the fake compiler cache daemon is considered
# ready. The wrapper never connects to it in these tests.
python3 - "$test_root/sccache.sock" "$protocol_cache/sccache.sock" \
  "$protocol_socket_a" "$protocol_socket_b" <<'PY' &
import socket
import sys
import time

servers = []
for path in sys.argv[1:]:
    server = socket.socket(socket.AF_UNIX)
    server.bind(path)
    server.listen(1)
    servers.append(server)
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

run_explicit_wrapper() {
  NUDOX_TEST_WORKTREE="$1" \
  NUDOX_TEST_LOG="$2" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/cache-explicit" \
  NUDOX_CARGO_BUILD_SLOTS="${NUDOX_TEST_SLOTS:-2}" \
  NUDOX_CARGO_WORKTREE_WAIT_MS="${NUDOX_TEST_WORKTREE_WAIT_MS:-300000}" \
  NUDOX_CARGO_SLOT_WAIT_MS="${NUDOX_TEST_SLOT_WAIT_MS:-300000}" \
  CARGO_BUILD_BUILD_DIR="$3" \
  CARGO_TARGET_DIR="$4" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" "${5:-build}" "${6:-}"
}

# The tracked cargo-in source uses an explicit role root while leaving final
# artifacts in the worktree-local target. Exercise it through the real wrapper.
cargo_in_root="$test_root/roots/cargo-in"
cargo_in_log="$test_root/cargo-in.log"
mkdir -p "$cargo_in_root/.local/devenv" "$test_root/wrapper-bin"
ln -s "$test_root/wrapper" "$test_root/wrapper-bin/cargo"
printf 'PATH="%s:$PATH"; export PATH\n' "$test_root/wrapper-bin" \
  > "$cargo_in_root/.local/devenv/development.sh"
NUDOX_TEST_WORKTREE="$cargo_in_root" NUDOX_TEST_LOG="$cargo_in_log" \
  PATH="$test_root/bin:$PATH" CARGO_BUILD_JOBS="" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/cargo-in-cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$repo_root/.config/scripts/cargo-in.sh" shell build --locked -p demo
cargo_in_build="$cargo_in_root/.local/build/shell/.nudox-cargo/slot-0"
cargo_in_target="$cargo_in_root/.local/target"
assert_eq "$cargo_in_build" "$(cut -d '|' -f 2 "$cargo_in_log")"
assert_eq "$cargo_in_target" "$(cut -d '|' -f 3 "$cargo_in_log")"
assert_eq 'build --locked -p demo' "$(cut -d '|' -f 4 "$cargo_in_log")"
assert_eq 3 "$(cut -d '|' -f 5 "$cargo_in_log")"
assert_eq "$cargo_in_root" "$(cat "$cargo_in_build/.nudox-worktree-root")"
assert_eq "$cargo_in_root" "$(cat "$cargo_in_target/.nudox-worktree-root")"
assert_eq "$test_root/rustc-cache-wrapper" "$(cut -d '|' -f 6 "$cargo_in_log")"

# The development shell exports this exact in-worktree default target before
# the Cargo wrapper runs. It remains usable when populated, while receiving a
# root stamp that cannot authorize another worktree.
default_target_root="$test_root/roots/default"
default_target="$default_target_root/.local/target"
mkdir -p "$default_target/debug"
printf 'existing final artifact\n' > "$default_target/debug/keep.bin"
default_target_log="$test_root/default-target.log"
NUDOX_TEST_WORKTREE="$default_target_root" NUDOX_TEST_LOG="$default_target_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/default-target-cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_TARGET_DIR="$default_target" SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build
assert_eq "$default_target" "$(cut -d '|' -f 3 "$default_target_log")"
assert_eq "$default_target_root" "$(cat "$default_target/.nudox-worktree-root")"
assert_eq 'existing final artifact' "$(cat "$default_target/debug/keep.bin")"

cargo_in_explicit_log="$test_root/cargo-in-explicit.log"
cargo_in_explicit_build="$test_root/cargo-in-explicit-build"
cargo_in_explicit_target="$test_root/cargo-in-explicit-target"
NUDOX_TEST_WORKTREE="$cargo_in_root" NUDOX_TEST_LOG="$cargo_in_explicit_log" \
  PATH="$test_root/bin:$PATH" CARGO_BUILD_JOBS=7 RUSTC_WRAPPER="$test_root/custom-rustc-wrapper" \
  CARGO_BUILD_BUILD_DIR="$cargo_in_explicit_build" CARGO_TARGET_DIR="$cargo_in_explicit_target" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/cargo-in-cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$repo_root/.config/scripts/cargo-in.sh" shell check --offline -p demo
assert_eq "$cargo_in_explicit_build/.nudox-cargo/slot-0" "$(cut -d '|' -f 2 "$cargo_in_explicit_log")"
assert_eq "$cargo_in_explicit_target" "$(cut -d '|' -f 3 "$cargo_in_explicit_log")"
assert_eq 'check --offline -p demo' "$(cut -d '|' -f 4 "$cargo_in_explicit_log")"
assert_eq 7 "$(cut -d '|' -f 5 "$cargo_in_explicit_log")"
assert_eq "$test_root/custom-rustc-wrapper" "$(cut -d '|' -f 6 "$cargo_in_explicit_log")"

# Only ordinary registry/git library source is routed to the immutable
# compiler cache. Workspace, vendored, build-script, proc-macro, and unknown
# invocations retain exact direct-rustc behavior.
rustc_cache_home="$test_root/cargo home"
rustc_cache_workspace="$test_root/workspace"
mkdir -p "$rustc_cache_home/registry/src/index.crates.io-1/serde-1/src" \
  "$rustc_cache_home/git/checkouts/example-1/commit/src" \
  "$rustc_cache_workspace/src" "$rustc_cache_workspace/vendor/example/src"
registry_source="$rustc_cache_home/registry/src/index.crates.io-1/serde-1/src/lib.rs"
git_source="$rustc_cache_home/git/checkouts/example-1/commit/src/lib.rs"
workspace_source="$rustc_cache_workspace/src/lib.rs"
vendor_source="$rustc_cache_workspace/vendor/example/src/lib.rs"
for source in "$registry_source" "$git_source" "$workspace_source" "$vendor_source"; do : > "$source"; done
registry_workspace_symlink="$rustc_cache_home/registry/src/index.crates.io-1/serde-1/src/workspace-link.rs"
ln -s "$workspace_source" "$registry_workspace_symlink"
rustc_cache_log="$test_root/rustc-cache.log"
rustc_direct_log="$test_root/rustc-direct.log"
: > "$rustc_cache_log"
: > "$rustc_direct_log"
if CARGO_HOME="$rustc_cache_home" SCCACHE_TEST_LOG="$rustc_cache_log" \
  SCCACHE_TEST_STATUS=19 RUSTC_TEST_LOG="$rustc_direct_log" \
  "$test_root/rustc-cache-wrapper" "$test_root/bin/rustc" \
  --crate-name serde --crate-type lib "$registry_source" --out-dir "$test_root/output"; then
  fail "external library cache status was not propagated"
else
  assert_eq 19 "$?"
fi
assert_file_lines "$rustc_cache_log" 1
assert_eq 0 "$(wc -l < "$rustc_direct_log" | tr -d ' ')"
case "$(cat "$rustc_cache_log")" in
  *"$test_root/bin/rustc --crate-name serde --crate-type lib $registry_source --out-dir $test_root/output"*) ;;
  *) fail "external compiler arguments changed before sccache" ;;
esac
CARGO_HOME="$rustc_cache_home" SCCACHE_TEST_LOG="$rustc_cache_log" \
  SCCACHE_TEST_STATUS=0 RUSTC_TEST_LOG="$rustc_direct_log" \
  "$test_root/rustc-cache-wrapper" "$test_root/bin/rustc" \
  --crate-name example --crate-type=rlib "$git_source" --out-dir "$test_root/output"
assert_file_lines "$rustc_cache_log" 2
if CARGO_HOME="$rustc_cache_home" SCCACHE_TEST_LOG="$rustc_cache_log" \
  SCCACHE_TEST_STATUS=0 RUSTC_TEST_LOG="$rustc_direct_log" RUSTC_TEST_STATUS=23 \
  "$test_root/rustc-cache-wrapper" "$test_root/bin/rustc" \
  --crate-name application --crate-type lib "$workspace_source" --out-dir "$test_root/output"; then
  fail "workspace direct-rustc status was not propagated"
else
  assert_eq 23 "$?"
fi
assert_file_lines "$rustc_cache_log" 2
assert_file_lines "$rustc_direct_log" 1
case "$(cat "$rustc_direct_log")" in
  *"--crate-name application --crate-type lib $workspace_source --out-dir $test_root/output"*) ;;
  *) fail "workspace compiler arguments changed before rustc" ;;
esac
run_rustc_cache_case() {
  case "$2" in
    "")
      CARGO_HOME="$rustc_cache_home" SCCACHE_TEST_LOG="$rustc_cache_log" \
        SCCACHE_TEST_STATUS=0 RUSTC_TEST_LOG="$rustc_direct_log" \
        "$test_root/rustc-cache-wrapper" "$test_root/bin/rustc" \
        --crate-name "$1" "$3" --out-dir "$test_root/output"
      ;;
    *)
      CARGO_HOME="$rustc_cache_home" SCCACHE_TEST_LOG="$rustc_cache_log" \
        SCCACHE_TEST_STATUS=0 RUSTC_TEST_LOG="$rustc_direct_log" \
        "$test_root/rustc-cache-wrapper" "$test_root/bin/rustc" \
        --crate-name "$1" --crate-type "$2" "$3" --out-dir "$test_root/output"
      ;;
  esac
}
run_rustc_cache_case build_script_build bin "$registry_source"
run_rustc_cache_case serde_derive proc-macro "$registry_source"
run_rustc_cache_case vendor_crate lib "$vendor_source"
run_rustc_cache_case ambiguous "" "$registry_source"
run_rustc_cache_case symlinked_workspace lib "$registry_workspace_symlink"
assert_file_lines "$rustc_cache_log" 2
assert_file_lines "$rustc_direct_log" 6
case "$(tail -n 1 "$rustc_direct_log")" in
  *"$registry_workspace_symlink"*) ;;
  *) fail "symlinked workspace source was not passed directly to rustc" ;;
esac

# Metadata and formatting remain unrestricted and do not require a socket or
# a mutable build directory.
metadata_log="$test_root/metadata.log"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$metadata_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/no-cache" SCCACHE_SERVER_UDS="$test_root/missing.sock" \
  "$test_root/wrapper" metadata
assert_file_lines "$metadata_log" 1
metadata_dir="$(cut -d '|' -f 2 "$metadata_log")"
[ -z "$metadata_dir" ] || fail "metadata unexpectedly leased $metadata_dir"

# Cargo's manifest can name a different worktree from the caller's cwd. When
# the shell supplied the caller worktree's ordinary default target, redirect
# only that managed default to the manifest worktree. Its graph, target stamp,
# and affinity are then keyed to the actual Cargo source tree.
manifest_invoking_root="$test_root/roots/manifest-invoker"
manifest_workspace_root="$test_root/roots/manifest-source"
manifest_directory="$manifest_workspace_root/crates/demo"
mkdir -p "$manifest_invoking_root/.local/target" "$manifest_directory"
: > "$manifest_directory/Cargo.toml"
manifest_log="$test_root/manifest-default.log"
NUDOX_TEST_WORKTREE="$manifest_invoking_root" \
  NUDOX_TEST_MANIFEST_WORKTREE="$manifest_workspace_root" \
  NUDOX_TEST_LOG="$manifest_log" NUDOX_BUILD_CACHE_ROOT="$test_root/manifest-cache" \
  NUDOX_CARGO_BUILD_SLOTS=1 CARGO_TARGET_DIR="$manifest_invoking_root/.local/target" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build --manifest-path "$manifest_directory/Cargo.toml" --locked
manifest_default_target="$manifest_workspace_root/.local/target"
assert_eq "$manifest_default_target" "$(cut -d '|' -f 3 "$manifest_log")"
manifest_default_build="$(cut -d '|' -f 2 "$manifest_log")"
assert_eq "$manifest_workspace_root" "$(cat "$manifest_default_build/.nudox-worktree-root")"
assert_eq "$manifest_workspace_root" "$(cat "$manifest_default_target/.nudox-worktree-root")"
assert_eq "build --manifest-path $manifest_directory/Cargo.toml --locked" \
  "$(cut -d '|' -f 4 "$manifest_log")"

# Caller-selected paths stay exact for a cross-worktree manifest invocation.
# The explicit role receives a manifest-owned subgraph and target stamp, while
# the pooled lane's affinity and prior graph remain untouched.
manifest_explicit_cache="$test_root/manifest-explicit-cache"
manifest_explicit_log="$test_root/manifest-explicit.log"
manifest_explicit_build="$test_root/manifest-role"
manifest_explicit_target="$test_root/manifest-target"
mkdir -p "$manifest_explicit_cache/build/slot-0" "$manifest_explicit_cache/affinity"
printf '%s\n' "$test_root/roots/a" > "$manifest_explicit_cache/affinity/slot-0.owner"
printf 'warm pooled graph\n' > "$manifest_explicit_cache/build/slot-0/preserved.rmeta"
NUDOX_TEST_WORKTREE="$manifest_invoking_root" \
  NUDOX_TEST_MANIFEST_WORKTREE="$manifest_workspace_root" \
  NUDOX_TEST_LOG="$manifest_explicit_log" NUDOX_BUILD_CACHE_ROOT="$manifest_explicit_cache" \
  NUDOX_CARGO_BUILD_SLOTS=1 CARGO_BUILD_BUILD_DIR="$manifest_explicit_build" \
  CARGO_TARGET_DIR="$manifest_explicit_target" SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" check --manifest-path="$manifest_directory/Cargo.toml" --offline
manifest_explicit_graph="$manifest_explicit_build/.nudox-cargo/slot-0"
assert_eq "$manifest_explicit_graph" "$(cut -d '|' -f 2 "$manifest_explicit_log")"
assert_eq "$manifest_explicit_target" "$(cut -d '|' -f 3 "$manifest_explicit_log")"
assert_eq "$manifest_workspace_root" "$(cat "$manifest_explicit_graph/.nudox-worktree-root")"
assert_eq "$manifest_workspace_root" "$(cat "$manifest_explicit_target/.nudox-worktree-root")"
assert_eq "$test_root/roots/a" "$(cat "$manifest_explicit_cache/affinity/slot-0.owner")"
assert_eq 'warm pooled graph' "$(cat "$manifest_explicit_cache/build/slot-0/preserved.rmeta")"

# An explicit build role or target that resolves through a symlink into any
# pooled Cargo slot is refused before either caller path or pooled graph is
# mutated. This guards the no-overflow pool invariant even under path aliases.
pooled_alias_cache="$test_root/pooled-alias-cache"
pooled_alias_build_log="$test_root/pooled-alias-build.log"
pooled_alias_target_log="$test_root/pooled-alias-target.log"
mkdir -p "$pooled_alias_cache/build" "$test_root/pooled-build-alias" "$test_root/pooled-target-alias"
ln -s "$pooled_alias_cache/build" "$test_root/pooled-build-alias/cache"
ln -s "$pooled_alias_cache/build" "$test_root/pooled-target-alias/cache"
if NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$pooled_alias_build_log" \
  NUDOX_BUILD_CACHE_ROOT="$pooled_alias_cache" NUDOX_CARGO_BUILD_SLOTS=2 \
  CARGO_BUILD_BUILD_DIR="$test_root/pooled-build-alias/cache/slot-1" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build; then
  fail "explicit build role aliasing pooled slot was accepted"
fi
if NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$pooled_alias_target_log" \
  NUDOX_BUILD_CACHE_ROOT="$pooled_alias_cache" NUDOX_CARGO_BUILD_SLOTS=2 \
  CARGO_TARGET_DIR="$test_root/pooled-target-alias/cache/slot-1" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build; then
  fail "explicit target aliasing pooled slot was accepted"
fi
[ ! -e "$pooled_alias_build_log" ] || fail "pooled build-role alias reached Cargo"
[ ! -e "$pooled_alias_target_log" ] || fail "pooled target alias reached Cargo"
[ ! -e "$pooled_alias_cache/build/slot-1/.nudox-worktree-root" ] \
  || fail "pooled slot alias mutated the pooled graph"

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

# The default machine-wide pool admits at most four active Cargo processes.
# Start four independent roots, wait until each reaches fake Cargo, and verify
# a fifth caller fails at the zero-wait boundary without reaching Cargo.
four_cache="$test_root/four-cache"
four_log="$test_root/four.log"
: > "$four_log"
four_pids=""
for root_name in c d e f; do
  four_root="$test_root/roots/$root_name"
  mkdir -p "$four_root"
  NUDOX_TEST_WORKTREE="$four_root" NUDOX_TEST_LOG="$four_log" \
    NUDOX_BUILD_CACHE_ROOT="$four_cache" NUDOX_CARGO_BUILD_SLOTS=4 \
    NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=3 \
    SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build &
  four_pids="$four_pids $!"
done
four_waited=0
while [ "$four_waited" -lt 120 ] && [ "$(wc -l < "$four_log" 2>/dev/null | tr -d ' ')" != 4 ]; do
  sleep 0.05
  four_waited="$((four_waited + 1))"
done
assert_file_lines "$four_log" 4
if NUDOX_TEST_WORKTREE="$test_root/roots/g" NUDOX_TEST_LOG="$four_log" \
  NUDOX_BUILD_CACHE_ROOT="$four_cache" NUDOX_CARGO_BUILD_SLOTS=4 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=0 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build; then
  fail "default four-slot ceiling admitted a fifth Cargo process"
fi
assert_file_lines "$four_log" 4
for four_pid in $four_pids; do wait "$four_pid"; done

# A caller cannot raise the host-wide cap beyond four, even by setting the
# wrapper's configurable slot count directly.
five_cache="$test_root/five-cache"
five_log="$test_root/five.log"
if NUDOX_TEST_WORKTREE="$test_root/roots/g" NUDOX_TEST_LOG="$five_log" \
  NUDOX_BUILD_CACHE_ROOT="$five_cache" NUDOX_CARGO_BUILD_SLOTS=5 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build; then
  fail "five-slot override bypassed the hard machine-wide cap"
else
  assert_eq 64 "$?"
fi
[ ! -e "$five_log" ] || fail "invalid five-slot override reached Cargo"

# An explicit build-dir is a role root. Its leased child is stamped, isolated
# between worktrees, and the caller's parent directory remains intact.
explicit_log="$test_root/explicit.log"
explicit_build_root="$test_root/explicit-build"
explicit_target_root="$test_root/explicit-target"
printf 'preserve caller data\n' > "$explicit_build_root-sentinel"
NUDOX_TEST_SLOTS=1 run_explicit_wrapper "$test_root/roots/a" "$explicit_log" \
  "$explicit_build_root" "$explicit_target_root" build
explicit_dir="$(cut -d '|' -f 2 "$explicit_log")"
explicit_target="$(cut -d '|' -f 3 "$explicit_log")"
assert_eq "$explicit_build_root/.nudox-cargo/slot-0" "$explicit_dir"
assert_eq "$explicit_target_root" "$explicit_target"
assert_eq "$test_root/roots/a" "$(cat "$explicit_dir/.nudox-worktree-root")"
assert_eq "$test_root/roots/a" "$(cat "$explicit_target/.nudox-worktree-root")"
assert_eq 'preserve caller data' "$(cat "$explicit_build_root-sentinel")"

# A different worktree gets another role-local graph while one is available;
# reusing the exact target path is still refused.
before_lines="$(wc -l < "$explicit_log" | tr -d ' ')"
if NUDOX_TEST_WORKTREE="$test_root/roots/b" NUDOX_TEST_LOG="$explicit_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/cache-explicit" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_BUILD_BUILD_DIR="$explicit_build_root" CARGO_TARGET_DIR="$explicit_target_root" \
  NUDOX_CARGO_SLOT_WAIT_MS=0 SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" consumer; then
  fail "target directory stamped for another worktree was accepted"
fi
assert_eq "$test_root/roots/a" "$(cat "$explicit_dir/.nudox-worktree-root")"
explicit_b_graph="$explicit_build_root/.nudox-cargo/slot-1"
assert_eq "$test_root/roots/b" "$(cat "$explicit_b_graph/.nudox-worktree-root")"
assert_eq "$test_root/roots/a" "$(cat "$explicit_target_root/.nudox-worktree-root")"
assert_eq "$before_lines" "$(wc -l < "$explicit_log" | tr -d ' ')"
assert_eq 'preserve caller data' "$(cat "$explicit_build_root-sentinel")"

# The host permit number is independent of the role graph slot. Occupying the
# worktree's deterministic first permit forces the next invocation to use the
# other host permit, while the ownership stamp makes it reacquire graph 0 and
# fake Cargo verifies the prior graph data survived intact.
role_capacity_slot="$(printf '%s' "$test_root/roots/a" | cksum | awk '{print $1 % 2}')"
role_alternate_capacity_slot="$((1 - role_capacity_slot))"
role_capacity_lock="$test_root/cache-explicit/locks/slot-$role_capacity_slot.lock"
mkdir -p "$role_capacity_lock"
printf '%s\n' "$$" > "$role_capacity_lock/pid"
printf 'warm role graph\n' > "$explicit_dir/warm-owner-graph.rmeta"
role_reuse_log="$test_root/role-reuse.log"
: > "$role_reuse_log"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$role_reuse_log" \
  NUDOX_TEST_REQUIRE_BUILD_MARKER=warm-owner-graph.rmeta \
  NUDOX_TEST_EXPECT_BUILD_MARKER_CONTENT='warm role graph' \
  NUDOX_TEST_CARGO_SLEEP=2 NUDOX_BUILD_CACHE_ROOT="$test_root/cache-explicit" \
  NUDOX_CARGO_BUILD_SLOTS=2 NUDOX_CARGO_SLOT_WAIT_MS=0 \
  CARGO_BUILD_BUILD_DIR="$explicit_build_root" CARGO_TARGET_DIR="$explicit_target_root" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build &
role_reuse_pid="$!"
role_reuse_waited=0
while [ "$role_reuse_waited" -lt 80 ] \
  && [ "$(wc -l < "$role_reuse_log" 2>/dev/null | tr -d ' ')" != 1 ]; do
  sleep 0.05
  role_reuse_waited="$((role_reuse_waited + 1))"
done
assert_file_lines "$role_reuse_log" 1
assert_eq "$explicit_dir" "$(cut -d '|' -f 2 "$role_reuse_log")"
[ -d "$test_root/cache-explicit/locks/slot-$role_alternate_capacity_slot.lock" ] \
  || fail "role graph reacquisition did not use the alternate host permit"
[ -d "$explicit_build_root/.nudox-cargo/leases/slot-0.lock" ] \
  || fail "role graph lease was not independent of the host permit"
wait "$role_reuse_pid"
assert_eq 'warm role graph' "$(cat "$explicit_dir/warm-owner-graph.rmeta")"
[ ! -d "$explicit_build_root/.nudox-cargo/leases/slot-0.lock" ] \
  || fail "role graph lease remained after fake Cargo exited"
rm -rf "$role_capacity_lock"

# Distinct explicit roles own distinct role-local slot 0 graphs even while
# simultaneous builds receive distinct global host permits.
parallel_role_cache="$test_root/parallel-role-cache"
parallel_role_log="$test_root/parallel-role.log"
parallel_role_a="$test_root/parallel-role-a"
parallel_role_b="$test_root/parallel-role-b"
parallel_target_a="$test_root/parallel-target-a"
parallel_target_b="$test_root/parallel-target-b"
: > "$parallel_role_log"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$parallel_role_log" \
  NUDOX_BUILD_CACHE_ROOT="$parallel_role_cache" NUDOX_CARGO_BUILD_SLOTS=2 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=2 \
  CARGO_BUILD_BUILD_DIR="$parallel_role_a" CARGO_TARGET_DIR="$parallel_target_a" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build &
parallel_role_pid_a="$!"
NUDOX_TEST_WORKTREE="$test_root/roots/b" NUDOX_TEST_LOG="$parallel_role_log" \
  NUDOX_BUILD_CACHE_ROOT="$parallel_role_cache" NUDOX_CARGO_BUILD_SLOTS=2 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=2 \
  CARGO_BUILD_BUILD_DIR="$parallel_role_b" CARGO_TARGET_DIR="$parallel_target_b" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" build &
parallel_role_pid_b="$!"
parallel_role_waited=0
while [ "$parallel_role_waited" -lt 80 ] \
  && [ "$(wc -l < "$parallel_role_log" | tr -d ' ')" != 2 ]; do
  sleep 0.05
  parallel_role_waited="$((parallel_role_waited + 1))"
done
assert_file_lines "$parallel_role_log" 2
assert_eq "$parallel_role_a/.nudox-cargo/slot-0" \
  "$(awk -F '|' -v root="$test_root/roots/a" '$1 == root { print $2; exit }' "$parallel_role_log")"
assert_eq "$parallel_role_b/.nudox-cargo/slot-0" \
  "$(awk -F '|' -v root="$test_root/roots/b" '$1 == root { print $2; exit }' "$parallel_role_log")"
assert_eq 2 "$(find "$parallel_role_cache/locks" -maxdepth 1 -type d -name 'slot-*.lock' -print | wc -l | tr -d ' ')"
parallel_permit_owners="$(for slot in 0 1; do cat "$parallel_role_cache/locks/slot-$slot.lock/workspace"; done | sort | tr '\n' ':')"
assert_eq "$test_root/roots/a:$test_root/roots/b:" "$parallel_permit_owners"
[ -d "$parallel_role_a/.nudox-cargo/leases/slot-0.lock" ] \
  || fail "first role graph lacks its independent lease"
[ -d "$parallel_role_b/.nudox-cargo/leases/slot-0.lock" ] \
  || fail "second role graph lacks its independent lease"
wait "$parallel_role_pid_a"
wait "$parallel_role_pid_b"
for parallel_role_root in "$parallel_role_a" "$parallel_role_b"; do
  if find "$parallel_role_root/.nudox-cargo/leases" -type d -name 'slot-*.lock' -print -quit | grep . >/dev/null; then
    fail "role graph lease remained after fake Cargo exited"
  fi
done

# Explicit roots retain no more than four role-local graphs as worktrees come
# and go. A fifth owner reuses a stamped graph under its independent lease.
pool_cache="$test_root/role-pool-cache"
pool_build_root="$test_root/role-pool-build"
pool_log="$test_root/role-pool.log"
: > "$pool_log"
pool_index=1
while [ "$pool_index" -le 5 ]; do
  pool_workspace="$test_root/roots/pool-$pool_index"
  pool_target="$test_root/pool-target-$pool_index"
  mkdir -p "$pool_workspace"
  NUDOX_TEST_WORKTREE="$pool_workspace" NUDOX_TEST_LOG="$pool_log" \
    NUDOX_BUILD_CACHE_ROOT="$pool_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
    NUDOX_CARGO_SLOT_WAIT_MS=0 CARGO_BUILD_BUILD_DIR="$pool_build_root" \
    CARGO_TARGET_DIR="$pool_target" SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
    "$test_root/wrapper" build
  pool_graph_count="$(find "$pool_build_root/.nudox-cargo" -maxdepth 1 -type d -name 'slot-*' -print | wc -l | tr -d ' ')"
  [ "$pool_graph_count" -le 4 ] || fail "role graph pool exceeded four retained graphs"
  pool_index="$((pool_index + 1))"
done
assert_file_lines "$pool_log" 5
assert_eq 4 "$(find "$pool_build_root/.nudox-cargo" -maxdepth 1 -type d -name 'slot-*' -print | wc -l | tr -d ' ')"

# An unmarked non-empty managed child is refused rather than adopted/erased.
refused_build_root="$test_root/refused-build"
refused_child="$refused_build_root/.nudox-cargo/slot-0"
mkdir -p "$refused_child"
printf 'unowned content\n' > "$refused_child/keep.txt"
before_lines="$(wc -l < "$explicit_log" | tr -d ' ')"
if NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$explicit_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/cache-explicit" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_BUILD_BUILD_DIR="$refused_build_root" CARGO_TARGET_DIR="$explicit_target_root" \
  NUDOX_CARGO_SLOT_WAIT_MS=0 SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build; then
  fail "unmarked explicit graph was adopted"
fi
assert_eq "$before_lines" "$(wc -l < "$explicit_log" | tr -d ' ')"
assert_eq 'unowned content' "$(cat "$refused_child/keep.txt")"

# Caller-selected target directories with content but no ownership stamp are
# refused and preserved; only the exact standard worktree target is adopted.
unmarked_target_root="$test_root/unmarked-target"
mkdir -p "$unmarked_target_root"
printf 'caller artifact\n' > "$unmarked_target_root/keep.bin"
if NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$explicit_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/cache-explicit" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_BUILD_BUILD_DIR="$explicit_build_root" CARGO_TARGET_DIR="$unmarked_target_root" \
  NUDOX_CARGO_SLOT_WAIT_MS=0 SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build; then
  fail "unmarked custom target directory was adopted"
fi
assert_eq "$before_lines" "$(wc -l < "$explicit_log" | tr -d ' ')"
assert_eq 'caller artifact' "$(cat "$unmarked_target_root/keep.bin")"

# A contradictory target-root stamp is refused with all prior data retained.
conflicted_target_root="$test_root/conflicted-target"
mkdir -p "$conflicted_target_root"
printf '%s\n' "$test_root/roots/b" > "$conflicted_target_root/.nudox-worktree-root"
printf 'keep target\n' > "$conflicted_target_root/keep.txt"
if NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$explicit_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/cache-explicit" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_BUILD_BUILD_DIR="$explicit_build_root" CARGO_TARGET_DIR="$conflicted_target_root" \
  NUDOX_CARGO_SLOT_WAIT_MS=0 SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build; then
  fail "target namespace with another worktree stamp was accepted"
fi
assert_eq "$before_lines" "$(wc -l < "$explicit_log" | tr -d ' ')"
assert_eq 'keep target' "$(cat "$conflicted_target_root/keep.txt")"

if find "$test_root/cache-explicit/locks" -mindepth 1 -print -quit 2>/dev/null | grep . >/dev/null; then
  fail "explicit build-dir leaked a capacity lease"
fi
if find "$test_root/cache-explicit/affinity" -type f -print -quit 2>/dev/null | grep . >/dev/null; then
  fail "explicit build-dir changed default graph affinity"
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
signal_target="$test_root/signal-target"
signal_build="$test_root/signal-build"
signal_child_pid="$test_root/signal-child.pid"
signal_child_done="$test_root/signal-child.done"
signal_sleep="${NUDOX_TEST_SIGNAL_SLEEP:-1}"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$signal_log" \
  NUDOX_BUILD_CACHE_ROOT="$signal_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP="$signal_sleep" \
  NUDOX_TEST_CHILD_PID_FILE="$signal_child_pid" NUDOX_TEST_CHILD_DONE_FILE="$signal_child_done" \
  CARGO_BUILD_BUILD_DIR="$signal_build" CARGO_TARGET_DIR="$signal_target" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build &
signal_pid="$!"
signal_waited=0
while [ "$signal_waited" -lt 80 ] && [ ! -f "$signal_child_pid" ]; do
  sleep 0.05
  signal_waited="$((signal_waited + 1))"
done
kill -TERM "$signal_pid"
signal_observed=0
signal_waited=0
while [ "$signal_waited" -lt 80 ] && [ ! -f "$signal_child_done" ]; do
  if ! find "$signal_cache/locks" -type d -name 'worktree-*.lock' -print -quit 2>/dev/null | grep . >/dev/null; then
    fail "cancelled compile released the lease before fake Cargo acknowledged TERM"
  fi
  if ! find "$signal_build/.nudox-cargo/leases" -type d -name 'slot-*.lock' -print -quit 2>/dev/null | grep . >/dev/null; then
    fail "cancelled compile released the role-graph lease before fake Cargo acknowledged TERM"
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
if find "$signal_build/.nudox-cargo/leases" -type d -name 'slot-*.lock' -print -quit 2>/dev/null | grep . >/dev/null; then
  fail "cancelled compile left its role-graph lease behind"
fi
[ -f "$signal_child_done" ] || fail "cancelled compile did not reap fake Cargo"
signal_child="$(cat "$signal_child_pid")"
if kill -0 "$signal_child" 2>/dev/null; then
  fail "cancelled compile left the Cargo child alive"
fi
signal_manifest="$(find "$signal_target/.nudox-provenance" -maxdepth 1 -type f -name '*.json' -print | head -n 1)"
[ -n "$signal_manifest" ] || fail "cancelled compile did not finalize provenance"
python3 - "$signal_manifest" <<'PY'
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert value["cargo_exit_status"] == 143
PY

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
ceiling_waited=0
while [ "$ceiling_waited" -lt 80 ] && [ ! -f "$ceiling_log" ]; do
  sleep 0.05
  ceiling_waited="$((ceiling_waited + 1))"
done
[ -f "$ceiling_log" ] || fail "leased compiler did not reach fake Cargo"
if NUDOX_TEST_WORKTREE="$test_root/roots/b" NUDOX_TEST_LOG="$ceiling_log" \
  NUDOX_BUILD_CACHE_ROOT="$ceiling_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=0 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" check; then
  fail "busy build ceiling unexpectedly admitted another compiler"
fi
assert_file_lines "$ceiling_log" 1
# Named role roots obey the same ceiling. Cargo must never run and the role
# root must stay absent while another worktree owns the only slot.
if NUDOX_TEST_WORKTREE="$test_root/roots/b" NUDOX_TEST_LOG="$ceiling_log" \
  NUDOX_BUILD_CACHE_ROOT="$ceiling_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_BUILD_BUILD_DIR="$test_root/blocked-explicit-build" \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=0 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" check; then
  fail "explicit directory bypassed the busy build ceiling"
fi
assert_file_lines "$ceiling_log" 1
[ ! -d "$test_root/blocked-explicit-build" ] || fail "blocked explicit compile started Cargo"
wait "$ceiling_owner"
NUDOX_TEST_WORKTREE="$test_root/roots/b" NUDOX_TEST_LOG="$ceiling_log" \
  NUDOX_BUILD_CACHE_ROOT="$ceiling_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=0 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" check
assert_file_lines "$ceiling_log" 2
# After release the same explicit request starts normally without altering
# the default warm slot's previous owner or graph.
pooled_owner="$(cat "$ceiling_cache/affinity/slot-0.owner")"
pooled_marker="$(cat "$ceiling_cache/build/slot-0/fake-public-api.rmeta")"
NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$ceiling_log" \
  NUDOX_BUILD_CACHE_ROOT="$ceiling_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_BUILD_BUILD_DIR="$test_root/blocked-explicit-build" \
  NUDOX_CARGO_SLOT_WAIT_MS=0 NUDOX_TEST_CARGO_SLEEP=0 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" "$test_root/wrapper" check
assert_file_lines "$ceiling_log" 3
assert_eq "$pooled_owner" "$(cat "$ceiling_cache/affinity/slot-0.owner")"
assert_eq "$pooled_marker" "$(cat "$ceiling_cache/build/slot-0/fake-public-api.rmeta")"
assert_eq "$test_root/blocked-explicit-build/.nudox-cargo/slot-0" "$(tail -n 1 "$ceiling_log" | cut -d '|' -f 2)"
if find "$ceiling_cache/build" -maxdepth 1 -type d -name 'overflow-*' -print -quit 2>/dev/null | grep . >/dev/null; then
  fail "hard build ceiling created an overflow directory"
fi

# A Cargo wrapper launched outside the Nix shell must not let an ambient
# Homebrew Rustc shadow the toolchain selected when the wrapper was built.
# Cargo receives exact absolute RUSTC/RUSTDOC paths, and provenance records the
# same RUSTC executable Cargo receives. An explicit RUSTC override remains
# exact and is reflected in provenance; an explicit RUSTDOC is passed through.
ambient_tools="$test_root/ambient-tools"
mkdir -p "$ambient_tools"
printf '%s\n' '#!/bin/sh' \
  'if [ "${1:-}" = --version ]; then printf "rustc 1.98.1-homebrew-test\\n"; exit 0; fi' \
  'if [ -n "${RUSTC_TEST_LOG:-}" ]; then printf "ambient:%s\\n" "$*" >> "$RUSTC_TEST_LOG"; fi' \
  'exit 0' > "$ambient_tools/rustc"
printf '%s\n' '#!/bin/sh' \
  'if [ "${1:-}" = --version ]; then printf "rustdoc 1.98.1-homebrew-test\\n"; exit 0; fi' \
  'exit 0' > "$ambient_tools/rustdoc"
chmod +x "$ambient_tools/rustc" "$ambient_tools/rustdoc"
ambient_shadow_root="$test_root/roots/ambient-rust-shadow"
ambient_shadow_log="$test_root/ambient-rust-shadow.log"
ambient_shadow_toolchain_log="$test_root/ambient-rust-shadow-tools.log"
ambient_shadow_rustc_log="$test_root/ambient-rust-shadow-rustc.log"
PATH="$ambient_tools:$test_root/bin:$PATH" \
  NUDOX_TEST_WORKTREE="$ambient_shadow_root" NUDOX_TEST_LOG="$ambient_shadow_log" \
  NUDOX_TEST_TOOLCHAIN_LOG="$ambient_shadow_toolchain_log" \
  RUSTC_TEST_LOG="$ambient_shadow_rustc_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/ambient-rust-shadow-cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build --locked
assert_eq "$test_root/bin/rustc|$test_root/bin/rustdoc" "$(cat "$ambient_shadow_toolchain_log")"
case "$(cat "$ambient_shadow_rustc_log")" in
  selected:--version\ --verbose) ;;
  *) fail "Cargo provenance queried the ambient Rustc instead of the selected Rustc" ;;
esac
ambient_shadow_manifest="$(find "$ambient_shadow_root/.local/target/.nudox-provenance" -maxdepth 1 -type f -name '*.json' -print | head -n 1)"
[ -n "$ambient_shadow_manifest" ] || fail "ambient-shadow provenance manifest was not emitted"
python3 - "$ambient_shadow_manifest" "$test_root/bin/rustc" <<'PY'
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert value["toolchain"]["rustc_path"] == sys.argv[2]
assert value["toolchain"]["rustc"].startswith("rustc 1.97.1-test")
PY

explicit_tool_root="$test_root/roots/explicit-rust-overrides"
explicit_tool_log="$test_root/explicit-rust-overrides.log"
explicit_toolchain_log="$test_root/explicit-rust-overrides-tools.log"
NUDOX_TEST_WORKTREE="$explicit_tool_root" NUDOX_TEST_LOG="$explicit_tool_log" \
  NUDOX_TEST_TOOLCHAIN_LOG="$explicit_toolchain_log" \
  NUDOX_BUILD_CACHE_ROOT="$test_root/explicit-rust-overrides-cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  RUSTC="$ambient_tools/rustc" RUSTDOC="$ambient_tools/rustdoc" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" check --offline
assert_eq "$ambient_tools/rustc|$ambient_tools/rustdoc" "$(cat "$explicit_toolchain_log")"
explicit_tool_manifest="$(find "$explicit_tool_root/.local/target/.nudox-provenance" -maxdepth 1 -type f -name '*.json' -print | head -n 1)"
[ -n "$explicit_tool_manifest" ] || fail "explicit-tool provenance manifest was not emitted"
python3 - "$explicit_tool_manifest" "$ambient_tools/rustc" <<'PY'
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert value["toolchain"]["rustc_path"] == sys.argv[2]
assert value["toolchain"]["rustc"].startswith("rustc 1.98.1-homebrew-test")
PY

# Provenance is emitted for a failed Cargo invocation too, while the original
# arguments and exact Cargo exit status remain intact. The executable output
# hash, source/lock identity, toolchain, wrapper and feature selection are all
# tied to the same namespaced paths.
provenance_root="$test_root/roots/a"
provenance_build_root="$test_root/provenance-build"
provenance_target_root="$test_root/provenance-target"
provenance_log="$test_root/provenance.log"
printf 'test lockfile\n' > "$provenance_root/Cargo.lock"
if NUDOX_TEST_WORKTREE="$provenance_root" NUDOX_TEST_LOG="$provenance_log" \
  NUDOX_TEST_CREATE_OUTPUT=1 NUDOX_TEST_CARGO_STATUS=17 \
  NUDOX_BUILD_CACHE_ROOT="$test_root/provenance-cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_BUILD_BUILD_DIR="$provenance_build_root" CARGO_TARGET_DIR="$provenance_target_root" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build --locked --features smoke; then
  fail "fake Cargo failure was not propagated"
else
  cargo_status="$?"
fi
assert_eq 17 "$cargo_status"
assert_eq 'build --locked --features smoke' "$(cut -d '|' -f 4 "$provenance_log")"
provenance_target="$provenance_target_root"
provenance_manifest="$(find "$provenance_target/.nudox-provenance" -maxdepth 1 -type f -name '*.json' -print | head -n 1)"
[ -n "$provenance_manifest" ] || fail "build provenance manifest was not emitted"
python3 - "$provenance_manifest" "$provenance_root" \
  "$provenance_build_root/.nudox-cargo/slot-0" "$provenance_target" <<'PY'
import hashlib
import json
import pathlib
import sys

manifest_path, root, build_dir, target_dir = map(pathlib.Path, sys.argv[1:])
value = json.loads(manifest_path.read_text())
assert value["workspace_root"] == str(root)
assert value["git_head"] == "0123456789abcdef0123456789abcdef01234567"
assert value["source_dirty_sha256"] == value["source_dirty_sha256_after"]
assert len(value["source_dirty_sha256"]) == 64
assert value["cargo_lock_sha256"] == hashlib.sha256((root / "Cargo.lock").read_bytes()).hexdigest()
assert value["cargo_lock_sha256_after"] == value["cargo_lock_sha256"]
assert value["cargo_build_dir"] == str(build_dir)
assert value["cargo_target_dir"] == str(target_dir)
assert value["features"]["features"] == ["smoke"]
assert value["cargo_exit_status"] == 17
assert value["toolchain"]["cargo"] == "cargo 1.97.1-test"
assert value["toolchain"]["rustc"].startswith("rustc 1.97.1-test")
assert value["toolchain"]["rustdoc"] == "rustdoc 1.97.1-test"
assert value["toolchain"]["cargo_path"].endswith("/bin/cargo")
assert value["toolchain"]["rustc_path"].endswith("/bin/rustc")
assert value["toolchain"]["rustdoc_path"].endswith("/bin/rustdoc")
assert value["toolchain"]["capture_complete"] is True
assert value["toolchain"]["changed_during_build"] is False
for executable in ("cargo", "rustc", "rustdoc"):
    before = value["toolchain"]["executables_before"][executable]
    after = value["toolchain"]["executables_after"][executable]
    assert len(before["sha256"]) == 64
    assert before["sha256"] == after["sha256"]
    assert before["resolved_path"] == after["resolved_path"]
assert len(value["wrapper"]["runtime_sha256"]) == 64
assert len(value["wrapper"]["source_sha256"]) == 64
assert value["wrapper"]["rustc_path"].endswith("rustc-cache-wrapper")
assert len(value["wrapper"]["rustc_sha256"]) == 64
outputs = {item["path"]: item["sha256"] for item in value["outputs"]}
assert outputs["debug/fake-bin"] == hashlib.sha256(b"test executable\n").hexdigest()
PY

# Missing tool bytes/version data is an explicit capture refusal, not a
# successful-looking manifest with null compiler identities.
incomplete_build_root="$test_root/incomplete-build"
incomplete_target_root="$test_root/incomplete-target"
mkdir -p "$incomplete_build_root" "$incomplete_target_root"
if python3 "$repo_root/.config/scripts/cargo-provenance.py" begin \
  "$provenance_root" "$incomplete_build_root" "$incomplete_target_root" \
  "$test_root/wrapper" "$repo_root/.config/scripts/cargo-shared-cache.sh" \
  "$test_root/bin/cargo" "$test_root/bin/rustc" "$test_root/bin/missing-rustdoc" \
  "$test_root/bin/sccache" build 2> "$test_root/incomplete.stderr"; then
  fail "incomplete executable identity was accepted"
else
  incomplete_status="$?"
fi
assert_eq 78 "$incomplete_status"
if ! grep -q 'incomplete tool identity' "$test_root/incomplete.stderr"; then
  fail "incomplete tool identity failure was not explained"
fi
if find "$incomplete_build_root" -name '.nudox-provenance-start-*.json' -print | grep -q .; then
  fail "incomplete tool identity emitted a start record that could look complete"
fi

# Cargo can finish successfully after a selected compiler is replaced. Keep
# an after-hash manifest, but refuse the build result with a distinct status.
mutation_root="$test_root/mutation-root"
mutation_build_root="$test_root/mutation-build"
mutation_target_root="$test_root/mutation-target"
mutation_log="$test_root/mutation.log"
mkdir -p "$mutation_root"
printf 'test lockfile\n' > "$mutation_root/Cargo.lock"
cp -p "$test_root/bin/rustc" "$test_root/bin/rustc.before-mutation-test"
if NUDOX_TEST_WORKTREE="$mutation_root" NUDOX_TEST_LOG="$mutation_log" \
  NUDOX_TEST_MUTATE_RUSTC="$test_root/bin/rustc" NUDOX_TEST_CARGO_STATUS=0 \
  NUDOX_BUILD_CACHE_ROOT="$test_root/mutation-cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_BUILD_BUILD_DIR="$mutation_build_root" CARGO_TARGET_DIR="$mutation_target_root" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build --locked; then
  fail "Cargo result was accepted after selected rustc changed"
else
  mutation_status="$?"
fi
assert_eq 74 "$mutation_status"
mutation_manifest="$(find "$mutation_target_root/.nudox-provenance" -maxdepth 1 -type f -name '*.json' -print | head -n 1)"
[ -n "$mutation_manifest" ] || fail "changed-tool provenance manifest was not emitted"
python3 - "$mutation_manifest" <<'PY'
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text())
toolchain = value["toolchain"]
assert toolchain["capture_complete"] is True
assert toolchain["changed_during_build"] is True
assert toolchain["changed_executables"] == ["rustc"]
assert toolchain["executables_before"]["rustc"]["sha256"] != toolchain["executables_after"]["rustc"]["sha256"]
PY
mv "$test_root/bin/rustc.before-mutation-test" "$test_root/bin/rustc"

# A successful compiler process is still refused if its after-build evidence
# cannot be persisted; retain the start record for recovery/inspection.
finalize_root="$test_root/finalize-root"
finalize_build_root="$test_root/finalize-build"
finalize_target_root="$test_root/finalize-target"
finalize_log="$test_root/finalize.log"
mkdir -p "$finalize_root"
printf 'test lockfile\n' > "$finalize_root/Cargo.lock"
if NUDOX_TEST_WORKTREE="$finalize_root" NUDOX_TEST_LOG="$finalize_log" \
  NUDOX_TEST_BREAK_PROVENANCE=1 NUDOX_TEST_CARGO_STATUS=0 \
  NUDOX_BUILD_CACHE_ROOT="$test_root/finalize-cache" NUDOX_CARGO_BUILD_SLOTS=1 \
  CARGO_BUILD_BUILD_DIR="$finalize_build_root" CARGO_TARGET_DIR="$finalize_target_root" \
  SCCACHE_SERVER_UDS="$test_root/sccache.sock" \
  "$test_root/wrapper" build --locked 2> "$test_root/finalize.stderr"; then
  fail "successful Cargo exit was accepted without a final provenance manifest"
else
  finalize_status="$?"
fi
assert_eq 74 "$finalize_status"
if ! grep -q 'refusing Cargo result; exact provenance finalization failed' "$test_root/finalize.stderr"; then
  fail "provenance write failure did not refuse the Cargo result"
fi
if ! find "$finalize_build_root" -name '.nudox-provenance-start-*.json' -print | grep -q .; then
  fail "provenance start record was not retained after finalization failure"
fi

# Leave the obsolete daemon socket alive. Each pinned client must select its
# own protocol endpoint, not adopt that socket or retire another build's daemon.
endpoint_log="$test_root/cache-endpoints.log"
for provider in a b; do
  case "$provider" in
    a) protocol_wrapper="$test_root/wrapper" ;;
    b) protocol_wrapper="$test_root/wrapper-v2" ;;
  esac
  NUDOX_TEST_WORKTREE="$test_root/roots/a" NUDOX_TEST_LOG="$test_root/protocol-cargo.log" \
    NUDOX_TEST_CACHE_ENDPOINT_LOG="$endpoint_log" \
    NUDOX_BUILD_CACHE_ROOT="$protocol_cache" NUDOX_CARGO_BUILD_SLOTS=1 \
    CARGO_TARGET_DIR="$test_root/protocol-target-$provider" \
    CARGO_BUILD_BUILD_DIR="$test_root/protocol-build-$provider" \
    SCCACHE_SERVER_UDS="" SCCACHE_DIR="" \
    "$protocol_wrapper" build --locked
done
assert_file_lines "$endpoint_log" 2
assert_eq "$protocol_socket_a|$protocol_cache/sccache" "$(sed -n '1p' "$endpoint_log")"
assert_eq "$protocol_socket_b|$protocol_cache/sccache" "$(sed -n '2p' "$endpoint_log")"
[ "$protocol_socket_a" != "$protocol_socket_b" ] || fail "distinct providers share a daemon protocol socket"
[ -S "$protocol_cache/sccache.sock" ] || fail "the legacy daemon socket was changed"

echo "cargo-shared-cache: PASS (affinity, role-graph leases, isolation, hard ceiling, stamps, provenance, daemon protocol isolation, exit propagation, stale recovery)"

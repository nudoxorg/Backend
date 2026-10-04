#!/bin/bash
set -euo pipefail
umask 077

COMMIT=9a896bf197aed9028e9dbf14f0532eb00daad077
TREE=19146a7b96b3dda49d856ba777dc3794a7e20695
LOCK_SHA=5f997f548c3f47161dd044b441381936a51cb3db35d7ae699c46473769d5467f
MANIFEST_SHA=534314ceba4c04932855a22c3cca406f03ba46eeebd189ff38fbcecab44b23cd
DELTA_SHA=78c35b5b7d1c992bdb507d8d4dafcbadf537eaa1cbc7c18f938a873d4e85cdba
VERIFIER_SHA=0d1ef016a560185374a204b122cee62d70fe69b9c297340fbea4174a638e8cfc
SHELL_DIR=/nix/store/in1nscrrm6dc2pkvz5lf6zb69h77x3rf-nudox-luna-tools-shell
PYTHON_BIN=/nix/store/llk2h8rxqzv7zh53bi413ffibjrxskxw-python3-3.14.6/bin/python3.14
PYTHON_VERSION=3.14.6
PYTHON_SHA256=4f3c8b2a70cc7967e1ce900871443300357d017c1c67dfcca74bfe3e9bb6c8ad
PYTHON_REALPATH=/nix/store/llk2h8rxqzv7zh53bi413ffibjrxskxw-python3-3.14.6/bin/python3.14
RELEASE="$HOME/.local/share/nudox/source-releases/$COMMIT"
SOURCE="$RELEASE/source"
RECEIPT="$HOME/.local/share/nudox/build-receipts/validation-$COMMIT"
SHARED_CARGO_HOME="$HOME/.cache/nudox/cargo-1.97"
CARGO_HOME="$RECEIPT/cargo-home"
CARGO_TARGET_DIR="$RECEIPT/cargo-target"
CARGO_BUILD_BUILD_DIR="$RECEIPT/cargo-build-graph"
NUDOX_BUILD_CACHE_ROOT="$SHARED_CARGO_HOME"
SCCACHE_DIR="$SHARED_CARGO_HOME/sccache"
SCCACHE_SERVER_UDS="/tmp/nudox-sccache-$(id -u)/validation-13d9919/s.sock"
SOCKET_DIR="${SCCACHE_SERVER_UDS%/*}"

check_private_dir() {
  [ ! -L "$1" ] && [ -d "$1" ] &&
    [ "$("$SHELL_DIR/bin/stat" -c %u "$1")" = "$(id -u)" ] &&
    [ "$("$SHELL_DIR/bin/stat" -c %a "$1")" = 700 ]
}
check_regular_file() {
  [ ! -L "$1" ] && [ -f "$1" ]
}
verify_python_runtime() {
  [ -x "$PYTHON_BIN" ] || { echo 'approved Nix Python is unavailable' >&2; return 73; }
  PYTHON_RUNTIME_SHA256="$("$SHELL_DIR/bin/sha256sum" "$PYTHON_BIN" | awk '{print $1}')" || return 73
  [ "$PYTHON_RUNTIME_SHA256" = "$PYTHON_SHA256" ] || { echo 'approved Nix Python hash mismatch' >&2; return 73; }
  PYTHON_RUNTIME_VERSION="$("$PYTHON_BIN" --version 2>&1)" || return 73
  [ "$PYTHON_RUNTIME_VERSION" = "Python $PYTHON_VERSION" ] || { echo 'approved Nix Python version mismatch' >&2; return 73; }
  PYTHON_RUNTIME_INFO="$("$PYTHON_BIN" -c 'import os,sys; print(sys.executable); print(os.path.realpath(sys.executable)); print(".".join(map(str,sys.version_info[:3])))')" || return 73
  PYTHON_RUNTIME_EXPECTED="$(printf '%s\n%s\n%s' "$PYTHON_BIN" "$PYTHON_REALPATH" "$PYTHON_VERSION")"
  [ "$PYTHON_RUNTIME_INFO" = "$PYTHON_RUNTIME_EXPECTED" ] || { echo 'approved Nix Python executable identity mismatch' >&2; return 73; }
}
verify_python_runtime || exit $?
check_private_dir "$RELEASE"
check_private_dir "$RELEASE/receipt"
check_private_dir "$RECEIPT"
check_private_dir "$CARGO_HOME"
check_private_dir "$CARGO_TARGET_DIR"
check_private_dir "$CARGO_BUILD_BUILD_DIR"
check_private_dir "$SCCACHE_DIR"
check_private_dir "$SOCKET_DIR"
[ -S "$SCCACHE_SERVER_UDS" ]
[ ! -L "$SCCACHE_SERVER_UDS" ]
[ "$("$SHELL_DIR/bin/stat" -c %u "$SCCACHE_SERVER_UDS")" = "$(id -u)" ]
check_regular_file "$SOURCE/Cargo.lock"
check_regular_file "$RELEASE/receipt/source-verification.json"
check_regular_file "$RELEASE/receipt/staging-result.json"
[ ! -e "$SOURCE/.git" ] && [ ! -L "$SOURCE/.git" ]
[ "$("$SHELL_DIR/bin/sha256sum" "$SOURCE/Cargo.lock" | awk '{print $1}')" = "$LOCK_SHA" ]
[ "$("$SHELL_DIR/bin/sha256sum" "$RELEASE/receipt/source-verification.json" | awk '{print $1}')" = "$MANIFEST_SHA" ]

export PATH="$SHELL_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export CARGO_HOME CARGO_TARGET_DIR CARGO_BUILD_BUILD_DIR NUDOX_BUILD_CACHE_ROOT
export SCCACHE_DIR SCCACHE_SERVER_UDS SCCACHE_CACHE_SIZE=10G SCCACHE_CLIENT_SIDE=1
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 NUDOX_CARGO_BUILD_SLOTS=1
export NUDOX_CARGO_CACHE_VERBOSE=1 MACOSX_DEPLOYMENT_TARGET=26.0
export CC="$SHELL_DIR/bin/clang" CXX="$SHELL_DIR/bin/clang++" CC_ENABLE_DEBUG_OUTPUT=1
unset SDKROOT
if [ "$(ulimit -Hn)" -lt 8192 ]; then
  echo 'hard nofile limit is below 8192; refusing to run Cargo' >&2
  exit 73
fi
ulimit -Sn 8192

case "${1:-}" in
  cargo)
    shift
    if [ "$#" -ne 9 ] || [ "$1" != test ] || [ "$2" != --locked ] || [ "$3" != --offline ] || \
       [ "$4" != -j1 ] || [ "$5" != -p ] || \
       { [ "$6" != backend-library ] && [ "$6" != backend-client ]; } || \
       [ "$7" != --lib ] || [ "$8" != -- ] || [ "$9" != --test-threads=2 ]; then
      echo 'refusing command outside the reviewed library/client test sequence' >&2
      exit 64
    fi
    STAGE_ID="${NUDOX_CARGO_STAGE:-}"
    case "$STAGE_ID" in ''|*[!A-Za-z0-9._-]*) echo 'NUDOX_CARGO_STAGE must be a safe nonempty attempt ID' >&2; exit 64 ;; esac
    OWNER_DIR="$RECEIPT/attempts/$STAGE_ID"
    check_private_dir "$OWNER_DIR"
    OWNER_RECORD="$OWNER_DIR/current-cargo-owner.txt"
    if [ -e "$OWNER_RECORD" ] || [ -L "$OWNER_RECORD" ]; then
      echo 'refusing existing Cargo owner record' >&2
      exit 73
    fi
    PYTHON_RECEIPT="$OWNER_DIR/python-runtime.txt"
    if [ -e "$PYTHON_RECEIPT" ] || [ -L "$PYTHON_RECEIPT" ]; then
      echo 'refusing existing Python runtime receipt' >&2
      exit 73
    fi
    ( set -o noclobber; {
        printf 'python_path=%s\npython_realpath=%s\npython_version=%s\npython_sha256=%s\n' \
          "$PYTHON_BIN" "$PYTHON_REALPATH" "$PYTHON_VERSION" "$PYTHON_RUNTIME_SHA256"
        printf 'python_version_output=%s\n' "$PYTHON_RUNTIME_VERSION"
      } > "$PYTHON_RECEIPT" ) || { echo 'could not create exclusive Python runtime receipt' >&2; exit 73; }
    chmod 600 "$PYTHON_RECEIPT"
    check_regular_file "$RELEASE/receipt/verify_source_manifest.py"
    [ "$("$SHELL_DIR/bin/sha256sum" "$RELEASE/receipt/verify_source_manifest.py" | awk '{print $1}')" = "$VERIFIER_SHA" ] || { echo 'refusing changed source verifier' >&2; exit 73; }
    "$PYTHON_BIN" "$RELEASE/receipt/verify_source_manifest.py" \
      --source "$SOURCE" --manifest "$RELEASE/receipt/source-verification.json" --manifest-kind final
    cd "$SOURCE"
    {
      printf 'stage=%s\npid=%s\n' "$STAGE_ID" "$$"
      LC_ALL=C ps -p "$$" -o lstart= | awk '{$1=$1; print "start=" $0}'
      printf 'source=%s\nsource_commit=%s\nsource_tree=%s\n' "$SOURCE" "$COMMIT" "$TREE"
      printf 'source_manifest_sha256=%s\ndelta_sha256=%s\nlock_sha256=%s\n' "$MANIFEST_SHA" "$DELTA_SHA" "$LOCK_SHA"
      printf 'cargo=%s\nrustc=%s\npython=%s\npython_realpath=%s\npython_version=%s\npython_sha256=%s\n' \
        "$SHELL_DIR/bin/cargo" "$SHELL_DIR/bin/rustc" "$PYTHON_BIN" "$PYTHON_REALPATH" "$PYTHON_VERSION" "$PYTHON_RUNTIME_SHA256"
      printf 'python_runtime_receipt=%s\npython_runtime_receipt_sha256=%s\n' \
        "$PYTHON_RECEIPT" "$("$SHELL_DIR/bin/sha256sum" "$PYTHON_RECEIPT" | awk '{print $1}')"
      printf 'cargo_home=%s\ntarget_dir=%s\nbuild_dir=%s\nsccache_dir=%s\nsccache_socket=%s\n' \
        "$CARGO_HOME" "$CARGO_TARGET_DIR" "$CARGO_BUILD_BUILD_DIR" "$SCCACHE_DIR" "$SCCACHE_SERVER_UDS"
      printf 'cargo_version=%s\n' "$("$SHELL_DIR/bin/cargo" --version)"
      printf 'rustc_version=%s\n' "$("$SHELL_DIR/bin/rustc" --version --verbose | tr '\n' ';')"
      printf 'macos_deployment_target=%s\nsoft_nofile=%s\nhard_nofile=%s\n' \
        "$MACOSX_DEPLOYMENT_TARGET" "$(ulimit -Sn)" "$(ulimit -Hn)"
      printf 'argv='
      printf '%q ' "$@"
      printf '\n'
    } > "$OWNER_RECORD"
    chmod 600 "$OWNER_RECORD"
    exec "$SHELL_DIR/bin/cargo" "$@"
    ;;
  *)
    echo 'usage: run-cargo.sh cargo test --locked --offline -j1 -p {backend-library|backend-client} --lib -- --test-threads=2' >&2
    exit 64
    ;;
esac

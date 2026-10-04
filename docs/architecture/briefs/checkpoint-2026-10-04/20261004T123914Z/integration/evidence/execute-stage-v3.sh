#!/bin/bash
set -u
set -o pipefail
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
RECEIPT="$HOME/.local/share/nudox/build-receipts/validation-$COMMIT"
RELEASE="$HOME/.local/share/nudox/source-releases/$COMMIT"
SOURCE="$RELEASE/source"
MANIFEST="$RELEASE/receipt/source-verification.json"
VERIFIER="$RELEASE/receipt/verify_source_manifest.py"
ATTEMPT_ID=validation-9a896bf-attempt01
stage="${1:-}"
package="${2:-}"
case "$stage:$package" in
  library:backend-library|client:backend-client) ;;
  *) echo 'usage: execute-stage.sh {library:backend-library|client:backend-client}' >&2; exit 64 ;;
esac
check_private_dir() {
  [ ! -L "$1" ] && [ -d "$1" ] &&
    [ "$("$SHELL_DIR/bin/stat" -c %u "$1")" = "$(id -u)" ] &&
    [ "$("$SHELL_DIR/bin/stat" -c %a "$1")" = 700 ]
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
if ! check_private_dir "$RECEIPT"; then
  echo 'refusing unsafe build receipt directory' >&2
  exit 73
fi
ATTEMPTS_DIR="$RECEIPT/attempts"
if [ -L "$ATTEMPTS_DIR" ]; then
  echo 'refusing symlink attempts directory' >&2
  exit 73
elif [ ! -e "$ATTEMPTS_DIR" ]; then
  mkdir -m 700 "$ATTEMPTS_DIR" || { echo 'could not create attempts directory' >&2; exit 73; }
fi
if ! check_private_dir "$ATTEMPTS_DIR"; then
  echo 'refusing unsafe attempts directory' >&2
  exit 73
fi
if [ "$stage" = client ]; then
  library_dir="$ATTEMPTS_DIR/$ATTEMPT_ID-library"
  library_result="$library_dir/result.txt"
  library_log="$library_dir/cargo.log"
  if ! check_private_dir "$library_dir" || [ -L "$library_result" ] || [ ! -f "$library_result" ] || \
     [ -L "$library_log" ] || [ ! -f "$library_log" ] || \
     ! grep -qx 'source_commit=9a896bf197aed9028e9dbf14f0532eb00daad077' "$library_result" || \
     ! grep -qx 'source_tree=19146a7b96b3dda49d856ba777dc3794a7e20695' "$library_result" || \
     ! grep -qx 'lock_sha256=5f997f548c3f47161dd044b441381936a51cb3db35d7ae699c46473769d5467f' "$library_result" || \
     ! grep -qx 'source_manifest_sha256=534314ceba4c04932855a22c3cca406f03ba46eeebd189ff38fbcecab44b23cd' "$library_result" || \
     ! grep -qx 'cargo_exit=0' "$library_result" || \
     ! grep -qx 'source_verification_after_exit=0' "$library_result" || \
     ! grep -qx "source_verifier_sha256_after=$VERIFIER_SHA" "$library_result" || \
     ! grep -qx 'gate_status=passed' "$library_result"; then
    echo 'client test is gated on an intact passing library-stage receipt' >&2
    exit 73
  fi
  expected_log_sha="$(awk -F= '$1 == "log_sha256" { print $2 }' "$library_result")"
  if [ -z "$expected_log_sha" ] || [ "$("$SHELL_DIR/bin/sha256sum" "$library_log" | awk '{print $1}')" != "$expected_log_sha" ]; then
    echo 'library-stage raw log does not match its receipt' >&2
    exit 73
  fi
fi
OWNER_ID="$ATTEMPT_ID-$stage"
OWNER_DIR="$ATTEMPTS_DIR/$OWNER_ID"
if [ -e "$OWNER_DIR" ] || [ -L "$OWNER_DIR" ]; then
  echo 'refusing existing attempt directory' >&2
  exit 73
fi
mkdir -m 700 "$OWNER_DIR" || { echo 'could not create attempt directory' >&2; exit 73; }
if ! check_private_dir "$OWNER_DIR"; then
  echo 'refusing unsafe attempt directory' >&2
  exit 73
fi
LOG="$OWNER_DIR/cargo.log"
RESULT="$OWNER_DIR/result.txt"
VERIFY_LOG="$OWNER_DIR/source-verification-after.txt"
if [ -e "$LOG" ] || [ -L "$LOG" ] || [ -e "$RESULT" ] || [ -L "$RESULT" ] || \
   [ -e "$OWNER_DIR/current-cargo-owner.txt" ] || [ -L "$OWNER_DIR/current-cargo-owner.txt" ] || \
   [ -e "$VERIFY_LOG" ] || [ -L "$VERIFY_LOG" ]; then
  echo 'refusing existing attempt output' >&2
  exit 73
fi
for stale in "$RESULT".tmp.*; do
  if [ -e "$stale" ] || [ -L "$stale" ]; then
    echo 'refusing existing temporary result evidence' >&2
    exit 73
  fi
done
if [ -L "$VERIFIER" ] || [ ! -f "$VERIFIER" ] || \
   [ "$("$SHELL_DIR/bin/sha256sum" "$VERIFIER" | awk '{print $1}')" != "$VERIFIER_SHA" ]; then
  echo 'refusing changed or missing source verifier' >&2
  exit 73
fi
verify_python_runtime || { echo 'refusing Cargo stage because approved Nix Python validation failed' >&2; exit 73; }
start_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
set +e
NUDOX_CARGO_STAGE="$OWNER_ID" bash "$RECEIPT/run-cargo.sh" cargo test --locked --offline -j1 \
  -p "$package" --lib -- --test-threads=2 > "$LOG" 2>&1
cargo_rc=$?
cargo_completed_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
PYTHON_RECEIPT="$OWNER_DIR/python-runtime.txt"
if [ -L "$PYTHON_RECEIPT" ] || [ ! -f "$PYTHON_RECEIPT" ] || \
   [ "$("$SHELL_DIR/bin/stat" -c %u "$PYTHON_RECEIPT" 2>/dev/null)" != "$(id -u)" ] || \
   [ "$("$SHELL_DIR/bin/stat" -c %a "$PYTHON_RECEIPT" 2>/dev/null)" != 600 ] || \
   ! grep -qx "python_path=$PYTHON_BIN" "$PYTHON_RECEIPT" || \
   ! grep -qx "python_realpath=$PYTHON_REALPATH" "$PYTHON_RECEIPT" || \
   ! grep -qx "python_version=$PYTHON_VERSION" "$PYTHON_RECEIPT" || \
   ! grep -qx "python_sha256=$PYTHON_SHA256" "$PYTHON_RECEIPT" || \
   ! verify_python_runtime; then
  printf 'VERIFY_FAILED: approved Python runtime receipt missing or runtime identity changed after Cargo\n' > "$VERIFY_LOG"
  verify_rc=73
elif [ -L "$VERIFIER" ] || [ ! -f "$VERIFIER" ] || \
     [ "$("$SHELL_DIR/bin/sha256sum" "$VERIFIER" | awk '{print $1}')" != "$VERIFIER_SHA" ]; then
  printf 'VERIFY_FAILED: verifier missing, symlinked or changed after Cargo\n' > "$VERIFY_LOG"
  verify_rc=73
else
  "$PYTHON_BIN" "$VERIFIER" --source "$SOURCE" --manifest "$MANIFEST" \
    --manifest-kind final > "$VERIFY_LOG" 2>&1
  verify_rc=$?
fi
verifier_sha_after="unavailable"
if [ -f "$VERIFIER" ] && [ ! -L "$VERIFIER" ]; then
  verifier_sha_after="$("$SHELL_DIR/bin/sha256sum" "$VERIFIER" | awk '{print $1}')"
fi
manifest_sha_after="unavailable"
if [ -f "$MANIFEST" ] && [ ! -L "$MANIFEST" ]; then
  manifest_sha_after="$("$SHELL_DIR/bin/sha256sum" "$MANIFEST" | awk '{print $1}')"
fi
completed_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
set -e
tmp="$RESULT.tmp.$$"
if [ -e "$tmp" ] || [ -L "$tmp" ] || [ -e "$RESULT" ] || [ -L "$RESULT" ]; then
  echo 'refusing preexisting result path' >&2
  exit 73
fi
{
  printf 'stage=%s\npackage=%s\nattempt_id=%s\nsource_commit=%s\nsource_tree=%s\n' \
    "$stage" "$package" "$ATTEMPT_ID" "$COMMIT" "$TREE"
  printf 'lock_sha256=%s\nsource_manifest_sha256=%s\ndelta_sha256=%s\n' "$LOCK_SHA" "$MANIFEST_SHA" "$DELTA_SHA"
  printf 'started_utc=%s\ncargo_completed_utc=%s\ncompleted_utc=%s\ncargo_exit=%s\nsource_verification_after_exit=%s\n' \
    "$start_utc" "$cargo_completed_utc" "$completed_utc" "$cargo_rc" "$verify_rc"
  printf 'python_path=%s\npython_realpath=%s\npython_version=%s\npython_sha256=%s\n' \
    "$PYTHON_BIN" "$PYTHON_REALPATH" "$PYTHON_VERSION" "$PYTHON_SHA256"
  printf 'python_runtime_receipt_path=%s\n' "$PYTHON_RECEIPT"
  if [ -f "$PYTHON_RECEIPT" ] && [ ! -L "$PYTHON_RECEIPT" ]; then
    printf 'python_runtime_receipt_sha256=%s\n' "$("$SHELL_DIR/bin/sha256sum" "$PYTHON_RECEIPT" | awk '{print $1}')"
  else
    printf 'python_runtime_receipt_sha256=unavailable\n'
  fi
  printf 'source_verifier_path=%s\nsource_verifier_sha256_after=%s\n' "$VERIFIER" "$verifier_sha_after"
  printf 'source_manifest_path=%s\nsource_manifest_sha256_after=%s\nsource_verification_log_sha256=%s\n' \
    "$MANIFEST" "$manifest_sha_after" "$("$SHELL_DIR/bin/sha256sum" "$VERIFY_LOG" | awk '{print $1}')"
  printf 'cargo_log_path=%s\n' "$LOG"
  printf 'command=%s\n' "cargo test --locked --offline -j1 -p $package --lib -- --test-threads=2"
  if [ -f "$OWNER_DIR/current-cargo-owner.txt" ] && [ ! -L "$OWNER_DIR/current-cargo-owner.txt" ]; then
    printf 'owner_record_sha256=%s\n' "$("$SHELL_DIR/bin/sha256sum" "$OWNER_DIR/current-cargo-owner.txt" | awk '{print $1}')"
  else
    printf 'owner_record=missing\n'
  fi
  printf 'log_sha256=%s\n' "$("$SHELL_DIR/bin/sha256sum" "$LOG" | awk '{print $1}')"
  if [ "$cargo_rc" -eq 0 ] && [ "$verify_rc" -eq 0 ] && [ "$verifier_sha_after" = "$VERIFIER_SHA" ] && [ "$manifest_sha_after" = "$MANIFEST_SHA" ]; then
    printf 'gate_status=passed\n'
    stage_rc=0
  else
    printf 'gate_status=failed_or_source_changed\n'
    if [ "$cargo_rc" -ne 0 ]; then stage_rc="$cargo_rc"; else stage_rc=74; fi
  fi
  printf 'stage_exit=%s\n' "$stage_rc"
} > "$tmp"
chmod 600 "$tmp" "$LOG"
mv "$tmp" "$RESULT"
printf 'STAGE_RESULT\n'
cat "$RESULT"
exit "$stage_rc"

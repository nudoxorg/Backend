#!/bin/sh
set -eu

root="$(CDPATH='' cd -P "$(dirname "$0")/.." && pwd -P)"
linker="$root/.config/scripts/cargo-umask-linker.sh"
config="$root/.cargo/config.toml"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/nudox-cargo-linker-umask.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

grep -F 'jobs = 2' "$config" >/dev/null
for target in \
  x86_64-unknown-linux-gnu \
  aarch64-unknown-linux-gnu \
  x86_64-apple-darwin \
  aarch64-apple-darwin
do
  grep -A1 -F "[target.$target]" "$config" | grep -F 'linker = ".config/scripts/cargo-umask-linker.sh"' >/dev/null
done

fake_bin="$test_root/fake bin"
mkdir -p "$fake_bin"
cat > "$fake_bin/cc" <<'FAKE_CC'
#!/bin/sh
set -eu
python3 - "$NUDOX_TEST_OUTPUT" "$NUDOX_TEST_ARGS" "$@" <<'PY'
import os
import sys

fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o777)
os.close(fd)
with open(sys.argv[2], "wb") as stream:
    for argument in sys.argv[3:]:
        stream.write(os.fsencode(argument) + b"\0")
PY
exit "${NUDOX_TEST_EXIT:-0}"
FAKE_CC
chmod 0755 "$fake_bin/cc"

expected_args="$test_root/expected-args"
printf '%s\0' '--output' 'path with spaces' '' '--link-arg=-fuse-ld=mold' > "$expected_args"

run_case() {
  caller_mask="$1"
  expected_mode="$2"
  (
    umask "$caller_mask"
    PATH="$fake_bin:$PATH" \
      NUDOX_TEST_OUTPUT="$test_root/linked-output" \
      NUDOX_TEST_ARGS="$test_root/args" \
      NUDOX_TEST_EXIT=0 \
      "$linker" '--output' 'path with spaces' '' '--link-arg=-fuse-ld=mold'
  )
  actual_mode="$(python3 - "$test_root/linked-output" <<'PY'
import os
import stat
import sys

print(format(stat.S_IMODE(os.stat(sys.argv[1]).st_mode), "04o"))
PY
)"
  [ "$actual_mode" = "$expected_mode" ]
  cmp "$expected_args" "$test_root/args"
  rm -f "$test_root/linked-output" "$test_root/args"
}

run_case 0002 0755
run_case 0077 0700
run_case 0000 0755

if (
  umask 0002
  PATH="$fake_bin:$PATH" \
    NUDOX_TEST_OUTPUT="$test_root/linked-output" \
    NUDOX_TEST_ARGS="$test_root/args" \
    NUDOX_TEST_EXIT=37 \
    "$linker" '--output' 'path with spaces' '' '--link-arg=-fuse-ld=mold'
); then
  printf '%s\n' 'linker wrapper discarded the child exit status' >&2
  exit 1
else
  status="$?"
  [ "$status" -eq 37 ]
fi

printf '%s\n' 'Cargo linker umask, argument, mold-flag, and exit-status controls passed'

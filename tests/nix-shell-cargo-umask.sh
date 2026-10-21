#!/bin/sh
set -eu

root="$(CDPATH='' cd -P "$(dirname "$0")/.." && pwd -P)"
hook="$root/.config/scripts/nix-cargo-output-umask.sh"
shell_config="$root/.config/nix/shells.nix"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/nudox-nix-umask.XXXXXX")"
fake_child="$test_root/fake child"
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

# Verify the same fragment is wired into the common Linux/Darwin shell hook,
# without introducing a Cargo compiler-wrapper selection override.
grep -F '. "${../scripts/nix-cargo-output-umask.sh}"' "$shell_config" >/dev/null
if grep -F 'RUSTC_WORKSPACE_WRAPPER' "$shell_config" >/dev/null; then
  echo 'Nix shell must not override Cargo workspace compiler wrappers' >&2
  exit 1
fi

cat > "$fake_child" <<'FAKE_CHILD'
#!/bin/sh
set -eu
umask > "$NUDOX_TEST_UMASK_OUTPUT"
printf '%s\n' "$NUDOX_TEST_ENV_VALUE" > "$NUDOX_TEST_ENV_OUTPUT"
printf '%s\0' "$@" > "$NUDOX_TEST_ARGS_OUTPUT"
FAKE_CHILD
chmod 0755 "$fake_child"

expected_args="$test_root/expected-args"
printf '%s\0' '--crate-name' 'backend crate' '' '--edition=2024' > "$expected_args"

run_case() {
  caller_mask="$1"
  expected_mask="$2"
  (
    umask "$caller_mask"
    . "$hook"
    NUDOX_TEST_UMASK_OUTPUT="$test_root/umask" \
      NUDOX_TEST_ENV_OUTPUT="$test_root/env" \
      NUDOX_TEST_ARGS_OUTPUT="$test_root/args" \
      NUDOX_TEST_ENV_VALUE='preserve me' \
      "$fake_child" '--crate-name' 'backend crate' '' '--edition=2024'
  )
  [ "$(cat "$test_root/umask")" = "$expected_mask" ]
  [ "$(cat "$test_root/env")" = 'preserve me' ]
  cmp "$expected_args" "$test_root/args"
}

run_case 0002 0022
run_case 0077 0077
run_case 0000 0022

printf '%s\n' 'Nix shell Cargo umask inheritance, stricter-mask, argument, and environment controls passed'

#!/bin/sh
set -eu

# Cargo invokes this as rustc's configured linker. Preserve a caller's stricter
# permissions while preventing the shared-group umask from making executables
# writable by the group or world.
_nudox_linker_caller_umask="$(umask)"
case "$_nudox_linker_caller_umask" in
  ""|*[!0-7]*)
    printf '%s\n' 'Cargo linker: could not read an octal umask' >&2
    exit 125
    ;;
esac
_nudox_linker_output_umask=$((0${_nudox_linker_caller_umask} | 0022))
umask "$(printf '%04o' "$_nudox_linker_output_umask")"
unset _nudox_linker_caller_umask _nudox_linker_output_umask

# GNU and Darwin Rust targets configured below use the compiler-driver `cc`.
# Keep every argument as its own argv entry; exec also preserves its status.
exec cc "$@"

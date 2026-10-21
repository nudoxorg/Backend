# Sourced by Linux/Darwin Cargo development shell hooks. Cargo and rustc
# inherit this process mask so newly linked product executables are not
# group/world-writable; a stricter caller mask is preserved unchanged.
_nudox_cargo_caller_umask="$(umask)"
case "$_nudox_cargo_caller_umask" in
  ""|*[!0-7]*)
    printf '%s\n' 'Nix Cargo shell: could not read an octal umask' >&2
    unset _nudox_cargo_caller_umask
    return 1
    ;;
esac
_nudox_cargo_output_umask=$((0${_nudox_cargo_caller_umask} | 0022))
umask "$(printf '%04o' "$_nudox_cargo_output_umask")"
unset _nudox_cargo_caller_umask _nudox_cargo_output_umask

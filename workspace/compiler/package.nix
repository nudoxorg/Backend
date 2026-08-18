# Pure-evaluation compiler package seam.
#
# Snowydeer supplies the real compiler daemon through `compilerTools`; pure
# flake evaluation still needs a deterministic executable placeholder so the
# package graph remains total.
{
  pkgs,
  compilerDaemon ? null,
  ...
}:

if compilerDaemon != null then
  compilerDaemon
else
  pkgs.writeShellScriptBin "compiler-daemon" ''
    echo "compiler-daemon requires NUDOX_COMPILER_DAEMON_PATH or snowydeer" >&2
    exit 1
  ''

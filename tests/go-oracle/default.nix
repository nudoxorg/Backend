# Compile + vet + schema-emit gate for the Go oracle.
#
# The package recipe lives next to the Go source so `nix build .#go-oracle`
# and this check share one derivation. This file only chooses the source
# snapshot: the flake tree in pure eval, `projectRoot` when set so a dirty
# oracle checkout is what gets compiled.
#
# See workspace/compiler/languages/oracle/go/package.nix for why this is
# `buildGoModule` rather than `mkNuCheck`, and for the schemaVersion check
# that is the other half of tests/go/oracle_staleness.rs.
{
  pkgs,
  projectRoot,
  goToolchain,
}:

import ../../workspace/compiler/languages/oracle/go/package.nix {
  inherit pkgs goToolchain;
  src = builtins.path {
    # Pure `nix flake check` has no PWD/PRJ_ROOT; use the flake snapshot.
    # Impure local builds overlay a dirty oracle checkout, matching the
    # other checks' extraSrc overlay.
    path =
      if projectRoot == "" then
        ../../workspace/compiler/languages/oracle/go
      else
        "${projectRoot}/workspace/compiler/languages/oracle/go";
    # Named to match the binary the producer looks up on PATH. A directory
    # named `go` collides with buildGoModule's GOPATH="$TMPDIR/go".
    name = "nudox-go-oracle";
  };
}

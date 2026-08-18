# Compile + schema-emit gate for the C# oracle.
#
# The package recipe lives next to the C# source so `nix build .#csharp-oracle`
# and this check share one derivation. Pure eval uses the flake tree;
# `projectRoot` overlays a dirty oracle checkout when set.
{
  pkgs,
  projectRoot,
  dotnet,
}:

import ../../workspace/compiler/languages/oracle/csharp/package.nix {
  inherit pkgs dotnet;
  src = builtins.path {
    path =
      if projectRoot == "" then
        ../../workspace/compiler/languages/oracle/csharp
      else
        "${projectRoot}/workspace/compiler/languages/oracle/csharp";
    name = "nudox-csharp-oracle";
    filter =
      path: type:
      let
        base = baseNameOf path;
      in
      if type == "directory" then
        !(builtins.elem base [
          "bin"
          "obj"
          "publish"
        ])
      else
        true;
  };
}

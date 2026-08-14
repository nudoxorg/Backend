/*
  Flake checks for NuDox Backend.

  Add a new integration test by:
    1. Creating tests/<name>/check.nu (+ optional default.nix)
    2. Wiring one attr below (or callPackage tests/<name>)

  Shared Nu modules live in tests/lib/ (NU_LIB_DIRS).
*/
{
  pkgs,
  fenixPackages,
  nixos,
  packages, # self.packages.${system}
  buck2,
  projectRoot,
}:

let
  helpers = import ../nix/lib.nix {
    inherit pkgs fenixPackages nixos;
  };

  inherit (helpers) mkNuCheck;

  nuLib = ./lib;

in
{
  backendImage = import ./backend-image {
    inherit (pkgs) lib;
    inherit
      pkgs
      mkNuCheck
      buck2
      projectRoot
      nuLib
      ;
    server = packages.server;
    backend = packages.backend;
    compilerDaemon = packages.compiler-daemon;
    rustToolchain = packages.rustc;
  };
}

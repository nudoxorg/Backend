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
  nixos ? null,
  packages, # self.packages.${system}
  # Optional: pre-built buck2 derivation for the compiler fallback.
  buck2,
  # Repo checkout for buck2 fallback when compiler is a placeholder.
  projectRoot ? "",
  # Optional override for compiler-daemon used by backendImage check.
  compilerDaemon ? null,
}:

let
  helpers = import ../build/nix/lib.nix {
    inherit pkgs fenixPackages nixos;
  };

  inherit (helpers) mkNuCheck;

  nuLib = ./lib;

  compilerForCheck = if compilerDaemon != null then compilerDaemon else packages.compiler-daemon;
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
    compilerDaemon = compilerForCheck;
    rustToolchain = packages.rustc;
  };
}

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
  packages, # self.packages.${system}
  buck2,
  projectRoot,
  smolvmSource,
  semanticModel,
  goToolchain,
  lindseyBundlePackaging,
  dotnet,
}:

let
  helpers = import ../nix/lib.nix {
    inherit pkgs fenixPackages;
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

  workspaceTests = import ./workspace-tests {
    inherit
      pkgs
      mkNuCheck
      nuLib
      projectRoot
      smolvmSource
      ;
    rustToolchain = packages.rustc;
    corpus = packages.corpus;
  };

  # `index` under `--features server`. Deliberately NOT folded into
  # `workspaceTests`: that check runs `--workspace` with default features, and
  # `server` is not one, so the whole serving/indexing plane — ~140 tests in 19
  # `#![cfg(feature = "server")]` files — was never built by `nix flake check`.
  # It once stopped compiling entirely while every check stayed green.
  indexTests = import ./index-tests {
    inherit
      pkgs
      mkNuCheck
      nuLib
      projectRoot
      smolvmSource
      ;
    rustToolchain = packages.rustc;
  };

  semanticTests = import ./semantic-tests {
    inherit
      pkgs
      mkNuCheck
      nuLib
      projectRoot
      semanticModel
      ;
    rustToolchain = packages.rustc;
  };

  # Compile + vet + schema-emit gate for the Go oracle. See
  # tests/go-oracle/default.nix for why this is a plain `buildGoModule`
  # derivation rather than an `mkNuCheck` instance like everything else here.
  goOracle = import ./go-oracle {
    inherit pkgs projectRoot goToolchain;
  };

  csharpOracle = import ./csharp-oracle {
    inherit pkgs projectRoot dotnet;
  };

  # cargo-bundle wrap contract: oracles, toolchains, and embed model land
  # in a packaged .app without building the GUI. See tests/lindsey-bundle.
  lindseyBundle = import ./lindsey-bundle {
    inherit
      pkgs
      mkNuCheck
      nuLib
      goToolchain
      semanticModel
      dotnet
      ;
    inherit (lindseyBundlePackaging)
      installWrapper
      macosWrapper
      envNames
      toolchainPath
      ;
    goOracle = packages.go-oracle;
    javaOracle = packages.java-oracle;
    csharpOracle = packages.csharp-oracle;
  };
}

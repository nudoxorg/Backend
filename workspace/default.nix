# Workspace package set — composed from workspace crate package.nix (+ images)
# and Buck toolchain re-exports. Imported by the root flake; not a sub-flake.
#
# Buck2 hermetic toolchains consume the same root flake package set, so there
# is no second toolchain flake to keep in sync.
{
  pkgs,
  fenixPackages,
  buildImage,
  src,
  version,
  # Snowydeer-imported compiler tools. An empty set produces the package's
  # intentional placeholder scripts during pure evaluation.
  compilerTools ? { },
}:

let
  inherit (pkgs) lib callPackage;

  helpers = import ../nix/lib.nix {
    inherit pkgs fenixPackages;
  };

  inherit (helpers)
    mkRustService
    mkServiceImage
    rustToolchain
    mkCxx
    ;

  # ── Service binaries ──────────────────────────────────────────────────────
  server = callPackage ./driver/package.nix {
    inherit mkRustService src version;
  };

  registry = callPackage ./registry/package.nix {
    inherit mkRustService src version;
  };

  compiler-daemon = callPackage ./compiler/package.nix compilerTools;

  # ── Container images (nix2container) ──────────────────────────────────────
  backend = callPackage ./driver/image.nix {
    inherit mkServiceImage buildImage server;
  };

  compilerImage = callPackage ./compiler/image.nix (
    {
      inherit mkServiceImage buildImage;
      compiler = compiler-daemon;
    }
    // lib.optionalAttrs (pkgs ? bwrap) {
      inherit (pkgs) bwrap;
    }
  );

  # ── Buck2 toolchain packages ──────────────────────────────────────────────
  rustc = rustToolchain;
  clippy = rustToolchain;
  cxx = mkCxx;
  ripgrep = pkgs.ripgrep;
in
{
  # Default = runnable server binary (nixos getExe / local `nix run`).
  default = server;

  inherit
    server
    registry
    compiler-daemon
    backend
    compilerImage
    rustc
    clippy
    cxx
    ripgrep
    ;
}

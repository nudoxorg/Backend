# Workspace package set — composed from workspace crate package.nix (+ images)
# and Buck toolchain re-exports. Imported by the root flake; not a sub-flake.
#
# Buck2 hermetic toolchains still use the thin flake at build/toolchains/nix
# (via flake.package). That sub-flake mirrors rustc/cxx/ripgrep attr names.
{
  pkgs,
  fenixPackages,
  nixos,
  buildImage,
  src,
  version,
  # Optional snowydeer-imported store paths for the compiler image path.
  # When empty (pure eval), compiler-daemon emits placeholder scripts.
  compilerDaemonPath ? null,
  producerWorkerPath ? null,
  goOraclePath ? null,
  javaOraclePath ? null,
  csharpOraclePath ? null,
}:

let
  inherit (pkgs) lib callPackage;

  helpers = import ../build/nix/lib.nix {
    inherit pkgs fenixPackages nixos;
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

  compiler-daemon = callPackage ./compiler/package.nix {
    compilerDaemon = compilerDaemonPath;
    producerWorker = producerWorkerPath;
    goOracle = goOraclePath;
    javaOracle = javaOraclePath;
    csharpOracle = csharpOraclePath;
  };

  # ── Container images (nix2container) ──────────────────────────────────────
  backend = callPackage ./driver/image.nix {
    inherit mkServiceImage buildImage server;
  };

  compilerImage = callPackage ./compiler/image.nix {
    inherit mkServiceImage buildImage;
    compiler = compiler-daemon;
    bwrap = if builtins.hasAttr "bwrap" pkgs then pkgs.bwrap else null;
  };

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

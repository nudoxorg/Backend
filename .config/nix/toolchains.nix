# Defines pinned stable and nightly Rust toolchains from one Fenix input.
# Separates shipping compilation from unstable timing and diagnostic tooling.
# Prevents ambient rustup state from changing repository results.
{
  inputs,
  pkgs,
  system,
}:
let
  stable =
    (inputs.fenix.packages.${system}.toolchainOf {
      channel = "1.97.1";
      sha256 = "sha256-A1abGIbOtcBSdrUMhDGrER3pRM1hQP4fp9gh3Y4PKc8=";
    }).withComponents
      [
        "cargo"
        "clippy"
        "rust-src"
        "rustc"
        "rustfmt"
      ];
  # Every platform the product ships on. The host toolchain above only carries
  # the host's standard library, so a platform-gated compile error (a
  # `cfg(unix)` import, a Windows-only module) is invisible to every local
  # build. `cross` is the same pinned channel plus the target standard
  # libraries, so `cargo check --target` proves each platform compiles.
  crossTargets = [
    "x86_64-pc-windows-msvc"
    "x86_64-pc-windows-gnu"
    "x86_64-unknown-linux-gnu"
    "aarch64-unknown-linux-gnu"
    "x86_64-apple-darwin"
    "aarch64-apple-darwin"
  ];
  cross = inputs.fenix.packages.${system}.combine (
    [ stable ]
    ++ map (
      target:
      (inputs.fenix.packages.${system}.targets.${target}.toolchainOf {
        channel = "1.97.1";
        sha256 = "sha256-A1abGIbOtcBSdrUMhDGrER3pRM1hQP4fp9gh3Y4PKc8=";
      }).rust-std
    ) (builtins.filter (target: target != hostTarget) crossTargets)
  );
  hostTarget =
    {
      "aarch64-darwin" = "aarch64-apple-darwin";
      "x86_64-darwin" = "x86_64-apple-darwin";
      "aarch64-linux" = "aarch64-unknown-linux-gnu";
      "x86_64-linux" = "x86_64-unknown-linux-gnu";
    }
    .${system};
  nightly = inputs.fenix.packages.${system}.latest.withComponents [
    "cargo"
    "clippy"
    "llvm-tools"
    "rust-src"
    "rustc"
    "rustc-dev"
    "rustfmt"
  ];
in
{
  inherit
    stable
    nightly
    cross
    crossTargets
    ;
  stableCargo = "${stable}/bin/cargo";
  nightlyCargo = "${nightly}/bin/cargo";
  dylintToolchain = "nightly-${system}";
  rustfmt = "${stable}/bin/rustfmt";
  clang = pkgs.clang;
}

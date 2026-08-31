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
  nightly = inputs.fenix.packages.${system}.latest.withComponents [
    "cargo"
    "rustc"
  ];
in
{
  inherit stable nightly;
  stableCargo = "${stable}/bin/cargo";
  nightlyCargo = "${nightly}/bin/cargo";
  rustfmt = "${stable}/bin/rustfmt";
  clang = pkgs.clang;
}

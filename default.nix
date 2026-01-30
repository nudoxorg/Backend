{
  system ? builtins.currentSystem,
  # Fenix for nightly
  fenix ? import (fetchTarball "https://github.com/nix-community/fenix/archive/main.tar.gz") { },

  # TODO: Switch to npins for reproducibility
  pkgs ? import <nixpkgs> { inherit system; },
}:
let
  rust-nightly = fenix.complete.withComponents [
    "cargo"
    "clippy"
    "rust-src"
    "rustc"
    "rustfmt"
  ];
in
pkgs.mkShellNoCC {
  packages = [
    rust-nightly
    pkgs.nushell
    pkgs.radicle-node
    pkgs.radicle-tui
    pkgs.nixpkgs-review
    pkgs.headscale
  ];
}

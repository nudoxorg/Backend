{
  system ? builtins.currentSystem,

  # Fenix for nightly
  fenix ? import (fetchTarball "https://github.com/nix-community/fenix/archive/main.tar.gz") { },

  # TODO: Switch to npins for reproducibility (Also port fenix)
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
    pkgs.git
    pkgs.jujutsu
    pkgs.rust-analyzer
    pkgs.typos
    pkgs.just
    pkgs.radicle-node
    pkgs.radicle-tui
    pkgs.headscale
    pkgs.cowsay
    pkgs.lolcat
  ];

   shellHook = ''
   cowsay "Welcome to the NuNuShell" | lolcat
   just
  '';
}

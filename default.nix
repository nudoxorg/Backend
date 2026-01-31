{
  # Import packages
  sources ? (import ./npins),
  nixpkgs ? sources.nixpkgs,
  system ? builtins.currentSystem,
  pkgs ? (import nixpkgs { inherit system; }),
}:
let
  # Evaluate fenix once and cache it
  fenix-pkg = import sources.fenix { };
  
  # Build the rust toolchain once
  rust-nightly = fenix-pkg.complete.withComponents [
    "cargo"
    "clippy"
    "rust-src"
    "rust-docs"
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

   # Ensure all repositories are up to date
   rad sync
   git pull
   git submodule update --init --recursive
  '';
}

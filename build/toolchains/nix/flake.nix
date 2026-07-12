# Toolchain packages for Buck2 via buck2.nix (https://github.com/tweag/buck2.nix).
#
# When the `nix` external cell is configured in .buckconfig, targets in
# build/toolchains/BUCK can reference these packages with flake.package():
#
#   load("@nix//flake.bzl", "flake")
#   flake.package(name = "clang", binary = "clang", path = "build/toolchains/nix")
#
# Running `nix flake lock build/toolchains/nix` pins the exact store paths in
# build/toolchains/nix/flake.lock, giving hermetic, reproducible tool versions.
{
  description = "Buck2 toolchain packages";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
    }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      eachSystem =
        f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system} fenix.packages.${system});
    in
    {
      packages = eachSystem (
        pkgs: fenix-pkg:
        let
          # Nightly Rust via fenix — matches the devshell toolchain.
          rust = fenix-pkg.complete.withComponents [
            "cargo"
            "clippy"
            "rustc"
            "rustdoc"
            "rustfmt"
            "rust-src"
          ];
        in
        {
          # Rust toolchain
          rustc = rust;
          clippy = rust;
          rustdoc = rust;

          # C/C++ toolchain — use the system clang on Darwin, nixpkgs clang on Linux.
          clang = if pkgs.stdenv.isDarwin then pkgs.darwin.apple_sdk_11_0.xcode else pkgs.clang;
          "clang++" = if pkgs.stdenv.isDarwin then pkgs.darwin.apple_sdk_11_0.xcode else pkgs.clang;
          ar = if pkgs.stdenv.isDarwin then pkgs.darwin.apple_sdk_11_0.xcode else pkgs.binutils;
        }
      );
    };
}

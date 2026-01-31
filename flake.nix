{
  description = "NuNuShell development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
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
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f {
            inherit system;
            pkgs = nixpkgs.legacyPackages.${system};
            fenix-pkg = fenix.packages.${system};
          }
        );
    in
    {
      devShells = eachSystem (
        {
          pkgs,
          system,
          fenix-pkg,
        }:
        let
          rust-nightly = fenix-pkg.complete.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rust-docs"
            "rustc"
            "rustfmt"
          ];
        in
        {
          default = pkgs.mkShellNoCC {
            NIX_CONFIG = ''
              extra-substituters = https://nix-community.cachix.org
              extra-trusted-public-keys = nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs=
            '';
            
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
          };
        }
      );

      # Expose devShell as a package for `nix shell` compatibility
      packages = eachSystem (
        { system, ... }:
        {
          default = self.devShells.${system}.default;
        }
      );

      formatter = eachSystem ({ pkgs, ... }: pkgs.nixfmt-rfc-style);
    };
}


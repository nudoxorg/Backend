{
  description = "NuNuShell development environment";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    devshell = {
      url = "github:numtide/devshell";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs =
    {
      self,
      nixpkgs,
      fenix,
      git-hooks,
      devshell,
    }:
    let
      prePushHook = hook: hook // { stages = [ "pre-push" ]; };

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
      checks = eachSystem (
        {
          pkgs,
          system,
          fenix-pkg,
          ...
        }:
        let
          rust-nightly = fenix-pkg.complete.withComponents [
            "cargo"
            "clippy"
            "rustc"
            "rustfmt"
            "rustc-codegen-cranelift-preview"
          ];
        in
        {
          pre-commit-check = git-hooks.lib.${system}.run {
            src = ./.;
            package = pkgs.prek;
            hooks = {
              nixfmt = prePushHook { enable = true; };
              rustfmt = prePushHook {
                enable = true;
                packageOverrides.cargo = rust-nightly;
                packageOverrides.rustfmt = rust-nightly;
              };
              clippy = prePushHook {
                enable = true;
                packageOverrides.cargo = rust-nightly;
                packageOverrides.clippy = rust-nightly;
              };
              cargo-check = prePushHook { enable = true; };
            };
          };
        }
      );

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
            "rustc-codegen-cranelift-preview"
          ];
          pre-commit-check = self.checks.${system}.pre-commit-check;
        in
        {
          default = (devshell.legacyPackages.${system}.mkShell) {
            name = "NuNuShell";
            # NIX_CONFIG = ''
            #   extra-substituters = https://nix-community.cachix.org
            #   extra-trusted-public-keys = nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs=
            # '';

            packages = [
              rust-nightly
              pkgs.nushell
              pkgs.ollama
              pkgs.git
              pkgs.clang
              pkgs.cargo-bump # Version bumping
              pkgs.jujutsu
              pkgs.rust-analyzer
              pkgs.flock
              pkgs.typos
              pkgs.just
              pkgs.radicle-node
              pkgs.radicle-tui
              pkgs.headscale
              pkgs.kittysay
              pkgs.dotacat # Rust lolcat
            ]
            ++ pkgs.lib.optional pkgs.stdenv.isLinux pkgs.wild;

            commands = [
              {
                name = "build";
                help = "build the project";
                command = "cargo build";
                category = "rust";
              }
              {
                name = "test";
                help = "run all tests";
                command = "cargo test";
                category = "rust";
              }
              {
                name = "lint";
                help = "run clippy lints";
                command = "cargo clippy";
                category = "rust";
              }
              {
                name = "fmt";
                help = "format rust and nix sources";
                command = "cargo fmt && nixfmt-rfc-style **/*.nix";
                category = "formatters";
              }
              {
                name = "spellcheck";
                help = "check for typos";
                command = "typos";
                category = "linters";
              }
              {
                name = "run-recipe";
                help = "run a just recipe (pass recipe name as arg)";
                command = "just \"$@\"";
                category = "utilities";
              }
              {
                name = "rad-sync";
                help = "manually sync radicle repos";
                command = "rad sync --fetch";
                category = "utilities";
              }
            ];
            devshell.startup.shellHook.text = ''
              ${pre-commit-check.shellHook}
              (
                # Use a lockfile to prevent multiple instances from stomping on Git
                flock -n 9 || exit 1

                # Ensure all repositories are up to date
                rad sync --fetch > /dev/null 2>&1

              ) 9>/tmp/nunu_sync.lock &

              # Immediately show the welcome message
              kittysay --think "the nu is the now" | dotacat
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

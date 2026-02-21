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
              nixfmt = prePushHook {
                enable = true;
              };
              convco = prePushHook {
                enable = true;
                pass_filenames = false;
                entry = toString (
                  pkgs.writeShellScript "convco-pre-push" ''
                    while read local_ref local_sha remote_ref remote_sha; do
                      ${pkgs.convco}/bin/convco check "$remote_sha..$local_sha"
                    done
                  ''
                );
              };
              rustfmt = prePushHook {
                enable = true;
                packageOverrides.cargo = rust-nightly;
                packageOverrides.rustfmt = rust-nightly;
              };
              markdownfmt = {
                enable = true;
                entry = "hongdown .";
                pass_filenames = false;
                files = "\\.md$";
                stages = [ "pre-push" ];
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
          mkCommand = name: help: category: {
            inherit name help category;
            command = "cd $PRJ_ROOT && nu .config/scripts/${name}.nu \"$@\"";
          };
        in
        {
          default = (devshell.legacyPackages.${system}.mkShell) {
            name = "NuNuShell";
            env = [
              {
                name = "MAIN_PACKAGE";
                value = "nudox";
              }
              {
                name = "OUTPUT_DIRECTORY";
                value = "dist";
              }
            ];

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
              pkgs.nixfmt
              pkgs.tombi
              pkgs.typos
              pkgs.hongdown
              pkgs.just
              pkgs.radicle-node
              pkgs.radicle-tui
              pkgs.headscale
              pkgs.kittysay
              pkgs.marksman
              pkgs.taplo
              pkgs.nil
              pkgs.dotacat # Rust lolcat
            ]
            ++ pkgs.lib.optional pkgs.stdenv.isLinux pkgs.wild;

            commands = [
              # --- Build & Check --- #
              (mkCommand "check" "Check workspace for compilation and syntax errors" "build")
              (mkCommand "build" "Build workspace in debug mode" "build")
              (mkCommand "build-release" "Build workspace in release mode" "build")

              # --- Packaging --- #
              (mkCommand "package" "Package release binary with completions for distribution" "packaging")
              (mkCommand "checksum" "Generate checksums for distribution files" "packaging")
              (mkCommand "compress" "Compress all release packages into tar.gz archives" "packaging")
              (mkCommand "release" "Complete release pipeline: build, checksum, and compress" "packaging")

              # --- Execution --- #
              (mkCommand "run" "Run application in debug mode" "execution")
              (mkCommand "run-release" "Run application in release mode" "execution")

              # --- Testing --- #
              (mkCommand "test" "Run all workspace tests" "testing")
              (mkCommand "test-with" "Run workspace tests with additional arguments" "testing")

              # --- Code Quality --- #
              (mkCommand "fmt" "Format all Rust code in the workspace" "quality")
              (mkCommand "fmt-check" "Check if Rust code is properly formatted" "quality")
              (mkCommand "lint" "Lint code with Clippy in debug mode" "quality")
              (mkCommand "lint-fix" "Automatically fix Clippy lints where possible" "quality")

              # --- Documentation --- #
              (mkCommand "doc" "Generate project documentation" "documentation")
              (mkCommand "doc-open" "Generate and open project documentation in browser" "documentation")

              # --- Maintenance --- #
              (mkCommand "create-notes" "Extract release notes from changelog for specified tag" "maintenance")
              (mkCommand "update" "Update Cargo dependencies" "maintenance")
              (mkCommand "clean" "Clean build artifacts" "maintenance")

              # --- Installation --- #
              (mkCommand "install" "Build and install binary to system" "installation")
              (mkCommand "install-force" "Force install binary" "installation")

              # --- Utilities --- #
              (mkCommand "rad-sync" "manually sync radicle repos" "utilities")
            ];
            devshell.startup.shellHook.text = ''
              export RUST_TARGET=$(rustc --version --verbose | grep '^host:' | awk '{print $2}')
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

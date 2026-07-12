{
  description = "NuDox development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixos.url = "git+https://dev.nudox.org/git/Nudox/MachineConfigurations.git";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    developmentShell = {
      url = "github:numtide/devshell";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    buck2-prelude = {
      url = "github:facebookincubator/buck2-prelude?rev=4b374e200a64838660463994b079899b5094689a";
      flake = false;
    };

    snowydeer = {
      url = "github:MercuryTechnologies/snowydeer";
      flake = false;
    };

    # tweag/buck2.nix — provides flake.package() + nix_rust_toolchain/nix_cxx_toolchain.
    # Fetched as a plain source input so the devshell can wire it as a `path`-type
    # external cell in .buckconfig, avoiding a git fetch at Buck2 build time.
    buck2-nix = {
      url = "github:tweag/buck2.nix/038b031b84846101030b9d081445003e82e3be5c";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      nixos,
      fenix,
      git-hooks,
      developmentShell,
      buck2-prelude,
      snowydeer,
      buck2-nix,
    }:
    let
      supportedSystemArchitectures = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      buck2Version = "2026-07-01";
      buck2ArtifactDetails = {
        "aarch64-darwin" = {
          platform = "aarch64-apple-darwin";
          hash = "sha256-cjgWmrQiLagv4lN3dgEl2l7wDmchntMGBgFyfztRLWA=";
        };
        "aarch64-linux" = {
          platform = "aarch64-unknown-linux-gnu";
          hash = "sha256-zMbZcliSzTyfdKxgxx5A1R3tibdHhAZLLdYNNJ6gu24=";
        };
        "x86_64-darwin" = {
          platform = "x86_64-apple-darwin";
          hash = "sha256-7czaJhavbkHkv/1JsXPbi3IeNptgZHMcGLdt14de2lU=";
        };
        "x86_64-linux" = {
          platform = "x86_64-unknown-linux-gnu";
          hash = "sha256-XQzRG7QQHId6nSNCcFsXme6j2oU8uqAOoNCoflPFwzg=";
        };
      };

      makeBuck2BinaryDerivation =
        nixPackages:
        let
          artifactDetails = buck2ArtifactDetails.${nixPackages.stdenv.hostPlatform.system};
        in
        nixPackages.stdenvNoCC.mkDerivation {
          pname = "buck2";
          version = buck2Version;
          src = nixPackages.fetchurl {
            url = "https://github.com/facebook/buck2/releases/download/${buck2Version}/buck2-${artifactDetails.platform}.zst";
            hash = artifactDetails.hash;
          };
          nativeBuildInputs = [ nixPackages.zstd ];
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/bin
            zstd -d $src -o $out/bin/buck2
            chmod +x $out/bin/buck2
          '';
          meta.mainProgram = "buck2";
        };

      generateForEverySystem =
        configurationFunction:
        nixpkgs.lib.genAttrs supportedSystemArchitectures (
          systemArchitecture:
          configurationFunction {
            systemArchitecture = systemArchitecture;
            nixPackages = import nixpkgs {
              system = systemArchitecture;
              overlays = [ fenix.overlays.default ];
            };
            fenixPackages = fenix.packages.${systemArchitecture};
          }
        );

    in
    {
      packages = generateForEverySystem (
        { systemArchitecture, nixPackages, ... }:
        let
          buck2PinningData = builtins.fromJSON (builtins.readFile ./nix/buck2-artifact.json);
          buck2StorePath = buck2PinningData.paths.${systemArchitecture} or null;
        in
        {
          developmentShell = self.devShells.${systemArchitecture}.default;
        }
        // nixpkgs.lib.optionalAttrs (buck2StorePath != null) {
          default = builtins.fetchClosure {
            fromStore = buck2PinningData.fromStore;
            fromPath = buck2StorePath;
          };
        }
      );

      apps = generateForEverySystem (
        { nixPackages, ... }:
        let
          buck2Binary = makeBuck2BinaryDerivation nixPackages;
          buckExtensionScript = nixPackages.writeShellApplication {
            name = "nudox-buck2-build";
            runtimeInputs = [ buck2Binary ];
            text = ''
              if [[ ! -f .buckconfig ]]; then
                echo "ERROR: run from the Backend repo root (no .buckconfig found)" >&2
                exit 1
              fi

              ln -sfn ${buck2-prelude} prelude
              preludeStampFile="build/prelude-local/.nix-source"

              if [[ ! -f "$preludeStampFile" || "$(cat "$preludeStampFile")" != "${buck2-prelude}" ]]; then
                bash build/setup-prelude.sh
                echo -n "${buck2-prelude}" > "$preludeStampFile"
              fi

              echo "nudox-buck2-build: invoking snowydeer BXL for //workspace/server:nudox_pkg" >&2
              buck2 bxl //snowydeer:snowydeer.bxl:main -- --target //workspace/server:nudox_pkg
            '';
          };
        in
        {
          buck2 = {
            type = "app";
            program = "${buckExtensionScript}/bin/nudox-buck2-build";
          };
        }
      );

      checks = generateForEverySystem (
        {
          systemArchitecture,
          nixPackages,
          fenixPackages,
        }:
        let
          rustNightlyToolchain = fenixPackages.complete.withComponents [
            "cargo"
            "clippy"
            "rustc"
            "rustfmt"
            "rustc-codegen-cranelift-preview"
          ];
        in
        {
          preCommitGitHooks = git-hooks.lib.${systemArchitecture}.run {
            src = ./.;
            package = nixPackages.prek;
            default_stages = [ "pre-push" ];
            hooks = {
              convco = {
                enable = true;
                pass_filenames = false;
                stages = [ "pre-push" ];
                entry = toString (
                  nixPackages.writeShellScript "convco-pre-push" ''
                    while read local_ref local_sha remote_ref remote_sha; do
                      ${nixPackages.convco}/bin/convco check "$remote_sha..$local_sha"
                    done
                  ''
                );
              };
              nixfmt.enable = true;
              rustfmt = {
                enable = true;
                packageOverrides = {
                  cargo = rustNightlyToolchain;
                  rustfmt = rustNightlyToolchain;
                };
              };
              markdownfmt = {
                enable = true;
                name = "hongdown";
                entry = "hongdown --write";
                files = "\\.md$";
                language = "system";
              };
              testrust = {
                enable = true;
                name = "testrust";
                entry = "cargo nextest run";
                language = "system";
                pass_filenames = false;
                stages = [ "pre-merge-commit" ];
              };
              clippy = {
                enable = true;
                stages = [
                  "pre-merge-commit"
                  "pre-push"
                ];
                packageOverrides = {
                  cargo = rustNightlyToolchain;
                  clippy = rustNightlyToolchain;
                };
              };
            };
          };
        }
      );

      devShells = generateForEverySystem (
        {
          systemArchitecture,
          nixPackages,
          fenixPackages,
        }:
        let
          rustNightlyToolchain = fenixPackages.complete.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rust-docs"
            "rustc"
            "rustfmt"
            "rustc-codegen-cranelift-preview"
          ];

          gitHookConfiguration = self.checks.${systemArchitecture}.preCommitGitHooks;

          makeNushellCommand = commandName: commandHelpText: commandCategory: {
            name = commandName;
            help = commandHelpText;
            category = commandCategory;
            command = "cd $PRJ_ROOT && nu .config/scripts/${commandName}.nu \"$@\"";
          };

          nixBuckconfigFragment = nixPackages.writeText "buckconfig-nix-generated" (
            nixPackages.lib.generators.toINI
              {
                mkKeyValue = k: v: "  ${k} = ${v}";
              }
              {
                # Enable Nix toolchains; overrides [nix] toolchain = 0 in the base
                # .buckconfig (which is the non-Nix default).  The @nix cell is
                # overridden below by [external_cells] to use the real buck2.nix
                # source instead of the build/nix-stub fallback.
                # See: https://github.com/tweag/buck2.nix and build/toolchains/nix/flake.nix
                nix = {
                  toolchain = "1";
                };
                external_cells = {
                  nix = "path";
                };
                external_cell_nix = {
                  path = toString buck2-nix;
                };
                go = {
                  go_binary = "${nixPackages.go}/bin/go";
                };
                java = {
                  java_home = toString nixPackages.jdk21_headless;
                };
              }
          );
        in
        {
          default = (developmentShell.legacyPackages.${systemArchitecture}.mkShell) {
            name = "NuNuShell";

            env = [
              {
                name = "RUSTC_BOOTSTRAP";
                value = "1";
              }
              {
                name = "LIBRARY_PATH";
                value = "$(nix eval --raw nixpkgs#libiconv.outPath)/lib";
              }
              {
                name = "MAIN_PACKAGE";
                value = "nudox";
              }
              {
                name = "OUTPUT_DIRECTORY";
                value = "dist";
              }
              {
                name = "LD_LIBRARY_PATH";
                value = "${nixPackages.openssl.out}/lib:$LD_LIBRARY_PATH";
              }
              {
                name = "OPENSSL_DIR";
                value = "${nixPackages.openssl.dev}";
              }
              {
                name = "OPENSSL_LIB_DIR";
                value = "${nixPackages.openssl.out}/lib";
              }
              {
                name = "OPENSSL_INCLUDE_DIR";
                value = "${nixPackages.openssl.dev}/include";
              }
            ];

            motd = ''
              $($(type -p kittysay) --think "the nu is the now" | dotacat)
            '';

            packages = [
              rustNightlyToolchain
              (makeBuck2BinaryDerivation nixPackages)
            ]
            ++ (with nixPackages; [
              git
              cargo-bump
              rust-analyzer
              flock
              nixfmt-rfc-style
              tombi
              typos
              hongdown
              radicle-node
              radicle-tui
              kittysay
              marksman
              taplo
              cargo-nextest
              libiconv
              nil
              jsonfmt
              dotacat
              goreleaser
              cuelsp
              b3sum
              go
              jdk21_headless
            ])
            ++ (
              with nixPackages.lib;
              optionals nixPackages.stdenv.isLinux (
                with nixPackages;
                [
                  wild-unwrapped
                  openssl
                  clang
                ]
              )
            );

            commands = [
              (makeNushellCommand "check" "Check workspace for compilation and syntax errors" "build")
              (makeNushellCommand "build" "Build workspace in debug mode" "build")
              (makeNushellCommand "build-release" "Build workspace in release mode" "build")
              {
                name = "ra-index";
                help = "Generate rust-project.json for rust-analyzer";
                category = "build";
                command = "bash build/gen-rust-project.sh";
              }
              {
                name = "release";
                help = "Complete release pipeline using GoReleaser";
                category = "packaging";
                command = "nu .config/scripts/release.nu";
              }
              (makeNushellCommand "run" "Run application in debug mode" "execution")
              (makeNushellCommand "run-release" "Run application in release mode" "execution")
              (makeNushellCommand "test" "Run all workspace tests" "testing")
              (makeNushellCommand "test-with" "Run workspace tests with additional arguments" "testing")
              (makeNushellCommand "fmt" "Format all Rust code in the workspace" "quality")
              (makeNushellCommand "fmt-check" "Check if Rust code is properly formatted" "quality")
              (makeNushellCommand "lint" "Lint code with Clippy in debug mode" "quality")
              (makeNushellCommand "lint-fix" "Automatically fix Clippy lints where possible" "quality")
              (makeNushellCommand "doc" "Generate project documentation" "documentation")
              (makeNushellCommand "doc-open" "Generate and open project documentation in browser" "documentation")
              (makeNushellCommand "create-notes" "Extract release notes from changelog for specified tag"
                "maintenance"
              )
              (makeNushellCommand "update" "Update Cargo dependencies" "maintenance")
              (makeNushellCommand "clean" "Clean build artifacts" "maintenance")
              (makeNushellCommand "patch" "Update or create a patch from a branch" "maintenance")
              (makeNushellCommand "install" "Build and install binary to system" "installation")
              (makeNushellCommand "install-force" "Force install binary" "installation")
              (makeNushellCommand "buck-build" "Build Buck2 targets" "buck2")
              (makeNushellCommand "buck-test" "Run Buck2 tests" "buck2")
              (makeNushellCommand "rad-sync" "manually sync radicle repos" "utilities")
            ];

            devshell.startup.shellHook.text = ''
              ln -sfn ${buck2-prelude} "$PRJ_ROOT/prelude"
              preludeStampFile="$PRJ_ROOT/build/prelude-local/.nix-source"

              if [[ ! -f "$preludeStampFile" || "$(cat "$preludeStampFile")" != "${buck2-prelude}" ]]; then
                bash "$PRJ_ROOT/build/setup-prelude.sh"
                echo -n "${buck2-prelude}" > "$preludeStampFile"
              fi

              # Strip any previous NIX-GENERATED block then append a fresh one.
              # Idempotent: safe to run on every shell entry.
              awk '/^# BEGIN NIX-GENERATED/{skip=1;next} /^# END NIX-GENERATED/{skip=0;next} !skip{print}' \
                "$PRJ_ROOT/.buckconfig" > "$PRJ_ROOT/.buckconfig.tmp"
              mv "$PRJ_ROOT/.buckconfig.tmp" "$PRJ_ROOT/.buckconfig"
              {
                printf '\n# BEGIN NIX-GENERATED — managed by flake.nix devshell, do not edit\n'
                cat "${nixBuckconfigFragment}"
                printf '# END NIX-GENERATED\n'
              } >> "$PRJ_ROOT/.buckconfig"

              export RUST_TARGET=$(rustc --version --verbose | grep '^host:' | awk '{print $2}')
              unset RUSTC_WRAPPER

              ${gitHookConfiguration.shellHook}

              (
                flock -n 9 || exit 1
              ) 9>/tmp/nunu_sync.lock &
            '';
          };
        }
      );

      formatter = generateForEverySystem ({ nixPackages, ... }: nixPackages.nixfmt-rfc-style);
    };
}

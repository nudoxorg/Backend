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

    nuenv = {
      url = "github:philocalyst/nuenv";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    buck2-prelude = {
      url = "github:facebookincubator/buck2-prelude?rev=4b374e200a64838660463994b079899b5094689a";
      flake = false;
    };

    # tweag/buck2.nix — provides flake.package() + nix_rust_toolchain/nix_cxx_toolchain.
    # Fetched as a plain source input so the devshell can wire it as a `path`-type
    # external cell in .buckconfig, avoiding a git fetch at Buck2 build time.
    buck2-nix = {
      url = "github:tweag/buck2.nix/038b031b84846101030b9d081445003e82e3be5c";
      flake = false;
    };

    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      nixos,
      fenix,
      git-hooks,
      nuenv,
      buck2-prelude,
      buck2-nix,
      nix2container,
    }:
    let
      supportedSystemArchitectures = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      reindeerVersion = "v2026.07.13.00";
      reindeerArtifactDetails = {
        "aarch64-darwin" = {
          platform = "aarch64-apple-darwin";
          hash = "sha256-SL2EJ5B+90i3O3Pw4NAcI4wnyUMp29yl0qo4wmM7jsg=";
        };
        "aarch64-linux" = {
          platform = "aarch64-unknown-linux-gnu";
          hash = "sha256-capEdrcgi4+Z17BPpKunlyKE6C1eUyGubIYn/Y2efwk=";
        };
        "x86_64-darwin" = {
          platform = "x86_64-apple-darwin";
          hash = "sha256-AScqudALl5webUyMJ2v/Lw0CCU+q7nJ1JrrAAQP2CSo=";
        };
        "x86_64-linux" = {
          platform = "x86_64-unknown-linux-gnu";
          hash = "sha256-IIfm1olh7P7VJOPiJM156g5wkdDgSE1DIH93/1UkE2k=";
        };
      };

      makeReindeerBinaryDerivation =
        nixPackages:
        let
          artifactDetails = reindeerArtifactDetails.${nixPackages.stdenv.hostPlatform.system};
        in
        nixPackages.stdenvNoCC.mkDerivation {
          pname = "reindeer";
          version = reindeerVersion;
          src = nixPackages.fetchurl {
            url = "https://github.com/facebookincubator/reindeer/releases/download/${reindeerVersion}/reindeer-${artifactDetails.platform}.zst";
            hash = artifactDetails.hash;
          };
          nativeBuildInputs = [ nixPackages.zstd ];
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/bin
            zstd -d $src -o $out/bin/reindeer
            chmod +x $out/bin/reindeer
          '';
          meta.mainProgram = "reindeer";
        };

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
              overlays = [
                fenix.overlays.default
                nuenv.overlays.default
              ];
            };
            fenixPackages = fenix.packages.${systemArchitecture};
          }
        );

    in
    {
      packages = generateForEverySystem (
        { systemArchitecture, nixPackages, ... }:
        let
          buildImage = nix2container.packages.${systemArchitecture}.nix2container.buildImage;
        in
        {
          default = (nixos.lib.build.rustService { pkgs = nixPackages; }) {
            pname = "nudox-backend";
            version = self.rev or "unknown";
            src = ./.;
            cargoPackage = "server";
            mainProgram = "server";
            description = "NuDox backend server";
          };

          backend = (nixos.lib.build.rustService { pkgs = nixPackages; }) {
            pname = "nudox-backend";
            version = self.rev or "unknown";
            src = ./.;
            cargoPackage = "server";
            mainProgram = "server";
            description = "NuDox backend server";
          };

          registry = ((nixos.lib.build.rustService { pkgs = nixPackages; }) {
            pname = "nudox-registry";
            version = self.rev or "unknown";
            src = ./.;
            cargoPackage = "registry";
            mainProgram = "";
            description = "NuDox registry library";
          }).overrideAttrs {
            postFixup = "";
          };

          compiler-daemon = nixPackages.callPackage ./build/nix/compiler.nix {
            # All paths are read from env vars at evaluation time.  In pure eval
            # (no --impure), builtins.getEnv returns "" so every path is null and
            # placeholder scripts are emitted.  When --impure is used with the
            # snowydeer-imported store paths set, the real binaries are wired in.
            #
            # The same pattern extends to the optional oracle resource paths.
            compilerDaemon =
              let p = builtins.getEnv "NUDOX_COMPILER_DAEMON_PATH";
              in if p != "" then builtins.storePath p else null;
            producerWorker =
              let p = builtins.getEnv "NUDOX_PRODUCER_WORKER_PATH";
              in if p != "" then builtins.storePath p else null;
            goOracle =
              let p = builtins.getEnv "NUDOX_GO_ORACLE_PATH";
              in if p != "" then builtins.storePath p else null;
            javaOracle =
              let p = builtins.getEnv "NUDOX_JAVA_ORACLE_PATH";
              in if p != "" then builtins.storePath p else null;
            csharpOracle =
              let p = builtins.getEnv "NUDOX_CSHARP_ORACLE_PATH";
              in if p != "" then builtins.storePath p else null;
          };

          compilerImage = nixPackages.callPackage ./build/nix/compiler-image.nix {
            inherit buildImage;
            compiler = self.packages.${systemArchitecture}.compiler-daemon;
            bwrap = if builtins.hasAttr "bwrap" nixPackages then nixPackages.bwrap else null;
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

          # Build script PATH: fenix rustc + nix package manager + system paths.
          # Consumed by build/third-party/defs.bzl via read_config("build","devshell_bin").
          devshellBin = nixPackages.lib.concatStringsSep ":" [
            "${rustNightlyToolchain}/bin"
            "${nixPackages.nix}/bin"
            "/usr/bin"
            "/bin"
            "/usr/sbin"
            "/sbin"
          ];

          nixBuckconfigFragment = nixPackages.writeText "buckconfig-nix-generated" (
            nixPackages.lib.generators.toINI
              {
                mkKeyValue = k: v: "  ${k} = ${v}";
              }
              {
                cells = {
                  nix = "build/nix-cell";
                };
                nix = {
                  toolchain = "1";
                };
                build = {
                  devshell_bin = devshellBin;
                };
                go = {
                  go_binary = "${nixPackages.go}/bin/go";
                };
                java = {
                  java_home = toString nixPackages.jdk21_headless;
                };
                csharp = {
                  dotnet = "${nixPackages.dotnetCorePackages.sdk_10_0}/bin/dotnet";
                  nuget_packages = "";
                };
              }
          );

          # ── Devshell command wrappers ────────────────────────────────────
          # Create a thin bash wrapper for each .nu script that cds to the
          # project root before running, matching the previous devshell
          # behaviour.
          mkDevshellCommand = cmdName: nixPackages.writeTextFile {
            name = "${cmdName}-nuenv";
            destination = "/bin/${cmdName}";
            executable = true;
            text = ''
              #!/bin/sh
              cd "$PRJ_ROOT" && exec ${nixPackages.nushell}/bin/nu .config/scripts/${cmdName}.nu "$@"
            '';
          };

          nuScriptCommands = [
            "build" "build-release" "check" "clean" "create-notes"
            "doc" "doc-open" "fmt" "fmt-check" "install" "install-force"
            "lint" "lint-fix" "patch" "release" "run" "run-release"
            "sync-deps" "test" "test-with" "test-all" "update"
            "buck-build" "buck-test" "ra-index"
            "snowydeer-import" "build-compiler-image"
          ];

          commandPackages = map mkDevshellCommand nuScriptCommands;
        in
        {
          default = nixPackages.mkShell {
            name = "NuNuShell";

            # ── Simple environment variables ────────────────────────────────
            RUSTC_BOOTSTRAP = "1";
            LIBRARY_PATH = "${nixPackages.libiconv}/lib";
            MAIN_PACKAGE = "nudox";
            OUTPUT_DIRECTORY = "dist";
            OPENSSL_DIR = "${nixPackages.openssl.dev}";
            OPENSSL_LIB_DIR = "${nixPackages.openssl.out}/lib";
            OPENSSL_INCLUDE_DIR = "${nixPackages.openssl.dev}/include";
            DOTNET_CLI_TELEMETRY_OPTOUT = "1";
            DOTNET_NOLOGO = "1";
            DOTNET_SKIP_FIRST_TIME_EXPERIENCE = "1";

            # ── Packages available in the shell ─────────────────────────────
            packages = commandPackages ++ [
              rustNightlyToolchain
              (makeBuck2BinaryDerivation nixPackages)
              (makeReindeerBinaryDerivation nixPackages)
            ]
            ++ (with nixPackages; [
              git cargo-bump rust-analyzer flock nixfmt-rfc-style
              tombi typos hongdown kittysay marksman taplo
              cargo-nextest libiconv nil jsonfmt dotacat goreleaser
              cuelsp b3sum go jdk21_headless dotnetCorePackages.sdk_10_0
            ])
            ++ (
              with nixPackages.lib;
              optionals nixPackages.stdenv.isLinux (
                with nixPackages;
                [ wild-unwrapped openssl clang ]
              )
            );

            # ── Shell hook ─────────────────────────────────────────────────
            shellHook = ''
              export PRJ_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo "$PWD")"

              # Environment vars that need shell expansion
              export LD_LIBRARY_PATH="${nixPackages.openssl.out}/lib:$LD_LIBRARY_PATH"
              export DOTNET_CLI_HOME="$TMPDIR/dotnet"

              # Sealed-producer PATH prefix (see sandbox::ToolchainSet). Hermetic
              # PATH alone is /usr/bin:/bin:/nix/var/nix/profiles/default/bin —
              # language oracles need go/javadoc/dotnet from the Nix store.
              export NUDOX_TOOLCHAIN_PATH="${nixPackages.go}/bin:${nixPackages.jdk21_headless}/bin:${nixPackages.dotnetCorePackages.sdk_10_0}/bin''${NUDOX_TOOLCHAIN_PATH:+:$NUDOX_TOOLCHAIN_PATH}"

              # MotD
              if command -v kittysay > /dev/null 2>&1; then
                kittysay --think "the nu is the now" | dotacat
              fi

              # Prelude setup
              ln -sfn ${buck2-prelude} "$PRJ_ROOT/prelude"
              ln -sfn ${buck2-nix} "$PRJ_ROOT/build/nix-cell"
              preludeStampFile="$PRJ_ROOT/build/prelude-local/.nix-source"

              if [[ ! -f "$preludeStampFile" || "$(cat "$preludeStampFile")" != "${buck2-prelude}" ]]; then
                bash "$PRJ_ROOT/build/setup-prelude.sh"
                echo -n "${buck2-prelude}" > "$preludeStampFile"
              fi

              # Update .buckconfig — strip old NIX-GENERATED block, append fresh one
              awk '/^# BEGIN NIX-GENERATED/{skip=1;next} /^# END NIX-GENERATED/{skip=0;next} !skip{print}' \
                "$PRJ_ROOT/.buckconfig" > "$PRJ_ROOT/.buckconfig.tmp"
              mv "$PRJ_ROOT/.buckconfig.tmp" "$PRJ_ROOT/.buckconfig"
              {
                printf '# BEGIN NIX-GENERATED — managed by flake.nix devshell, do not edit\n'
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

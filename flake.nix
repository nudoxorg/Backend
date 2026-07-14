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
      buck2-nix,
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
              overlays = [ fenix.overlays.default ];
            };
            fenixPackages = fenix.packages.${systemArchitecture};
          }
        );

    in
    {
      packages = generateForEverySystem (
        { systemArchitecture, nixPackages, ... }:
        {
          developmentShell = self.devShells.${systemArchitecture}.default;
          default = (nixos.lib.build.rustService { pkgs = nixPackages; }) {
            pname = "nudox-backend";
            version = self.rev or "unknown";
            src = ./.;
            cargoPackage = "server";
            mainProgram = "server";
            description = "NuDox backend server";
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

          # Build script PATH: fenix rustc + nix package manager + system paths.
          # Consumed by build/third-party/defs.bzl via read_config("build","devshell_bin").
          # Must include `rustc` (system-toolchain build script shims call it by bare
          # name) and `nix` (flake.package() actions call `nix build`).
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
                # Override the nix cell to the real buck2.nix source via a
                # repo-relative symlink (build/nix-cell → nix store path).
                # Buck2 requires relative cell paths; absolute nix store paths
                # are rejected at cell-resolver time. The devshell hook below
                # creates/refreshes the symlink on every shell entry.
                # Base .buckconfig keeps nix = build/nix-stub for non-Nix hosts.
                cells = {
                  nix = "build/nix-cell";
                };
                # Enable Nix toolchains; overrides [nix] toolchain = 0 in the base
                # .buckconfig (which is the non-Nix default).
                nix = {
                  toolchain = "1";
                };
                build = {
                  # Used by build/third-party/defs.bzl to set build script PATH.
                  # Avoids hardcoding the Nix store hash that changes on devshell rebuild.
                  devshell_bin = devshellBin;
                };
                go = {
                  go_binary = "${nixPackages.go}/bin/go";
                };
                java = {
                  java_home = toString nixPackages.jdk21_headless;
                };
                # TODO(csharp): csharp.nuget_packages should point at a Nix-materialized
                # offline NuGet folder feed built from
                # workspace/compiler/compile/csharp/oracle/packages.lock.json.
                # Once `packages.lock.json` is regenerated with a real SDK
                # (`dotnet restore --force-evaluate`), replace the empty string with
                # a fixed-output derivation such as:
                #
                #   nixPackages.fetchurl (or stdenvNoCC.mkDerivation) that runs
                #   `dotnet restore --packages $out --locked-mode` in a FOD sandbox,
                #   hash = "sha256-...";  # fill after first build
                #
                # Until then, the empty string lets the genrule fall back to an
                # online restore (dev-only; will fail in CI without internet).
                csharp = {
                  dotnet = "${nixPackages.dotnetCorePackages.sdk_10_0}/bin/dotnet";
                  nuget_packages = "";
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
                value = "${nixPackages.libiconv}/lib";
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
              # .NET SDK environment — suppress telemetry and first-run extraction;
              # DOTNET_CLI_HOME must be writable ($TMPDIR is always writable) because
              # dotnet writes SDKs/tools there at startup. Same class of fix as the
              # GOCACHE sandbox-PATH issue from the Go/Java snapshot-test drive.
              {
                name = "DOTNET_CLI_TELEMETRY_OPTOUT";
                value = "1";
              }
              {
                name = "DOTNET_NOLOGO";
                value = "1";
              }
              {
                name = "DOTNET_SKIP_FIRST_TIME_EXPERIENCE";
                value = "1";
              }
              {
                name = "DOTNET_CLI_HOME";
                value = "$TMPDIR/dotnet";
              }
            ];

            motd = ''
              $($(type -p kittysay) --think "the nu is the now" | dotacat)
            '';

            packages = [
              rustNightlyToolchain
              (makeBuck2BinaryDerivation nixPackages)
              (makeReindeerBinaryDerivation nixPackages)
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
              dotnetCorePackages.sdk_10_0
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
              (makeNushellCommand "test-all" "Run Cargo workspace tests + Buck2 compiler tests" "testing")
              (makeNushellCommand "sync-deps"
                "Sync Cargo deps into Buck2 third-party registry after Cargo.toml changes"
                "maintenance"
              )
              (makeNushellCommand "buck-build" "Build Buck2 targets" "buck2")
              (makeNushellCommand "buck-test" "Run Buck2 tests" "buck2")
            ];

            devshell.startup.shellHook.text = ''
              ln -sfn ${buck2-prelude} "$PRJ_ROOT/prelude"
              ln -sfn ${buck2-nix} "$PRJ_ROOT/build/nix-cell"
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

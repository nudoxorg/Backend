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

      # Shared helpers (rust components, mkNuCheck, envStorePath, …).
      helpersFor =
        nixPackages: fenixPackages:
        import ./build/nix/lib.nix {
          pkgs = nixPackages;
          inherit fenixPackages;
          inherit nixos;
        };

      # Build the reproducible corpus from manifest.toml
      buildCorpus =
        nixPackages:
        let
          manifestFile = builtins.readFile ./corpus/manifest.toml;
          manifest = builtins.fromTOML manifestFile;

          # Fetch and prepare a single crate version
          prepareCrate =
            name: version: hash:
            let
              crateArchive = nixPackages.fetchurl {
                url = "https://static.crates.io/crates/${name}/${name}-${version}.crate";
                sha256 = hash;
              };
            in
            nixPackages.runCommand "${name}-${version}-prepared" { } ''
              mkdir -p "$out"
              cd "$out"

              # Unpack the crate archive (tar.gz format, despite .crate extension)
              ${nixPackages.gnutar}/bin/tar -xzf "${crateArchive}"

              # Append empty [workspace] table to Cargo.toml if it exists
              crate_dir="${name}-${version}"
              if [ -f "$crate_dir/Cargo.toml" ]; then
                echo "" >> "$crate_dir/Cargo.toml"
                echo "[workspace]" >> "$crate_dir/Cargo.toml"
              fi
            '';

          # Collect all prepared crates as a flat list
          allPreparedCrates =
            nixPackages.lib.flatten (
              map (
                pkgEntry:
                map (
                  verEntry:
                  prepareCrate pkgEntry.name verEntry.version verEntry.hash
                ) pkgEntry.versions
              ) manifest.packages
            );

          # Assemble all prepared packages into one directory
          assembliedCorpus = nixPackages.runCommand "real-crates" { } ''
            mkdir -p "$out"
            ${nixPackages.lib.concatStringsSep "\n" (
              map (derivation:
                "cp -r ${derivation}/*-*/ $out/ 2>/dev/null || true"
              ) allPreparedCrates
            )}
          '';
        in
        assembliedCorpus;

    in
    {
      # Workspace package recipes + toolchain re-exports.
      packages = generateForEverySystem (
        {
          systemArchitecture,
          nixPackages,
          fenixPackages,
        }:
        let
          helpers = helpersFor nixPackages fenixPackages;
          inherit (helpers) envStorePath;
        in
        (import ./workspace {
          pkgs = nixPackages;
          inherit fenixPackages nixos;
          buildImage = nix2container.packages.${systemArchitecture}.nix2container.buildImage;
          src = ./.;
          version = self.rev or "unknown";
          # snowydeer paths — pure eval yields null → placeholder scripts.
          # With --impure + env vars set, real Buck2-built binaries are wired in.
          compilerDaemonPath = envStorePath "NUDOX_COMPILER_DAEMON_PATH";
          producerWorkerPath = envStorePath "NUDOX_PRODUCER_WORKER_PATH";
          goOraclePath = envStorePath "NUDOX_GO_ORACLE_PATH";
          javaOraclePath = envStorePath "NUDOX_JAVA_ORACLE_PATH";
          csharpOraclePath = envStorePath "NUDOX_CSHARP_ORACLE_PATH";
        })
        // {
          corpus = buildCorpus nixPackages;
        }
      );

      checks = generateForEverySystem (
        {
          systemArchitecture,
          nixPackages,
          fenixPackages,
        }:
        let
          helpers = helpersFor nixPackages fenixPackages;
          inherit (helpers) rustToolchainHooks;

          packagesForSystem = self.packages.${systemArchitecture};

          # Prefer an explicit snowydeer/nix-store path; else packages.compiler-daemon.
          compilerFromEnv =
            let
              p = builtins.getEnv "NUDOX_COMPILER_DAEMON_PATH";
            in
            if p == "" then
              null
            else
              nixPackages.runCommand "compiler-daemon-for-check" { } ''
                mkdir -p "$out/bin"
                src="${builtins.storePath p}"
                if [ -f "$src/bin/compiler-daemon" ]; then
                  cp -L "$src/bin/compiler-daemon" "$out/bin/compiler-daemon"
                elif [ -f "$src" ]; then
                  cp -L "$src" "$out/bin/compiler-daemon"
                else
                  echo "NUDOX_COMPILER_DAEMON_PATH=$src has no compiler-daemon binary" >&2
                  exit 1
                fi
                chmod +x "$out/bin/compiler-daemon"
              '';

          projectRoot =
            let
              prj = builtins.getEnv "PRJ_ROOT";
              pwd = builtins.getEnv "PWD";
            in
            if prj != "" then prj else pwd;

          integrationChecks = import ./tests {
            pkgs = nixPackages;
            inherit fenixPackages nixos;
            packages = packagesForSystem;
            buck2 = makeBuck2BinaryDerivation nixPackages;
            inherit projectRoot;
            compilerDaemon = if compilerFromEnv != null then compilerFromEnv else null;
          };
        in
        integrationChecks
        // {
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
                  cargo = rustToolchainHooks;
                  rustfmt = rustToolchainHooks;
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
                # A bare `cargo nextest run` fails at the build step on this
                # repo before any nextest.toml filter runs: `workspace/index`
                # has never compiled, and `driver`/`ir-vcs` both depend on it
                # (LIMITATIONS.md L6) — `--exclude` for those three is
                # required on every invocation, and `nudox-ir` needs
                # RUSTC_BOOTSTRAP=1 (unstable macro decls). This entry was
                # broken (would fail on every commit) before this fix.
                # `nextest-suite.nu` bakes both requirements in and runs the
                # `default` profile (root-workspace unit + integration only,
                # high concurrency, no real-crate/GUI cost) — the right size
                # for a per-commit gate; `nu .config/scripts/nextest-suite.nu
                # --all` is the full end-to-end suite for CI, not this hook.
                entry = "${nixPackages.nushell}/bin/nu .config/scripts/nextest-suite.nu";
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
                  cargo = rustToolchainHooks;
                  clippy = rustToolchainHooks;
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
          helpers = helpersFor nixPackages fenixPackages;
          inherit (helpers) rustToolchainDev;

          gitHookConfiguration = self.checks.${systemArchitecture}.preCommitGitHooks;

          # Build script PATH: fenix rustc + nix package manager + system paths.
          # Consumed by build/third-party/defs.bzl via read_config("build","devshell_bin").
          devshellBin = nixPackages.lib.concatStringsSep ":" [
            "${rustToolchainDev}/bin"
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
          mkDevshellCommand =
            cmdName:
            nixPackages.writeTextFile {
              name = "${cmdName}-nuenv";
              destination = "/bin/${cmdName}";
              executable = true;
              text = ''
                #!/bin/sh
                cd "$PRJ_ROOT" && exec ${nixPackages.nushell}/bin/nu .config/scripts/${cmdName}.nu "$@"
              '';
            };

          nuScriptCommands = [
            "build"
            "build-release"
            "check"
            "clean"
            "create-notes"
            "doc"
            "doc-open"
            "fmt"
            "fmt-check"
            "install"
            "install-force"
            "lint"
            "lint-fix"
            "patch"
            "release"
            "run"
            "run-release"
            "sync-deps"
            "test"
            "test-with"
            "test-all"
            "full-check"
            "recheck"
            "update"
            "buck-build"
            "buck-test"
            "ra-index"
            "snowydeer-import"
            "build-compiler-image"
          ];

          commandPackages = map mkDevshellCommand nuScriptCommands;
        in
        {
          default = nixPackages.mkShell {
            name = "NuNuShell";

            RUSTC_BOOTSTRAP = "1";
            LIBRARY_PATH = "${nixPackages.libiconv}/lib";
            # `nudox-producer-clang` (workspace/compiler/languages/clang) links
            # `clang-sys` with its `runtime` feature: libclang is `dlopen`'d at
            # first use, not linked at build time, so no binary that merely
            # links the crate can abort at process load — see that crate's
            # Cargo.toml. `LIBCLANG_PATH` still governs *which* libclang the
            # dlopen finds; pinning it to the flake's own nixpkgs derivation
            # (rather than leaving discovery to fall back to whatever Xcode
            # Command Line Tools / system package manager happens to be
            # installed) is what makes that discovery reproducible across
            # machines instead of an unstated assumption about the host.
            LIBCLANG_PATH = "${nixPackages.libclang.lib}/lib";
            MAIN_PACKAGE = "nudox";
            OUTPUT_DIRECTORY = "dist";
            OPENSSL_DIR = "${nixPackages.openssl.dev}";
            OPENSSL_LIB_DIR = "${nixPackages.openssl.out}/lib";
            OPENSSL_INCLUDE_DIR = "${nixPackages.openssl.dev}/include";
            DOTNET_CLI_TELEMETRY_OPTOUT = "1";
            DOTNET_NOLOGO = "1";
            DOTNET_SKIP_FIRST_TIME_EXPERIENCE = "1";

            packages =
              commandPackages
              ++ [
                rustToolchainDev
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
                libclang.lib
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

            shellHook = ''
              export PRJ_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo "$PWD")"

              export LD_LIBRARY_PATH="${nixPackages.openssl.out}/lib:$LD_LIBRARY_PATH"
              export DOTNET_CLI_HOME="$TMPDIR/dotnet"

              # Sealed-producer PATH prefix (see sandbox::ToolchainSet). Hermetic
              # PATH alone is /usr/bin:/bin:/nix/var/nix/profiles/default/bin —
              # language oracles need go/javadoc/dotnet from the Nix store.
              export NUDOX_TOOLCHAIN_PATH="${nixPackages.go}/bin:${nixPackages.jdk21_headless}/bin:${nixPackages.dotnetCorePackages.sdk_10_0}/bin''${NUDOX_TOOLCHAIN_PATH:+:$NUDOX_TOOLCHAIN_PATH}"

              if command -v kittysay > /dev/null 2>&1; then
                kittysay --think "the nu is the now" | dotacat
              fi

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

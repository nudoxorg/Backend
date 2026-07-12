{
  description = "NuNuShell development environment";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # NuDox shared build library (input-less flake in the deploy repo). Gives
    # lib.build.rustService so the production build recipe lives here rather
    # than in nixos/nix/pkgs/backend.nix. Pinned by remote for reproducibility;
    # for local iteration before it's pushed:
    #   nix build --override-input nixos path:../nixos
    nixos.url = "git+https://dev.nudox.org/git/Nudox/MachineConfigurations.git";

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

    buck2-prelude = {
      url = "github:facebookincubator/buck2-prelude?rev=4b374e200a64838660463994b079899b5094689a";
      flake = false;
    };

    # snowydeer — Buck2 → Nix store deploy boundary (Phase 4). NAR-packs a Buck2
    # output, ripgrep reference-scans the closure, and imports it content-addressed
    # via Lix >=2.95 `--references-list-json`/import_ca. Vendored as a source input
    # so snowydeer/package.bzl + snowydeer/snowydeer.bxl can be wired against its
    # upstream cells (`//toolchains//nix/nix_build.bzl`, `//constraints/link_style`,
    # build_store_path.py). Not yet consumed by a Buck2 external cell — see the
    # TODOs in snowydeer/snowydeer.bxl and PLANS.md for the remaining wiring.
    snowydeer = {
      url = "github:MercuryTechnologies/snowydeer";
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
      devshell,
      buck2-prelude,
      snowydeer,
    }:
    let
      # Everything that Nix supports right now
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      # Pre-built buck2 binaries pinned to the release matching our prelude.
      # Avoids both buckle's runtime download and a full Rust source build.
      buck2Version = "2026-07-01";
      buck2Artifacts = {
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

      mkBuck2 =
        pkgs:
        let
          art = buck2Artifacts.${pkgs.stdenv.hostPlatform.system};
        in
        pkgs.stdenvNoCC.mkDerivation {
          pname = "buck2";
          version = buck2Version;
          src = pkgs.fetchurl {
            url = "https://github.com/facebook/buck2/releases/download/${buck2Version}/buck2-${art.platform}.zst";
            hash = art.hash;
          };
          nativeBuildInputs = [ pkgs.zstd ];
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/bin
            zstd -d $src -o $out/bin/buck2
            chmod +x $out/bin/buck2
          '';
          meta.mainProgram = "buck2";
        };

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
      # Production build of the backend, exposed as a getFlake-consumable
      # `packages.<system>.default`. Two build paths feed it:
      #
      #   * buck2/snowydeer (the real current build) — see apps.<system>.buck2
      #     below. buck2 itself can't run inside a `nix build` sandbox (daemon,
      #     network, mutable prelude FS), so we DON'T try to compile it here.
      #     Instead the artifact is built imperatively (in CI, via the app),
      #     imported content-addressed into the Nix store, and pushed to the
      #     self-hosted Attic cache (cache.nudox.org). CI records the resulting
      #     per-system store path in nix/buck2-artifact.json, and
      #     `builtins.fetchClosure` below turns that cached path back into a
      #     normal derivation — PURE eval, no buck2 at eval or build time, just
      #     a signature-verified substitution. THIS is what makes the buck2
      #     build getFlake-consumable by nixos/app-flakes.nix.
      #
      #   * cargo (rustService) — the legacy source build, kept as a fallback
      #     on revs that still carry a committed Cargo.lock (pre-Buck2 revs, or
      #     branches that scaffold the manifests). rustService wants a
      #     fenix-overlaid pkgs.
      #
      # Precedence: prefer the pinned buck2 artifact; fall back to cargo when
      # there's no pin for this system but a lockfile is present.
      packages = eachSystem (
        { system, ... }:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };

          # nix/buck2-artifact.json schema:
          #   { "fromStore": "<attic substituter URL>",
          #     "paths": { "<system>": "/nix/store/…-nudox_pkg", … } }
          # Empty `paths` (the committed default) → no pin yet → buck2Artifact
          # is null and the flake falls back to cargo, still evaluating cleanly.
          # CI (nixos scripts.backendBuck2) rewrites this after `attic push`.
          buck2Pin = builtins.fromJSON (builtins.readFile ./nix/buck2-artifact.json);
          buck2Path = buck2Pin.paths.${system} or null;
          buck2Artifact =
            if buck2Path == null then
              null
            else
              builtins.fetchClosure {
                fromStore = buck2Pin.fromStore;
                fromPath = buck2Path;
                # snowydeer imports content-addressed (`nix store add-path`), so
                # the bare CA form { fromStore, fromPath } applies. If a pin is
                # ever input-addressed instead, that entry needs
                # `inputAddressed = true`.
              };

          # This repo has migrated to Buck2: the Rust `Cargo.toml`/`Cargo.lock`
          # are gitignored and scaffolded at runtime (see .gitignore), so they
          # are absent from most revs' source trees. Only expose the cargo
          # package on revs that actually carry a committed Cargo.lock —
          # otherwise `nix flake check`/eval would fail reading a missing lock.
          hasCargoLock = builtins.pathExists (./. + "/Cargo.lock");
          cargoBackend = nixos.lib.build.rustService { inherit pkgs; } {
            pname = "nudox-backend";
            version = self.rev or "dev";
            src = ./.;
            cargoPackage = "nudox";
            mainProgram = "nudox";
            description = "NuDox backend compiler and ingestion API";
          };
        in
        {
          # Kept for `nix shell` compatibility.
          devshell = self.devShells.${system}.default;
        }
        // nixpkgs.lib.optionalAttrs hasCargoLock {
          # Source-compiled backend (only on revs that still carry Cargo.lock).
          cargo = cargoBackend;
        }
        // nixpkgs.lib.optionalAttrs (buck2Path != null) {
          # The real production artifact, substituted from Attic via the pin.
          # NB: guard on buck2Path (a string), never on buck2Artifact — comparing
          # the fetchClosure result to null would force it, contacting the
          # substituter just to decide attribute presence.
          buck2 = buck2Artifact;
        }
        // nixpkgs.lib.optionalAttrs (buck2Path != null || hasCargoLock) {
          default = if buck2Path != null then buck2Artifact else cargoBackend;
        }
      );

      # apps.buck2 — non-hermetic buck2/snowydeer build entry point.
      #
      # Run with:  nix run .#buck2
      # (or from the nixos CI driver that knows about this app)
      #
      # The script must be run from the repo root. It:
      #   1. Ensures the prelude symlink is in place (same logic as the devshell
      #      shellHook) so buck2 can resolve the prelude cell.
      #   2. Invokes the snowydeer BXL against //workspace/server:nudox_pkg.
      #   3. Prints the resulting /nix/store/… path to stdout.
      #
      # This app is the PRODUCER of the artifact that `packages.default`
      # consumes. In CI (nixos scripts.backendBuck2) the printed store path is
      # pushed to the Attic cache and recorded in nix/buck2-artifact.json;
      # packages.default then substitutes it purely via builtins.fetchClosure
      # (see the packages block above). So the buck2 build IS getFlake-consumable
      # — just through a build→push→pin→fetch handoff rather than a direct
      # `nix build` (which a buck2 daemon + network + mutable prelude FS can't do
      # inside a sandbox). The build is reproducible (same source → same
      # content-addressed store path); CI runs this with Nix store write access
      # and buck2 daemon privileges.
      apps = eachSystem (
        { pkgs, system, ... }:
        let
          buck2-bin = mkBuck2 pkgs;
          patchedBuck2Prelude =
            pkgs.runCommand "buck2-prelude-patched"
              {
                nativeBuildInputs = [ pkgs.python3 ];
              }
              ''
                cp -a ${buck2-prelude}/. "$out"
                chmod -R u+w "$out"
                python3 ${./build/patch-prelude.py} "$out"
              '';
          bxlScript = pkgs.writeShellApplication {
            name = "nudox-buck2-build";
            runtimeInputs = [ buck2-bin ];
            text = ''
              # Must be run from the repo root (where .buckconfig lives).
              if [[ ! -f .buckconfig ]]; then
                echo "ERROR: run from the Backend repo root (no .buckconfig found)" >&2
                exit 1
              fi

              # Wire the prelude cell: patched derivation lives in the Nix store.
              ln -sfn ${patchedBuck2Prelude} prelude

              echo "nudox-buck2-build: invoking snowydeer BXL for //workspace/server:nudox_pkg" >&2
              buck2 bxl //snowydeer:snowydeer.bxl:main -- --target //workspace/server:nudox_pkg
            '';
          };
        in
        {
          buck2 = {
            type = "app";
            program = "${bxlScript}/bin/nudox-buck2-build";
          };
        }
      );

      checks = eachSystem (
        {
          pkgs,
          system,
          fenix-pkg,
          ...
        }:
        let
          # Nightly enables a lot of nice things, but mainly it allows us to build with rustfmt
          rust-nightly = fenix-pkg.complete.withComponents [
            "cargo"
            "clippy"
            "rustc"
            "rustfmt"
            "rustc-codegen-cranelift-preview"
          ];
        in
        # `nix flake check` compiles the backend from source when a cargo build
        # exists on this rev. It deliberately checks `cargo`, not `default`: the
        # buck2 `default` is a fetchClosure of a pre-built Attic artifact, and
        # `nix flake check` should exercise a real source build, not re-substitute
        # a cached closure (which would also need network + the pin present).
        nixpkgs.lib.optionalAttrs (self.packages.${system} ? cargo) {
          package = self.packages.${system}.cargo;
        }
        // {
          pre-commit-check = git-hooks.lib.${system}.run {
            src = ./.;
            package = pkgs.prek; # Prek for parellelizism
            default_stages = [ "pre-push" ];
            hooks = {
              # We have convco here to enforce commit messages
              # The goal here is to NOT slow down dev productivity
              # So we're encouraging a workflow where, if you're in "flow"
              # Just don't worry about pushing. The annoying part can hit then
              convco = {
                enable = true;
                pass_filenames = false;
                entry = toString (
                  pkgs.writeShellScript "convco-pre-push" ''
                    while read local_ref local_sha remote_ref remote_sha; do
                      ${pkgs.convco}/bin/convco check "$remote_sha..$local_sha"
                    done
                  ''
                );
                stages = [ "pre-push" ];
              };

              # Formatting is PURELY for QOL
              # Consistency is key for building patterns, and to that end, a priority should be enabling reliable dev setups so this doesn't trip up on pre-push
              nixfmt.enable = true;

              rustfmt = {
                enable = true;
                packageOverrides.cargo = rust-nightly;
                packageOverrides.rustfmt = rust-nightly;
              };

              markdownfmt = {
                enable = true;
                name = "hongdown";
                entry = "hongdown --write";
                files = "\\.md$";
                language = "system";
              };

              # The main branch needs to always be green
              # Both passing all tests and avoiding any clippy lints
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
                packageOverrides.cargo = rust-nightly;
                packageOverrides.clippy = rust-nightly;
                stages = [
                  "pre-merge-commit"
                  "pre-push"
                ];
              };
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
            # rust-analyzer component ships libexec/rust-analyzer-proc-macro-srv
            # so ra_ap_load_cargo can use ProcMacroServerChoice::Sysroot.
            "rustfmt"
            "rustc-codegen-cranelift-preview"
          ];
          hooks = self.checks.${system}.pre-commit-check;
          patchedBuck2Prelude =
            pkgs.runCommand "buck2-prelude-patched"
              {
                nativeBuildInputs = [ pkgs.python3 ];
              }
              ''
                cp -a ${buck2-prelude}/. "$out"
                chmod -R u+w "$out"
                python3 ${./build/patch-prelude.py} "$out"
              '';

          # Sourcing from nushell for our commands
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
                name = "RUSTC_BOOTSTRAP";
                value = "1";
              }
              {
                # TODO: See if there's a more reliable way to avoid this, just a linker issue I started facing
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
                value = "${pkgs.openssl.out}/lib:$LD_LIBRARY_PATH";
              }
              {

                name = "OPENSSL_DIR";
                value = "${pkgs.openssl.dev}";
              }
              {
                name = "OPENSSL_LIB_DIR";
                value = "${pkgs.openssl.out}/lib";
              }
              {
                name = "OPENSSL_INCLUDE_DIR";
                value = "${pkgs.openssl.dev}/include";
              }
            ];

            motd = ''
              $($(type -p kittysay) --think "the nu is the now" | dotacat)
            '';

            packages = builtins.filter (x: x != null) [
              rust-nightly # Rust nightly toolchain
              pkgs.git # Version control
              pkgs.cargo-bump # Bump crate versions
              pkgs.rust-analyzer # Rust LSP server
              pkgs.flock # For managing shell concurrency
              pkgs.nixfmt # Nix formatter
              pkgs.tombi # TOML formatter/linter
              pkgs.typos # Source code spell checker
              pkgs.hongdown # Markdown formatting
              pkgs.radicle-node # P2P code collaboration
              pkgs.radicle-tui # Radicle terminal UI
              pkgs.kittysay # Cat ASCII art
              pkgs.marksman # Markdown LSP server
              pkgs.taplo # TOML LSP/formatter
              pkgs.cargo-nextest # Next-gen test runner
              pkgs.libiconv # Character encoding library, associated with linker error
              pkgs.nil # Nix LSP server
              pkgs.jsonfmt # JSON formatting
              pkgs.dotacat # Colorful terminal output
              pkgs.goreleaser
              pkgs.cuelsp
              pkgs.b3sum
              pkgs.go # needed by system_go_toolchain for the Go oracle producer
              pkgs.jdk21_headless # Java 21 — prelude javacd sources require SourceVersion.RELEASE_21
              (mkBuck2 pkgs)
              (if pkgs.stdenv.isLinux then pkgs.wild-unwrapped else null) # Fast linker (RUST), only works with clang for now
              (if pkgs.stdenv.isLinux then pkgs.openssl else null) # Fast linker (RUST), only works with clang for now
              (if pkgs.stdenv.isLinux then pkgs.clang else null)
            ];
            commands = [
              # --- Build & Check --- #
              (mkCommand "check" "Check workspace for compilation and syntax errors" "build")
              (mkCommand "build" "Build workspace in debug mode" "build")
              (mkCommand "build-release" "Build workspace in release mode" "build")
              {
                name = "ra-index";
                help = "Generate rust-project.json for rust-analyzer (run once after dep changes)";
                category = "build";
                command = "bash build/gen-rust-project.sh";
              }

              # --- Packaging --- #
              {
                name = "release";
                help = "Complete release pipeline using GoReleaser (snapshot, single-target)";
                category = "packaging";
                command = "nu .config/scripts/release.nu";
              }

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
              (mkCommand "patch" "Update or create a patch from a branch" "maintenance")

              # --- Installation --- #
              (mkCommand "install" "Build and install binary to system" "installation")
              (mkCommand "install-force" "Force install binary" "installation")

              # --- Buck2 --- #
              (mkCommand "buck-build" "Build Buck2 targets (omit package for //..., or pass e.g. compiler)"
                "buck2"
              )
              (mkCommand "buck-test" "Run Buck2 tests (omit package for //..., or pass e.g. compiler)" "buck2")

              # --- Utilities --- #
              (mkCommand "rad-sync" "manually sync radicle repos" "utilities")
            ];
            devshell.startup.shellHook.text = ''
                            # Patched prelude lives in the Nix store; just symlink it.
                            ln -sfn ${patchedBuck2Prelude} "$PRJ_ROOT/prelude"
                            # Write .buckconfig.local with absolute Nix store paths for Go and Java.
                            # This survives daemon restarts (no PATH dependency) and is gitignored.
                            cat > "$PRJ_ROOT/.buckconfig.local" <<'BCFG'
              [go]
                go_binary = ${pkgs.go}/bin/go
              [java]
                java_home = ${pkgs.jdk21_headless}
              BCFG
                            export RUST_TARGET=$(rustc --version --verbose | grep '^host:' | awk '{print $2}')
                            # sccache intercepts rustc --version as a non-compilation call and returns empty output,
                            # breaking Buck2 build scripts (e.g. rustversion). Buck2 has its own caching.
                            unset RUSTC_WRAPPER
                            # Expose Go and Java binaries to the hermetic oracle sandbox PATH.
                            # ToolchainSet::from_env() reads NUDOX_TOOLCHAIN_PATH (colon-separated) and
                            # prepends it to the fixed hermetic PATH inside IsolatedCommand, so the Go oracle
                            # (which internally invokes `go list`) and the Java oracle (which spawns `javadoc`)
                            # can find their tools even under the sealed environment.
                            export NUDOX_TOOLCHAIN_PATH="${pkgs.go}/bin:${pkgs.jdk21_headless}/bin"
                            export NUDOX_TOOLCHAIN_JAVA_HOME="${pkgs.jdk21_headless}"
                            ${hooks.shellHook}
                            (
                              # Use a lockfile to prevent multiple instances from stomping on Git
                              flock -n 9 || exit 1

                            ) 9>/tmp/nunu_sync.lock &
            '';
          };
        }
      );

      formatter = eachSystem ({ pkgs, ... }: pkgs.nixfmt-rfc-style);
    };
}

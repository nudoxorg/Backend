{
  description = "Pinned workspace2 quality environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/0bb7ec54c8483066ec9d7720e780a5caa71f8612";
    fenix = {
      url = "github:nix-community/fenix/8d20dd64ad45ed4b5179a37cec75fff9f732e78d";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      ...
    }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      eachSystem =
        function:
        nixpkgs.lib.genAttrs systems (
          system:
          function {
            pkgs = import nixpkgs { inherit system; };
            fenixPackages = fenix.packages.${system};
          }
        );
    in
    {
      devShells = eachSystem (
        { pkgs, fenixPackages }:
        let
          stableRaw =
            (fenixPackages.toolchainOf {
              channel = "1.97.1";
              sha256 = "sha256-A1abGIbOtcBSdrUMhDGrER3pRM1hQP4fp9gh3Y4PKc8=";
            }).withComponents
              [
                "cargo"
                "clippy"
                "rustc"
                "rustfmt"
              ];
          dylintNightlyRaw =
            (fenixPackages.toolchainOf {
              channel = "nightly";
              date = "2026-05-28";
              sha256 = "sha256-rNsOYVHiSXXSDRGdg/StkiKCsyCTEPBfsP3R9spCu1c=";
            }).withComponents
              [
                "cargo"
                "llvm-tools-preview"
                "rustc"
                "rustc-dev"
                "rustfmt"
              ];
          dylintSource = pkgs.fetchFromGitHub {
            owner = "trailofbits";
            repo = "dylint";
            rev = "09bf11417d8cfc4d0c2ef9053898c6a9f378f794";
            hash = "sha256-CROuPpPzUobUcH3Xl2fpEOVxEBBppmFBaJSRXsEuaXg=";
          };
          dylintManifest = builtins.fromTOML (builtins.readFile (dylintSource + "/Cargo.toml"));
          dylintDriverWorkspace = (pkgs.formats.toml { }).generate "Cargo.toml" {
            workspace = {
              members = [ "internal" ];
              resolver = dylintManifest.workspace.resolver;
              dependencies = dylintManifest.workspace.dependencies;
              lints = dylintManifest.workspace.lints;
            };
          };
          qualityLockFiles = [
            ./Cargo.lock
            ./tools/dylint/Cargo.lock
            (dylintSource + "/Cargo.lock")
            (dylintSource + "/driver/Cargo.lock")
          ];
          qualityPackages = builtins.attrValues (
            builtins.listToAttrs (
              map
                (package: {
                  name = builtins.hashString "sha256" (
                    "${package.name}|${package.version}|${package.source or "local"}"
                  );
                  value = package;
                })
                (
                  pkgs.lib.concatMap (
                    lockFile: (builtins.fromTOML (builtins.readFile lockFile)).package
                  ) qualityLockFiles
                )
            )
          );
          qualityLock = (pkgs.formats.toml { }).generate "nudox-quality-Cargo.lock" {
            version = 4;
            package = qualityPackages;
          };
          qualityCargoRegistry = pkgs.rustPlatform.importCargoLock {
            lockFile = qualityLock;
            outputHashes = {
              "clippy_utils-0.1.98" = "sha256-eapzfPvyUcGAtl6RsImvQl4FsIYYjW1Hl/hjFqbLk5Y=";
              "gpui-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
              "gpui_ce_util-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
              "gpui_collections-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
              "gpui_derive_refineable-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
              "gpui_macros-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
              "gpui_media-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
              "gpui_refineable-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
              "gpui_scheduler-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
              "gpui_shared_string-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
              "gpui_sum_tree-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            };
          };
          qualityCargoDeps = pkgs.symlinkJoin {
            name = "nudox-cargo-vendor";
            paths = [ qualityCargoRegistry ];
            postBuild = ''
              rm "$out/dylint-6.0.4"
              cp -RL "${qualityCargoRegistry}/dylint-6.0.4" "$out/dylint-6.0.4"
              chmod -R u+w "$out/dylint-6.0.4"
              substituteInPlace "$out/dylint-6.0.4/src/driver_builder.rs" \
                --replace-fail 'components = ["llvm-tools-preview", "rustc-dev"]' \
                '# components supplied by the pinned Fenix toolchain'
              ln -s ${dylintDriverWorkspace} "$out/Cargo.toml"
              for sourceName in driver internal; do
                cp -R "${dylintSource}/$sourceName" "$out/$sourceName"
                chmod -R u+w "$out/$sourceName"
                printf '{"files":{},"package":null}\n' > "$out/$sourceName/.cargo-checksum.json"
              done
            '';
          };
          qualityCargoConfig = pkgs.writeText "nudox-cargo-config.toml" ''
            [source.crates-io]
            replace-with = "vendored-sources"

            [source.vendored-sources]
            directory = "${qualityCargoDeps}"

            [source."git+https://github.com/gpui-ce/gpui-ce?rev=d435891f47743d96bfc6d4ab74c9ecd05af2603e#d435891f47743d96bfc6d4ab74c9ecd05af2603e"]
            git = "https://github.com/gpui-ce/gpui-ce"
            rev = "d435891f47743d96bfc6d4ab74c9ecd05af2603e"
            replace-with = "vendored-sources"

            [source."git+https://github.com/rust-lang/rust-clippy?rev=9fca3bc9fc2bc83c60bde26d18ed68f11564b228#9fca3bc9fc2bc83c60bde26d18ed68f11564b228"]
            git = "https://github.com/rust-lang/rust-clippy"
            rev = "9fca3bc9fc2bc83c60bde26d18ed68f11564b228"
            replace-with = "vendored-sources"
          '';
          withPinnedCargoSources =
            name: toolchain:
            pkgs.symlinkJoin {
              inherit name;
              paths = [ toolchain ];
              nativeBuildInputs = [ pkgs.makeWrapper ];
              postBuild = ''
                wrapProgram "$out/bin/cargo" --add-flags "--config ${qualityCargoConfig}"
              '';
            };
          stable = withPinnedCargoSources "nudox-stable-toolchain" stableRaw;
          dylintNightly = withPinnedCargoSources "nudox-dylint-toolchain" dylintNightlyRaw;
          dylintTools = pkgs.rustPlatform.buildRustPackage {
            pname = "dylint-tools";
            version = "6.0.4";
            src = dylintSource;
            cargoHash = "sha256-9YAYtVoqMfyeG5sy8Jtt8a894k9AzyhIDEFzyqdzyeI=";
            cargoBuildFlags = [
              "-p"
              "cargo-dylint"
              "-p"
              "dylint-link"
            ];
            doCheck = false;
          };
        in
        {
          quality = pkgs.mkShell {
            packages = [
              dylintTools
              pkgs.clang
              pkgs.curl
              pkgs.jq
              pkgs.libiconv
              pkgs.qdrant
              pkgs.rustup
              pkgs.zlib
            ];
            NUDOX_STABLE_TOOLCHAIN = stable;
            NUDOX_DYLINT_TOOLCHAIN = dylintNightly;
            NUDOX_CARGO_CONFIG = qualityCargoConfig;
            NUDOX_CARGO_VENDOR = qualityCargoDeps;
            LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.libiconv
              pkgs.zlib
            ];
          };
          default = self.devShells.${pkgs.stdenv.hostPlatform.system}.quality;
        }
      );
    };
}

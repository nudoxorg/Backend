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
    { self, nixpkgs, fenix, ... }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      eachSystem = function:
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
          stable =
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
          dylintNightly =
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
          dylintTools = pkgs.rustPlatform.buildRustPackage {
            pname = "dylint-tools";
            version = "6.0.4";
            src = pkgs.fetchFromGitHub {
              owner = "trailofbits";
              repo = "dylint";
              rev = "09bf11417d8cfc4d0c2ef9053898c6a9f378f794";
              hash = "sha256-CROuPpPzUobUcH3Xl2fpEOVxEBBppmFBaJSRXsEuaXg=";
            };
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
              pkgs.libiconv
              pkgs.rustup
              pkgs.zlib
            ];
            NUDOX_STABLE_TOOLCHAIN = stable;
            NUDOX_DYLINT_TOOLCHAIN = dylintNightly;
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

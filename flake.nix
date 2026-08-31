# Pins the complete native development environment.
# Supplies every compiler and service exercised by the product crates.
# Keeps toolchain selection reproducible without repository-local scripts.
{
  description = "Backend development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/0bb7ec54c8483066ec9d7720e780a5caa71f8612";
    fenix = {
      url = "github:nix-community/fenix/8d20dd64ad45ed4b5179a37cec75fff9f732e78d";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
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
      eachSystem = function: nixpkgs.lib.genAttrs systems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          rust = (fenix.packages.${system}.toolchainOf {
            channel = "1.97.1";
            sha256 = "sha256-A1abGIbOtcBSdrUMhDGrER3pRM1hQP4fp9gh3Y4PKc8=";
          }).withComponents [
            "cargo"
            "clippy"
            "rustc"
            "rustfmt"
          ];
        in
        function { inherit pkgs rust; }
      );
    in
    {
      devShells = eachSystem (
        { pkgs, rust }:
        let
          development = pkgs.mkShell {
            packages = [
              rust
              pkgs.clang
              pkgs.curl
              pkgs.dotnet-sdk_8
              pkgs.go
              pkgs.jdk
              pkgs.libiconv
              pkgs.nodejs_22
              pkgs.python3
              pkgs.qdrant
              pkgs.typescript
              pkgs.zlib
            ];
            COMPILER_CSHARP_COMPILER = "${pkgs.dotnet-sdk_8}/bin/dotnet";
            COMPILER_GO_COMPILER = "${pkgs.go}/bin/go";
            COMPILER_JAVA_COMPILER = "${pkgs.jdk}/bin/javac";
            COMPILER_TYPESCRIPT_COMPILER = "${pkgs.typescript}/bin/tsc";
            LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.libiconv
              pkgs.zlib
            ];
          };
        in
        {
          default = development;
          development = development;
        }
      );
    };
}

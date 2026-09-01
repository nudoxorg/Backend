# Collects every executable admitted to repository tooling and development.
# Keeps optional platform tools explicit and prevents ambient PATH discovery.
# Supplies one source of truth to shells, commands, and checks.
{ pkgs, toolchains }:
let
  cargoDylint = pkgs.rustPlatform.buildRustPackage rec {
    pname = "cargo-dylint";
    version = "6.0.4";
    src = pkgs.fetchFromGitHub {
      owner = "trailofbits";
      repo = "dylint";
      rev = "v${version}";
      hash = "sha256-CROuPpPzUobUcH3Xl2fpEOVxEBBppmFBaJSRXsEuaXg=";
    };
    cargoHash = "sha256-9YAYtVoqMfyeG5sy8Jtt8a894k9AzyhIDEFzyqdzyeI=";
    cargoBuildFlags = [
      "--package"
      "cargo-dylint"
      "--no-default-features"
      "--features"
      "cargo-cli"
    ];
    cargoInstallFlags = cargoBuildFlags;
    doCheck = false;
  };
  nativeCompilers = [
    pkgs.clang
    pkgs.dotnet-sdk_8
    pkgs.go
    pkgs.jdk
    pkgs.nodejs_22
    pkgs.python3
    pkgs.typescript
  ];
  qualityTools = [
    pkgs.ast-grep
    pkgs.cargo-audit
    pkgs.cargo-deny
    pkgs.cargo-nextest
    pkgs.direnv
    pkgs.git
    pkgs.jq
    pkgs.koji
    pkgs.nixfmt
    pkgs.nix-direnv
    pkgs.nufmt
    pkgs.nushell
    pkgs.taplo
  ];
  verifierTools = [
    pkgs.cargo-bloat
    cargoDylint
    pkgs.cargo-llvm-cov
    pkgs.hyperfine
    pkgs.samply
  ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.valgrind ];
  serviceTools = [
    pkgs.curl
    pkgs.qdrant
  ];
in
{
  inherit
    nativeCompilers
    qualityTools
    serviceTools
    verifierTools
    ;
  development = [ toolchains.stable ] ++ nativeCompilers ++ qualityTools ++ serviceTools;
  verification = [ toolchains.nightly ] ++ verifierTools;
  nixFormatter = pkgs.nixfmt;
}

# Collects every executable admitted to repository tooling and development.
# Keeps optional platform tools explicit and prevents ambient PATH discovery.
# Supplies one source of truth to shells, commands, and checks.
{ pkgs, toolchains }:
let
  dylintSource = pkgs.fetchFromGitHub {
    owner = "trailofbits";
    repo = "dylint";
    rev = "v6.0.4";
    hash = "sha256-CROuPpPzUobUcH3Xl2fpEOVxEBBppmFBaJSRXsEuaXg=";
  };
  mkDylintTool =
    package:
    pkgs.rustPlatform.buildRustPackage {
      pname = package;
      version = "6.0.4";
      src = dylintSource;
      cargoDeps = cargoDylint.cargoDeps;
      cargoBuildFlags = [
        "--package"
        package
      ];
      cargoInstallFlags = [
        "--package"
        package
      ];
      doCheck = false;
    };
  cargoDylint = pkgs.rustPlatform.buildRustPackage rec {
    pname = "cargo-dylint";
    version = "6.0.4";
    src = dylintSource;
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
  dylintLink = mkDylintTool "dylint-link";
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
    pkgs.coreutils
    pkgs.direnv
    pkgs.git
    pkgs.jq
    pkgs.koji
    pkgs.nixfmt
    pkgs.nix-direnv
    pkgs.nufmt
    pkgs.nushell
    pkgs.taplo
    pkgs.yamlfmt
  ];
  verifierTools = [
    pkgs.cargo-bloat
    cargoDylint
    dylintLink
    pkgs.cargo-llvm-cov
    pkgs.hyperfine
    pkgs.samply
  ]
  ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.valgrind ];
  serviceTools = [
    pkgs.curl
    pkgs.qdrant
  ];
  observabilityTools = [
    pkgs.otel-cli
    pkgs.otel-desktop-viewer
    pkgs.opentelemetry-collector-contrib
  ];
in
{
  inherit
    cargoDylint
    dylintLink
    nativeCompilers
    qualityTools
    serviceTools
    observabilityTools
    verifierTools
    ;
  development = [ toolchains.stable ] ++ qualityTools;
  compiler = [ toolchains.stable ] ++ qualityTools ++ nativeCompilers;
  services = [ toolchains.stable ] ++ qualityTools ++ serviceTools;
  observability = [ toolchains.stable ] ++ qualityTools ++ observabilityTools;
  verification = [
    toolchains.stable
    toolchains.nightly
  ]
  ++ qualityTools
  ++ verifierTools;
  complete = [
    toolchains.stable
    toolchains.nightly
  ]
  ++ qualityTools
  ++ nativeCompilers
  ++ serviceTools
  ++ observabilityTools
  ++ verifierTools;
  nixFormatter = pkgs.nixfmt;
}

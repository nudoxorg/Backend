# Flake check: the cargo-bundle wrap contract, without building the GUI.
#
# `lindsey-bundle` wraps cargo-bundle so a packaged .app carries the Go/Java/C#
# oracles, toolchains, and embed model. Building the real GUI lives in
# `nix build .#lindsey-app`. The wrap is a post-processing step over any
# Contents/MacOS/lindsey, so a fake .app is the honest unit of the contract.
{
  pkgs,
  mkNuCheck,
  nuLib,
  goOracle,
  javaOracle,
  csharpOracle,
  semanticModel,
  installWrapper,
  macosWrapper,
  envNames,
  toolchainPath,
  goToolchain,
  dotnet,
}:

mkNuCheck {
  name = "lindsey-bundle";
  script = ./check.nu;
  inherit nuLib;

  runtimeInputs = [
    pkgs.coreutils
    pkgs.gnugrep
    pkgs.jq
    goToolchain
    dotnet
  ];

  env = {
    NUDOX_INSTALL_WRAPPER = "${installWrapper}/bin/lindsey-install-wrapper";
    NUDOX_MACOS_WRAPPER = toString macosWrapper;
    NUDOX_GO_ORACLE = toString goOracle;
    NUDOX_JAVA_ORACLE = toString javaOracle;
    NUDOX_CSHARP_ORACLE_PKG = toString csharpOracle;
    NUDOX_EMBED_MODEL = toString semanticModel;
    NUDOX_ENV_GO = envNames.goOracle;
    NUDOX_ENV_JAVA = envNames.javaOracle;
    NUDOX_ENV_CSHARP = envNames.csharpOracle;
    NUDOX_ENV_DOTNET = envNames.dotnet;
    NUDOX_ENV_MODEL = envNames.embedModel;
    NUDOX_ENV_LIBCLANG = envNames.libclang;
    NUDOX_TOOLCHAIN_PATH = toolchainPath;
  };

  passthruAttrs = {
    inherit
      goOracle
      javaOracle
      csharpOracle
      semanticModel
      ;
  };

  preferLocalBuild = true;
  allowSubstitutes = false;

  resultLines = [
    "lindsey-bundle: ok"
    "suite: wrap contract, subprocess oracle env, go/csharp oracle schema emit"
  ];
}

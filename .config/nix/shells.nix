# Defines the direnv development shell and its reproducible process contract.
# Routes compiler output and tool configuration into repository-local locations.
# Publishes native compiler authorities without embedding them in client binaries.
{
  pkgs,
  tools,
  toolchains,
  commands,
}:
let
  common = {
    CARGO_TARGET_DIR = "$PWD/.local/target";
    CLIPPY_CONF_DIR = "$PWD/.config";
    BACKEND_STABLE_CARGO = toolchains.stableCargo;
    BACKEND_RUSTFMT = toolchains.rustfmt;
    COMPILER_CSHARP_COMPILER = "${pkgs.dotnet-sdk_8}/bin/dotnet";
    COMPILER_GO_COMPILER = "${pkgs.go}/bin/go";
    COMPILER_JAVA_COMPILER = "${pkgs.jdk}/bin/javac";
    COMPILER_TYPESCRIPT_COMPILER = "${pkgs.typescript}/bin/tsc";
    LIBRARY_PATH = pkgs.lib.makeLibraryPath [
      pkgs.libiconv
      pkgs.zlib
    ];
  };
  development = pkgs.mkShell (
    common
    // {
      packages = tools.development ++ [ commands.backend ];
    }
  );
  verification = pkgs.mkShell (
    common
    // {
      packages = tools.development ++ tools.verification ++ [ commands.backendVerifier ];
      BACKEND_AGENT_ROLE = "terra-reviewer";
      BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
    }
  );
in
{
  default = development;
  inherit development verification;
}

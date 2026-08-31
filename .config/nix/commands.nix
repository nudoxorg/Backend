# Builds the single Nushell command surface from invariant-owned source parts.
# Injects only pinned tools and paths, leaving repository discovery to runtime.
# Statically checks the assembled script during every Nix package build.
{
  pkgs,
  tools,
  toolchains,
}:
let
  sourceParts = [
    ../nu/core/failure.nu
    ../nu/core/root.nu
    ../nu/core/process.nu
    ../nu/scope/git.nu
    ../nu/scope/cargo.nu
    ../nu/create/file.nu
    ../nu/create/crate.nu
    ../nu/quality/format.nu
    ../nu/quality/lint.nu
    ../nu/quality/test.nu
    ../nu/quality/observe.nu
    ../nu/quality/commit.nu
    ../nu/agents/generate.nu
    ../nu/main.nu
  ];
  source = builtins.concatStringsSep "\n\n" (map builtins.readFile sourceParts);
in
let
  makeBackend =
    runtimeEnv:
    pkgs.nuenv.writeShellApplication {
      name = "backend";
      text = source;
      runtimeInputs = tools.qualityTools ++ tools.serviceTools;
      runtimeEnv = {
        BACKEND_CONFIG = toString ../.;
        BACKEND_STABLE_CARGO = toolchains.stableCargo;
        BACKEND_RUSTFMT = toolchains.rustfmt;
      }
      // runtimeEnv;
    };
in
{
  backend = makeBackend { };
  backendVerifier = makeBackend {
    BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
  };
}

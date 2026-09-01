# Evaluates the repository formatter graph through treefmt's Nix module.
# Runs independent formatters in parallel while preserving changed-path selection.
# Keeps formatter binaries, include patterns, and exclusions statically validated.
{
  inputs,
  pkgs,
  toolchains,
}:
let
  evaluated = inputs.treefmt-nix.lib.evalModule pkgs {
    projectRootFile = "flake.nix";
    programs = {
      nixfmt.enable = true;
      taplo.enable = true;
      yamlfmt.enable = true;
    };
    settings = {
      global.excludes = [
        ".local/**"
        ".git/**"
      ];
      formatter = {
        nushell = {
          command = "${pkgs.nufmt}/bin/nufmt";
          includes = [ "*.nu" ];
        };
        rust = {
          command = toolchains.rustfmt;
          options = [
            "--edition"
            "2024"
            "--config-path"
            (toString ../rustfmt.toml)
          ];
          includes = [ "*.rs" ];
        };
      };
    };
  };
in
{
  inherit (evaluated.config.build) configFile wrapper;
  check = evaluated.config.build.check ../.;
}

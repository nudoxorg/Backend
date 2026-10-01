# Evaluates the repository formatter graph through treefmt's Nix module.
# Runs independent formatters in parallel while preserving changed-path selection.
# Keeps formatter binaries, include patterns, and exclusions statically validated.
{
  inputs,
  pkgs,
  toolchains,
  workspaceRoot,
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
        # A separate Cargo workspace (the index and compiler-worker programs,
        # not a member of the root [workspace]; see plan.md Track R). rustfmt
        # invoked per-file can't resolve its `mod` declarations against a
        # crate root it was never given.
        "workspace/**"
        # Third-party, patched in vendor/; not ours to reformat, and rustfmt
        # per-file can't resolve its `mod` declarations either.
        "vendor/**"
        # Raw parsing-corpus fixtures (a vendored serde_json snapshot, a
        # synthetic "repo" tree) for the semantics engine's tests. They are
        # test input data, not source we maintain, and some are deliberately
        # malformed and don't parse as Rust at all.
        "apps/facet/src/semantics/tests/fixtures/**"
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
            "${workspaceRoot}/.config/rustfmt.toml"
          ];
          includes = [ "*.rs" ];
        };
      };
    };
  };
in
{
  inherit (evaluated.config.build) configFile wrapper;
  check = evaluated.config.build.check workspaceRoot;
}

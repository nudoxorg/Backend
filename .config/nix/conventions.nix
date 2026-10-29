# Installs the shared NuDox git hooks (REPO-CONVENTIONS.md §3) in the development shell.
# Adds the six repo verbs (§4) as aliases onto the backend lane system.
# Keeps the lane system the source of truth for what each verb runs.
{
  inputs,
  pkgs,
  workspaceRoot,
  formatting,
  commands,
}:
let
  conventions = inputs.nixos.lib.conventions;
  backend = "${commands.backend}/bin/backend";

  # Whitespace fixers only touch maintained text sources. Fixtures, snapshots
  # and vendored trees are test input, where the exact bytes are the point.
  maintained = {
    enable = true;
    files = "\\.(rs|nix|nu|toml|md|py|yml|yaml|sh)$";
    excludes = [
      "^vendor/"
      "^workspace/"
      "^third_party/"
      "/fixtures/"
      "/snapshots/"
      "\\.snap$"
    ];
  };

  hooks = conventions.mkHooks {
    inherit pkgs;
    inherit (inputs) git-hooks;
    src = workspaceRoot;
    formatter = formatting.wrapper;
    prePush.lint.entry = "${backend} lint changed";
    extra = {
      trim-trailing-whitespace = maintained;
      end-of-file-fixer = maintained;
    };
  };

  verb =
    name: command:
    pkgs.writeShellScriptBin name ''
      cd "$(${pkgs.git}/bin/git rev-parse --show-toplevel)" && exec ${command} "$@"
    '';
in
{
  inherit hooks;

  verbs = [
    (verb "fmt" "nix fmt")
    (verb "check" "${backend} lint changed")
    (verb "test" "${backend} test changed")
    (verb "dev" "cargo run -p backend-desktop")
    (verb "build" "cargo build --locked -p backend-locald -p backend-cli -p backend-mcp")
    (verb "ci" "${backend} test pr")
  ];
}

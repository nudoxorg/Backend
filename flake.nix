# The only flake (REPO-CONVENTIONS.md §2.1): a stub at the workspace root, so
# pure derivations see the Cargo sources beside it. All Nix logic is in the
# small, independently evaluable modules under `.config/nix`.
{
  description = "Backend workspace development and validation environment";

  # Flake inputs must remain a literal set for pure evaluation.
  inputs = {
    bundle-nixpkgs.url = "github:philocalyst/nixpkgs/ea2e2b146a72a1a03318c215bdf7bb40f90044bf";
    nixpkgs.url = "github:NixOS/nixpkgs/0bb7ec54c8483066ec9d7720e780a5caa71f8612";
    fenix = {
      url = "github:nix-community/fenix/8d20dd64ad45ed4b5179a37cec75fff9f732e78d";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nuenv = {
      url = "github:xav-ie/nuenv/ba517a66dc4e8322855617262c3972892190908b";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # lib.conventions (shared hooks). Only the development shell uses it. For
    # local iteration: --override-input nixos path:../MachineConfigurations
    nixos.url = "git+ssh://forgejo@dev.nudox.org:2222/Nudox/MachineConfigurations.git";
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs:
    import ./.config/nix {
      inherit inputs;
      workspaceRoot = ./.;
    };
}

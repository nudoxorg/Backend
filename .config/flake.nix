# Pins the repository's complete development and validation environment.
# Delegates package composition to small, independently evaluable Nix modules.
# Exposes Nushell as the only project scripting surface.
{
  description = "Backend reproducible development control plane";

  # Flake inputs must remain a literal set for pure evaluation. The workspace
  # root wrapper repeats these pins so Cargo sources beside this configuration
  # are visible to pure derivation evaluation.
  inputs = {
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
  };

  outputs =
    inputs:
    import ./nix {
      inherit inputs;
      workspaceRoot = ./.;
    };
}

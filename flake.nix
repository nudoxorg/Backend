# Root workspace entrypoint for the backend reproducible development control
# plane. It reuses the small, independently evaluable Nix modules under
# `.config/nix` and pins the workspace root so pure derivations beside the
# Cargo sources are visible.
{
  description = "Backend workspace development and validation environment";

  # These pins repeat `.config/flake.nix` exactly so both entrypoints resolve
  # the same locked inputs. Keep them in lockstep; `flake.lock` mirrors
  # `.config/flake.lock`.
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
    import ./.config/nix {
      inherit inputs;
      workspaceRoot = ./.;
    };
}

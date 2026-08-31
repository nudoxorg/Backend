# Constructs every flake output for each supported host architecture.
# Keeps toolchain, command, shell, and check assembly in separate modules.
# Makes the same pinned control plane available to developers and automation.
{ inputs }:
let
  systems = [
    "aarch64-darwin"
    "aarch64-linux"
    "x86_64-linux"
  ];
  forAllSystems = inputs.nixpkgs.lib.genAttrs systems;
  perSystem =
    system:
    let
      pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = [ inputs.nuenv.overlays.default ];
      };
      toolchains = import ./toolchains.nix { inherit inputs pkgs system; };
      tools = import ./tools.nix { inherit pkgs toolchains; };
      commands = import ./commands.nix { inherit pkgs tools toolchains; };
      shells = import ./shells.nix {
        inherit
          pkgs
          tools
          toolchains
          commands
          ;
      };
      checks = import ./checks.nix { inherit pkgs tools commands; };
    in
    {
      inherit
        pkgs
        toolchains
        tools
        commands
        shells
        checks
        ;
    };
in
{
  packages = forAllSystems (
    system:
    let
      value = perSystem system;
    in
    {
      default = value.commands.backend;
      backend = value.commands.backend;
      backend-verifier = value.commands.backendVerifier;
    }
  );

  apps = forAllSystems (
    system:
    let
      value = perSystem system;
    in
    {
      default = {
        type = "app";
        program = "${value.commands.backend}/bin/backend";
      };
      backend = {
        type = "app";
        program = "${value.commands.backend}/bin/backend";
      };
    }
  );

  devShells = forAllSystems (system: (perSystem system).shells);
  checks = forAllSystems (system: (perSystem system).checks);
  formatter = forAllSystems (system: (perSystem system).tools.nixFormatter);
}

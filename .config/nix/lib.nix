# Defines the small Nix constructors shared by command, check, and skill outputs.
# Keeps system iteration, JSON materialization, and nuenv wiring in one module.
# Makes additions declarative without hiding tool inputs or runtime environment.
{ inputs }:
let
  systems = [
    "aarch64-darwin"
    "aarch64-linux"
    "x86_64-linux"
  ];
in
{
  inherit systems;
  eachSystem = inputs.nixpkgs.lib.genAttrs systems;

  controlFile =
    pkgs: control:
    pkgs.writeTextFile {
      name = "backend-control-plane";
      destination = "/share/backend/control-plane.json";
      text = builtins.toJSON control;
    };

  nuCheck =
    {
      pkgs,
      name,
      packages,
      environment ? { },
      build,
    }:
    pkgs.nuenv.mkDerivation (
      {
        inherit name packages build;
        src = pkgs.writeTextDir "empty/.keep" "";
      }
      // environment
    );
}

# Defines the small Nix constructors shared by command, check, and skill outputs.
# Keeps system iteration, JSON materialization, and checked Nu execution together.
# Makes additions declarative without hiding tool inputs or runtime environment.
{ inputs }:
let
  systems = [
    "aarch64-darwin"
    "aarch64-linux"
    "x86_64-linux"
  ];
  # nuenv continues after a failed external command unless Nu is inside an
  # error-catching context. Never publish a validated marker after failure.
  nuCheckScript = build: ''
    try {
      ${build}
    } catch {|failure|
      error make {msg: $failure.msg}
    }
  '';

in
{
  inherit systems nuCheckScript;
  eachSystem = inputs.nixpkgs.lib.genAttrs systems;

  controlFile =
    pkgs: control:
    pkgs.writeTextFile {
      name = "backend-control-plane";
      destination = "/share/backend/control-plane.json";
      text = builtins.toJSON control;
    };

  # nuenv's phase pipeline can lose a child's failing status even when an
  # output directory already exists. The standard builder checks Nu's exit
  # directly; the same Nu test body and explicit runtime packages remain.
  nuCheck =
    {
      pkgs,
      name,
      packages,
      environment ? { },
      build,
    }:
    pkgs.runCommand name ({ nativeBuildInputs = [ pkgs.nushell ] ++ packages; } // environment) ''
      nu --no-config-file ${pkgs.writeText "${name}-check.nu" (nuCheckScript build)}
    '';
}

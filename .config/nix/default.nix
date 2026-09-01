# Constructs every flake output for each supported host architecture.
# Keeps toolchain, command, shell, and check assembly in separate modules.
# Makes the same pinned control plane available to developers and automation.
{ inputs }:
let
  helpers = import ./lib.nix { inherit inputs; };
  declaredControl = import ./control.nix;
  roleRuntimeDigest = builtins.hashString "sha256" (
    builtins.concatStringsSep "\n" (
      map builtins.readFile [
        ./role-tools.nix
        ../nu/core/capability.nu
        ../nu/core/telemetry.nu
        ../nu/core/process.nu
      ]
    )
  );
  control = declaredControl // {
    roles = builtins.mapAttrs (
      _: role:
      role
      // {
        contractDigest = builtins.hashString "sha256" (
          builtins.toJSON {
            inherit roleRuntimeDigest;
            contract = role;
          }
        );
      }
    ) declaredControl.roles;
  };
  perSystem =
    system:
    let
      pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = [ inputs.nuenv.overlays.default ];
      };
      controlFile = helpers.controlFile pkgs control;
      artifacts = import ./artifacts.nix { inherit pkgs control; };
      formatting = import ./format.nix { inherit inputs pkgs toolchains; };
      astGrepSuite = import ./ast-grep-suite.nix {
        inherit pkgs;
        rules = control.lint.syntax;
      };
      toolchains = import ./toolchains.nix { inherit inputs pkgs system; };
      tools = import ./tools.nix { inherit pkgs toolchains; };
      commands = import ./commands.nix {
        inherit
          astGrepSuite
          artifacts
          formatting
          controlFile
          control
          pkgs
          toolchains
          tools
          ;
      };
      shells = import ./shells.nix {
        inherit
          pkgs
          tools
          toolchains
          commands
          ;
      };
      checks = import ./checks.nix {
        inherit
          astGrepSuite
          artifacts
          commands
          control
          controlFile
          helpers
          formatting
          pkgs
          toolchains
          tools
          ;
      };
    in
    {
      inherit
        pkgs
        toolchains
        tools
        commands
        shells
        checks
        artifacts
        astGrepSuite
        formatting
        controlFile
        ;
    };
in
{
  packages = helpers.eachSystem (
    system:
    let
      value = perSystem system;
    in
    {
      default = value.commands.backend;
      backend = value.commands.backend;
      backend-verifier = value.commands.backendVerifier;
      agent-skills = value.commands.agentSkills;
      luna-tools = value.commands.roleBundles."luna-pair";
      terra-tools = value.commands.roleBundles."terra-academic";
      reviewer-tools = value.commands.roleBundles."terra-reviewer";
      sol-tools = value.commands.roleBundles."sol-integrator";
      control-plane = value.controlFile;
      ast-grep-suite = value.astGrepSuite;
      formatter = value.formatting.wrapper;
      telemetry = value.commands.telemetry;
    }
  );

  apps = helpers.eachSystem (
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
      telemetry = {
        type = "app";
        program = "${value.commands.telemetry}/bin/telemetry";
      };
    }
  );

  devShells = helpers.eachSystem (system: (perSystem system).shells);
  checks = helpers.eachSystem (system: (perSystem system).checks);
  formatter = helpers.eachSystem (system: (perSystem system).formatting.wrapper);
}

# Constructs every flake output for each supported host architecture.
# Keeps toolchain, command, shell, and check assembly in separate modules.
# Makes the same pinned control plane available to developers and automation.
{ inputs, workspaceRoot }:
let
  helpers = import ./lib.nix { inherit inputs; };
  policyRoot = import ./policy/contracts.nix;
  readPolicyTree =
    directory:
    let
      entries = builtins.readDir directory;
      names = builtins.attrNames entries;
    in
    builtins.concatLists (
      map (
        name:
        let
          path = builtins.toPath "${toString directory}/${name}";
        in
        if entries.${name} == "regular" then
          [ path ]
        else if entries.${name} == "directory" then
          readPolicyTree path
        else
          [ ]
      ) names
    );
  workspacePolicyFiles = if builtins.pathExists (workspaceRoot + "/Cargo.toml") then [
    (workspaceRoot + "/flake.nix")
    (workspaceRoot + "/flake.lock")
  ] else [ ];
  policyFiles = workspacePolicyFiles ++ [
    ../flake.nix
    ../flake.lock
  ]
  ++ readPolicyTree ./.
  ++ readPolicyTree ../nu/core
  ++ readPolicyTree ../nu/scope
  ++ readPolicyTree ../nu/cutover
  ++ readPolicyTree ../contracts
  ++ readPolicyTree ../gui
  ++ readPolicyTree ../fixtures;
  policyRootDigest = builtins.hashString "sha256" (
    builtins.concatStringsSep "\n" (
      map (path: "${toString path}:" + builtins.readFile path) policyFiles
    )
  );
  canonicalSchema = builtins.fromJSON (builtins.readFile ../contracts/schema.json);
  projectedSchemas = map (path: builtins.fromJSON (builtins.readFile path)) (
    readPolicyTree ../contracts/schemas
  );
  projectedNames = map (
    schema: builtins.replaceStrings [ "backend.control-plane.v1/" ] [ "" ] schema."$id"
  ) projectedSchemas;
  definitionNames = builtins.attrNames canonicalSchema.definitions;
  projectionFor =
    name:
    builtins.filter (
      schema: builtins.replaceStrings [ "backend.control-plane.v1/" ] [ "" ] schema."$id" == name
    ) projectedSchemas;
  projectionFields = schema: builtins.sort builtins.lessThan schema.required;
  definitionFields = name: builtins.sort builtins.lessThan canonicalSchema.definitions.${name};
  validatedPolicyRoot =
    assert policyRoot.schemaVersion == 1;
    assert policyRoot.unknownFields == "reject";
    assert builtins.pathExists ../contracts/schema.json;
    assert builtins.pathExists ../fixtures/control-plane/scope-exactly-one.json;
    assert
      builtins.sort builtins.lessThan projectedNames == builtins.sort builtins.lessThan definitionNames;
    assert builtins.all (
      schema: schema.x-backend-schema-version == canonicalSchema.schema_version
    ) projectedSchemas;
    assert builtins.all (
      name:
      let
        matches = projectionFor name;
      in
      builtins.length matches == 1 && projectionFields (builtins.head matches) == definitionFields name
    ) definitionNames;
    policyRoot;
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
    inherit policyRootDigest;
    policyRoot = validatedPolicyRoot;
    roles = builtins.mapAttrs (
      _: role:
      role
      // {
        contractDigest = builtins.hashString "sha256" (
          builtins.toJSON {
            inherit roleRuntimeDigest policyRootDigest;
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
      tools = import ./tools.nix { inherit pkgs toolchains workspaceRoot; };
      gui = import ./gui.nix {
        inherit pkgs control workspaceRoot;
        resolvedSourceHashes = tools.gpuiOutputHashes;
      };
      corpus = import ./corpus.nix { inherit pkgs workspaceRoot; };
      commands = import ./commands.nix {
        inherit
          astGrepSuite
          artifacts
          formatting
          controlFile
          control
          gui
          pkgs
          toolchains
          tools
          workspaceRoot
          ;
      };
      shells = import ./shells.nix {
        inherit
          pkgs
          tools
          toolchains
          commands
          control
          controlFile
          corpus
          gui
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
          gui
          pkgs
          toolchains
          tools
          ;
        lunaTools = tools.lunaTools;
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
        corpus
        gui
        ;
    };
in
{
  packages = helpers.eachSystem (
    system:
    let
      lightweightPkgs = import inputs.nixpkgs {
        inherit system;
      };
      lightweightGui = import ./gui.nix {
        pkgs = lightweightPkgs;
        inherit control workspaceRoot;
      };
      value = perSystem system;
    in
    {
      default = value.commands.backend;
      backend = value.commands.backend;
      backend-verifier = value.commands.backendVerifier;
      agent-skills = value.commands.agentSkills;
      # Keep the public lane bootstrap cheap. The complete role command
      # surface remains under this explicit name for policy/control checks.
      luna-tools = value.tools.lunaTools;
      luna-role-tools = value.commands.roleBundles."luna-pair";
      terra-tools = value.commands.roleBundles."terra-academic";
      reviewer-tools = value.commands.roleBundles."terra-reviewer";
      sol-tools = value.commands.roleBundles."sol-integrator";
      control-plane = value.controlFile;
      ast-grep-suite = value.astGrepSuite;
      formatter = value.formatting.wrapper;
      telemetry = value.commands.telemetry;
      gui-control = lightweightGui.configFile;
      gui-fonts = lightweightGui.fontConfig;
      gui-tools = lightweightGui.toolsBundle;
      gui-harness = value.commands.backend;
    }
    // value.pkgs.lib.optionalAttrs (value.tools.backendControl != null) {
      backend-control = value.tools.backendControl;
    }
    // value.pkgs.lib.optionalAttrs (value.tools.guiRuntime != null) {
      gui-runtime = value.tools.guiRuntime;
    }
    // value.pkgs.lib.optionalAttrs (value.corpus != null) {
      fleet-corpus-rust = value.corpus.rust;
      fleet-corpus-typescript = value.corpus.typescript;
      fleet-corpus-python = value.corpus.python;
      fleet-corpus-go = value.corpus.go;
      fleet-corpus-java = value.corpus.java;
      fleet-corpus-csharp = value.corpus.csharp;
      fleet-corpus-clang = value.corpus.clang;
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

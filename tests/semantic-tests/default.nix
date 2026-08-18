{
  pkgs,
  mkNuCheck,
  nuLib,
  projectRoot,
  rustToolchain,
  semanticModel,
}:

mkNuCheck {
  name = "semantic-tests";
  script = ./check.nu;
  src = ../..;
  inherit nuLib;

  runtimeInputs = with pkgs; [
    cargo-nextest
    cargo
    git
    pkg-config
  ] ++ [ rustToolchain ];

  env = {
    NUDOX_EMBED_MODEL_DIR = semanticModel;
  };

  extraSrcTrees = [
    {
      relPath = "workspace/nudox-engine/src";
      source = builtins.path {
        path = "${projectRoot}/workspace/nudox-engine/src";
        name = "semantic-tests-engine-src";
        filter = path: type: type == "directory" || builtins.match ".*\\.rs$" path != null;
      };
    }
  ];

  extraSrcFiles = [
    {
      relPath = "workspace/nudox-engine/tests/engine/semantic_real_model.rs";
      source = builtins.path {
        path = "${projectRoot}/workspace/nudox-engine/tests/engine/semantic_real_model.rs";
        name = "semantic-tests-real-model";
      };
    }
  ];

  noChroot = true;
  preferLocalBuild = true;
  allowSubstitutes = false;

  resultLines = [
    "semantic-tests: ok"
    "suite: pinned Nix model, ONNX runtime, restart persistence"
  ];
}

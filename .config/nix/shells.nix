# Defines the direnv development shell and its reproducible process contract.
# Routes compiler output and tool configuration into repository-local locations.
# Publishes native compiler authorities without embedding them in client binaries.
{
  pkgs,
  tools,
  toolchains,
  commands,
  control,
  controlFile,
  corpus,
  gui,
}:
let
  compilers = tools.compilers;
  optionalEnv = pkgs.lib.optionalAttrs (tools.typescriptChecker != null) {
    NUDOX_TYPESCRIPT_CHECKER_BIN = "${tools.typescriptChecker}/bin/nudox-typescript-checker";
  };
  optionalOracleEnv = pkgs.lib.optionalAttrs (tools.goOracle != null) {
    NUDOX_GO_ORACLE_BIN = "${tools.goOracle}/bin/oracle";
  };
  # Real package sources for the seven-language corpus. Every value names a
  # pinned store path built by `.config/nix/corpus.nix`; absent when this
  # module is evaluated from the configuration-only `.config` flake, whose
  # source root cannot reach the workspace tree.
  optionalCorpusEnv = pkgs.lib.optionalAttrs (corpus != null) {
    NUDOX_RUST_CORPUS_DIR = "${corpus.rust}";
    NUDOX_TYPESCRIPT_CORPUS_DIR = "${corpus.typescript}";
    NUDOX_PYTHON_CORPUS_DIR = "${corpus.python}";
    NUDOX_GO_CORPUS_DIR = "${corpus.go}";
    NUDOX_JAVA_CORPUS_DIR = "${corpus.java}";
    NUDOX_CSHARP_CORPUS_DIR = "${corpus.csharp}";
    NUDOX_CLANG_CORPUS_DIR = "${corpus.clang}";
  };
  # Absolute native-toolchain authorities for the seven-language corpus. Every
  # value names a pinned nixpkgs binary; nothing is discovered from ambient
  # `PATH`. `RUSTC` and `COMPILER_STABLE_TOOLCHAIN` both resolve to the pinned
  # Fenix 1.97.1 toolchain, and `NUDOX_JDK` names a JDK root whose `bin/javac`
  # is the Java authority.
  corpusEnv = {
    COMPILER_CLANG_COMPILER = "${compilers.clang}/bin/clang";
    COMPILER_CSHARP_COMPILER = "${compilers.dotnet}/bin/dotnet";
    COMPILER_GO_COMPILER = "${compilers.go}/bin/go";
    COMPILER_JAVA_COMPILER = "${compilers.jdk}/bin/javac";
    COMPILER_PYTHON_COMPILER = "${compilers.python}/bin/python3";
    COMPILER_STABLE_TOOLCHAIN = "${toolchains.stable}";
    COMPILER_TYPESCRIPT_COMPILER = "${compilers.typescript}/bin/tsc";
    LIBCLANG_PATH = "${compilers.libclang.lib}/lib";
    NUDOX_CLANG = "${compilers.clang}/bin/clang";
    NUDOX_CLANG_DRIVER = "${compilers.clang}/bin/clang";
    NUDOX_CSHARP_DOTNET = "${compilers.dotnet}/bin/dotnet";
    NUDOX_DOTNET = "${compilers.dotnet}/bin/dotnet";
    NUDOX_GO = "${compilers.go}/bin/go";
    NUDOX_JAVAC = "${compilers.jdk}/bin/javac";
    NUDOX_JDK = compilers.jdk.home;
    NUDOX_PYREFLY = "${compilers.pyrefly}/bin/pyrefly";
    NUDOX_PYREFLY_BIN = "${compilers.pyrefly}/bin/pyrefly";
    NUDOX_PYTHON = "${compilers.python}/bin/python3";
    NUDOX_RUSTC = "${toolchains.stable}/bin/rustc";
    NUDOX_TSC = "${compilers.typescript}/bin/tsc";
    NUDOX_TYPESCRIPT_MODULE_ROOT = "${compilers.typescript}/lib/node_modules";
    NUDOX_TYPESCRIPT_NODE = "${compilers.node}/bin/node";
    RUSTC = "${toolchains.stable}/bin/rustc";
  }
  // optionalEnv
  // optionalOracleEnv
  // optionalCorpusEnv;
  common = corpusEnv // {
    BACKEND_STABLE_CARGO = toolchains.stableCargo;
    BACKEND_RUSTFMT = toolchains.rustfmt;
    BACKEND_CONFIG_SNAPSHOT = toString ../.;
    BACKEND_CONTROL_PLANE = "${controlFile}/share/backend/control-plane.json";
    BACKEND_POLICY_ROOT_DIGEST = control.policyRootDigest;
    BACKEND_GUI_CONFIG = "${gui.configFile}/share/nudox/gui-control-plane.json";
    BACKEND_GUI_FONTCONFIG = gui.fontConfig;
    BACKEND_GUI_FONT_MANIFEST = "${gui.fontManifest}/share/nudox/fonts.sha256";
    NUDOX_GUI_GPUI_SOURCE_DIGEST = gui.gpuiSourceDigest;
    NUDOX_GUI_GPUI_COMPONENT_SOURCE_DIGEST =
      if gui.gpuiComponentSourceDigest == null then "" else gui.gpuiComponentSourceDigest;
    NUDOX_GUI_GPUI_SOURCE_MANIFEST = gui.gpuiSourceManifest;
    NUDOX_GUI_DEPENDENCY_GRAPH_SHA256 = gui.dependencyGraphDigest;
    NUDOX_GUI_GPU_BACKEND = control.gui.gpu.defaultGpuBackend;
    NUDOX_GUI_GPU_DEVICE = control.gui.gpu.expectedGpuDevice;
    NUDOX_GUI_GPU_PROBE = "${gui.gpuProbe}/bin/nudox-gui-gpu-probe";
    WGPU_BACKEND = control.gui.gpu.forceEnvironment.WGPU_BACKEND;
    LIBGL_ALWAYS_SOFTWARE = control.gui.gpu.forceEnvironment.LIBGL_ALWAYS_SOFTWARE;
    MESA_LOADER_DRIVER_OVERRIDE = control.gui.gpu.forceEnvironment.MESA_LOADER_DRIVER_OVERRIDE;
    NUDOX_GUI_TOOLCHAIN = toString toolchains.stable;
    NUDOX_GUI_ENCODER_VERSION = pkgs.ffmpeg.version;
    NUDOX_GUI_TOOL_CLOSURE = "${gui.toolsBundle}";
    NUDOX_GUI_HARNESS = "nix shell .#gui-harness .#gui-tools .#gui-runtime";
    LIBRARY_PATH = pkgs.lib.makeLibraryPath [
      pkgs.libiconv
      pkgs.zlib
    ];
    shellHook = ''
      export CARGO_TARGET_DIR="$PWD/.local/target"
      export CLIPPY_CONF_DIR="$PWD/.config"
      export BACKEND_WORKSPACE_SNAPSHOT="$PWD"
    '';
  };
  development = pkgs.mkShell (
    common
    // {
      packages = tools.development ++ [
        commands.backend
        gui.toolsBundle
      ];
    }
  );
  verification = pkgs.mkShell (
    common
    // {
      packages = tools.verification ++ [
        commands.backendVerifier
        gui.toolsBundle
      ];
      BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
    }
  );
  compiler = pkgs.mkShell (
    common
    // {
      packages = tools.compiler ++ [
        commands.backend
        gui.toolsBundle
      ];
    }
  );
  services = pkgs.mkShell (
    common
    // {
      packages = tools.services ++ [
        commands.backend
        gui.toolsBundle
      ];
    }
  );
  observability = pkgs.mkShell (
    common
    // {
      packages = tools.observability ++ [
        commands.backendVerifier
        gui.toolsBundle
      ];
      BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
    }
  );
  complete = pkgs.mkShell (
    common
    // {
      packages = tools.complete ++ [
        commands.backendVerifier
        gui.toolsBundle
      ];
      BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
    }
  );
in
{
  default = development;
  inherit
    compiler
    complete
    development
    observability
    services
    verification
    ;
}

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
}:
let
  compilers = tools.compilers;
  optionalEnv = pkgs.lib.optionalAttrs (tools.typescriptChecker != null) {
    NUDOX_TYPESCRIPT_CHECKER_BIN = "${tools.typescriptChecker}/bin/nudox-typescript-checker";
  };
  optionalOracleEnv = pkgs.lib.optionalAttrs (tools.goOracle != null) {
    NUDOX_GO_ORACLE_BIN = "${tools.goOracle}/bin/oracle";
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
  // optionalOracleEnv;
  common = corpusEnv // {
    BACKEND_STABLE_CARGO = toolchains.stableCargo;
    BACKEND_RUSTFMT = toolchains.rustfmt;
    BACKEND_CONFIG_SNAPSHOT = toString ../.;
    BACKEND_CONTROL_PLANE = "${controlFile}/share/backend/control-plane.json";
    BACKEND_POLICY_ROOT_DIGEST = control.policyRootDigest;
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
      packages = tools.development ++ [ commands.backend ];
    }
  );
  verification = pkgs.mkShell (
    common
    // {
      packages = tools.verification ++ [ commands.backendVerifier ];
      BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
    }
  );
  compiler = pkgs.mkShell (
    common
    // {
      packages = tools.compiler ++ [ commands.backend ];
    }
  );
  services = pkgs.mkShell (
    common
    // {
      packages = tools.services ++ [ commands.backend ];
    }
  );
  observability = pkgs.mkShell (
    common
    // {
      packages = tools.observability ++ [ commands.backendVerifier ];
      BACKEND_NIGHTLY_CARGO = toolchains.nightlyCargo;
    }
  );
  complete = pkgs.mkShell (
    common
    // {
      packages = tools.complete ++ [ commands.backendVerifier ];
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

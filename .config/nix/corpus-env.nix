# Native-toolchain and corpus environment shared by every consumer that runs
# the seven-language test suite: the interactive `.#complete` shell
# (`shells.nix`) and the `backend`/`backendVerifier` command wrappers
# (`commands.nix`). Kept in one place so the gate and the interactive shell
# can never drift apart on which compiler or corpus a test sees.
{
  pkgs,
  tools,
  toolchains,
  corpus,
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
in
{
  # Absolute native-toolchain authorities for the seven-language corpus. Every
  # value names a pinned nixpkgs binary; nothing is discovered from ambient
  # `PATH`. `RUSTC` and `COMPILER_STABLE_TOOLCHAIN` both resolve to the pinned
  # Fenix 1.97.1 toolchain, and `NUDOX_JDK` names a JDK root whose `bin/javac`
  # is the Java authority.
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
  # Native build scripts (e.g. `blake3`) link against libiconv/zlib; the
  # linker only finds them via `LIBRARY_PATH`, not `PATH`.
  LIBRARY_PATH = pkgs.lib.makeLibraryPath [
    pkgs.libiconv
    pkgs.zlib
  ];
}
// optionalEnv
// optionalOracleEnv
// optionalCorpusEnv

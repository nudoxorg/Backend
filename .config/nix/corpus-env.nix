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
  inherit (tools) compilers;
  # `nix develop` assembles `NIX_CFLAGS_COMPILE`/`NIX_LDFLAGS` (and the
  # per-target-triple "role marker" that gates them, e.g.
  # `NIX_CC_WRAPPER_TARGET_HOST_<triple>`) from every `packages`/`buildInput`'s
  # setup hook as the shell starts; the `backend`/`backendVerifier` wrappers
  # (`pkgs.nuenv.writeShellApplication` in `commands.nix`) are a plain script
  # with a fixed `runtimeEnv`, so no setup hook ever runs for them and those
  # variables are silently absent. That gap is invisible for most tests
  # (their C toolchain calls resolve `-isystem`/`-isysroot` from the pinned
  # driver's own baked-in defaults, or through
  # `frontends/clang/system_includes.rs`, which asks the pinned driver
  # directly), but a real C corpus package that itself `#include <...>`s an
  # angle-bracket header belonging to another pinned tool
  # (`brotli/c/common/platform.h` reaching for `<brotli/types.h>`, which the
  # checked-out corpus source does not ship on any `-I` path of its own)
  # depends on that closure's setup-hook-propagated dev headers to resolve at
  # all. Both `.#compiler` and `.#complete` include `tools.nativeCompilers`;
  # their service and verifier tools do not supply these language headers.
  # Build a real `stdenv.mkDerivation` (`runCommand`, unlike `mkShell`, actually
  # runs its build phase) over this shared native-compiler subset and capture
  # its setup hooks. This keeps the gate's compiler environment aligned with
  # the interactive shells without realizing optional services such as
  # Qdrant during evaluation. The role-marker variable
  # names themselves carry the host triple (e.g. `_arm64_apple_darwin`),
  # which must stay whatever this build platform's own cc-wrapper spells it
  # as, not a hardcoded string, so this discovers their names from the
  # captured environment rather than assuming one.
  nativeCompilerShellEnv =
    pkgs.runCommand "backend-native-compiler-shell-env"
      {
        nativeBuildInputs = tools.nativeCompilers;
      }
      ''
        {
          for name in NIX_CFLAGS_COMPILE NIX_CFLAGS_COMPILE_BEFORE NIX_CFLAGS_LINK \
            NIX_LDFLAGS NIX_LDFLAGS_BEFORE NIX_CXXSTDLIB_COMPILE NIX_CXXSTDLIB_LINK \
            NIX_HARDENING_ENABLE NIX_ENFORCE_NO_NATIVE; do
            printf '%s\t%s\n' "$name" "''${!name-}"
          done
          # The setup-hook role markers (`NIX_CC_WRAPPER_TARGET_HOST_<triple>`
          # and friends) that gate whether the wrapped compiler picks up the
          # variables above at all; every wrapped-compiler invocation
          # re-derives its own triple-suffixed flags from these plus the
          # bare names each time it runs, so both must be forwarded.
          env | grep -E '^NIX_(CC|BINTOOLS|PKG_CONFIG)_WRAPPER_TARGET_HOST_' | while IFS='=' read -r roleName roleValue; do
            printf '%s\t%s\n' "$roleName" "$roleValue"
          done
        } > $out
      '';
  nativeCompilerShellEnvVars = builtins.listToAttrs (
    map
      (
        line:
        let
          parts = pkgs.lib.splitString "\t" line;
        in
        {
          name = builtins.unsafeDiscardStringContext (builtins.elemAt parts 0);
          value = builtins.elemAt parts 1;
        }
      )
      (
        builtins.filter (line: line != "") (
          pkgs.lib.splitString "\n" (builtins.readFile nativeCompilerShellEnv)
        )
      )
  );
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
  # Tests that execute coreutils after ProcessEnvironment::env_clear() must
  # pass an absolute executable, not rely on the host's /bin layout or PATH.
  NUDOX_TEST_COREUTILS_BIN = "${pkgs.coreutils}/bin";
  NUDOX_PROCESS_SHELL = "${pkgs.bash}/bin/sh";
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
// nativeCompilerShellEnvVars

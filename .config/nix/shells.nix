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
  # Native-toolchain and corpus authorities for the seven-language corpus,
  # shared with the `backend`/`backendVerifier` command wrappers in
  # `commands.nix` so the gate sees exactly what this shell sees.
  corpusEnv = import ./corpus-env.nix {
    inherit
      pkgs
      tools
      toolchains
      corpus
      ;
  };
  # Linux software GPU drivers for the desktop window journeys; empty elsewhere.
  baseEnv = corpusEnv // tools.linuxGraphicsEnvironment;
  common = baseEnv // {
    BACKEND_STABLE_CARGO = toolchains.stableCargo;
    # PR build/test commands opt into the leased wrapper; general tooling keeps
    # raw Cargo so immutable Nix checks do not require a mutable Git worktree.
    BACKEND_PARALLEL_CARGO = "${tools.parallelCargo}/bin/cargo";
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
    # Cargo test executables do not receive Nix's fixup/RPATH pass. On Linux,
    # nextest needs the declared GUI shared libraries even to list tests.
    LD_LIBRARY_PATH = tools.linuxDesktopRuntimePath;
    shellHook = ''
      export CARGO_TARGET_DIR="$PWD/.local/target"
      export CLIPPY_CONF_DIR="$PWD/.config"
      export BACKEND_WORKSPACE_SNAPSHOT="$PWD"
      # LocalHost fingerprints these runtime paths explicitly. Resolve each
      # through the pinned toolchain or selected Cargo environment and export
      # it only when the exact target exists on this process host.
      if [ -z "''${NUDOX_RUST_SYSROOT:-}" ]; then
        rust_sysroot="$("${toolchains.stable}/bin/rustc" --print sysroot 2>/dev/null || true)"
        case "$rust_sysroot" in
          /*) if [ -d "$rust_sysroot" ]; then export NUDOX_RUST_SYSROOT="$rust_sysroot"; fi ;;
        esac
      fi
      if [ -z "''${NUDOX_CARGO_HOME:-}" ]; then
        cargo_home="''${CARGO_HOME:-''${HOME:-}/.cargo}"
        case "$cargo_home" in
          /*) if [ -d "$cargo_home" ]; then export NUDOX_CARGO_HOME="$cargo_home"; fi ;;
        esac
      fi
      # LocalHost accepts the Go module cache only as an explicit absolute
      # authority. Resolve it through the pinned Go executable at shell entry
      # so an existing workspace cache is admitted without guessing from PATH;
      # leave the variable absent when the cache directory is not present.
      if [ -z "''${NUDOX_GO_ROOT:-}" ]; then
        go_module_cache="''${GOMODCACHE:-$(${tools.compilers.go}/bin/go env GOMODCACHE 2>/dev/null || true)}"
        case "$go_module_cache" in
          /*) if [ -d "$go_module_cache" ]; then export NUDOX_GO_ROOT="$go_module_cache"; fi ;;
        esac
      fi
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
  # C sources compiled by build scripts (tree-sitter grammars and friends)
  # need a C compiler for the target even under `cargo check`; zig provides
  # one hermetic cross compiler for every non-MSVC target.
  # cc-rs adds its own `--target=<rust triple>`, which zig does not accept;
  # the wrapper replaces it with zig's spelling of the same target.
  zigCc =
    name: target:
    pkgs.writeShellScriptBin name ''
      arguments=()
      for argument in "$@"; do
        case "$argument" in
          --target=*) ;;
          # Preprocess-only runs feed resource compilers, and llvm-rc
          # rejects GNU line markers; emit none.
          -E) arguments+=("-E" "-P") ;;
          *) arguments+=("$argument") ;;
        esac
      done
      # zig reads the host's Nix compiler variables to find system headers;
      # every target here is foreign, so none of the host's may leak in.
      unset NIX_CFLAGS_COMPILE NIX_CFLAGS_LINK NIX_LDFLAGS SDKROOT
      # zig turns on UBSan for unoptimised C (Cargo's dev profile). Its own
      # linker supplies the runtime; the emulated lanes link with the
      # target's GCC, which does not (`__ubsan_handle_*` undefined in ring).
      exec ${pkgs.zig}/bin/zig cc -target ${target} -fno-sanitize=undefined "''${arguments[@]}"
    '';
  zigAr = pkgs.writeShellScriptBin "ar-zig" ''
    exec ${pkgs.zig}/bin/zig ar "$@"
  '';
  # gpui (`embed_resource`) and turso embed a Windows version/manifest
  # resource from their build scripts. LLVM's windres is a drop-in for the
  # MinGW one; clang preprocesses the `.rc` sources it includes.
  windres = pkgs.writeShellScriptBin "windres" ''
    exec ${pkgs.llvmPackages.bintools-unwrapped}/bin/llvm-windres \
      --preprocessor=${pkgs.llvmPackages.clang-unwrapped}/bin/clang \
      --preprocessor-arg=-E --preprocessor-arg=-xc --preprocessor-arg=-DRC_INVOKED \
      "$@"
  '';
  # llvm-rc resolves resource paths against the preprocessed file in
  # OUT_DIR; rc.exe also searches the build script's directory. Adding it as
  # an include path (the build script's crate, which embed_resource
  # runs from OUT_DIR) matches that. The probe output is unchanged.
  llvmRc = pkgs.writeShellScriptBin "llvm-rc" ''
    exec ${pkgs.llvmPackages.bintools-unwrapped}/bin/llvm-rc /I "''${CARGO_MANIFEST_DIR:-$PWD}" "$@"
  '';
  mingwWindres = pkgs.writeShellScriptBin "x86_64-w64-mingw32-windres" ''
    exec ${windres}/bin/windres "$@"
  '';
  crossCompilers = [
    (zigCc "cc-x86_64-windows-gnu" "x86_64-windows-gnu")
    (zigCc "cc-x86_64-linux-gnu" "x86_64-linux-gnu")
    (zigCc "cc-aarch64-linux-gnu" "aarch64-linux-gnu")
    zigAr
    windres
    mingwWindres
  ];
  # Compile-checks every shipped platform: `cargo check --workspace
  # --all-targets --target <triple>` for each of `toolchains.crossCheckTargets`.
  # Deliberately does NOT spread `common`: a compile-only lane needs neither the
  # seven-language corpus, the native compiler authorities, nor the GUI capture
  # closure, and importing them would drag `tools.complete` (Qdrant included)
  # into every cross run. `nushell` runs the standalone lane runner at
  # `.config/ci/cross-check.nu`; nothing here closes over the `backend` command.
  crossAttrs = {
    # Fast CI checks use disposable checkouts. Incremental state for every
    # workspace test wrote 11GB for Windows alone without surviving a run.
    CARGO_INCREMENTAL = "0";
    # Cargo's debug C builds use -O0. jemalloc's configure probes add -Werror,
    # which turns glibc's "_FORTIFY_SOURCE requires optimization" into a
    # failure. This compile/test shell does not configure release hardening.
    hardeningDisable = [
      "fortify"
      "fortify3"
    ];
    packages = [
      toolchains.cross
      pkgs.python3
      pkgs.nushell
    ]
    ++ crossCompilers
    ++ [ pkgs.zig ];
    # The cross rustc carries every target's standard library; the host-only
    # stable toolchain cannot `--target` a foreign triple.
    RUSTC = "${toolchains.cross}/bin/rustc";
    NUDOX_CROSS_TARGETS = builtins.concatStringsSep " " toolchains.crossTargets;
    # The subset this lane compiles on a Linux host; the rest are gated on their
    # own native/emulated lanes (see `toolchains.crossCheckTargets`).
    NUDOX_CROSS_CHECK_TARGETS = builtins.concatStringsSep " " toolchains.crossCheckTargets;
    # Compile checks have no Linux sysroot for pkg-config to search; the
    # dlopen configuration of fontconfig-sys compiles without one.
    RUST_FONTCONFIG_DLOPEN = "on";
    CC_x86_64_pc_windows_gnu = "cc-x86_64-windows-gnu";
    # Native build scripts (jemalloc's configure) execute C probes. The Nix
    # compiler links the store loader; Zig uses generic /lib64, unavailable
    # in our CI containers. Foreign targets retain the Zig cross compiler.
    CC_x86_64_unknown_linux_gnu =
      if pkgs.stdenv.hostPlatform.system == "x86_64-linux" then
        "${pkgs.stdenv.cc}/bin/cc"
      else
        "cc-x86_64-linux-gnu";
    CC_aarch64_unknown_linux_gnu = "cc-aarch64-linux-gnu";
    AR_x86_64_pc_windows_gnu = "ar-zig";
    # `embed_resource` (gpui) identifies its compiler by probing it; llvm-rc
    # is a variant it recognises and preprocesses through the target CC.
    RC_x86_64_pc_windows_gnu = "${llvmRc}/bin/llvm-rc";
    AR_x86_64_unknown_linux_gnu = "ar-zig";
    AR_aarch64_unknown_linux_gnu = "ar-zig";
    # Host build scripts (blake3, ring) link libiconv/zlib through LIBRARY_PATH,
    # which `common` used to provide.
    LIBRARY_PATH = pkgs.lib.makeLibraryPath [
      pkgs.libiconv
      pkgs.zlib
    ];
    shellHook = ''
      export CARGO_TARGET_DIR="''${CARGO_TARGET_DIR:-$PWD/.local/target}"
      export ZIG_GLOBAL_CACHE_DIR="''${ZIG_GLOBAL_CACHE_DIR:-$PWD/.local/zig-cache}"
      export ZIG_LOCAL_CACHE_DIR="''${ZIG_LOCAL_CACHE_DIR:-$PWD/.local/zig-cache}"
    '';
  };
  cross = pkgs.mkShell crossAttrs;
  # Emulated test lanes: the `cross` shell plus what running a test needs that
  # a compile check does not. Each target gets a real linker and C library
  # (zig compiles the C sources; the target's GCC links), and a runner Cargo
  # and nextest put in front of every test binary, so tests do not know they
  # are emulated. Linux hosts only: neither runner builds on Darwin.
  mingw = pkgs.pkgsCross.mingwW64;
  arm64 = pkgs.pkgsCross.aarch64-multiplatform;
  emulatedLanes = pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
    # Windows (x86_64-pc-windows-gnu) test binaries under Wine (WoW64).
    windows-wine = pkgs.mkShell (
      crossAttrs
      // {
        packages = crossAttrs.packages ++ [
          pkgs.cargo-nextest
          pkgs.wineWow64Packages.stable
        ];
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${mingw.stdenv.cc}/bin/x86_64-w64-mingw32-gcc";
        # std's windows-gnu runtime links winpthread. iroh declares a cdylib,
        # and GNU ld auto-exports every symbol of a DLL with no explicit
        # exports, past PE's 65535-ordinal limit ("export ordinal too large").
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L native=${mingw.windows.pthreads}/lib -C link-arg=-Wl,--exclude-all-symbols";
        # See arm64-emu: lean debug info for throwaway emulated test trees.
        CARGO_PROFILE_DEV_DEBUG = "line-tables-only";
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER = "${pkgs.wineWow64Packages.stable}/bin/wine";
        WINEDEBUG = "-all";
        # A new prefix would offer to install Wine Mono and Gecko and wait
        # for an answer no one is there to give; tests need neither.
        WINEDLLOVERRIDES = "mscoree=;mshtml=";
        # raw-dylib imports (windows-sys) need MinGW's dlltool. It goes after
        # the shell's own tools so the llvm windres wrapper above still wins.
        shellHook = crossAttrs.shellHook + ''
          export PATH="$PATH:${mingw.stdenv.cc.bintools}/bin"
          export WINEPREFIX="$PWD/.local/wine-prefix"
        '';
      }
    );
    # aarch64 Linux test binaries under QEMU user-mode emulation. The Nix
    # cross GCC links against an absolute store glibc, so qemu-aarch64 needs
    # no sysroot and the host needs no binfmt registration.
    arm64-emu = pkgs.mkShell (
      crossAttrs
      // {
        packages = crossAttrs.packages ++ [
          pkgs.cargo-nextest
          pkgs.qemu-user
          # Process tests run host tools as children (the embedding fixtures
          # are Python scripts); an emulated test execs them natively.
          pkgs.python3
          pkgs.coreutils
        ];
        # The pinned process tools the Linux lane's shell exports (corpus-env):
        # without them, tests that clear their environment exit 127.
        NUDOX_TEST_COREUTILS_BIN = corpusEnv.NUDOX_TEST_COREUTILS_BIN;
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER = "${arm64.stdenv.cc}/bin/aarch64-unknown-linux-gnu-gcc";
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER = "${pkgs.qemu-user}/bin/qemu-aarch64";
        # Emulated test trees are built once per run and thrown away; full
        # DWARF only costs disk and link time. Backtraces keep line numbers.
        CARGO_PROFILE_DEV_DEBUG = "line-tables-only";
      }
    );
  };
in
{
  default = development;
  inherit
    compiler
    cross
    complete
    development
    observability
    services
    verification
    ;
}
// emulatedLanes

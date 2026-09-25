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
  common = corpusEnv // {
    # Use the repository's leased Cargo wrapper so independent worktrees share
    # compiler objects through sccache without sharing mutable target graphs.
    BACKEND_STABLE_CARGO = "${tools.parallelCargo}/bin/cargo";
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
      exec ${pkgs.zig}/bin/zig cc -target ${target} "''${arguments[@]}"
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
  # --all-targets --target <triple>` for each of `toolchains.crossTargets`.
  cross = pkgs.mkShell (
    common
    // {
      packages = [ toolchains.cross ] ++ crossCompilers ++ [ pkgs.zig ];
      # `common` pins RUSTC to the host-only toolchain; the cross check must use
      # the rustc that carries the target standard libraries.
      RUSTC = "${toolchains.cross}/bin/rustc";
      NUDOX_CROSS_TARGETS = builtins.concatStringsSep " " toolchains.crossTargets;
      # Compile checks have no Linux sysroot for pkg-config to search; the
      # dlopen configuration of fontconfig-sys compiles without one.
      RUST_FONTCONFIG_DLOPEN = "on";
      CC_x86_64_pc_windows_gnu = "cc-x86_64-windows-gnu";
      CC_x86_64_unknown_linux_gnu = "cc-x86_64-linux-gnu";
      CC_aarch64_unknown_linux_gnu = "cc-aarch64-linux-gnu";
      AR_x86_64_pc_windows_gnu = "ar-zig";
      # `embed_resource` (gpui) identifies its compiler by probing it; llvm-rc
      # is a variant it recognises and preprocesses through the target CC.
      RC_x86_64_pc_windows_gnu = "${llvmRc}/bin/llvm-rc";
      AR_x86_64_unknown_linux_gnu = "ar-zig";
      AR_aarch64_unknown_linux_gnu = "ar-zig";
      shellHook = common.shellHook + ''
        export ZIG_GLOBAL_CACHE_DIR="$PWD/.local/zig-cache"
        export ZIG_LOCAL_CACHE_DIR="$PWD/.local/zig-cache"
      '';
    }
  );
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

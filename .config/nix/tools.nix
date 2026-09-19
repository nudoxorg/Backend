# Collects every executable admitted to repository tooling and development.
# Keeps optional platform tools explicit and prevents ambient PATH discovery.
# Supplies one source of truth to shells, commands, and checks.
{
  pkgs,
  toolchains,
  workspaceRoot,
}:
let
  workspaceAvailable = builtins.pathExists (workspaceRoot + "/Cargo.toml");
  stableRustPlatform = pkgs.makeRustPlatform {
    cargo = toolchains.stable;
    rustc = toolchains.stable;
  };
  controlSourceRoots = [
    "crates"
    "frontends"
    "extensions"
    "apps"
    "tests"
    "tools"
  ];
  workspaceSource =
    if workspaceAvailable then
      pkgs.lib.cleanSourceWith {
        src = workspaceRoot;
        filter =
          path: type:
          let
            absolute = toString path;
            root = toString workspaceRoot;
            relative = pkgs.lib.removePrefix "${root}/" absolute;
          in
          absolute == root
          || builtins.elem relative (["Cargo.toml" "Cargo.lock"] ++ controlSourceRoots)
          || builtins.any (root: pkgs.lib.hasPrefix "${root}/" relative) controlSourceRoots;
      }
    else
      null;
  dylintSource = pkgs.fetchFromGitHub {
    owner = "trailofbits";
    repo = "dylint";
    rev = "v6.0.4";
    hash = "sha256-CROuPpPzUobUcH3Xl2fpEOVxEBBppmFBaJSRXsEuaXg=";
  };
  mkDylintTool =
    package:
    pkgs.rustPlatform.buildRustPackage {
      pname = package;
      version = "6.0.4";
      src = dylintSource;
      cargoDeps = cargoDylint.cargoDeps;
      cargoBuildFlags = [
        "--package"
        package
      ];
      cargoInstallFlags = [
        "--package"
        package
      ];
      doCheck = false;
    };
  cargoDylint = pkgs.rustPlatform.buildRustPackage rec {
    pname = "cargo-dylint";
    version = "6.0.4";
    src = dylintSource;
    cargoHash = "sha256-9YAYtVoqMfyeG5sy8Jtt8a894k9AzyhIDEFzyqdzyeI=";
    cargoBuildFlags = [
      "--package"
      "cargo-dylint"
      "--no-default-features"
      "--features"
      "cargo-cli"
    ];
    cargoInstallFlags = [
      "--package"
      "cargo-dylint"
    ];
    doCheck = false;
  };
  dylintLink = mkDylintTool "dylint-link";
  backendControl =
    if workspaceAvailable then
      stableRustPlatform.buildRustPackage {
        cargoBuildFlags = [
          "--package"
          "backend-control"
          "--bin"
          "backend-control"
        ];
        pname = "backend-control";
        version = "0.1.0";
        src = workspaceSource;
        postPatch = ''
                substituteInPlace Cargo.toml \
                  --replace-fail \
                  'members = [
              "crates/*",
              "frontends/*",
              "extensions/*",
              "apps/*",
              "tests/*",
              "tools/*",
          ]' \
                  'members = [
              "tools/control",
              "crates/store",
              "crates/version",
          ]'
        '';
        cargoLock = {
          lockFile = workspaceRoot + "/Cargo.lock";
          # `importCargoLock` vendors every entry in the workspace lock even
          # though `postPatch` restricts the build to the control-plane member
          # set. The two git checkouts are the desktop runtime (`gpui-ce`) and
          # the query engine (`trustfall`); these are the exact NAR hashes of
          # those pinned revisions, one entry per package cargo names.
          outputHashes = {
            "gpui-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_ce_util-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_collections-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_derive_refineable-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_elements-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_linux-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_macos-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_macros-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_media-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_platform-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_refineable-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_scheduler-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_shared_string-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_sum_tree-0.2.2" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_web-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_wgpu-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "gpui_windows-0.1.0" = "sha256-Jjn4gCrqt/VgrraeKX+d4dA23q9y/teId6SFvEkrUqc=";
            "trustfall-0.8.1" = "sha256-YZwoezIrScE01mo+PqEWVi8hDZQwpm793bMQ4vizSXc=";
            "trustfall_core-0.8.1" = "sha256-YZwoezIrScE01mo+PqEWVi8hDZQwpm793bMQ4vizSXc=";
            "trustfall_derive-0.3.1" = "sha256-YZwoezIrScE01mo+PqEWVi8hDZQwpm793bMQ4vizSXc=";
          };
        };
        cargoInstallFlags = [
          "--package"
          "backend-control"
          "--bin"
          "backend-control"
        ];
        doCheck = false;
      }
    else
      null;
  # Complete seven-language compiler set plus the native libraries the
  # workspace's compiled frontends load at build or run time. Every attribute
  # below was probed against the pinned nixpkgs revision on all three declared
  # systems; the few Linux-only GUI dependencies of the desktop surface are
  # guarded by `isLinux` so they never enter a Darwin evaluation.
  compilers = {
    clang = pkgs.clang;
    dotnet = pkgs.dotnet-sdk;
    go = pkgs.go;
    jdk = pkgs.jdk21;
    libclang = pkgs.libclang;
    libiconv = pkgs.libiconv;
    node = pkgs.nodejs_22;
    pyrefly = pkgs.pyrefly;
    python = pkgs.python3;
    typescript = pkgs.typescript;
    uv = pkgs.uv;
  };
  nativeLibraries = [
    pkgs.cmake
    pkgs.openssl
    pkgs.pkg-config
    pkgs.sqlite
    pkgs.zlib
  ];
  # The GPUI desktop host resolves X11, Wayland, Vulkan, audio, and udev at
  # build/link time on Linux. They are not part of the headless corpus path,
  # so they stay out of the Darwin closure entirely.
  linuxDesktopLibraries = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
    pkgs.alsa-lib
    pkgs.fontconfig
    pkgs.freetype
    pkgs.glib
    pkgs.libdrm
    pkgs.libglvnd
    pkgs.libx11
    pkgs.libxcb
    pkgs.libxext
    pkgs.libxfixes
    pkgs.libxkbcommon
    pkgs.libXcursor
    pkgs.libXi
    pkgs.libXrandr
    pkgs.udev
    pkgs.vulkan-loader
    pkgs.wayland
    pkgs.wayland-protocols
    pkgs.xorg.libXdmcp
  ];
  nativeCompilers =
    builtins.attrValues compilers ++ nativeLibraries ++ linuxDesktopLibraries;
  # Go semantic oracle. The coordinate is the workspace's own vendored Go
  # module (`frontends/go/src/legacy/oracle`); its `vendorHash` is the exact
  # fixed-output hash of `golang.org/x/{tools,mod,sync} v0.30.0/v0.23.0/v0.11.0`.
  # It is null when this module is evaluated from the configuration-only
  # `.config` flake, whose source root cannot reach the workspace tree.
  goOracle =
    if workspaceAvailable then
      pkgs.buildGoModule {
        pname = "nudox-go-oracle";
        version = "0.1.0";
        src = workspaceRoot + "/frontends/go/src/legacy/oracle";
        vendorHash = "sha256-oZyvmlZ9m8v3h1UIL9s9Ko26yZF3f0muBF4nV4t3Y7o=";
        subPackages = [ "." ];
        doCheck = false;
      }
    else
      null;
  # TypeScript checker seam expected by `NUDOX_TYPESCRIPT_CHECKER_BIN`: the
  # vendored driver plus the pinned `typescript` npm package on `NODE_PATH`.
  # Null under the configuration-only flake for the same reason as `goOracle`.
  typescriptChecker =
    if workspaceAvailable then
      pkgs.writeScriptBin "nudox-typescript-checker" ''
        #!${pkgs.runtimeShell}
        export NODE_PATH="${pkgs.typescript}/lib/node_modules''${NODE_PATH:+:$NODE_PATH}"
        exec ${pkgs.nodejs_22}/bin/node ${workspaceRoot + "/frontends/typescript/src/legacy/checker/main.cjs"} "$@"
      ''
    else
      null;
  qualityTools = [
    pkgs.ast-grep
    pkgs.b3sum
    pkgs.cargo-audit
    pkgs.cargo-deny
    pkgs.cargo-nextest
    pkgs.coreutils
    pkgs.direnv
    pkgs.git
    pkgs.jq
    pkgs.koji
    pkgs.nixfmt
    pkgs.nix-direnv
    pkgs.nufmt
    pkgs.nushell
    pkgs.taplo
    pkgs.yamlfmt
  ];
  verifierTools = [
    pkgs.cargo-bloat
    cargoDylint
    dylintLink
    pkgs.cargo-llvm-cov
    pkgs.hyperfine
    pkgs.samply
  ]
  ++ pkgs.lib.optional (backendControl != null) backendControl
  ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.valgrind ];
  serviceTools = [
    pkgs.curl
    pkgs.qdrant
  ];
  observabilityTools = [
    pkgs.otel-cli
    pkgs.otel-desktop-viewer
    pkgs.opentelemetry-collector-contrib
  ];
  # Semantic-authority helpers must be realized by the shell that exports
  # their absolute paths, otherwise the environment variable would name a store
  # path that nothing built.
  authorityHelpers =
    pkgs.lib.optional (goOracle != null) goOracle
    ++ pkgs.lib.optional (typescriptChecker != null) typescriptChecker;
in
{
  inherit
    authorityHelpers
    backendControl
    cargoDylint
    compilers
    dylintLink
    goOracle
    nativeCompilers
    qualityTools
    serviceTools
    typescriptChecker
    observabilityTools
    verifierTools
    ;
  development = [ toolchains.stable ] ++ qualityTools ++ nativeCompilers ++ authorityHelpers;
  compiler = [ toolchains.stable ] ++ qualityTools ++ nativeCompilers ++ authorityHelpers;
  services = [ toolchains.stable ] ++ qualityTools ++ serviceTools;
  observability = [ toolchains.stable ] ++ qualityTools ++ observabilityTools;
  verification = [
    toolchains.stable
    toolchains.nightly
  ]
  ++ qualityTools
  ++ verifierTools;
  complete = [
    toolchains.stable
    toolchains.nightly
  ]
  ++ qualityTools
  ++ nativeCompilers
  ++ authorityHelpers
  ++ serviceTools
  ++ observabilityTools
  ++ verifierTools;
  nixFormatter = pkgs.nixfmt;
}

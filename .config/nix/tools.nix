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
    "vendor/gpui_ce_components"
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
          || builtins.elem relative (
            [
              "Cargo.toml"
              "Cargo.lock"
            ]
            ++ controlSourceRoots
          )
          || builtins.any (root: pkgs.lib.hasPrefix "${root}/" relative) controlSourceRoots
          # A nested root (`vendor/gpui_ce_components`) is only reached when
          # its ancestor directories survive the filter as well.
          || (type == "directory" && builtins.any (root: pkgs.lib.hasPrefix "${relative}/" root) controlSourceRoots);
      }
    else
      null;
  # A bounded pool of Cargo 1.97 build directories shares intermediate
  # artifacts without making parallel worktrees wait on one build lock.
  # sccache shares compiler results across the four isolated lanes; callers
  # wait or fail with status 75 when every lane is occupied.
  parallelCargo = pkgs.writeShellApplication {
    name = "cargo";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.git
      pkgs.sccache
      pkgs.gawk
      pkgs.procps
    ];
    text =
      builtins.replaceStrings
        [ "@cargo@" "@git@" "@sccache@" ]
        [ "${toolchains.stable}/bin/cargo" "${pkgs.git}/bin/git" "${pkgs.sccache}/bin/sccache" ]
        (builtins.readFile ../scripts/cargo-shared-cache.sh);
  };
  # `luna-tools` is the cheap, pinned command closure used to enter a lane
  # and run Cargo checks. Keep it independent from the backend command
  # surface, GUI capture stack, corpus, and service binaries: those are
  # separate products and must not be realized just to ask for `cargo
  # --version`. The full role command bundle remains available as
  # `commands.roleBundles."luna-pair"` for control-plane checks.
  lunaToolClosure = pkgs.buildEnv {
    name = "nudox-luna-tools";
    paths = [
      (pkgs.lib.hiPrio parallelCargo)
      toolchains.stable
      pkgs.bash
      pkgs.coreutils
      pkgs.fd
      pkgs.git
      pkgs.jq
      pkgs.nushell
      pkgs.pkg-config
      pkgs.ripgrep
      pkgs.stdenv.cc
      pkgs.libiconv
      pkgs.zlib
      pkgs.sccache
    ];
    pathsToLink = [
      "/bin"
      "/include"
      "/lib"
    ];
  };
  # `nix shell .#luna-tools` does not evaluate a development-shell hook, so
  # native Cargo builds must receive their link search path from the package
  # itself. Wrap both entry points: Cargo covers workspace commands while the
  # rustc wrapper keeps direct compiler probes reproducible.
  lunaTools = pkgs.symlinkJoin {
    name = "nudox-luna-tools-shell";
    paths = [ lunaToolClosure ];
    nativeBuildInputs = [ pkgs.makeWrapper ];
    postBuild = ''
      wrapProgram "$out/bin/cargo" \
        --set LIBRARY_PATH "${
          pkgs.lib.makeLibraryPath [
            pkgs.libiconv
            pkgs.zlib
          ]
        }"
      wrapProgram "$out/bin/rustc" \
        --set LIBRARY_PATH "${
          pkgs.lib.makeLibraryPath [
            pkgs.libiconv
            pkgs.zlib
          ]
        }"
    '';
  };
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
  # Cargo's locked Git sources are shared by the control binary and the
  # optional GUI runtime closure. GPUI CE and its component library are
  # crates.io packages now, so Cargo.lock checksums authenticate them and they
  # must not appear in this fixed-output Git map. Keeping the remaining Git
  # sources here prevents one package from silently accepting another source.
  gpuiOutputHashes = {
    "trustfall-0.8.1" = "sha256-YZwoezIrScE01mo+PqEWVi8hDZQwpm793bMQ4vizSXc=";
    "trustfall_core-0.8.1" = "sha256-YZwoezIrScE01mo+PqEWVi8hDZQwpm793bMQ4vizSXc=";
    "trustfall_derive-0.3.1" = "sha256-YZwoezIrScE01mo+PqEWVi8hDZQwpm793bMQ4vizSXc=";
  };
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
          outputHashes = gpuiOutputHashes;
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
  # Optional real protocol binaries for a GUI lane. They are intentionally a
  # separate closure from `gui-tools`, so obtaining pinned display/fonts/media
  # tools never builds the workspace. When present, these are the exact
  # locald/CLI/MCP binaries used by live parity journeys.
  guiRuntime =
    if workspaceAvailable then
      stableRustPlatform.buildRustPackage {
        pname = "nudox-gui-runtime";
        version = "0.1.0";
        src = workspaceSource;
        cargoBuildFlags = [
          "--package"
          "backend-locald"
          "--package"
          "backend-cli"
          "--package"
          "backend-mcp"
        ];
        cargoLock = {
          lockFile = workspaceRoot + "/Cargo.lock";
          outputHashes = gpuiOutputHashes;
        };
        doCheck = false;
        installPhase = ''
                    mkdir -p "$out/bin"
                    for binary in backend-locald backend-cli backend-mcp; do
                      if [ ! -x "target/release/$binary" ]; then
                        echo "gui runtime did not build expected $binary" >&2
                        exit 1
                      fi
                      cp "target/release/$binary" "$out/bin/$binary"
                    done
                    mkdir -p "$out/share/nudox"
                    cargo metadata --locked --offline --format-version 1 > "$out/share/nudox/cargo-metadata.json"
                    cargo tree --locked --offline --duplicates --prefix none > "$out/share/nudox/cargo-tree-duplicates.txt"
                    cat > "$out/share/nudox/resolved-sources.txt" <<'EOF'
          ${pkgs.lib.concatStringsSep "\n" (
            pkgs.lib.mapAttrsToList (name: hash: "resolved-source/${name}=${hash}") gpuiOutputHashes
          )}
          EOF
                    metadata_sha=$(sha256sum "$out/share/nudox/cargo-metadata.json" | cut -d' ' -f1)
                    tree_sha=$(sha256sum "$out/share/nudox/cargo-tree-duplicates.txt" | cut -d' ' -f1)
                    resolved_sha=$(sha256sum "$out/share/nudox/resolved-sources.txt" | cut -d' ' -f1)
                    graph_sha=$(cat "$out/share/nudox/cargo-metadata.json" "$out/share/nudox/cargo-tree-duplicates.txt" | sha256sum | cut -d' ' -f1)
                    cat > "$out/share/nudox/gpui-provenance.txt" <<EOF
          runtime-cargo-metadata-sha256=$metadata_sha
          runtime-cargo-tree-duplicates-sha256=$tree_sha
          dependency-graph-runtime-sha256=$graph_sha
          resolved-source-manifest-sha256=$resolved_sha
          EOF
                    cat > "$out/bin/nudox-gui-service" <<'EOF'
          #!/bin/sh
          set -eu
          action=''${1:-status}
          if [ "$#" -gt 0 ]; then shift; fi
          endpoint=''${NUDOX_GUI_LOCALD_ENDPOINT:-}
          workspace=''${NUDOX_GUI_WORKSPACE:-}
          while [ "$#" -gt 0 ]; do
            case "$1" in
              --endpoint) endpoint=''${2:?missing endpoint value}; shift 2 ;;
              --workspace) workspace=''${2:?missing workspace value}; shift 2 ;;
              *) echo "usage: nudox-gui-service <start|stop|status> --endpoint PATH [--workspace PATH]" >&2; exit 64 ;;
            esac
          done
          if [ -z "$endpoint" ]; then echo "NUDOX_GUI_LOCALD_ENDPOINT or --endpoint is required" >&2; exit 64; fi
          case "$endpoint" in unix:*) endpoint=''${endpoint#unix:} ;; esac
          if [ -z "$workspace" ]; then workspace=$(pwd)/.local/gui-live; fi
          pidfile="$endpoint.pid"
          ownerfile="$endpoint.owner"
          logfile="$endpoint.log"
          bin_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
          locald_executable="$bin_dir/backend-locald"
          process_start() {
            pid="$1"
            if [ -r "/proc/$pid/stat" ]; then
              awk '{print $22}' "/proc/$pid/stat"
            else
              ps -p "$pid" -o lstart= | sed 's/^ *//'
            fi
          }
          process_executable() {
            pid="$1"
            if [ -r "/proc/$pid/exe" ]; then
              readlink "/proc/$pid/exe"
            else
              ps -p "$pid" -o command= | awk '{print $1}'
            fi
          }
          acquire_lock() {
            lockdir="$pidfile.lock"
            if ! mkdir "$lockdir" 2>/dev/null; then
              echo "locald lifecycle is already owned: $endpoint" >&2
              exit 75
            fi
            trap 'rmdir "$lockdir"' EXIT HUP INT TERM
          }
          owner_value() {
            key="$1"
            sed -n "s/^$key=//p" "$ownerfile"
          }
          validate_owner() {
            if [ ! -f "$ownerfile" ]; then echo "locald owner record is missing: $ownerfile" >&2; return 75; fi
            owner_pid=$(owner_value pid)
            owner_nonce=$(owner_value nonce)
            owner_executable=$(owner_value executable)
            owner_endpoint=$(owner_value endpoint)
            owner_workspace=$(owner_value workspace)
            owner_start=$(owner_value process_start)
            if [ -z "$owner_pid" ] || [ -z "$owner_nonce" ] || [ -z "$owner_start" ]; then echo "locald owner record is incomplete" >&2; return 75; fi
            if [ "$owner_executable" != "$locald_executable" ] || [ "$owner_endpoint" != "$endpoint" ] || [ "$owner_workspace" != "$workspace" ]; then
              echo "locald owner record does not match executable, endpoint, or workspace" >&2
              return 75
            fi
            if ! kill -0 "$owner_pid" 2>/dev/null; then echo "locald owner pid is not running: $owner_pid" >&2; return 1; fi
            actual_executable=$(process_executable "$owner_pid")
            actual_start=$(process_start "$owner_pid")
            if [ "$actual_executable" != "$locald_executable" ] || [ "$actual_start" != "$owner_start" ]; then
              echo "refusing reused locald pid: $owner_pid" >&2
              return 75
            fi
          }
          case "$action" in
            start)
              mkdir -p "$(dirname -- "$endpoint")" "$workspace"
              acquire_lock
              if [ -f "$ownerfile" ]; then
                if validate_owner; then echo "locald already running: $(owner_value pid)" >&2; exit 0; fi
                echo "refusing to replace a stale or reused locald owner record: $ownerfile" >&2
                exit 75
              fi
              "$locald_executable" --endpoint "$endpoint" --workspace "$workspace" --profile builtin --idle-timeout-ms 0 >"$logfile" 2>&1 &
              pid=$!
              for attempt in $(seq 1 20); do kill -0 "$pid" 2>/dev/null && break; sleep 0.05; done
              if ! kill -0 "$pid" 2>/dev/null; then echo "backend-locald exited before ownership could be recorded" >&2; exit 1; fi
              nonce="$(date +%s)-$$-$pid"
              start_identity=$(process_start "$pid")
              pid_tmp="$pidfile.$$"
              owner_tmp="$ownerfile.$$"
              echo "$pid" > "$pid_tmp"
              mv -f "$pid_tmp" "$pidfile"
              {
                echo "pid=$pid"
                echo "nonce=$nonce"
                echo "executable=$locald_executable"
                echo "endpoint=$endpoint"
                echo "workspace=$workspace"
                echo "process_start=$start_identity"
              } > "$owner_tmp"
              mv -f "$owner_tmp" "$ownerfile"
              ;;
            stop)
              if [ ! -f "$ownerfile" ]; then exit 0; fi
              acquire_lock
              validate_owner
              pid=$(owner_value pid)
              kill "$pid"
              for attempt in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.05; done
              if kill -0 "$pid" 2>/dev/null; then echo "backend-locald did not stop cleanly; retaining owner record" >&2; exit 75; fi
              rm -f "$pidfile"
              rm -f "$ownerfile"
              ;;
            status)
              if [ ! -f "$ownerfile" ]; then echo "locald stopped" >&2; exit 1; fi
              acquire_lock
              validate_owner
              "$bin_dir/nudox-gui-probe" --endpoint "$endpoint" --workspace "$workspace"
              ;;
            *) echo "unknown service action: $action" >&2; exit 64 ;;
          esac
          EOF
                    cat > "$out/bin/nudox-gui-probe" <<'EOF'
          #!/bin/sh
          set -eu
          endpoint=''${NUDOX_GUI_LOCALD_ENDPOINT:-}
          workspace=''${NUDOX_GUI_WORKSPACE:-}
          while [ "$#" -gt 0 ]; do
            case "$1" in
              --endpoint) endpoint=''${2:?missing endpoint value}; shift 2 ;;
              --workspace) workspace=''${2:?missing workspace value}; shift 2 ;;
              *) echo "usage: nudox-gui-probe --endpoint PATH [--workspace PATH]" >&2; exit 64 ;;
            esac
          done
          if [ -z "$endpoint" ]; then echo "NUDOX_GUI_LOCALD_ENDPOINT or --endpoint is required" >&2; exit 64; fi
          case "$endpoint" in unix:*) endpoint=''${endpoint#unix:} ;; esac
          if [ -z "$workspace" ]; then workspace=$(pwd)/.local/gui-live; fi
          bin_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
          exec "$bin_dir/backend-cli" --endpoint "$endpoint" --workspace "$workspace" --format json health
          EOF
                    cat > "$out/bin/nudox-gui-service-test" <<'EOF'
          #!/bin/sh
          set -eu
          bin_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
          root=$(mktemp -d "''${TMPDIR:-/tmp}/nudox-gui-service-test.XXXXXX")
          endpoint="$root/locald.sock"
          workspace="$root/workspace"
          cleanup() { "$bin_dir/nudox-gui-service" stop --endpoint "$endpoint" --workspace "$workspace" >/dev/null 2>&1 || true; rm -rf "$root"; }
          trap cleanup EXIT HUP INT TERM
          "$bin_dir/nudox-gui-service" start --endpoint "$endpoint" --workspace "$workspace"
          "$bin_dir/nudox-gui-service" status --endpoint "$endpoint" --workspace "$workspace" >/dev/null
          mkdir "$endpoint.pid.lock"
          if "$bin_dir/nudox-gui-service" start --endpoint "$endpoint" --workspace "$workspace" >/dev/null 2>&1; then
            echo "service lock contention was not rejected" >&2
            exit 1
          fi
          rmdir "$endpoint.pid.lock"
          mv "$endpoint.owner" "$endpoint.owner.real"
          cat > "$endpoint.owner" <<OWNER
          pid=$$
          nonce=stale-test
          executable=$bin_dir/backend-locald
          endpoint=$endpoint
          workspace=$workspace
          process_start=stale-test
          OWNER
          if "$bin_dir/nudox-gui-service" status --endpoint "$endpoint" --workspace "$workspace" >/dev/null 2>&1; then
            echo "reused PID owner was not rejected" >&2
            exit 1
          fi
          mv "$endpoint.owner.real" "$endpoint.owner"
          "$bin_dir/nudox-gui-service" stop --endpoint "$endpoint" --workspace "$workspace"
          echo "service ownership, contention, stale-owner, and reused-PID checks passed"
          EOF
                    chmod 0555 "$out/bin/nudox-gui-service" "$out/bin/nudox-gui-probe" "$out/bin/nudox-gui-service-test"
        '';
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
  nativeCompilers = builtins.attrValues compilers ++ nativeLibraries ++ linuxDesktopLibraries;
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
        exec ${pkgs.nodejs_22}/bin/node ${
          workspaceRoot + "/frontends/typescript/src/legacy/checker/main.cjs"
        } "$@"
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
    gpuiOutputHashes
    guiRuntime
    lunaTools
    parallelCargo
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
  development = [
    (pkgs.lib.hiPrio parallelCargo)
    toolchains.stable
  ]
  ++ qualityTools
  ++ nativeCompilers
  ++ authorityHelpers;
  compiler = [
    (pkgs.lib.hiPrio parallelCargo)
    toolchains.stable
  ]
  ++ qualityTools
  ++ nativeCompilers
  ++ authorityHelpers;
  services = [
    (pkgs.lib.hiPrio parallelCargo)
    toolchains.stable
  ]
  ++ qualityTools
  ++ serviceTools;
  observability = [
    (pkgs.lib.hiPrio parallelCargo)
    toolchains.stable
  ]
  ++ qualityTools
  ++ observabilityTools;
  verification = [
    (pkgs.lib.hiPrio parallelCargo)
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
